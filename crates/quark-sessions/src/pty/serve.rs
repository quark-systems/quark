//! Serving a [`PtySupervisor`] on a Unix socket.

use quark_core::session::SessionBackend;
use quark_core::{CoreError, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use super::proto::{decode, Frame, Request, Response};
use super::PtySupervisor;

/// Answers connections on `listener` until it fails. Each connection runs
/// on its own task, so a stuck viewer never holds up another.
pub async fn serve(listener: UnixListener, supervisor: PtySupervisor) -> std::io::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let supervisor = supervisor.clone();
        tokio::spawn(async move {
            if let Err(e) = connection(stream, supervisor).await {
                tracing::debug!(error = %e, "pty connection ended");
            }
        });
    }
}

async fn connection(stream: UnixStream, sup: PtySupervisor) -> std::io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        let request = match serde_json::from_str::<Request>(&line) {
            Ok(r) => r,
            Err(e) => {
                send(
                    &mut write,
                    &Response::from(CoreError::Invalid(e.to_string())),
                )
                .await?;
                continue;
            }
        };
        if let Request::Attach { id } = request {
            let mut stream = match sup.attach(&id).await {
                Ok(s) => s,
                Err(e) => {
                    send(&mut write, &Response::from(e)).await?;
                    continue;
                }
            };
            send(&mut write, &Response::Done).await?;
            while let Some(out) = stream.next().await {
                send(&mut write, &Frame::from(out)).await?;
            }
            return Ok(());
        }
        let response = handle(&sup, request).await.unwrap_or_else(Response::from);
        send(&mut write, &response).await?;
    }
    Ok(())
}

async fn handle(sup: &PtySupervisor, request: Request) -> Result<Response> {
    Ok(match request {
        Request::Create { spec } => Response::Session(sup.create(&spec).await?),
        Request::Input { id, data } => {
            sup.input(&id, &decode(&data)?).await?;
            Response::Done
        }
        Request::Resize { id, size } => {
            sup.resize(&id, size).await?;
            Response::Done
        }
        Request::Snapshot { id } => sup.snapshot(&id).await?.into(),
        Request::Kill { id } => {
            sup.kill(&id).await?;
            Response::Done
        }
        Request::List => Response::Sessions(sup.list().await?),
        Request::Attach { .. } => unreachable!("handled by the connection"),
    })
}

async fn send<T: serde::Serialize>(
    write: &mut tokio::net::unix::OwnedWriteHalf,
    value: &T,
) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    line.push(b'\n');
    write.write_all(&line).await
}
