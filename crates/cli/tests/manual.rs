use std::net::SocketAddr;
use std::process::{Output, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_master::{serve, Session};
use agent_master_client::{AgentMaster, ProcessSpec, State, WindowSize};
use base64::Engine;
use std::collections::BTreeMap;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::process::Command;

async fn listening() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("bound address");
    tokio::spawn(serve(listener, Arc::new(Session::new())));
    address
}

async fn amctl(address: &SocketAddr, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_amctl"))
        .arg("--server")
        .arg(address.to_string())
        .args(args)
        .output()
        .await
        .expect("run amctl")
}

fn base64_of(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn said(run: &Output) -> String {
    assert!(run.status.success(), "amctl failed: {}", String::from_utf8_lossy(&run.stderr));
    String::from_utf8_lossy(&run.stdout).trim_end().to_string()
}

async fn window_showing(address: &SocketAddr, needle: &str) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let run = amctl(address, &["window"]).await;
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        if String::from_utf8_lossy(&run.stdout).contains(needle) {
            return run.stdout;
        }
        assert!(Instant::now() < deadline, "the window never showed {needle:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn the_whole_cycle_can_be_driven_by_hand() {
    let address = listening().await;
    assert_eq!(said(&amctl(&address, &["state"]).await), "not started");

    let started = amctl(
        &address,
        &["start", "--rows", "30", "--cols", "100", "--env", "GREETING=hello", "--", "sh"],
    )
    .await;
    assert_eq!(said(&started), "running");

    amctl(&address, &["input", "printf \"$GREETING\\n\""]).await;
    let mut typing = Command::new(env!("CARGO_BIN_EXE_amctl"))
        .arg("--server")
        .arg(address.to_string())
        .arg("input")
        .stdin(Stdio::piped())
        .spawn()
        .expect("run amctl");
    typing.stdin.take().expect("stdin").write_all(b"\r").await.expect("write");
    assert!(typing.wait().await.expect("wait").success());

    window_showing(&address, "hello").await;
    assert!(said(&amctl(&address, &["resize", "--rows", "40", "--cols", "120"]).await).is_empty());
    amctl(&address, &["input", "stty size\r"]).await;
    window_showing(&address, "40 120").await;

    let stopped = said(&amctl(&address, &["stop", "--grace-ms", "500"]).await);
    assert!(stopped.starts_with("exited:"), "{stopped}");
    assert!(said(&amctl(&address, &["state"]).await).starts_with("exited:"));
}

#[tokio::test]
async fn what_is_typed_on_the_command_line_is_what_the_process_gets() {
    let address = listening().await;
    let cwd = std::env::temp_dir().join("amctl-cwd-test");
    std::fs::create_dir_all(&cwd).expect("create the working directory");

    let started = amctl(
        &address,
        &[
            "start",
            "--cwd",
            cwd.to_str().expect("a printable path"),
            "--rows",
            "30",
            "--cols",
            "100",
            "--env",
            "GREETING=privet",
            "--",
            "sh",
            "-c",
            "stty size; basename \"$PWD\"; echo \"$GREETING\"; sleep 5",
        ],
    )
    .await;
    assert_eq!(said(&started), "running");

    let window = String::from_utf8_lossy(&window_showing(&address, "privet").await).to_string();
    assert!(window.contains("30 100"), "{window}");
    assert!(window.contains("amctl-cwd-test"), "{window}");
}

#[tokio::test]
async fn the_window_comes_out_of_amctl_exactly_as_the_library_sees_it() {
    let address = listening().await;
    amctl(&address, &["start", "--", "sh", "-c", "printf '\\033[31mred\\033[0m'"]).await;
    let printed = window_showing(&address, "red").await;

    let mut master = AgentMaster::connect(address).await.expect("connect");
    assert_eq!(printed, master.window().await.expect("window").contents);
}

#[tokio::test]
async fn what_the_process_copied_comes_out_of_amctl_byte_for_byte() {
    let address = listening().await;
    let copied: Vec<u8> = b"\x00\xffhttps://claude.ai/oauth/authorize?code=true\n".to_vec();
    let payload = base64_of(&copied);
    let script = format!("printf '\\033]52;c;{payload}\\007'; printf 'copied\\n'");
    amctl(&address, &["start", "--", "sh", "-c", &script]).await;
    window_showing(&address, "copied").await;

    let run = amctl(&address, &["clipboard"]).await;
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(run.stdout, copied);
}

#[tokio::test]
async fn an_empty_clipboard_comes_out_of_amctl_as_nothing_at_all() {
    let address = listening().await;
    amctl(&address, &["start", "--", "sh", "-c", "read _"]).await;

    let run = amctl(&address, &["clipboard"]).await;
    assert!(said(&run).is_empty(), "{run:?}");
}

async fn amctl_under_a_terminal(watched: &SocketAddr, extra: &[&str]) -> AgentMaster {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("bound address");
    tokio::spawn(serve(listener, Arc::new(Session::new())));

    let mut seat = AgentMaster::connect(address).await.expect("connect");
    let spec = ProcessSpec {
        command: env!("CARGO_BIN_EXE_amctl").to_string(),
        args: ["--server", &watched.to_string(), "window"]
            .into_iter()
            .chain(extra.iter().copied())
            .map(str::to_string)
            .collect(),
        cwd: std::env::temp_dir(),
        env: BTreeMap::from([("TERM".to_string(), "xterm-256color".to_string())]),
        size: WindowSize { rows: 24, cols: 80 },
    };
    seat.start(spec).await.expect("start amctl under a terminal");
    seat
}

#[tokio::test]
async fn a_window_shown_on_a_terminal_waits_for_the_reader() {
    let watched = listening().await;
    amctl(&watched, &["start", "--", "sh", "-c", "printf 'ON THE WATCHED SCREEN\\n'; read _"])
        .await;
    window_showing(&watched, "ON THE WATCHED SCREEN").await;

    let mut seat = amctl_under_a_terminal(&watched, &[]).await;

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let window = seat.window().await.expect("window");
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(&window.contents);
        if parser.screen().contents().contains("ON THE WATCHED SCREEN") {
            break;
        }
        assert!(Instant::now() < deadline, "amctl never drew the window it was given");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(seat.state().await.expect("state"), State::Running, "amctl should be waiting");

    seat.input(b"q").await.expect("input");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let State::Exited { code, .. } = seat.state().await.expect("state") {
            assert_eq!(code, Some(0));
            break;
        }
        assert!(Instant::now() < deadline, "amctl did not leave on q");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn seat_showing(seat: &mut AgentMaster, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let window = seat.window().await.expect("window");
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(&window.contents);
        if parser.screen().contents().contains(needle) {
            return;
        }
        assert!(Instant::now() < deadline, "the shown window never held {needle:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn seat_never_shows(seat: &mut AgentMaster, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let window = seat.window().await.expect("window");
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(&window.contents);
        assert!(!parser.screen().contents().contains(needle), "the shown window followed along");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn the_terminal_is_left_as_it_was_found() {
    let watched = listening().await;
    let paints_and_hides = "printf '\\033[?25l\\033[31mRED AND HIDDEN\\n'; read _";
    amctl(&watched, &["start", "--", "sh", "-c", paints_and_hides]).await;
    window_showing(&watched, "RED AND HIDDEN").await;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("bound address");
    tokio::spawn(serve(listener, Arc::new(Session::new())));
    let mut seat = AgentMaster::connect(address).await.expect("connect");
    let after_the_view = format!(
        "{} --server {watched} window; printf 'AFTER\\r\\n'; read _",
        env!("CARGO_BIN_EXE_amctl")
    );
    let spec = ProcessSpec {
        command: "sh".to_string(),
        args: vec!["-c".to_string(), after_the_view],
        cwd: std::env::temp_dir(),
        env: BTreeMap::from([("TERM".to_string(), "xterm-256color".to_string())]),
        size: WindowSize { rows: 24, cols: 80 },
    };
    seat.start(spec).await.expect("start");
    seat_showing(&mut seat, "RED AND HIDDEN").await;

    seat.input(b"q").await.expect("input");
    seat_showing(&mut seat, "AFTER").await;

    let window = seat.window().await.expect("window");
    let mut parser = vt100::Parser::new(24, 80, 0);
    parser.process(&window.contents);
    let screen = parser.screen();
    assert!(!screen.hide_cursor(), "the terminal was left without a cursor");
    let after = (0..24)
        .flat_map(|row| (0..80).map(move |col| (row, col)))
        .find(|&(row, col)| screen.cell(row, col).is_some_and(|cell| cell.contents() == "A"));
    let (row, col) = after.expect("what the shell printed afterwards");
    assert_eq!(
        screen.cell(row, col).expect("a painted cell").fgcolor(),
        vt100::Color::Default,
        "the terminal was left painting in the process's colour"
    );
}

#[tokio::test]
async fn a_shown_window_follows_the_process() {
    let watched = listening().await;
    let script = "printf 'FIRST LINE\\n'; read _; printf 'SECOND LINE\\n'; read _";
    amctl(&watched, &["start", "--", "sh", "-c", script]).await;
    window_showing(&watched, "FIRST LINE").await;

    let mut seat = amctl_under_a_terminal(&watched, &[]).await;
    seat_showing(&mut seat, "FIRST LINE").await;

    amctl(&watched, &["input", "\r"]).await;
    seat_showing(&mut seat, "SECOND LINE").await;
}

#[tokio::test]
async fn a_frozen_window_stays_as_it_was() {
    let watched = listening().await;
    let script = "printf 'FIRST LINE\\n'; read _; printf 'SECOND LINE\\n'; read _";
    amctl(&watched, &["start", "--", "sh", "-c", script]).await;
    window_showing(&watched, "FIRST LINE").await;

    let mut seat = amctl_under_a_terminal(&watched, &["--freeze"]).await;
    seat_showing(&mut seat, "FIRST LINE").await;

    amctl(&watched, &["input", "\r"]).await;
    window_showing(&watched, "SECOND LINE").await;
    seat_never_shows(&mut seat, "SECOND LINE").await;
}

#[tokio::test]
async fn an_address_nothing_listens_on_ends_amctl_with_a_complaint() {
    let unused = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = unused.local_addr().expect("bound address");
    drop(unused);

    let run = amctl(&address, &["state"]).await;
    assert!(!run.status.success());
    let complaint = String::from_utf8_lossy(&run.stderr);
    assert!(complaint.contains("cannot reach agent-master"), "{complaint}");
}

#[tokio::test]
async fn a_refusal_ends_amctl_with_the_reason() {
    let address = listening().await;
    let run = amctl(&address, &["input", "nothing is running"]).await;
    assert!(!run.status.success());
    let complaint = String::from_utf8_lossy(&run.stderr);
    assert!(complaint.contains("no process has been started"), "{complaint}");
}
