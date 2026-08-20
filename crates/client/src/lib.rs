use std::time::Duration;

use agent_master_protocol::{read_message, write_message, Incoming, Request, Response};
use tokio::io::BufReader;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpStream, ToSocketAddrs};

pub use agent_master_protocol::{ProcessSpec, Refusal, State, Window, WindowSize};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot reach agent-master: {0}")]
    Unreachable(#[from] std::io::Error),
    #[error("agent-master hung up")]
    HungUp,
    #[error("agent-master said something unreadable: {0}")]
    Unreadable(String),
    #[error("agent-master refused: {0}")]
    Refused(Refusal),
    #[error("agent-master answered {answer} to {asked}")]
    Mismatched { asked: &'static str, answer: &'static str },
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct AgentMaster {
    write: OwnedWriteHalf,
    read: BufReader<OwnedReadHalf>,
}

impl AgentMaster {
    pub async fn connect(address: impl ToSocketAddrs) -> Result<Self> {
        let (read, write) = TcpStream::connect(address).await?.into_split();
        Ok(Self { write, read: BufReader::new(read) })
    }

    pub async fn start(&mut self, spec: ProcessSpec) -> Result<State> {
        expect_state(self.ask(Request::Start(spec)).await?, "start")
    }

    pub async fn window(&mut self) -> Result<Window> {
        match self.ask(Request::Window).await? {
            Response::Window(window) => Ok(window),
            other => Err(mismatch("window", &other)),
        }
    }

    pub async fn input(&mut self, data: &[u8]) -> Result<()> {
        expect_done(self.ask(Request::Input { data: data.to_vec() }).await?, "input")
    }

    pub async fn resize(&mut self, size: WindowSize) -> Result<()> {
        expect_done(self.ask(Request::Resize { size }).await?, "resize")
    }

    pub async fn stop(&mut self, grace: Duration) -> Result<State> {
        let request = Request::Stop { grace_ms: grace.as_millis().try_into().unwrap_or(u64::MAX) };
        expect_state(self.ask(request).await?, "stop")
    }

    pub async fn state(&mut self) -> Result<State> {
        expect_state(self.ask(Request::State).await?, "state")
    }

    pub async fn clipboard(&mut self) -> Result<Option<Vec<u8>>> {
        match self.ask(Request::Clipboard).await? {
            Response::Clipboard { data } => Ok(data),
            other => Err(mismatch("clipboard", &other)),
        }
    }

    async fn ask(&mut self, request: Request) -> Result<Response> {
        write_message(&mut self.write, &request).await?;
        match read_message(&mut self.read).await? {
            Incoming::Message(Response::Refused(refusal)) => Err(Error::Refused(refusal)),
            Incoming::Message(response) => Ok(response),
            Incoming::Unreadable(message) => Err(Error::Unreadable(message)),
            Incoming::Ended => Err(Error::HungUp),
        }
    }
}

fn expect_state(response: Response, asked: &'static str) -> Result<State> {
    match response {
        Response::State(state) => Ok(state),
        other => Err(mismatch(asked, &other)),
    }
}

fn expect_done(response: Response, asked: &'static str) -> Result<()> {
    match response {
        Response::Done => Ok(()),
        other => Err(mismatch(asked, &other)),
    }
}

fn mismatch(asked: &'static str, answer: &Response) -> Error {
    let answer = match answer {
        Response::Done => "done",
        Response::Window(_) => "a window",
        Response::State(_) => "a state",
        Response::Clipboard { .. } => "a clipboard",
        Response::Refused(_) => "a refusal",
    };
    Error::Mismatched { asked, answer }
}
