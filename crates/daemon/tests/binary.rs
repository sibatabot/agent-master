use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use agent_master_client::{AgentMaster, State};

fn lately_free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.local_addr().expect("bound address").port()
}

async fn answering(address: &str) -> Option<AgentMaster> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(mut master) = AgentMaster::connect(address).await {
            if master.state().await.is_ok() {
                return Some(master);
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    None
}

enum Told {
    Flag,
    Environment,
    Both,
}

const UNBINDABLE: &str = "127.0.0.1:1";

async fn started(told: Told) -> (Daemon, AgentMaster) {
    for _ in 0..5 {
        let address = format!("127.0.0.1:{}", lately_free_port());
        let mut command = Command::new(env!("CARGO_BIN_EXE_svora-am"));
        match told {
            Told::Flag => {
                command.args(["--listen", &address]);
            }
            Told::Environment => {
                command.env("AGENT_MASTER_LISTEN", &address);
            }
            Told::Both => {
                command.env("AGENT_MASTER_LISTEN", UNBINDABLE).args(["--listen", &address]);
            }
        }
        let daemon = Daemon(command.spawn().expect("run agent-master"));
        if let Some(master) = answering(&address).await {
            return (daemon, master);
        }
    }
    panic!("agent-master never came up");
}

struct Daemon(std::process::Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn the_program_serves_the_address_it_was_given() {
    let (_daemon, mut master) = started(Told::Flag).await;
    assert_eq!(master.state().await.expect("state"), State::NotStarted);
}

#[tokio::test]
async fn the_address_may_come_from_the_environment_instead() {
    let (_daemon, mut master) = started(Told::Environment).await;
    assert_eq!(master.state().await.expect("state"), State::NotStarted);
}

#[tokio::test]
async fn the_given_address_wins_over_the_one_in_the_environment() {
    let (_daemon, mut master) = started(Told::Both).await;
    assert_eq!(master.state().await.expect("state"), State::NotStarted);
}

#[tokio::test]
async fn the_program_says_which_address_it_ended_up_on() {
    let mut daemon = tokio::process::Command::new(env!("CARGO_BIN_EXE_svora-am"))
        .args(["--listen", "127.0.0.1:0"])
        .env("RUST_LOG", "info")
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("run agent-master");

    let said = daemon.stderr.take().expect("stderr");
    let mut lines = tokio::io::AsyncBufReadExt::lines(tokio::io::BufReader::new(said));
    let announcement = loop {
        let line = lines.next_line().await.expect("read").expect("a line");
        if line.contains("listening on") {
            break line;
        }
    };
    let address = announcement
        .rsplit_once("listening on ")
        .map(|(_, address)| address.trim().to_string())
        .expect("an address in the announcement");
    assert!(!address.ends_with(":0"), "the announced address is the asked-for one: {announcement}");

    let mut master = answering(&address).await.expect("the announced address answers");
    assert_eq!(master.state().await.expect("state"), State::NotStarted);
}

#[test]
fn an_address_it_cannot_have_is_reported_and_ends_the_program() {
    let taken = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = taken.local_addr().expect("bound address").to_string();

    let run = Command::new(env!("CARGO_BIN_EXE_svora-am"))
        .args(["--listen", &address])
        .stderr(Stdio::piped())
        .output()
        .expect("run agent-master");

    assert!(!run.status.success(), "taking a busy address should have failed");
    let complaint = String::from_utf8_lossy(&run.stderr);
    assert!(complaint.contains(&address), "{complaint}");
}

#[test]
fn an_address_it_was_never_given_is_reported() {
    let run = Command::new(env!("CARGO_BIN_EXE_svora-am"))
        .env_remove("AGENT_MASTER_LISTEN")
        .stderr(Stdio::piped())
        .output()
        .expect("run agent-master");

    assert!(!run.status.success(), "running with no address should have failed");
    let complaint = String::from_utf8_lossy(&run.stderr);
    assert!(complaint.contains("--listen"), "{complaint}");
}
