//! The `onlyne-web` binary: resolve the admin socket, mint the token, bind
//! loopback, print the URL, and serve the boards.
//!
//! The socket is resolved exactly as the TUI resolves it — `--socket`, then
//! `ONLYNE_SOCKET`, then `--server-root`, then the current directory walked
//! upward for the owner tree — with one difference: this surface needs the
//! admin socket, so a tree whose registration names a client is refused with
//! the same sentence the TUI gives.

use onlyne_web::{ensure_loopback, mint_token, App};
use onlyne_wire::socket::{read_registration, socket_path, RegistrationKind};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;

const USAGE: &str = "\
onlyne-web — the optional graphical front end

USAGE:
    onlyne-web [--server-root <dir> | --socket <path> | --workspace <dir>]
               [--bind <addr:port>] [--timeout <ms>] [--open]

The bind is 127.0.0.1 with an assigned port unless --bind names another one,
which is the explicit flag a non-loopback bind requires. A random token is
minted at startup and printed in the URL; every request must carry it.";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut socket: Option<PathBuf> = None;
    let mut server_root: Option<PathBuf> = None;
    let mut workspace: Option<PathBuf> = None;
    let mut bind: Option<SocketAddr> = None;
    let mut timeout_ms: u64 = 8000;
    let mut open = false;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let mut value = || -> String {
            index += 1;
            args.get(index)
                .cloned()
                .unwrap_or_else(|| format!("{arg} needs a value"))
        };
        match arg {
            "--socket" => socket = Some(PathBuf::from(value())),
            "--server-root" => server_root = Some(PathBuf::from(value())),
            "--workspace" => workspace = Some(PathBuf::from(value())),
            "--bind" => match value().parse() {
                Ok(addr) => bind = Some(addr),
                Err(_) => die(&format!("{arg} wants an <addr:port>")),
            },
            "--timeout" => match value().parse() {
                Ok(ms) => timeout_ms = ms,
                Err(_) => die(&format!("{arg} wants a number of milliseconds")),
            },
            "--open" => open = true,
            "--help" | "-h" => {
                println!("{USAGE}");
                return;
            }
            "--version" | "-V" => {
                println!("onlyne-web {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            other => die(&format!("unknown flag {other}; --help lists them")),
        }
        index += 1;
    }

    let socket = resolve_socket(
        socket.as_deref(),
        server_root.as_deref(),
        workspace.as_deref(),
    );
    let bind = bind.unwrap_or(SocketAddr::from(([127, 0, 0, 1], 0)));
    if let Err(reason) = ensure_loopback(bind, bind_flag_given(&args)) {
        die(&reason);
    }
    let token = mint_token();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("start the tokio runtime");
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(bind)
            .await
            .unwrap_or_else(|error| die(&format!("bind {bind}: {error}")));
        let served = listener
            .local_addr()
            .expect("a bound listener names its address");
        let app = App::new(socket.clone(), timeout_ms, token.clone(), served);
        let url = format!("http://{served}/?token={token}");
        println!("onlyne-web: serving {url}");
        println!("onlyne-web: watching {}", socket.display());
        if open {
            open_browser(&url);
        }
        axum::serve(listener, app.router())
            .await
            .expect("serve the web surface");
    });
}

/// Whether the operator passed `--bind`, which is the explicit flag a
/// non-loopback bind requires.
fn bind_flag_given(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--bind")
}

/// Resolve the admin socket: `--socket`, then `ONLYNE_SOCKET`, then
/// `--server-root`, then `--workspace` or the current directory walking
/// upward for the owner tree — the precedence every verb uses.
fn resolve_socket(
    flag: Option<&Path>,
    server_root: Option<&Path>,
    workspace: Option<&Path>,
) -> PathBuf {
    if let Some(path) = flag {
        return absolutize(path);
    }
    if let Ok(path) = std::env::var("ONLYNE_SOCKET") {
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }
    let start = server_root
        .or(workspace)
        .map(|start| std::path::absolute(start).unwrap_or_else(|_| start.to_path_buf()));
    let root = match start {
        Some(start) => owner_root(&start),
        None => owner_root(&std::env::current_dir().expect("name the current directory")),
    };
    let Some(root) = root else {
        die("onlyne: no socket; pass --server-root <dir> or --socket <path>");
    };
    let path = socket_path(&root).unwrap_or_else(|error| {
        die(&format!("onlyne: no socket: {error}"));
    });
    match read_registration(&root) {
        Ok(Some(reg)) if reg.kind == RegistrationKind::Server => path,
        _ => die(
            "onlyne-web needs the admin surface; pass --server-root <dir>, \
             or --socket <path>",
        ),
    }
}

/// The owner tree `dir` belongs to: walk upward for `.onlyne/`.
fn owner_root(dir: &Path) -> Option<PathBuf> {
    let mut current = Some(dir);
    while let Some(dir) = current {
        if dir.join(".onlyne").is_dir() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

fn absolutize(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

fn open_browser(url: &str) {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    match Command::new(program).arg(url).spawn() {
        Ok(_) => {}
        Err(error) => eprintln!("onlyne-web: could not open the browser: {error}"),
    }
}

/// Exit with one sentence; the operator's next move should be readable in it.
fn die(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(2);
}
