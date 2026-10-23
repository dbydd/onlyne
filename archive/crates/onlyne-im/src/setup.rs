//! `onlyne-im setup`: the guided path from an empty gateway root to a
//! conversation the cluster can address.
//!
//! Four steps, and each one exists because leaving it out produces a gateway
//! that starts and quietly does nothing:
//!
//! 1. **Which backend.** A root may declare several; the operator picks one.
//! 2. **Authenticate.** The backend is spawned and asked what it needs. The
//!    credential is pasted, written to the gateway root 0600, and *verified*
//!    against the platform before anything else happens — a wrong token is a
//!    sentence here, not a silent gateway.
//! 3. **Which conversations.** The backend is asked what it can see; the
//!    operator picks. A platform that gives no listing takes a hand-typed id.
//! 4. **Bind.** Each chosen conversation gets its own workspace, its own chat
//!    key, and its ACL edge to the roles that serve it — which is exactly what
//!    `generate` writes, so this step calls `generate` rather than repeating it.
//!
//! The wizard's own logic is separated from its input: [`Prompter`] is the only
//! thing that touches the terminal, so the flow is testable and a richer front
//! end can replace one type rather than rewriting the steps.

use crate::workspace::{GenerateArgs, generate};
use anyhow::{Context, Result, bail};
use onlyne_im::backend_host::BackendHost;
use onlyne_im::config::{BackendEntry, GatewayFile};
use onlyne_im_backend::frame::{ImConversation, LoginArgs as WireLoginArgs, OnboardingKind};
use std::path::{Path, PathBuf};

/// `onlyne-im setup`.
#[derive(Debug, Clone)]
pub struct SetupArgs {
    pub root: PathBuf,
    /// The `[[im.backend]]` to set up; asked for when there is more than one.
    pub gateway: Option<String>,
    /// The credential, for a non-interactive run. Skipped when one is already
    /// stored and verified.
    pub credential: Option<String>,
    /// Conversation ids to bind, for a non-interactive run. When empty the
    /// wizard asks.
    pub conversations: Vec<String>,
    /// The roles this conversation may address and be addressed by.
    pub peers: Vec<String>,
    pub server_root: Option<PathBuf>,
    pub socket: Option<PathBuf>,
    /// Answer with defaults and refuse anything that needs a human. A run that
    /// carries every flag it needs never prompts anyway.
    pub non_interactive: bool,
}

/// Everything that touches the terminal.
///
/// One implementation reads a TTY and one answers from the flags, so a test
/// drives the whole wizard without a terminal and an operator gets the real
/// prompts.
pub trait Prompter {
    /// One line of prose the operator reads.
    fn say(&mut self, line: &str);
    /// Ask one question and read a line of text. `default` is offered in
    /// brackets and returned for an empty answer.
    fn ask(&mut self, question: &str, default: Option<&str>) -> Result<String>;
    /// Ask for a secret: never echoed, never logged.
    fn ask_secret(&mut self, question: &str) -> Result<String>;
    /// Ask the operator to pick from a list, answering the chosen index.
    fn choose(&mut self, question: &str, options: &[String]) -> Result<usize>;
    /// Whether a human is there to answer. A non-interactive run says no.
    fn is_interactive(&self) -> bool;
}

/// The real one: stdin, stdout, and no echo for secrets.
pub struct TermPrompter {
    interactive: bool,
}

impl TermPrompter {
    pub fn new(interactive: bool) -> Self {
        Self { interactive }
    }
}

impl Prompter for TermPrompter {
    fn say(&mut self, line: &str) {
        println!("{line}");
    }

    fn ask(&mut self, question: &str, default: Option<&str>) -> Result<String> {
        use std::io::Write;
        match default {
            Some(default) if !default.is_empty() => print!("{question} [{default}]: "),
            _ => print!("{question}: "),
        }
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        let answer = answer.trim().to_string();
        if answer.is_empty() {
            if let Some(default) = default {
                return Ok(default.to_string());
            }
        }
        Ok(answer)
    }

