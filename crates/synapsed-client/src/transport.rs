//! A small bounded socket owner keeps ping/pong and cancellation alive even when
//! a consumer temporarily drops its `next()` future or is not reading UI events.
use crate::{Client, ClientError, ServerEvent, Socket, socket_error, wire};
use futures_util::{SinkExt, StreamExt};
use tokio::{sync::mpsc, task::JoinHandle, time::timeout};
use tokio_tungstenite::tungstenite::Message;

pub(crate) fn start(
    socket: Socket,
    client: Client,
    token: u64,
) -> (mpsc::Receiver<ServerEvent>, JoinHandle<()>) {
    let (sender, receiver) = mpsc::channel(8);
    let task = tokio::spawn(async move {
        let result = drive(socket, &client, token, sender).await;
        match result {
            Err(error) if error.is_terminal() => client.inner.fail_socket(token, &error),
            _ => client.inner.disconnect(token),
        }
    });
    (receiver, task)
}
async fn drive(
    mut socket: Socket,
    client: &Client,
    token: u64,
    sender: mpsc::Sender<ServerEvent>,
) -> Result<(), ClientError> {
    let mut changed = client.inner.changed.subscribe();
    loop {
        client.inner.state().check_socket(token)?;
        let frame = tokio::select! {
            frame = timeout(client.inner.socket_idle_timeout, socket.next()) => {
                frame.map_err(|_| ClientError::Timeout)?
            }
            _ = changed.changed() => {
                continue;
            }
            _ = sender.closed() => {
                return Ok(());
            }
        };
        client.inner.state().check_socket(token)?;
        match frame {
            Some(Ok(Message::Text(text))) => {
                let event = wire::decode(text.as_bytes())?;
                // A slow consumer must reconnect for a fresh whole state, never
                // accumulate unbounded copied snapshots or lose a log silently.
                sender.try_send(event).map_err(|_| ClientError::Network)?;
            }
            Some(Ok(Message::Ping(bytes))) => {
                tokio::select! {
                    result = timeout(client.inner.request_timeout, socket.send(Message::Pong(bytes))) => {
                        result.map_err(|_| ClientError::Timeout)?.map_err(socket_error)?;
                    }
                    _ = changed.changed() => {
                        client.inner.state().check_socket(token)?;
                    }
                }
            }
            Some(Ok(Message::Pong(_))) => {}
            Some(Ok(Message::Close(frame))) => {
                if frame.is_some_and(|frame| {
                    matches!(u16::from(frame.code), 1002 | 1003 | 1007 | 1008 | 1009)
                }) {
                    return Err(ClientError::Protocol(
                        "WebSocket closed for protocol or access-policy violation.",
                    ));
                }
                return Ok(());
            }
            Some(Ok(_)) => {
                return Err(ClientError::Protocol(
                    "Expected a JSON text WebSocket event.",
                ));
            }
            Some(Err(error)) => return Err(socket_error(error)),
            None => return Ok(()),
        }
    }
}
