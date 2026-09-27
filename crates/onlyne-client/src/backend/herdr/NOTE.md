# Herdr backend note

Status: **kept.** Dated 2026-09-23, by operator decision.

The backend was a candidate for removal for a cost that lands on the exact workload it
was written for: herdr wedges the system tty once a workspace carries many panes, and
one herdr tab per role with one split pane per session is the shape this backend
produces. The operator keeps it, so it stays a first-class selection: `herdr` stays in
`BACKEND_NAMES`, `auto` probes herdr first, and `ONLYNE_BACKEND=herdr` selects it.

## What a reader should know

- **A workspace that wants another backend names it** in `config.toml`
  (`backend = "orca"`, `"zellij"`, `"exec"`, `"acp"`, `"fake"`). A role that never
  reaches herdr is unaffected by the cost above.
- **`orca` carries the same shape with the fuller resource surface** — attach, probe,
  focus, rename, the worktree policy. `zellij` and `exec` cover the terminal cases,
  `acp` covers an agent that speaks the protocol on a pipe, and `fake` covers tests.
- **The warning capture is deterministic, and the fix is worth knowing about.**
  `workspace_create_warns_with_the_rename_remedy` failed only under a full-workspace
  parallel load, and the miss was the capture's, not the code's: the case reads one warning
  through `tracing::subscriber::with_default`, a thread-local subscriber, while sibling
  tests in the same binary reach the same `tracing::warn!` callsite (`policy.rs`,
  `find_or_create_workspace`) with no subscriber installed, which caches that callsite as
  never enabled. The backend is synchronous and emits the warning unconditionally on the
  create path. Both capture cases now read a process-wide collector
  (`tests/herdr.rs`, `warn_capture`), one global subscriber installed once, whose lines
  carry the thread that emitted them; the cache has no thread-local dispatcher to miss and
  two parallel cases still read only their own warnings. The empty capture had also let
  `a_found_workspace_emits_no_rename_remedy` pass for the wrong reason. Rebuilding the
  interest cache inside a thread-local scope was tried first and left the flake in place.

This file exists so a reader knows the tree's standing before spending a day on it.