    fn ask_secret(&mut self, question: &str) -> Result<String> {
        use std::io::Write;
        print!("{question}: ");
        std::io::stdout().flush()?;
        // Echo off while the operator types, so a token never lands in the
        // scrollback this tool's own login verb refuses to print to. A piped
        // stdin is not a terminal and has nothing to echo.
        let echo_off = turn_echo(false);
        let mut answer = String::new();
        let read = std::io::stdin().read_line(&mut answer);
        if echo_off {
            turn_echo(true);
            println!();
        }
        read?;
        Ok(answer.trim().to_string())
    }

    fn choose(&mut self, question: &str, options: &[String]) -> Result<usize> {
        println!("{question}");
        for (index, option) in options.iter().enumerate() {
            println!("  {}) {option}", index + 1);
        }
        loop {
            let answer = self.ask("pick a number", Some("1"))?;
            match answer.trim().parse::<usize>() {
                Ok(picked) if picked >= 1 && picked <= options.len() => return Ok(picked - 1),
                _ => println!("  a number between 1 and {}", options.len()),
            }
        }
    }

    fn is_interactive(&self) -> bool {
        self.interactive
    }
}

/// Turn terminal echo off (`on = false`) and answer whether it was a terminal
/// at all. `stty` is the one tool that does this without a dependency, and it
/// is present wherever this tool's Unix sockets are.
fn turn_echo(on: bool) -> bool {
    #[cfg(unix)]
    {
        if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            return false;
        }
        let flag = if on { "echo" } else { "-echo" };
        std::process::Command::new("stty")
            .arg(flag)
            .stdin(std::process::Stdio::inherit())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = on;
        false
    }
}

/// `onlyne-im login --interactive`: authenticate one backend and verify the
/// credential against the platform, without touching any conversation.
///
/// This is `setup`'s second step on its own, which is what an operator wants
/// when a token expired: a conversation and a cluster ACL already exist, and the
/// only thing that changed is the credential.
pub async fn login(args: &SetupArgs) -> Result<()> {
    let mut prompter = TermPrompter::new(!args.non_interactive);
    let file = GatewayFile::load(&args.root)
        .with_context(|| format!("reading the gateway root {}", args.root.display()))?;
    if file.backends.is_empty() {
        bail!(
            "{} declares no `[[im.backend]]`; add one with `onlyne-im init` or by hand",
            args.root.display()
        );
    }
    let backend = pick_backend(args.gateway.as_deref(), &file, &mut prompter)?;
    authenticate(
        &args.root,
        args.credential.as_deref(),
        &file,
        &backend,
        &mut prompter,
    )
    .await?;
    prompter.say(&format!(
        "{} is authenticated; `onlyne-im run --root {}` will reach the platform",
        backend.gateway,
        args.root.display()
    ));
    Ok(())
}

/// Run the wizard.
pub async fn setup(args: &SetupArgs) -> Result<()> {
    let mut prompter = TermPrompter::new(!args.non_interactive);
    let file = GatewayFile::load(&args.root)
        .with_context(|| format!("reading the gateway root {}", args.root.display()))?;
    if file.backends.is_empty() {
        bail!(
            "{} declares no `[[im.backend]]`; add one with `onlyne-im init` or by hand",
            args.root.display()
        );
    }

    let backend = pick_backend(args.gateway.as_deref(), &file, &mut prompter)?;
    authenticate(
        &args.root,
        args.credential.as_deref(),
        &file,
        &backend,
        &mut prompter,
    )
    .await?;

    let conversations = pick_conversations(args, &backend, &mut prompter).await?;
    if conversations.is_empty() {
        bail!("no conversation was chosen; nothing to bind");
    }

    for conversation in &conversations {
        let slug = ask_slug(&mut prompter, &backend, conversation)?;
        let generate_args = GenerateArgs {
            root: args.root.clone(),
            gateway: backend.gateway.clone(),
            conversation: conversation.clone(),
            role: Some(slug.clone()),
            peers: args.peers.clone(),
            server_root: args.server_root.clone(),
            socket: args.socket.clone(),
        };
        generate(&generate_args)
            .await
            .with_context(|| format!("binding conversation {conversation} as `{slug}`"))?;
    }

    prompter.say("");
    prompter.say(&format!(
        "bound {} conversation(s) to backend {}",
        conversations.len(),
        backend.gateway
    ));
    prompter.say("start the gateway with `onlyne-im run --root <this root>`");
    Ok(())
}

