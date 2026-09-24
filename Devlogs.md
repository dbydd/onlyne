# Development logs

This file records engineering history and release evidence. User-facing installation, operation, and architecture guidance lives in `README.md` and `README.zh-CN.md`. Dated release narratives and historical test readings live in `CHANGELOG.md` and `docs/live-evidence-1.4.0.md`.

## v1.4.0 release window

- The workspace version is 1.4.0 across nineteen Rust crates.
- Git tag `v1.4.0` points to the release commit.
- The npm package `pi-onlyne` is version 1.2.0.
- The local gate passed: formatting, clippy, and workspace tests. The test run recorded 1101 passed, 0 failed, and 1 ignored across 69 targets.
- The Windows CI job on the release commit reports handbook line-ending failures and a restart timing failure. The Linux job is green.
- The shipped handbooks are regular files under `crates/onlyne-cli/skills/`. `onlyne skill export` writes the same bytes into a workspace skill tree.
- The current `onlyne` binary reports version 1.4.0 and protocol 1. The locally rebuilt daemon binaries are resolved from the repository target directory.

## Product changes recorded during this window

- The client session lifecycle derives state from plugin reports, heartbeat liveness, durable delivery records, and reconnect policy.
- `reuse` and `session_sync` were removed. Each task owns a session, and the client-to-server vocabulary has twelve verbs.
- `onlyne_handoff{to, text, image}` continues a task family from a plugin session. `onlyne_send{kind:"task"}` starts a new family.
- Task causality carries `family`, `hop_budget`, `origin`, `deadline`, and `labels`.
- `requeue_ttl_secs` can settle an automatically requeued delivery as `expired` with reason `requeue_ttl`. The default value 0 leaves the prior queue behavior unchanged.
- The TUI accepts `--once --state active|all`; `a` toggles the global view; detail views accept `Home`, `End`, and `G`.
- The seven supervisor verbs require `--force` and `--yes-i-am-supervisor-not-other-role` before opening a socket.
- The pi adapter carries the handoff tool and assignment hop information.

## Evidence

The live acceptance record is `docs/live-evidence-1.4.0.md`. It contains the readings behind the release notes and the field observations that shaped the fixes. The historical release ledger is `CHANGELOG.md`.
