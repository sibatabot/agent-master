use std::os::unix::process::ExitStatusExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as SyncMutex, MutexGuard, PoisonError};
use std::time::Duration;

use agent_master_protocol::{ProcessSpec, Refusal, State, Window, WindowSize};
use anyhow::Context;
use base64::Engine;
use pty_process::{OwnedReadPty, OwnedWritePty};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{watch, Mutex};
use tokio::task::{AbortHandle, JoinHandle};

/// The emulator panics on a grid smaller than this.
pub const LEAST_WINDOW: WindowSize = WindowSize { rows: 2, cols: 2 };

const LAST_FRAME_WAIT: Duration = Duration::from_millis(200);

type Screen = SyncMutex<vt100::Parser<Clipboard>>;
type Outcome<T> = std::result::Result<T, Refusal>;

#[derive(Default)]
struct Clipboard(Option<Vec<u8>>);

impl vt100::Callbacks for Clipboard {
    fn copy_to_clipboard(&mut self, _: &mut vt100::Screen, _selection: &[u8], data: &[u8]) {
        match base64::engine::general_purpose::STANDARD.decode(data) {
            Ok(copied) => self.0 = Some(copied),
            Err(error) => log::warn!("the process copied something unreadable: {error}"),
        }
    }
}

#[derive(Debug, Clone)]
struct Exit {
    code: Option<i32>,
    signal: Option<i32>,
    window: Window,
}

struct Terminal {
    pty: Mutex<OwnedWritePty>,
    screen: Screen,
}

impl Terminal {
    fn open(size: WindowSize) -> Outcome<(Arc<Self>, pty_process::Pts, JoinHandle<()>)> {
        let (pty, pts) = pty_process::open().context("opening a terminal").map_err(failed)?;
        let (read, write) = pty.into_split();
        let mut terminal = Self {
            pty: Mutex::new(write),
            screen: SyncMutex::new(vt100::Parser::new_with_callbacks(
                LEAST_WINDOW.rows,
                LEAST_WINDOW.cols,
                0,
                Clipboard::default(),
            )),
        };
        Self::fit(terminal.pty.get_mut(), &terminal.screen, size)?;

        let terminal = Arc::new(terminal);
        let painting = tokio::spawn(terminal.clone().paint(read));
        Ok((terminal, pts, painting))
    }

    async fn type_in(&self, data: &[u8]) -> Outcome<()> {
        self.pty
            .lock()
            .await
            .write_all(data)
            .await
            .context("typing into the terminal")
            .map_err(failed)
    }

    async fn resize(&self, size: WindowSize) -> Outcome<()> {
        let pty = self.pty.lock().await;
        Self::fit(&pty, &self.screen, size)
    }

    fn window(&self) -> Window {
        let parser = painted(&self.screen);
        let (rows, cols) = parser.screen().size();
        Window { size: WindowSize { rows, cols }, contents: parser.screen().contents_formatted() }
    }

    fn clipboard(&self) -> Option<Vec<u8>> {
        painted(&self.screen).callbacks().0.clone()
    }

    async fn paint(self: Arc<Self>, mut pty: OwnedReadPty) {
        let mut buf = [0u8; 4096];
        loop {
            match pty.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(read) => {
                    // The emulator is the only reader of the terminal. If a byte
                    // sequence it cannot digest took the reader down with it, the
                    // process would fill the terminal and hang; losing the frame
                    // is the lesser harm.
                    let painting = std::panic::AssertUnwindSafe(|| {
                        painted(&self.screen).process(&buf[..read])
                    });
                    if std::panic::catch_unwind(painting).is_err() {
                        log::error!(
                            "the emulator could not take {read} bytes of what the process printed"
                        );
                    }
                }
            }
        }
    }

    fn fit(pty: &OwnedWritePty, screen: &Screen, size: WindowSize) -> Outcome<()> {
        if size.rows < LEAST_WINDOW.rows || size.cols < LEAST_WINDOW.cols {
            return Err(Refusal::UnusableWindowSize { least: LEAST_WINDOW });
        }
        painted(screen).screen_mut().set_size(size.rows, size.cols);
        pty.resize(pty_process::Size::new(size.rows, size.cols))
            .context("sizing the terminal")
            .map_err(failed)
    }
}

struct Process {
    terminal: Arc<Terminal>,
    leader: i32,
    pid_released: Arc<AtomicBool>,
    exit: watch::Receiver<Option<Exit>>,
    painting: AbortHandle,
}

impl Drop for Process {
    fn drop(&mut self) {
        self.painting.abort();
    }
}

impl Process {
    fn running(&self) -> bool {
        self.exit.borrow().is_none()
    }

    fn window(&self) -> Window {
        match &*self.exit.borrow() {
            None => self.terminal.window(),
            Some(exit) => exit.window.clone(),
        }
    }

    fn state(&self) -> State {
        match &*self.exit.borrow() {
            None => State::Running,
            Some(exit) => {
                State::Exited { code: exit.code, signal: exit.signal, window: exit.window.clone() }
            }
        }
    }

