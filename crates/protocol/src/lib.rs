use std::collections::BTreeMap;
use std::path::PathBuf;

use base64::Engine;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowSize {
    pub rows: u16,
    pub cols: u16,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ProcessSpec {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub size: WindowSize,
}

/// `contents` is a byte stream that repaints the window in full — colours,
/// attributes and cursor included — when fed to a terminal.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub size: WindowSize,
    #[serde(with = "base64_bytes")]
    pub contents: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum State {
    NotStarted,
    Running,
    Exited { code: Option<i32>, signal: Option<i32>, window: Window },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Start(ProcessSpec),
    Window,
    Input {
        #[serde(with = "base64_bytes")]
        data: Vec<u8>,
    },
    Resize {
        size: WindowSize,
    },
    Stop {
        grace_ms: u64,
    },
    State,
    Clipboard,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Response {
    Done,
    Window(Window),
    State(State),
    Clipboard {
        #[serde(with = "base64_bytes_or_nothing")]
        data: Option<Vec<u8>>,
    },
    Refused(Refusal),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "fault", rename_all = "snake_case")]
pub enum Refusal {
    NothingStarted,
    AlreadyRunning,
    AlreadyExited,
    UnusableWindowSize { least: WindowSize },
    Failed { message: String },
    Unreadable { message: String },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NothingStarted => write!(out, "no process has been started"),
            Self::AlreadyRunning => write!(out, "a process is already running"),
            Self::AlreadyExited => write!(out, "the process has already exited"),
            Self::UnusableWindowSize { least } => {
                write!(out, "a window smaller than {}x{} cannot be drawn", least.rows, least.cols)
            }
            Self::Failed { message } | Self::Unreadable { message } => write!(out, "{message}"),
        }
    }
}

pub enum Incoming<T> {
    Message(T),
    Unreadable(String),
    Ended,
}

pub async fn write_message<W, T>(sink: &mut W, message: &T) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
    T: Serialize,
{
    use tokio::io::AsyncWriteExt;

    let mut line = serde_json::to_string(message).expect("wire messages are always serializable");
    line.push('\n');
    sink.write_all(line.as_bytes()).await
}

pub async fn read_message<R, T>(source: &mut R) -> std::io::Result<Incoming<T>>
where
    R: tokio::io::AsyncBufRead + Unpin,
    T: DeserializeOwned,
{
    use tokio::io::AsyncBufReadExt;

    let mut line = String::new();
    if source.read_line(&mut line).await? == 0 {
        return Ok(Incoming::Ended);
    }
    let line = line.trim_end_matches(['\r', '\n']);
    Ok(match serde_json::from_str(line) {
        Ok(message) => Incoming::Message(message),
        Err(error) => Incoming::Unreadable(format!("{error}: {line}")),
    })
}

