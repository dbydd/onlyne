# Herdr is deprecated

Status: **candidate for removal.** Dated 2026-09-23, by operator decision.

## Why

Herdr wedges the system tty once a workspace carries many panes, which is the shape
this backend produces: one herdr tab per role and one split pane per session. The
operator runs the ring topologies this repository is built for, so the backend's cost
lands on the exact workload it was written for.

## What this means for a reader

- **A failure in this tree, in `crates/onlyne-session/tests/herdr.rs`, or in
  `crates/onlyne-session/tests/herdr_live.rs` carries no product signal.** Do not chase
  one; record it and move on.
- The known flake is
  `workspace_create_warns_with_the_rename_remedy`. It fails only under a full-workspace
  parallel load, and it is a test defect: the test captures one warning through
  `tracing::subscriber::with_default`, a thread-local subscriber, while sibling tests in
  the same binary reach the same `tracing::warn!` callsite (`policy.rs`,
  `find_or_create_workspace`) with no subscriber installed, which caches that callsite as
  never enabled. The backend itself is synchronous and emits the warning unconditionally
  on the create path, so the missing line is the capture's, not the code's.
- `BACKEND_NAMES` still accepts `herdr`, and `auto` probes **herdr first**, then orca,
  then zellij. A workspace that wants another backend names it in its `config.toml`
  (`backend = "orca"`, `"zellij"`, `"exec"`, `"acp"`).

## What to use

`orca` carries the same shape and the fuller resource surface (attach, probe, focus,
rename, the worktree policy). `zellij` and `exec` cover the terminal cases, `acp` covers
an agent that speaks the protocol on a pipe, and `fake` covers tests.

## What removal would touch

- this module (`session.rs`, `resource.rs`, `policy.rs`, `cli.rs`, `tests.rs` and the
  `crates/onlyne-session/src/backend/herdr/` parts)
- `crates/onlyne-session/tests/herdr.rs` and `herdr_live.rs`
- `detect_host`'s probe table and the `herdr` arm of `BackendName`
- `BACKEND_NAMES` and `NO_SUPPORTED_HOST`, both of which name it to the operator
- the docs that describe it: `docs/v1-ARCHITECTURE.md`, `docs/operations.md`,
  `AGENTS.md`, and `docs/v1-PLAN.md`'s port notes (provenance; leave those)

Nothing here is scheduled. This file exists so a reader knows the tree's standing before
spending a day on it.