    async fn input(&self, data: &[u8]) -> Outcome<()> {
        if !self.running() {
            return Err(Refusal::AlreadyExited);
        }
        self.terminal.type_in(data).await
    }

    async fn resize(&self, size: WindowSize) -> Outcome<()> {
        if !self.running() {
            return Err(Refusal::AlreadyExited);
        }
        self.terminal.resize(size).await
    }

    fn clipboard(&self) -> Option<Vec<u8>> {
        self.terminal.clipboard()
    }

    async fn stop(&self, grace: Duration) -> State {
        if self.running() {
            let mut exit = self.exit.clone();
            self.signal_session(libc::SIGTERM);
            if tokio::time::timeout(grace, ended(&mut exit)).await.is_err() {
                self.signal_session(libc::SIGKILL);
                ended(&mut exit).await;
            }
        }
        self.state()
    }

    fn signal_session(&self, signal: i32) {
        if self.pid_released.load(Ordering::SeqCst) {
            return;
        }
        unsafe {
            libc::kill(-self.leader, signal);
        }
    }
}

#[derive(Default)]
pub struct Session {
    process: Mutex<Option<Arc<Process>>>,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn start(&self, spec: ProcessSpec) -> Outcome<State> {
        let mut slot = self.process.lock().await;
        if slot.as_ref().is_some_and(|process| process.running()) {
            return Err(Refusal::AlreadyRunning);
        }
        let process = spawn(spec)?;
        let state = process.state();
        *slot = Some(Arc::new(process));
        Ok(state)
    }

    pub async fn window(&self) -> Outcome<Window> {
        Ok(self.current().await?.window())
    }

    pub async fn clipboard(&self) -> Outcome<Option<Vec<u8>>> {
        Ok(self.current().await?.clipboard())
    }

    pub async fn input(&self, data: &[u8]) -> Outcome<()> {
        self.current().await?.input(data).await
    }

    pub async fn resize(&self, size: WindowSize) -> Outcome<()> {
        self.current().await?.resize(size).await
    }

    pub async fn stop(&self, grace: Duration) -> Outcome<State> {
        Ok(self.current().await?.stop(grace).await)
    }

    pub async fn state(&self) -> State {
        self.process.lock().await.as_ref().map_or(State::NotStarted, |process| process.state())
    }

    async fn current(&self) -> Outcome<Arc<Process>> {
        self.process.lock().await.clone().ok_or(Refusal::NothingStarted)
    }
}

fn spawn(spec: ProcessSpec) -> Outcome<Process> {
    let (terminal, pts, painting) = Terminal::open(spec.size)?;

    let child = pty_process::Command::new(&spec.command)
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .envs(spec.env)
        .spawn(pts)
        .with_context(|| format!("spawning {}", spec.command))
        .map_err(failed)?;
    let pid = child
        .id()
        .context("the spawned process has no pid")
        .and_then(|pid| i32::try_from(pid).context("the spawned process has an unreadable pid"))
        .map_err(failed)?;

    let (exit_tx, exit_rx) = watch::channel(None);
    let abandon = painting.abort_handle();
    let pid_released = Arc::new(AtomicBool::new(false));
    tokio::spawn(reap(child, painting, terminal.clone(), pid_released.clone(), exit_tx));

    Ok(Process { terminal, leader: pid, pid_released, exit: exit_rx, painting: abandon })
}

fn painted(screen: &Screen) -> MutexGuard<'_, vt100::Parser<Clipboard>> {
    screen.lock().unwrap_or_else(PoisonError::into_inner)
}

fn failed(error: anyhow::Error) -> Refusal {
    Refusal::Failed { message: format!("{error:#}") }
}

async fn ended(exit: &mut watch::Receiver<Option<Exit>>) {
    while exit.borrow().is_none() {
        if exit.changed().await.is_err() {
            return;
        }
    }
}

async fn reap(
    mut child: tokio::process::Child,
    painting: JoinHandle<()>,
    terminal: Arc<Terminal>,
    pid_released: Arc<AtomicBool>,
    exit: watch::Sender<Option<Exit>>,
) {
    let status = child.wait().await;
    pid_released.store(true, Ordering::SeqCst);
    let _ = tokio::time::timeout(LAST_FRAME_WAIT, painting).await;
    let (code, signal) = match status {
        Ok(status) => (status.code(), status.signal()),
        Err(error) => {
            log::error!("cannot tell how the process ended: {error}");
            (None, None)
        }
    };
    let _ = exit.send(Some(Exit { code, signal, window: terminal.window() }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_terminal_left_poisoned_is_still_readable() {
        let (terminal, _pts, _painting) = Terminal::open(LEAST_WINDOW).expect("open a terminal");
        std::thread::scope(|threads| {
            threads
                .spawn(|| {
                    let _guard = terminal.screen.lock().expect("lock");
                    panic!("something went wrong while painting");
                })
                .join()
                .expect_err("the painting thread should have panicked");
        });

        assert!(terminal.screen.lock().is_err(), "the screen should be poisoned by now");
        assert_eq!(terminal.window().size, LEAST_WINDOW);
    }
}
