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
use onlyne_im_backend::frame::{
    BackendOp, CoreOp, ImConversation, ListConversationsArgs, LoginArgs, LoginResult, SendArgs,
    SentArgs, StopArgs,
};
use onlyne_wire::frame::{FrameReader, write_frame};
use std::path::Path;
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
    /// Spawn `command` as a backend.
    ///
    /// Two variables ride in: the gateway id this backend serves, and the path
    /// to the credential `onlyne-im login` wrote for it. Sending the *path*
    /// rather than the secret keeps a token out of the process table, which is
    /// readable by every process on the machine.
    pub async fn spawn(command: &[String], gateway: &str, credential: &Path) -> Result<Self> {
        let Some(program) = command.first() else {
            bail!("the backend command is empty");
        };
        let mut child = Command::new(program)
            .args(&command[1..])
            .env(onlyne_im_backend::GATEWAY_ENV, gateway)
            .env(onlyne_im_backend::CREDENTIAL_ENV, credential)
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

    /// Ask the backend to render and send one payload, and read what the
    /// platform made of it.
    ///
    /// Waiting is the point: the platform's id for the message is what a later
    /// reply points at. Shaped like [`BackendHost::login`], which also asks one
    /// question and reads one answer.
    pub async fn send(&mut self, args: SendArgs) -> Result<SentArgs> {
        write_frame(&mut self.stdin, &CoreOp::Send(args)).await?;
        loop {
            match self.next_op().await? {
                Some(BackendOp::Sent(sent)) => return Ok(sent),
                Some(BackendOp::Hello(args)) => {
                    bail!("the backend sent a second hello while sending: {args:?}")
                }
                // An event that arrives while the send is in flight is ordinary
                // traffic; the next read gets the answer.
                Some(_) => continue,
                None => bail!("the backend closed before answering the send"),
            }
        }
    }

    /// Hand the backend a credential and read its verdict.
    ///
    /// The answer arrives as a frame, so a refused credential is a sentence an
    /// operator reads rather than a dead process. A frame of another kind in
    /// between is skipped rather than mistaken for the answer: a backend may
    /// still be reporting the events it saw before the login arrived.
    pub async fn login(&mut self, args: LoginArgs) -> Result<LoginResult> {
        write_frame(&mut self.stdin, &CoreOp::Login(args)).await?;
        loop {
            match self.next_op().await? {
                Some(BackendOp::Login(result)) => return Ok(result),
                Some(BackendOp::Hello(args)) => {
                    bail!("the backend sent a second hello while logging in: {args:?}")
                }
                Some(_) => continue,
                None => bail!("the backend closed before answering the login"),
            }
        }
    }

    /// Ask the backend which conversations it can see.
    pub async fn list_conversations(&mut self) -> Result<Vec<ImConversation>> {
        write_frame(
            &mut self.stdin,
            &CoreOp::ListConversations(ListConversationsArgs {
                gateway: String::new(),
            }),
        )
        .await?;
        loop {
            match self.next_op().await? {
                Some(BackendOp::Conversations(list)) => return Ok(list.conversations),
                Some(BackendOp::Hello(args)) => {
                    bail!("the backend sent a second hello while listing: {args:?}")
                }
                Some(_) => continue,
                None => bail!("the backend closed before answering the listing"),
            }
        }
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
                authenticated: true,
                onboarding: None,
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
                // The auth half, scripted the way a real backend answers it:
                // a login verdict naming the identity, and a conversation
                // list. Both are the calls the setup wizard makes.
                CoreOp::Login(args) => {
                    let ok = args.credential == "good-token";
                    write_frame(
                        &mut write,
                        &BackendOp::Login(onlyne_im_backend::frame::LoginResult {
                            ok,
                            reason: (!ok).then(|| "the token is not this bot's".to_string()),
                            identity: ok.then(|| "onlyne_bot".to_string()),
                        }),
                    )
                    .await?;
                }
                CoreOp::ListConversations(_) => {
                    write_frame(
                        &mut write,
                        &BackendOp::Conversations(onlyne_im_backend::frame::ImConversations {
                            conversations: vec![onlyne_im_backend::frame::ImConversation {
                                id: "-1007788".into(),
                                title: Some("Project".into()),
                                kind: Some("group".into()),
                            }],
                        }),
                    )
                    .await?;
                }
            }
        }
    }

    #[tokio::test]
    async fn a_login_verdict_and_a_conversation_list_read_the_same_way() {
        let (client, server) = duplex(64 * 1024);
        let backend = tokio::spawn(scripted(server));
        let (mut read, mut write) = tokio::io::split(client);
        let mut reader = FrameReader::default();

        // The backend's own hello comes first.
        let hello = read_op(&mut reader, &mut read).await.unwrap().unwrap();
        assert!(matches!(hello, BackendOp::Hello(_)));

        write_frame(
            &mut write,
            &CoreOp::Login(onlyne_im_backend::frame::LoginArgs {
                gateway: "tg1".into(),
                credential: "bad-token".into(),
            }),
        )
        .await
        .unwrap();
        match read_op(&mut reader, &mut read).await.unwrap().unwrap() {
            BackendOp::Login(result) => {
                assert!(!result.ok);
                assert_eq!(
                    result.reason.as_deref(),
                    Some("the token is not this bot's")
                );
                assert!(result.identity.is_none());
            }
            other => panic!("expected a login verdict, got {other:?}"),
        }

        write_frame(
            &mut write,
            &CoreOp::Login(onlyne_im_backend::frame::LoginArgs {
                gateway: "tg1".into(),
                credential: "good-token".into(),
            }),
        )
        .await
        .unwrap();
        match read_op(&mut reader, &mut read).await.unwrap().unwrap() {
            BackendOp::Login(result) => {
                assert!(result.ok);
                assert_eq!(result.identity.as_deref(), Some("onlyne_bot"));
            }
            other => panic!("expected an accepted login, got {other:?}"),
        }

        write_frame(
            &mut write,
            &CoreOp::ListConversations(onlyne_im_backend::frame::ListConversationsArgs {
                gateway: "tg1".into(),
            }),
        )
        .await
        .unwrap();
        match read_op(&mut reader, &mut read).await.unwrap().unwrap() {
            BackendOp::Conversations(list) => {
                assert_eq!(list.conversations.len(), 1);
                assert_eq!(list.conversations[0].id, "-1007788");
                assert_eq!(list.conversations[0].title.as_deref(), Some("Project"));
            }
            other => panic!("expected a conversation list, got {other:?}"),
        }

        drop(read);
        drop(write);
        backend.await.unwrap().unwrap();
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
