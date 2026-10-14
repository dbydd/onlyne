//! The stdio host loop: what a platform backend's `main` runs.
//!
//! A backend implements [`Backend`] and calls [`serve`]; this module owns the
//! framing, so every platform backend writes only its own translation between
//! the platform and the standard payloads. Frames are length-prefixed JSON on
//! stdin/stdout, the same codec every other Onlyne connection uses.

use crate::backend::Backend;
use crate::frame::{BackendOp, CoreOp, ImConversations, LoginResult};
use anyhow::Result;
use onlyne_wire::frame::{FrameReader, write_frame};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, stdin, stdout};

/// Serve one backend over the process's stdin/stdout.
pub async fn serve<B>(backend: B) -> Result<()>
where
    B: Backend,
{
    serve_with(backend, stdin(), stdout()).await
}

/// Serve one backend over a supplied reader and writer.
///
/// Two things run at once: platform events flow out (`next_event` → an `event`
/// frame), and core frames flow in (`send` → the backend renders it). Neither
/// blocks the other: a backend whose `next_event` parks on a platform long-poll
/// still answers a `send`.
pub async fn serve_with<B, R, W>(backend: B, read: R, mut write: W) -> Result<()>
where
    B: Backend,
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    let mut backend = backend;
    let hello = backend.hello();
    let authenticated = hello.authenticated;
    write_frame(&mut write, &BackendOp::Hello(hello)).await?;
    write.flush().await?;

    // A backend with no credential does not poll. This is the one checkpoint
    // that makes "the gateway keeps running while a platform is unauthenticated"
    // true: polling a platform with an empty token fails on its first call
    // (Telegram answers 404, Feishu and QQ cannot open their websocket), and a
    // backend whose event stream ends is a backend the gateway loses — so the
    // sentence `hello` already carried would be the last thing an operator ever
    // heard from it. Parked, it keeps its process, answers `login`, and starts
    // polling the moment a credential is accepted.
    let mut polling = authenticated;

    let (tx, mut rx) = tokio::sync::mpsc::channel::<CoreOp>(64);
    let reader = tokio::spawn(async move {
        let mut read = read;
        let mut frames = FrameReader::default();
        loop {
            match frames.next::<_, CoreOp>(&mut read).await {
                Ok(Some(op)) => {
                    if tx.send(op).await.is_err() {
                        return;
                    }
                }
                Ok(None) | Err(_) => return,
            }
        }
    });

    loop {
        tokio::select! {
            op = rx.recv() => match op {
                Some(CoreOp::Login(args)) => {
                    // The answer goes back as a frame rather than an exit code:
                    // a backend that refused a credential by dying would take
                    // its platform's other conversations down with it, and the
                    // gateway would have no sentence to show an operator.
                    let result = match backend.login(args).await {
                        Ok(identity) => LoginResult {
                            ok: true,
                            reason: None,
                            identity,
                        },
                        Err(error) => LoginResult {
                            ok: false,
                            reason: Some(format!("{error:#}")),
                            identity: None,
                        },
                    };
                    // An accepted credential turns the poll on; a refused one
                    // leaves it where it was, so a bad token cannot silence a
                    // backend that was already working.
                    polling |= result.ok;
                    write_frame(&mut write, &BackendOp::Login(result)).await?;
                    write.flush().await?;
                }
                Some(CoreOp::ListConversations(_)) => {
                    let conversations = backend.list_conversations().await.unwrap_or_default();
                    write_frame(
                        &mut write,
                        &BackendOp::Conversations(ImConversations { conversations }),
                    )
                    .await?;
                    write.flush().await?;
                }
                Some(CoreOp::Send(args)) => {
                    if let Err(error) = backend.send(args).await {
                        eprintln!("backend send failed: {error:#}");
                    }
                }
                Some(CoreOp::Stop(_)) | None => break,
            },
            event = backend.next_event(), if polling => match event {
                Ok(Some(event)) => {
                    write_frame(&mut write, &BackendOp::Event(event)).await?;
                    write.flush().await?;
                }
                Ok(None) => break,
                Err(error) => {
                    eprintln!("backend event stream failed: {error:#}");
                    break;
                }
            },
        }
    }
    reader.abort();
    let _ = backend.stop().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{HelloArgs, SendArgs};
    use crate::payload::{ImEvent, ImEventKind, ImPayload};
    use async_trait::async_trait;
    use tokio::sync::mpsc;

    /// A backend that yields whatever a test hands it and records every send.
    struct Fake {
        events: mpsc::Receiver<ImEvent>,
        sends: mpsc::Sender<SendArgs>,
    }

    #[async_trait]
    impl Backend for Fake {
        fn hello(&self) -> HelloArgs {
            HelloArgs {
                platform: "fake".into(),
                version: "1".into(),
                capabilities: vec!["message".into()],
                authenticated: true,
                onboarding: None,
            }
        }
        async fn next_event(&mut self) -> Result<Option<ImEvent>> {
            Ok(self.events.recv().await)
        }
        async fn send(&mut self, args: SendArgs) -> Result<()> {
            let _ = self.sends.send(args).await;
            Ok(())
        }
    }

    #[tokio::test]
    async fn the_loop_forwards_events_out_and_sends_in() {
        let (core_side, backend_side) = tokio::io::duplex(64 * 1024);
        let (core_read, core_write) = tokio::io::split(core_side);
        let (backend_read, backend_write) = tokio::io::split(backend_side);
        let (events_tx, events_rx) = mpsc::channel(4);
        let (sends_tx, mut sends_rx) = mpsc::channel(4);
        let backend = Fake {
            events: events_rx,
            sends: sends_tx,
        };
        let served = tokio::spawn(serve_with(backend, backend_read, backend_write));

        let mut frames = FrameReader::default();
        let mut core_read = core_read;
        let mut core_write = core_write;

        // The backend's hello comes first.
        let op = frames
            .next::<_, BackendOp>(&mut core_read)
            .await
            .unwrap()
            .unwrap();
        match op {
            BackendOp::Hello(args) => assert_eq!(args.platform, "fake"),
            other => panic!("expected a hello, got {other:?}"),
        }

        // A platform event reaches the core as an event frame.
        events_tx
            .send(ImEvent {
                gateway: "g".into(),
                platform: "fake".into(),
                conversation: "c".into(),
                id: "m1".into(),
                kind: ImEventKind::Message,
                at: "2026-10-10T12:00:00Z".into(),
                ..ImEvent::default()
            })
            .await
            .unwrap();
        let op = frames
            .next::<_, BackendOp>(&mut core_read)
            .await
            .unwrap()
            .unwrap();
        match op {
            BackendOp::Event(event) => assert_eq!(event.id, "m1"),
            other => panic!("expected an event, got {other:?}"),
        }

        // A `send` from the core reaches the backend.
        write_frame(
            &mut core_write,
            &CoreOp::Send(SendArgs {
                conversation: "c".into(),
                payload: ImPayload {
                    text: "hi".into(),
                    ..ImPayload::default()
                },
            }),
        )
        .await
        .unwrap();
        let received = tokio::time::timeout(std::time::Duration::from_secs(5), sends_rx.recv())
            .await
            .expect("a send arrives")
            .expect("the channel is open");
        assert_eq!(received.payload.text, "hi");

        // `stop` ends the loop cleanly.
        write_frame(
            &mut core_write,
            &CoreOp::Stop(crate::frame::StopArgs {
                reason: "test".into(),
            }),
        )
        .await
        .unwrap();
        served.await.unwrap().unwrap();
    }
}
