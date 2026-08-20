use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use std::collections::BTreeMap;
use std::path::PathBuf;

use agent_master_client::{AgentMaster, Error, ProcessSpec, Refusal, WindowSize};
use agent_master_protocol::{write_message, Response, State, Window};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

type Asked = Arc<Mutex<Vec<String>>>;

async fn answering(answer: Response) -> SocketAddr {
    listening(Some(Frame::Reply(answer))).await.0
}

async fn hanging_up() -> SocketAddr {
    listening(None).await.0
}

async fn saying(answer: &'static str) -> SocketAddr {
    listening(Some(Frame::Raw(answer))).await.0
}

enum Frame {
    Reply(Response),
    Raw(&'static str),
}

async fn listening(answer: Option<Frame>) -> (SocketAddr, Asked) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("bound address");
    let asked: Asked = Arc::new(Mutex::new(Vec::new()));
    let heard = asked.clone();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (read, mut write) = stream.into_split();
        let mut request = String::new();
        BufReader::new(read).read_line(&mut request).await.expect("read");
        heard.lock().expect("heard").push(request);
        match answer {
            Some(Frame::Reply(reply)) => {
                write_message(&mut write, &reply).await.expect("write");
            }
            Some(Frame::Raw(line)) => write.write_all(line.as_bytes()).await.expect("write"),
            None => write.shutdown().await.expect("hang up"),
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    (address, asked)
}

fn window() -> Window {
    Window { size: WindowSize { rows: 24, cols: 80 }, contents: b"painted".to_vec() }
}

#[tokio::test]
async fn an_address_nothing_listens_on_is_reported() {
    let unused = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = unused.local_addr().expect("bound address");
    drop(unused);

    let error = AgentMaster::connect(address).await.expect_err("connect");
    assert!(matches!(error, Error::Unreachable(_)), "{error}");
    assert!(error.to_string().starts_with("cannot reach agent-master"), "{error}");
}

#[tokio::test]
async fn a_peer_that_hangs_up_is_reported() {
    let mut master = AgentMaster::connect(hanging_up().await).await.expect("connect");
    let error = master.state().await.expect_err("state");
    assert!(matches!(error, Error::HungUp), "{error}");
}

#[tokio::test]
async fn an_answer_that_is_not_a_reply_is_reported() {
    let mut master = AgentMaster::connect(saying("gibberish\n").await).await.expect("connect");
    let error = master.state().await.expect_err("state");
    match error {
        Error::Unreadable(complaint) => assert!(complaint.contains("gibberish"), "{complaint}"),
        other => panic!("{other}"),
    }
}

#[tokio::test]
async fn a_clipboard_that_is_not_readable_text_is_reported() {
    let address = saying("{\"reply\":\"clipboard\",\"data\":\"!!!!\"}\n").await;
    let mut master = AgentMaster::connect(address).await.expect("connect");
    let error = master.clipboard().await.expect_err("clipboard");
    assert!(matches!(error, Error::Unreadable(_)), "{error}");
}

#[tokio::test]
async fn a_refusal_arrives_as_itself() {
    let refused = Response::Refused(Refusal::AlreadyRunning);
    let mut master = AgentMaster::connect(answering(refused).await).await.expect("connect");
    let error = master.state().await.expect_err("state");
    assert!(matches!(error, Error::Refused(Refusal::AlreadyRunning)), "{error}");
}

#[tokio::test]
async fn an_answer_to_a_different_question_is_reported() {
    let address = answering(Response::Done).await;
    let mut master = AgentMaster::connect(address).await.expect("connect");
    let error = master.state().await.expect_err("state");
    assert!(matches!(error, Error::Mismatched { asked: "state", answer: "done" }), "{error}");

    let address = answering(Response::Window(window())).await;
    let mut master = AgentMaster::connect(address).await.expect("connect");
    let error = master.input(b"x").await.expect_err("input");
    assert!(matches!(error, Error::Mismatched { asked: "input", answer: "a window" }), "{error}");

    let address = answering(Response::State(State::Running)).await;
    let mut master = AgentMaster::connect(address).await.expect("connect");
    let error = master.window().await.expect_err("window");
    assert!(matches!(error, Error::Mismatched { asked: "window", answer: "a state" }), "{error}");

    let address = answering(Response::Done).await;
    let mut master = AgentMaster::connect(address).await.expect("connect");
    let error = master.stop(Duration::from_secs(1)).await.expect_err("stop");
    assert!(matches!(error, Error::Mismatched { asked: "stop", answer: "done" }), "{error}");

    let address = answering(Response::Window(window())).await;
    let mut master = AgentMaster::connect(address).await.expect("connect");
    let size = WindowSize { rows: 10, cols: 40 };
    let error = master.resize(size).await.expect_err("resize");
    assert!(matches!(error, Error::Mismatched { asked: "resize", answer: "a window" }), "{error}");

    let address = answering(Response::Done).await;
    let mut master = AgentMaster::connect(address).await.expect("connect");
    let error = master.clipboard().await.expect_err("clipboard");
    assert!(matches!(error, Error::Mismatched { asked: "clipboard", answer: "done" }), "{error}");

    let address = answering(Response::Clipboard { data: None }).await;
    let mut master = AgentMaster::connect(address).await.expect("connect");
    let error = master.state().await.expect_err("state");
    assert!(
        matches!(error, Error::Mismatched { asked: "state", answer: "a clipboard" }),
        "{error}"
    );
}

#[tokio::test]
async fn an_answer_to_a_start_that_is_not_a_state_is_reported() {
    let address = answering(Response::Done).await;
    let mut master = AgentMaster::connect(address).await.expect("connect");
    let spec = ProcessSpec {
        command: "sh".to_string(),
        args: Vec::new(),
        cwd: PathBuf::from("/"),
        env: BTreeMap::new(),
        size: WindowSize { rows: 24, cols: 80 },
    };
    let error = master.start(spec).await.expect_err("start");
    assert!(matches!(error, Error::Mismatched { asked: "start", answer: "done" }), "{error}");
}

#[tokio::test]
async fn a_peer_that_went_away_is_reported_on_the_next_question_too() {
    let mut master = AgentMaster::connect(hanging_up().await).await.expect("connect");
    assert!(matches!(master.state().await.expect_err("state"), Error::HungUp));
    let error = master.state().await.expect_err("state again");
    assert!(matches!(error, Error::Unreachable(_) | Error::HungUp), "{error}");
}

#[tokio::test]
async fn the_grace_travels_as_whole_milliseconds_and_never_overflows() {
    for (grace, expected) in [
        (Duration::from_millis(250), "\"grace_ms\":250"),
        (Duration::MAX, &format!("\"grace_ms\":{}", u64::MAX)[..]),
    ] {
        let (address, asked) = listening(Some(Frame::Reply(Response::State(State::Running)))).await;
        let mut master = AgentMaster::connect(address).await.expect("connect");
        let _ = master.stop(grace).await;
        let heard = asked.lock().expect("heard").concat();
        assert!(heard.contains(expected), "{heard}");
    }
}

#[test]
fn every_complaint_reads_plainly() {
    let unreachable =
        Error::Unreachable(std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "no one"));
    assert_eq!(unreachable.to_string(), "cannot reach agent-master: no one");
    assert_eq!(Error::HungUp.to_string(), "agent-master hung up");
    assert_eq!(
        Error::Unreadable("gibberish".to_string()).to_string(),
        "agent-master said something unreadable: gibberish"
    );
    assert_eq!(
        Error::Refused(Refusal::AlreadyRunning).to_string(),
        "agent-master refused: a process is already running"
    );
    assert_eq!(
        Error::Mismatched { asked: "state", answer: "done" }.to_string(),
        "agent-master answered done to state"
    );
}
