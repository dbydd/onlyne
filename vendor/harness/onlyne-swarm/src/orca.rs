use anyhow::Context;
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

/// Blocking single-request client for swarm.sock (used by CLI + TUI bootstrap).
pub fn client_request(sock: &Path, req: Value) -> anyhow::Result<Value> {
    let mut s = UnixStream::connect(sock).with_context(|| format!("connect {}", sock.display()))?;
    let mut line = serde_json::to_string(&req)?;
    line.push('\n');
    // Every caller here is a one-shot CLI request, so the read is bounded: a
    // scheduler that is alive but wedged must produce an error and a non-zero
    // exit, not a terminal that hangs until someone kills it.
    const CLI_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
    s.set_read_timeout(Some(CLI_RESPONSE_TIMEOUT))
        .with_context(|| format!("set read timeout on {}", sock.display()))?;
    s.write_all(line.as_bytes())?;
    let mut r = BufReader::new(&s);
    let mut out = String::new();
    if let Err(err) = r.read_line(&mut out) {
        if err.kind() == std::io::ErrorKind::WouldBlock
            || err.kind() == std::io::ErrorKind::TimedOut
        {
            anyhow::bail!(
                "no reply from {} within {}s; the scheduler is running but did not answer",
                sock.display(),
                CLI_RESPONSE_TIMEOUT.as_secs()
            );
        }
        return Err(err).with_context(|| format!("read from {}", sock.display()))?;
    }
    if out.trim().is_empty() {
        anyhow::bail!("{} closed the connection without answering", sock.display());
    }
    Ok(serde_json::from_str(&out).with_context(|| "parse swarm.sock response")?)
}