/// Which `[[im.backend]]` this run is for.
///
/// `wanted` is the `--gateway` name, or `None` to ask (or to take the only one),
/// so the same choice serves `setup` and `login --interactive`.
fn pick_backend(
    wanted: Option<&str>,
    file: &GatewayFile,
    prompter: &mut dyn Prompter,
) -> Result<BackendEntry> {
    if let Some(wanted) = wanted.filter(|name| !name.trim().is_empty()) {
        return file.backend(wanted).cloned().ok_or_else(|| {
            let known: Vec<&str> = file.backends.iter().map(|b| b.gateway.as_str()).collect();
            anyhow::anyhow!(
                "no `[[im.backend]]` names gateway {wanted:?}; this root declares {}",
                if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                }
            )
        });
    }
    if file.backends.len() == 1 {
        return Ok(file.backends[0].clone());
    }
    let labels: Vec<String> = file
        .backends
        .iter()
        .map(|backend| format!("{} ({})", backend.gateway, backend.run.join(" ")))
        .collect();
    let picked = prompter.choose("which backend does this run set up?", &labels)?;
    Ok(file.backends[picked].clone())
}

/// Get a credential the platform accepts, and verify it before anything else.
///
/// `root` is the gateway root the credential belongs to and `supplied` is a
/// credential the caller passed, if any; everything else is read from disk, so
/// `setup` and `login --interactive` share this one path.
async fn authenticate(
    root: &Path,
    supplied: Option<&str>,
    file: &GatewayFile,
    backend: &BackendEntry,
    prompter: &mut dyn Prompter,
) -> Result<()> {
    let credential_path = onlyne_im_backend::credential_path(root, &backend.gateway);

    // A stored credential is tried first: a second `setup` run should not ask
    // for a token the operator already gave.
    if credential_path.is_file() {
        let stored = onlyne_im_backend::credential::read_file(&credential_path).unwrap_or_default();
        if !stored.trim().is_empty() {
            match verify(root, backend, &file.server).await {
                Ok(Some(identity)) => {
                    prompter.say(&format!("already authenticated as {identity}"));
                    return Ok(());
                }
                Ok(None) => {
                    prompter.say("the stored credential was accepted");
                    return Ok(());
                }
                Err(error) => {
                    prompter.say(&format!("the stored credential was refused: {error:#}"))
                }
            }
        }
    }

    let mut attempt = supplied.map(str::to_string);
    loop {
        let credential = match attempt.take() {
            Some(credential) => credential,
            None if prompter.is_interactive() => {
                let prompt = onboarding_prompt(root, backend, &file.server)
                    .await?
                    .unwrap_or_else(|| "paste the credential".to_string());
                prompter.say(&prompt);
                let answer = prompter.ask_secret("credential")?;
                if answer.is_empty() {
                    bail!("no credential given");
                }
                answer
            }
            None => bail!(
                "this backend has no credential and this run is non-interactive; \
                 pass --credential"
            ),
        };

        // Write first, verify second: the login path is driven by the gateway
        // root's own credential file, so what gets verified is exactly what a
        // later `run` will read.
        std::fs::create_dir_all(
            credential_path
                .parent()
                .context("the credential path has no parent")?,
        )
        .with_context(|| format!("creating {}", credential_path.display()))?;
        std::fs::write(&credential_path, credential.as_bytes())
            .with_context(|| format!("writing {}", credential_path.display()))?;
        onlyne_wire::socket::apply_private_mode(&credential_path)
            .map_err(|error| anyhow::anyhow!("{}: {error}", credential_path.display()))?;

        match verify(root, backend, &file.server).await {
            Ok(Some(identity)) => {
                prompter.say(&format!("authenticated as {identity}"));
                return Ok(());
            }
            Ok(None) => {
                prompter.say("the credential was accepted");
                return Ok(());
            }
            Err(error) => {
                prompter.say(&format!("that credential was refused: {error:#}"));
                if !prompter.is_interactive() {
                    return Err(error);
                }
                let again = prompter.ask("try another credential? (y/n)", Some("y"))?;
                if !again.starts_with('y') && !again.starts_with('Y') {
                    return Err(error);
                }
            }
        }
    }
}

