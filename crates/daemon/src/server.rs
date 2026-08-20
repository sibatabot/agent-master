use std::sync::Arc;
use std::time::Duration;

use agent_master_protocol::{read_message, write_message, Incoming, Refusal, Request, Response};
use anyhow::Result;
use tokio::io::BufReader;
use tokio::net::{TcpListener, TcpStream};

use crate::session::Session;

pub async fn serve(listener: TcpListener, session: Arc<Session>) -> Result<()> {
    loop {
        let (stream, peer) = listener.accept().await?;
        let session = session.clone();
        tokio::spawn(async move {
            if let Err(error) = converse(stream, session).await {
                log::warn!("connection from {peer} ended: {error}");
            }
        });
    }
}

async fn converse(stream: TcpStream, session: Arc<Session>) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    loop {
        let response = match read_message(&mut read).await? {
            Incoming::Message(request) => act(&session, request).await,
            Incoming::Unreadable(message) => Response::Refused(Refusal::Unreadable { message }),
            Incoming::Ended => return Ok(()),
        };
        write_message(&mut write, &response).await?;
    }
}

async fn act(session: &Session, request: Request) -> Response {
    let done = match request {
        Request::Start(spec) => session.start(spec).await.map(Response::State),
        Request::Window => session.window().await.map(Response::Window),
        Request::Input { data } => session.input(&data).await.map(|()| Response::Done),
        Request::Resize { size } => session.resize(size).await.map(|()| Response::Done),
        Request::Stop { grace_ms } => {
            session.stop(Duration::from_millis(grace_ms)).await.map(Response::State)
        }
        Request::State => Ok(Response::State(session.state().await)),
        Request::Clipboard => session.clipboard().await.map(|data| Response::Clipboard { data }),
    };
    done.unwrap_or_else(Response::Refused)
}