fn to_text(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn from_text<E: serde::de::Error>(text: &str) -> Result<Vec<u8>, E> {
    base64::engine::general_purpose::STANDARD.decode(text).map_err(E::custom)
}

mod base64_bytes_or_nothing {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        bytes: &Option<Vec<u8>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match bytes {
            Some(bytes) => serializer.serialize_some(&super::to_text(bytes)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Vec<u8>>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(|encoded| super::from_text(&encoded))
            .transpose()
    }
}

mod base64_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&super::to_text(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        super::from_text(&String::deserialize(deserializer)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> Window {
        Window { size: WindowSize { rows: 24, cols: 80 }, contents: vec![0x1b, b'[', b'2', b'J'] }
    }

    async fn wire<T: Serialize>(message: &T) -> Vec<u8> {
        let mut wire = Vec::new();
        write_message(&mut wire, message).await.expect("write");
        wire
    }

    async fn round_trip<T: Serialize + DeserializeOwned>(message: &T) -> T {
        let wire = wire(message).await;
        assert!(wire.ends_with(b"\n"), "{:?}", String::from_utf8_lossy(&wire));
        match read_message(&mut wire.as_slice()).await.expect("read") {
            Incoming::Message(back) => back,
            Incoming::Unreadable(complaint) => panic!("unreadable: {complaint}"),
            Incoming::Ended => panic!("nothing came back"),
        }
    }

    #[tokio::test]
    async fn every_request_survives_the_wire() {
        let requests = vec![
            Request::Start(ProcessSpec {
                command: "claude".to_string(),
                args: vec!["--model".to_string(), "opus".to_string()],
                cwd: PathBuf::from("/workdir"),
                env: BTreeMap::from([("TERM".to_string(), "xterm-256color".to_string())]),
                size: WindowSize { rows: 80, cols: 100 },
            }),
            Request::Window,
            Request::Input { data: vec![0x1b, b'[', b'A', 0x0d] },
            Request::Resize { size: WindowSize { rows: 30, cols: 120 } },
            Request::Stop { grace_ms: 5000 },
            Request::State,
            Request::Clipboard,
        ];
        for request in requests {
            assert_eq!(round_trip(&request).await, request);
        }
    }

    #[tokio::test]
    async fn every_response_survives_the_wire() {
        let responses = vec![
            Response::Done,
            Response::Window(window()),
            Response::State(State::NotStarted),
            Response::State(State::Running),
            Response::State(State::Exited { code: Some(7), signal: None, window: window() }),
            Response::State(State::Exited { code: None, signal: Some(9), window: window() }),
            Response::Clipboard { data: None },
            Response::Clipboard { data: Some(Vec::new()) },
            Response::Clipboard { data: Some(vec![0x00, 0xff, b'h', b'i']) },
            Response::Refused(Refusal::NothingStarted),
            Response::Refused(Refusal::AlreadyRunning),
            Response::Refused(Refusal::AlreadyExited),
            Response::Refused(Refusal::UnusableWindowSize {
                least: WindowSize { rows: 2, cols: 2 },
            }),
            Response::Refused(Refusal::Failed { message: "spawning claude".to_string() }),
            Response::Refused(Refusal::Unreadable { message: "not json".to_string() }),
        ];
        for response in responses {
            assert_eq!(round_trip(&response).await, response);
        }
    }

    #[tokio::test]
    async fn byte_payloads_travel_as_base64_text() {
        let wire = wire(&Request::Input { data: vec![0x1b, b'[', b'A'] }).await;
        let wire = String::from_utf8(wire).expect("the wire is text");
        assert!(wire.contains(r#""data":"G1tB""#), "{wire}");
    }

    #[tokio::test]
    async fn a_window_of_arbitrary_bytes_comes_back_byte_for_byte() {
        let contents: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
        let sent = Window { size: WindowSize { rows: 24, cols: 80 }, contents };
        assert_eq!(round_trip(&Response::Window(sent.clone())).await, Response::Window(sent));
    }

    #[tokio::test]
    async fn an_empty_byte_payload_is_carried_as_such() {
        let sent = Request::Input { data: Vec::new() };
        assert_eq!(round_trip(&sent).await, Request::Input { data: Vec::new() });
    }

    #[tokio::test]
    async fn lines_that_are_not_messages_come_back_as_unreadable() {
        let unreadable = [
            "not json",
            r#"{"op":"fly"}"#,
            r#"{"op":"input","data":"!!!!"}"#,
            r#"{"op":"stop","grace_ms":"soon"}"#,
            "",
        ];
        for line in unreadable {
            let mut wire = format!("{line}\n");
            let read: Incoming<Request> = read_message(&mut wire.as_bytes()).await.expect("read");
            assert!(
                matches!(read, Incoming::Unreadable(_)),
                "{line:?} should not have been read as a request"
            );
            wire.clear();
        }
    }

    #[tokio::test]
    async fn a_clipboard_that_is_not_readable_text_is_rejected() {
        let mut line = r#"{"reply":"clipboard","data":"!!!!"}"#.to_string();
        line.push('\n');
        let read: Incoming<Response> = read_message(&mut line.as_bytes()).await.expect("read");
        assert!(matches!(read, Incoming::Unreadable(_)));
    }

    #[tokio::test]
    async fn a_closed_source_ends_the_conversation() {
        let read = read_message::<_, Request>(&mut b"".as_slice()).await.expect("read");
        assert!(matches!(read, Incoming::Ended));
    }

    #[tokio::test]
    async fn messages_are_read_one_line_at_a_time() {
        let mut wire = Vec::new();
        write_message(&mut wire, &Request::Window).await.expect("write");
        write_message(&mut wire, &Request::State).await.expect("write");
        let mut source = wire.as_slice();
        for expected in [Request::Window, Request::State] {
            match read_message::<_, Request>(&mut source).await.expect("read") {
                Incoming::Message(request) => assert_eq!(request, expected),
                _ => panic!("expected {expected:?}"),
            }
        }
        assert!(matches!(
            read_message::<_, Request>(&mut source).await.expect("read"),
            Incoming::Ended
        ));
    }

    #[test]
    fn every_refusal_reads_plainly() {
        assert_eq!(Refusal::NothingStarted.to_string(), "no process has been started");
        assert_eq!(Refusal::AlreadyRunning.to_string(), "a process is already running");
        assert_eq!(Refusal::AlreadyExited.to_string(), "the process has already exited");
        assert_eq!(
            Refusal::UnusableWindowSize { least: WindowSize { rows: 2, cols: 2 } }.to_string(),
            "a window smaller than 2x2 cannot be drawn"
        );
        assert_eq!(
            Refusal::Failed { message: "no such file".to_string() }.to_string(),
            "no such file"
        );
        assert_eq!(Refusal::Unreadable { message: "not json".to_string() }.to_string(), "not json");
    }
}
