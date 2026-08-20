use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

use agent_master_client::{AgentMaster, ProcessSpec, State, WindowSize};
use anyhow::{anyhow, bail, Result};
use clap::{Parser, Subcommand};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Parser)]
#[command(about = "Drives one agent-master program by hand")]
struct Cli {
    /// Address the agent-master program listens on.
    #[arg(long, env = "AGENT_MASTER_ADDR")]
    server: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the process.
    Start {
        /// Working directory, as agent-master sees it.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
        /// Environment variable to set on top of what agent-master itself runs with.
        #[arg(long = "env", value_name = "NAME=VALUE", value_parser = split_env)]
        env: Vec<(String, String)>,
        /// Height of the window, in rows.
        #[arg(long, default_value_t = 24)]
        rows: u16,
        /// Width of the window, in columns.
        #[arg(long, default_value_t = 80)]
        cols: u16,
        /// The command and its arguments.
        #[arg(required = true, last = true)]
        argv: Vec<String>,
    },
    /// Write the window to stdout, as the terminal would paint it.
    ///
    /// On a terminal it takes a screen of its own and follows the process until
    /// you press q; anywhere else it writes the bytes once, as they are.
    Window {
        /// Show the window as it is now instead of following it.
        #[arg(long)]
        freeze: bool,
    },
    /// Type into the process: the given text, or raw bytes read from stdin.
    ///
    /// Keys that are not plain text travel as the bytes a terminal sends for them:
    ///
    ///   up      $'\033[A'    Enter      $'\r'
    ///   down    $'\033[B'    Tab        $'\t'
    ///   right   $'\033[C'    Escape     $'\033'
    ///   left    $'\033[D'    Backspace  $'\177'
    ///                        Ctrl-C     $'\003'
    ///
    /// An application that has switched its cursor keys to the application mode
    /// wants $'\033OB' for down, and so on for the other three.
    ///
    /// The $'...' form is bash and zsh; anywhere else, pipe the bytes in instead:
    ///
    ///   printf '\033[B' | amctl input
    #[command(verbatim_doc_comment)]
    Input {
        /// What to type. Left out, the bytes are read from stdin instead.
        text: Option<String>,
    },
    /// Set the window size.
    Resize {
        /// Height of the window, in rows.
        #[arg(long)]
        rows: u16,
        /// Width of the window, in columns.
        #[arg(long)]
        cols: u16,
    },
    /// Stop the process, gently first and forcibly once the grace runs out.
    Stop {
        /// How long to wait for it to end gently, in milliseconds.
        #[arg(long, default_value_t = 5_000)]
        grace_ms: u64,
    },
    /// Report whether the process runs, or how it ended.
    State,
    /// Write out what the process last put on the clipboard.
    Clipboard,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut master = AgentMaster::connect(&cli.server).await?;
    match cli.command {
        Command::Start { cwd, env, rows, cols, argv } => {
            let (command, args) = argv.split_first().ok_or_else(|| anyhow!("no command given"))?;
            let spec = ProcessSpec {
                command: command.clone(),
                args: args.to_vec(),
                cwd,
                env: BTreeMap::from_iter(env),
                size: WindowSize { rows, cols },
            };
            println!("{}", describe(&master.start(spec).await?));
        }
        Command::Window { freeze } => show(&mut master, freeze).await?,
        Command::Input { text } => {
            let data = match text {
                Some(text) => text.into_bytes(),
                None => {
                    let mut data = Vec::new();
                    tokio::io::stdin().read_to_end(&mut data).await?;
                    data
                }
            };
            master.input(&data).await?;
        }
        Command::Resize { rows, cols } => master.resize(WindowSize { rows, cols }).await?,
        Command::Stop { grace_ms } => {
            println!("{}", describe(&master.stop(Duration::from_millis(grace_ms)).await?));
        }
        Command::State => println!("{}", describe(&master.state().await?)),
        Command::Clipboard => {
            let mut stdout = tokio::io::stdout();
            stdout.write_all(&master.clipboard().await?.unwrap_or_default()).await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}

async fn show(master: &mut AgentMaster, freeze: bool) -> Result<()> {
    use std::io::Write;

    let mut shown = master.window().await?.contents;
    if !(std::io::stdout().is_terminal() && std::io::stdin().is_terminal()) {
        std::io::stdout().write_all(&shown)?;
        return Ok(std::io::stdout().flush()?);
    }

    let _held = HeldScreen::enter()?;
    draw(&shown)?;
    loop {
        match key_within(REFRESH)? {
            Key::Pressed(b'q' | b'Q' | ESCAPE | INTERRUPT) | Key::Ended => return Ok(()),
            Key::Pressed(_) => continue,
            Key::Nothing => {}
        }
        if freeze {
            continue;
        }
        let next = master.window().await?.contents;
        if next != shown {
            draw(&next)?;
            shown = next;
        }
    }
}

fn draw(window: &[u8]) -> Result<()> {
    use std::io::Write;

    std::io::stdout().write_all(window)?;
    Ok(std::io::stdout().flush()?)
}

enum Key {
    Pressed(u8),
    Nothing,
    Ended,
}

fn key_within(patience: Duration) -> Result<Key> {
    use std::io::Read;

    let mut waiting = libc::pollfd { fd: libc::STDIN_FILENO, events: libc::POLLIN, revents: 0 };
    let ready = unsafe {
        libc::poll(&mut waiting, 1, i32::try_from(patience.as_millis()).unwrap_or(i32::MAX))
    };
    if ready < 0 {
        bail!("cannot wait for a key");
    }
    if ready == 0 {
        return Ok(Key::Nothing);
    }
    let mut key = [0u8; 1];
    match std::io::stdin().read(&mut key)? {
        0 => Ok(Key::Ended),
        _ => Ok(Key::Pressed(key[0])),
    }
}

const REFRESH: Duration = Duration::from_millis(100);
const ESCAPE: u8 = 0x1b;
const INTERRUPT: u8 = 0x03;
const ALTERNATE_SCREEN_ON: &[u8] = b"\x1b[?1049h";
const ALTERNATE_SCREEN_OFF: &[u8] = b"\x1b[?1049l";
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";

struct HeldScreen {
    settings: libc::termios,
}

impl HeldScreen {
    fn enter() -> Result<Self> {
        use std::io::Write;

        let mut settings: libc::termios = unsafe { std::mem::zeroed() };
        unsafe {
            if libc::tcgetattr(libc::STDIN_FILENO, &mut settings) != 0 {
                bail!("cannot read the terminal's settings");
            }
            let mut raw = settings;
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) != 0 {
                bail!("cannot take single keys from the terminal");
            }
        }
        std::io::stdout().write_all(ALTERNATE_SCREEN_ON)?;
        std::io::stdout().flush()?;
        Ok(Self { settings })
    }
}

impl Drop for HeldScreen {
    fn drop(&mut self) {
        use std::io::Write;

        let _ = std::io::stdout().write_all(ALTERNATE_SCREEN_OFF);
        let _ = std::io::stdout().write_all(SHOW_CURSOR);
        let _ = std::io::stdout().flush();
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.settings);
        }
    }
}

