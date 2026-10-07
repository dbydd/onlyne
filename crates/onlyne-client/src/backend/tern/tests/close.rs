//! Close: one command, and a close that is a no-op when the block is already
//! gone.

use super::*;

#[test]
fn close_sends_the_block_id_with_no_force_flag() {
    // `tern close` takes no force flag and no reason, so both close paths are
    // the same command and the reason rides in the log.
    let script = Script::default().reply(
        "close 2147483660",
        serde_json::json!({"block": 2147483660u64}).to_string(),
    );
    let (backend, script) = backend(script);
    backend
        .close(&session_ref("2147483660"), CloseReason::Completed, true)
        .unwrap();
    assert_eq!(
        script.calls(),
        vec!["/Applications/Tern.app/Contents/MacOS/tern close 2147483660 --json".to_string()]
    );
}

#[test]
fn close_is_idempotent_for_a_block_that_is_already_closed() {
    // Measured: a closed block is refused with `no block is called \`N\`` on
    // stderr, exit 1, and no JSON body. There is no code to branch on — the
    // message is the whole machine-readable part of a refusal — so the second
    // end of a session is a no-op rather than a failure.
    let script = Script::default().refused(
        "close 2147483660",
        "tern close: no block is called `2147483660`",
    );
    let (backend, script) = backend(script);
    backend
        .close(&session_ref("2147483660"), CloseReason::Completed, false)
        .unwrap();
    assert_eq!(script.call_count(), 1);
}

#[test]
fn close_treats_a_gone_session_as_a_gone_block() {
    // A daemon that dropped the whole session refuses with `no session is
    // called`. The block went with it, so this end is a no-op too.
    let script = Script::default().refused(
        "close 2147483660",
        "tern close: no session is called `2147483648`",
    );
    let (backend, _) = backend(script);
    backend
        .close(&session_ref("2147483660"), CloseReason::Shutdown, false)
        .unwrap();
}

#[test]
fn close_reports_a_refusal_that_is_not_about_absence() {
    // Only the two "no such resource" wordings open the no-op path. Anything
    // else is the answer and reaches the caller unchanged, because a refusal
    // this backend cannot read is a session whose end did not happen.
    let script = Script::default().refused(
        "close 2147483660",
        "tern close: a session is already called `Default`",
    );
    let (backend, _) = backend(script);
    let error = backend
        .close(&session_ref("2147483660"), CloseReason::Completed, false)
        .unwrap_err();
    assert!(error.to_string().contains("already called"), "{error}");
}