/// Spawn the backend, hand it the stored credential, and read its verdict.
///
/// The process is thrown away afterwards: this is the handshake, not the run.
async fn verify(root: &Path, backend: &BackendEntry, _server: &str) -> Result<Option<String>> {
    let credential = onlyne_im_backend::credential_path(root, &backend.gateway);
    let mut host = BackendHost::spawn(&backend.run, &backend.gateway, &credential).await?;
    let hello = host.hello().await?;
    if hello.authenticated {
        // Some platforms can prove the credential in their hello (a local
        // token file is not proof, but a backend that already knows it is fine
        // with it says so); ask anyway, so the operator sees the identity.
    }
    let credential_value = onlyne_im_backend::credential::read_file(&credential)?;
    let result = host
        .login(WireLoginArgs {
            gateway: backend.gateway.clone(),
            credential: credential_value,
        })
        .await;
    let _ = host.stop("setup verified the credential").await;
    let result = result?;
    if result.ok {
        Ok(result.identity)
    } else {
        bail!(
            "{}",
            result
                .reason
                .unwrap_or_else(|| "the backend refused the credential".to_string())
        )
    }
}

/// What the backend says it needs, asked live so the sentence is the platform's
/// own rather than a guess made here.
async fn onboarding_prompt(
    root: &Path,
    backend: &BackendEntry,
    _server: &str,
) -> Result<Option<String>> {
    let credential = onlyne_im_backend::credential_path(root, &backend.gateway);
    let mut host = match BackendHost::spawn(&backend.run, &backend.gateway, &credential).await {
        Ok(host) => host,
        Err(error) => {
            // A backend that will not start is worth saying out loud, but the
            // wizard can still take a credential a human supplies.
            return Ok(Some(format!(
                "the backend did not start ({error:#}); paste its credential"
            )));
        }
    };
    let hello = host.hello().await?;
    let prompt = hello.onboarding.as_ref().map(|prompt| {
        let hint = match prompt.kind {
            OnboardingKind::ManualCode => "paste",
            OnboardingKind::Qr => "scan the QR the platform shows, or paste",
        };
        format!("{hint}: {}", prompt.payload)
    });
    let _ = host.stop("setup read the onboarding prompt").await;
    Ok(prompt)
}