fn split_env(pair: &str) -> Result<(String, String)> {
    let (name, value) = pair.split_once('=').ok_or_else(|| anyhow!("expected NAME=VALUE"))?;
    Ok((name.to_string(), value.to_string()))
}

fn describe(state: &State) -> String {
    match state {
        State::NotStarted => "not started".to_string(),
        State::Running => "running".to_string(),
        State::Exited { code, signal, window } => {
            let ending = match (code, signal) {
                (Some(code), _) => format!("exit code {code}"),
                (None, Some(signal)) => format!("killed by signal {signal}"),
                (None, None) => "ended for an unknown reason".to_string(),
            };
            format!(
                "exited: {ending}; last window {}x{}, {} bytes",
                window.size.rows,
                window.size.cols,
                window.contents.len()
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_master_client::Window;

    fn window() -> Window {
        Window { size: WindowSize { rows: 24, cols: 80 }, contents: vec![b'x'; 12] }
    }

    use clap::CommandFactory;

    fn parse(argv: &[&str]) -> Cli {
        Cli::try_parse_from(argv).expect("parse")
    }

    #[test]
    fn the_command_line_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn starting_takes_the_command_after_a_double_dash_and_fills_in_the_rest() {
        let cli = parse(&["amctl", "--server", "here:1", "start", "--", "sh", "-c", "echo hi"]);
        let Command::Start { cwd, env, rows, cols, argv } = cli.command else {
            panic!("expected a start");
        };
        assert_eq!(cwd, PathBuf::from("."));
        assert!(env.is_empty());
        assert_eq!((rows, cols), (24, 80));
        assert_eq!(argv, vec!["sh", "-c", "echo hi"]);
    }

    #[test]
    fn environment_pairs_pile_up_and_the_last_of_a_name_wins() {
        let cli = parse(&[
            "amctl", "--server", "here:1", "start", "--env", "A=1", "--env", "A=2", "--env", "B=3",
            "--", "sh",
        ]);
        let Command::Start { env, .. } = cli.command else { panic!("expected a start") };
        assert_eq!(
            BTreeMap::from_iter(env),
            BTreeMap::from([
                ("A".to_string(), "2".to_string()),
                ("B".to_string(), "3".to_string())
            ])
        );
    }

    #[test]
    fn a_start_without_a_command_is_rejected() {
        assert!(Cli::try_parse_from(["amctl", "--server", "here:1", "start"]).is_err());
    }

    #[test]
    fn an_environment_pair_without_a_value_is_rejected_at_the_command_line() {
        assert!(Cli::try_parse_from([
            "amctl", "--server", "here:1", "start", "--env", "NOEQ", "--", "sh"
        ])
        .is_err());
    }

    #[test]
    fn stopping_waits_five_seconds_unless_told_otherwise() {
        let Command::Stop { grace_ms } = parse(&["amctl", "--server", "here:1", "stop"]).command
        else {
            panic!("expected a stop")
        };
        assert_eq!(grace_ms, 5_000);
    }

    #[test]
    fn the_address_may_come_from_the_environment_instead() {
        std::env::set_var("AGENT_MASTER_ADDR", "from-the-environment:1");
        assert_eq!(parse(&["amctl", "state"]).server, "from-the-environment:1");
        std::env::remove_var("AGENT_MASTER_ADDR");
    }

    #[test]
    fn an_environment_pair_splits_on_the_first_equals_sign() {
        assert_eq!(
            split_env("URL=http://x/?a=b").expect("split"),
            ("URL".to_string(), "http://x/?a=b".to_string())
        );
    }

    #[test]
    fn an_empty_value_is_still_a_pair() {
        assert_eq!(split_env("EMPTY=").expect("split"), ("EMPTY".to_string(), String::new()));
    }

    #[test]
    fn a_pair_without_an_equals_sign_is_rejected() {
        assert!(split_env("URL").is_err());
    }

    #[test]
    fn each_state_reads_plainly() {
        assert_eq!(describe(&State::NotStarted), "not started");
        assert_eq!(describe(&State::Running), "running");
        assert!(describe(&State::Exited { code: Some(7), signal: None, window: window() })
            .starts_with("exited: exit code 7; last window 24x80, 12 bytes"));
        assert!(describe(&State::Exited { code: None, signal: Some(9), window: window() })
            .contains("killed by signal 9"));
        assert!(describe(&State::Exited { code: None, signal: None, window: window() })
            .contains("ended for an unknown reason"));
    }
}
