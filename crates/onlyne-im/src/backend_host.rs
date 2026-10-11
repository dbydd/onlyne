//! The backend half: spawn one platform backend and speak its standard
//! protocol over stdio (`docs/v2.2-IM-GATEWAY-PLAN.md` §6).
//!
//! One `[[im.backend]]` is one of these. The gateway owns the process; the
//! frames are length-prefixed JSON, the same codec every other Onlyne
//! connection uses (`onlyne_wire::frame`). The backend never sees a role or a
//! session: it reports platform events one way and renders standard payloads
//! the other, and the runloop maps `gateway + conversation` to the conversation
//! workspace that serves it.

use anyhow::{Context, Result, bail};
use onlyne_im_backend::frame::{BackendOp, CoreOp, SendArgs, StopArgs};
use onlyne_wire::frame::{FrameReader, write_frame};
use std::process::Stdio;
use tokio::io::AsyncRead;
use tokio::process::{Child, Command};

/// A spawned backend: its process and its two protocol streams.
pub struct BackendHost {
    child: Child,
    reader: FrameReader,
    stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
}

impl BackendHost {
    /// Spawn `command` as a backend and read its `hello`.
    pub async fn spawn(command: &[String]) -> Result<Self> {
        let Some(program) = command.first() else {
            bail!("the backend command is empty");
        };
        let mut child = Command::new(program)
            .args(&command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("could not start the backend {program}"))?;
        let stdin = child.stdin.take().context("the backend has no stdin")?;
        let stdout = child.stdout.take().context("the backend has no stdout")?;
        Ok(BackendHost {
            child,
            reader: FrameReader::default(),
            stdin,
            stdout,
        })
    }

    /// The backend's `hello`, the first frame it must send.
    pub async fn hello(&mut self) -> Result<onlyne_im_backend::frame::HelloArgs> {
        match self.next_op().await? {
            Some(BackendOp::Hello(args)) => Ok(args),
            Some(other) => bail!("the backend's first frame was {other:?}, not a hello"),
            None => bail!("the backend closed before a hello"),
        }
    }

    /// Read one backend frame; `None` when the backend ended its stream.
    pub async fn next_op(&mut self) -> Result<Option<BackendOp>> {
        read_op(&mut self.reader, &mut self.stdout).await
    }

    /// Ask the backend to render and send one payload.
    pub async fn send(&mut self, args: SendArgs) -> Result<()> {
        write_frame(&mut self.stdin, &CoreOp::Send(args)).await?;
        Ok(())
    }

    /// Ask the backend to stop and wait for it to leave.
    pub async fn stop(mut self, reason: &str) -> Result<()> {
        let _ = write_frame(
            &mut self.stdin,
            &CoreOp::Stop(StopArgs {
                reason: reason.to_string(),
            }),
        )
        .await;
        drop(self.stdin);
        let _ = self.child.wait().await;
        Ok(())
    }
}

/// Read one `BackendOp` through a `FrameReader`, so a cancelled read keeps the
/// bytes it has (`FrameReader::next` is cancel-safe).
async fn read_op<R>(reader: &mut FrameReader, stream: &mut R) -> Result<Option<BackendOp>>
where
    R: AsyncRead + Unpin,
{
    reader
        .next::<_, BackendOp>(stream)
        .await
        .map_err(|error| anyhow::anyhow!("reading a backend frame: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use onlyne_im_backend::payload::ImPayload;
    use tokio::io::AsyncWrite;
    use tokio::io::duplex;

    /// A scripted backend over a byte stream: it says hello, emits one event,
    /// then echoes every `send` back as a `health` frame so the test can read
    /// what arrived.
    async fn scripted<S>(stream: S) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let (mut read, mut write) = tokio::io::split(stream);
        write_frame(
            &mut write,
            &BackendOp::Hello(onlyne_im_backend::frame::HelloArgs {
                platform: "telegram".into(),
                version: "1".into(),
                capabilities: vec!["message".into()],
            }),
        )
        .await?;
        loop {
            let mut reader = FrameReader::default();
            let Some(op) = reader.next::<_, CoreOp>(&mut read).await? else {
                return Ok(());
            };
            match op {
                CoreOp::Send(args) => {
                    write_frame(
                        &mut write,
                        &BackendOp::Health(onlyne_im_backend::frame::HealthArgs {
                            state: "online".into(),
                            detail: Some(args.payload.text),
                            uptime_s: 0,
                        }),
                    )
                    .await?;
                }
                CoreOp::Stop(_) => return Ok(()),
            }
        }
    }

    #[tokio::test]
    async fn a_backend_hello_and_a_rendered_send_travel_the_standard_protocol() {
        // Drive the protocol directly over a duplex, proving the frame shapes
        // round-trip through the host's reader and writer.
        let (host_side, backend_side) = duplex(64 * 1024);
        let backend = tokio::spawn(scripted(backend_side));
        let (mut read, mut write) = tokio::io::split(host_side);
        let mut reader = FrameReader::default();

        let hello = read_op(&mut reader, &mut read).await.unwrap().unwrap();
        match &hello {
            BackendOp::Hello(args) => assert_eq!(args.platform, "telegram"),
            other => panic!("expected a hello, got {other:?}"),
        }

        write_frame(
            &mut write,
            &CoreOp::Send(SendArgs {
                conversation: "-100".into(),
                payload: ImPayload {
                    text: "hi".into(),
                    ..ImPayload::default()
                },
            }),
        )
        .await
        .unwrap();

        let echoed = read_op(&mut reader, &mut read).await.unwrap().unwrap();
        match echoed {
            BackendOp::Health(args) => assert_eq!(args.detail.as_deref(), Some("hi")),
            other => panic!("expected a health echo, got {other:?}"),
        }
        // Close the host side so the backend loop sees EOF and returns; it
        // otherwise waits for a frame the test will never send.
        drop(read);
        drop(write);
        backend.await.unwrap().unwrap();
    }
}