/// Which conversations to bind: the backend's own list when it has one, and a
/// hand-typed id when it does not.
async fn pick_conversations(
    args: &SetupArgs,
    backend: &BackendEntry,
    prompter: &mut dyn Prompter,
) -> Result<Vec<String>> {
    if !args.conversations.is_empty() {
        return Ok(args.conversations.clone());
    }
    if !prompter.is_interactive() {
        bail!("no conversation named; pass --conversation <id>");
    }

    let listed = list_conversations(&args.root, backend)
        .await
        .unwrap_or_default();
    if listed.is_empty() {
        prompter.say(
            "this backend reports no conversation list; name the conversation id it \
             should serve (for a group, the chat id the platform shows)",
        );
        let mut chosen = Vec::new();
        loop {
            let id = prompter.ask("conversation id", None)?;
            if id.is_empty() {
                break;
            }
            chosen.push(id);
            let more = prompter.ask("another? (y/n)", Some("n"))?;
            if !more.starts_with('y') && !more.starts_with('Y') {
                break;
            }
        }
        return Ok(chosen);
    }

    prompter.say("the backend can see these conversations:");
    for conversation in &listed {
        prompter.say(&format!("  {}", describe(conversation)));
    }
    prompter.say("enter the numbers to bind, separated by spaces (or an id by hand)");
    let answer = prompter.ask("conversations", None)?;
    let mut chosen = Vec::new();
    for token in answer.split_whitespace() {
        match token.parse::<usize>() {
            Ok(index) if index >= 1 && index <= listed.len() => {
                chosen.push(listed[index - 1].id.clone())
            }
            _ => chosen.push(token.to_string()),
        }
    }
    Ok(chosen)
}

/// The backend's own listing, thrown away on any failure: a backend that cannot
/// list is not a backend that cannot be set up.
async fn list_conversations(root: &Path, backend: &BackendEntry) -> Result<Vec<ImConversation>> {
    let credential = onlyne_im_backend::credential_path(root, &backend.gateway);
    let mut host = BackendHost::spawn(&backend.run, &backend.gateway, &credential).await?;
    let _ = host.hello().await?;
    let listed = host.list_conversations().await?;
    let _ = host.stop("setup read the conversation list").await;
    Ok(listed)
}

/// One conversation as a line an operator reads.
fn describe(conversation: &ImConversation) -> String {
    match (&conversation.title, &conversation.kind) {
        (Some(title), Some(kind)) => format!("{} — {title} ({kind})", conversation.id),
        (Some(title), None) => format!("{} — {title}", conversation.id),
        (None, Some(kind)) => format!("{} ({kind})", conversation.id),
        (None, None) => conversation.id.clone(),
    }
}

/// The slug for one conversation: a name derived from the ids, confirmed by the
/// operator, because the directory name is the only handle a human has on it.
fn ask_slug(
    prompter: &mut dyn Prompter,
    backend: &BackendEntry,
    conversation: &str,
) -> Result<String> {
    let suggested = crate::workspace::derive_slug(&backend.gateway, conversation);
    if !prompter.is_interactive() {
        return Ok(suggested);
    }
    let answer = prompter.ask(
        &format!("short name for conversation {conversation}"),
        Some(&suggested),
    )?;
    crate::workspace::validate_slug(&answer)?;
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A prompter that answers from a script and records what it was asked.
    #[derive(Default)]
    struct Scripted {
        answers: Vec<String>,
        secrets: Vec<String>,
        choices: Vec<usize>,
        said: Vec<String>,
        questions: Vec<String>,
    }

    impl Scripted {
        fn with(answers: &[&str], secrets: &[&str], choices: &[usize]) -> Self {
            Self {
                answers: answers.iter().map(|s| s.to_string()).collect(),
                secrets: secrets.iter().map(|s| s.to_string()).collect(),
                choices: choices.to_vec(),
                ..Self::default()
            }
        }
    }

    impl Prompter for Scripted {
        fn say(&mut self, line: &str) {
            self.said.push(line.to_string());
        }
        fn ask(&mut self, question: &str, default: Option<&str>) -> Result<String> {
            self.questions.push(question.to_string());
            if self.answers.is_empty() {
                return Ok(default.unwrap_or_default().to_string());
            }
            let answer = self.answers.remove(0);
            if answer.is_empty() {
                Ok(default.unwrap_or_default().to_string())
            } else {
                Ok(answer)
            }
        }
        fn ask_secret(&mut self, _question: &str) -> Result<String> {
            if self.secrets.is_empty() {
                bail!("no secret scripted");
            }
            Ok(self.secrets.remove(0))
        }
        fn choose(&mut self, question: &str, _options: &[String]) -> Result<usize> {
            self.questions.push(question.to_string());
            if self.choices.is_empty() {
                return Ok(0);
            }
            Ok(self.choices.remove(0))
        }
        fn is_interactive(&self) -> bool {
            true
        }
    }

    #[test]
    fn a_lone_backend_is_chosen_without_asking() {
        let file = GatewayFile {
            role: "im".into(),
            server: "127.0.0.1:1".into(),
            cert_pin: "sha256/x".into(),
            backends: vec![entry("tg1")],
            pushes: Vec::new(),
            listen: Default::default(),
        };
        let args = SetupArgs {
            root: PathBuf::from("/root"),
            gateway: None,
            credential: None,
            conversations: Vec::new(),
            peers: Vec::new(),
            server_root: None,
            socket: None,
            non_interactive: true,
        };
        let mut prompter = Scripted::default();
        let picked = pick_backend(args.gateway.as_deref(), &file, &mut prompter).unwrap();
        assert_eq!(picked.gateway, "tg1");
        assert!(prompter.questions.is_empty(), "it asked anyway");
    }

    #[test]
    fn a_named_backend_that_is_absent_is_refused_with_the_known_ones() {
        let file = GatewayFile {
            role: "im".into(),
            server: "127.0.0.1:1".into(),
            cert_pin: "sha256/x".into(),
            backends: vec![entry("tg1")],
            pushes: Vec::new(),
            listen: Default::default(),
        };
        let args = SetupArgs {
            root: PathBuf::from("/root"),
            gateway: Some("nope".into()),
            credential: None,
            conversations: Vec::new(),
            peers: Vec::new(),
            server_root: None,
            socket: None,
            non_interactive: true,
        };
        let mut prompter = Scripted::default();
        let error = pick_backend(args.gateway.as_deref(), &file, &mut prompter).unwrap_err();
        assert!(error.to_string().contains("tg1"), "{error}");
    }

    #[test]
    fn several_backends_are_offered_as_a_menu() {
        let file = GatewayFile {
            role: "im".into(),
            server: "127.0.0.1:1".into(),
            cert_pin: "sha256/x".into(),
            backends: vec![entry("tg1"), entry("fs1")],
            pushes: Vec::new(),
            listen: Default::default(),
        };
        let args = SetupArgs {
            root: PathBuf::from("/root"),
            gateway: None,
            credential: None,
            conversations: Vec::new(),
            peers: Vec::new(),
            server_root: None,
            socket: None,
            non_interactive: true,
        };
        let mut prompter = Scripted::with(&[], &[], &[1]);
        let picked = pick_backend(args.gateway.as_deref(), &file, &mut prompter).unwrap();
        assert_eq!(picked.gateway, "fs1");
    }

    #[test]
    fn a_slug_defaults_to_the_derived_name_and_an_answer_overrides_it() {
        let backend = entry("tg1");
        let mut agreed = Scripted::with(&[""], &[], &[]);
        assert_eq!(
            ask_slug(&mut agreed, &backend, "-1007788").unwrap(),
            "tg1-1007788"
        );
        let mut overridden = Scripted::with(&["project-alpha"], &[], &[]);
        assert_eq!(
            ask_slug(&mut overridden, &backend, "-1007788").unwrap(),
            "project-alpha"
        );
    }

    #[test]
    fn a_conversation_line_names_what_the_platform_reports() {
        let full = ImConversation {
            id: "-100".into(),
            title: Some("Project".into()),
            kind: Some("group".into()),
        };
        assert_eq!(describe(&full), "-100 — Project (group)");
        let bare = ImConversation {
            id: "-100".into(),
            ..ImConversation::default()
        };
        assert_eq!(describe(&bare), "-100");
    }

    fn entry(gateway: &str) -> BackendEntry {
        BackendEntry {
            gateway: gateway.to_string(),
            run: vec!["onlyne-im-backend-fake".to_string()],
        }
    }
}
