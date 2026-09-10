---
name: onlyne
description: Use when an agent needs to send tasks, inspect the ledger, watch events, manage clients, generate workspaces, or operate Onlyne v1.0.0 sockets.
---

# Onlyne

## Overview

Onlyne v1.0.0 is a local channel and routing layer for agents. The server routes envelopes and holds the ledger. The client runs one role's sessions inside one workspace. The gateway translates one chat platform. Agent and gateway plugins use the same adapter protocol on different mount kinds.

## Rules

- Keep runtime state under the selected `<server-root>/.onlyne/` or `<workspace>/.onlyne/` tree.
- Treat `<server-root>/.onlyne/spec.toml` as the single source of truth for roles, keys, ACLs, prose, gateways, and routes.
- Use `onlyne-client init` or `onlyne server generate` to create role keys and workspace config.
- Append generated `[[client]]` fragments to `spec.toml`, then run `onlyne reload`.
- Use `ONLYNE_BACKEND=fake` plus `onlyne-agent-fake` for local e2e checks.
- Keep real platform credentials out of smoke runs.

## Resolve a socket

CLI socket resolution order:

1. `--socket <path>` uses the exact socket path.
2. `--server-root <dir>` uses `<dir>/.onlyne/run/s` for the admin surface.
3. `--workspace <dir>` uses `<dir>/.onlyne/run/s` for the client surface.
4. Upward discovery from the current directory finds `.onlyne/run/s`.

Missing socket failure is exit 3:

```text
onlyne: no onlyne socket found; pass --socket, --server-root, or --workspace
```

Useful probes:

```bash
onlyne --server-root "$SERVER" status
onlyne --workspace "$WS" ping
onlyne --socket "$SOCK" status
```

## Send a task

Send a role-addressed task through the admin surface:

```bash
onlyne --server-root "$SERVER" send --from planner --to builder --text "build the patch"
```

`--from` is required for this admin-surface send and rejected for client-surface sends.
Send from a role workspace through the client surface:

```bash
onlyne --workspace "$WS" send --to reviewer --text "review this change"
```

Use `--task <id>` to attach causality to an existing task family. Use `--note` for free text that creates no session. Offline `note` delivery returns `recipient_offline`, and the row settles `rejected`. `reply --to <envelope-id>` answers that ledger row and addresses its recipient.

## Check a ledger row

Read a task from the server ledger:

```bash
onlyne --server-root "$SERVER" ledger --task "$TASK"
```

Expected task path in the primary smoke run:

```text
queued -> in_flight -> acked
```

The completion head appears in `out_head`. Session state lives in the server projection and client-authoritative `client.db`.

## Watch events

Watch the admin event stream:

```bash
onlyne --server-root "$SERVER" watch
```

Watch from a role workspace:

```bash
onlyne --workspace "$WS" watch
```

Observation events are at-most-once. Event frames carry monotonic `seq`. A lagging client resubscribes with `since_seq` after comparing `pong.server_seq` with its cursor.

## CLI contract

Top-level groups are `onlyne server <verb>`, `onlyne client <verb>`, and `onlyne gateway <verb>`. Under `server`, the lifecycle verbs (`init`, `run`, `start`, `stop`, `status`, `generate`, `reload`) exec `onlyne-server` and the admin nouns (`roles`, `sessions`, `ledger`, `faults`, `watch`, `history`, `repair`) query the admin socket. There is no `onlyne forward` verb. `spec_diff` takes the `spec-diff` alias. `--timeout` is primary with the `--timeout-ms` alias. `wait-ready` takes `--interval-ms` (default 200) under the global `--timeout` bound (default 10000). `--from` is a per-verb flag on `send`, `reply`, `complete`, `handoff`, and `control` for the admin surface only. Message and admin verbs print one JSON line; `cluster export-prose` prints raw prose unless `--json`. Exit codes: 0 success, 1 failed daemon answer or `wait-ready` bound hit, 2 local validation, 3 no socket, 127 missing sibling binary, 4 propagated generate-child failure.

## Start and stop a client

Foreground role daemon:

```bash
onlyne-client run --workspace "$WS"
```

Managed client commands through the thin entrypoint:

```bash
onlyne client start --workspace "$WS"
onlyne client status --workspace "$WS"
onlyne client stop --workspace "$WS"
```

A client owns one role. Running sessions reach terminal state during disconnect. Outgoing completions persist as intents and flush after reconnect.

## Register a role with init

Create a minimal workspace and a `[[client]]` fragment:

```bash
onlyne-client init --workspace "$WS" --role "$ROLE" --server-root "$SERVER" > "$ROLE.spec.toml"
cat "$ROLE.spec.toml" >> "$SERVER/.onlyne/spec.toml"
onlyne --server-root "$SERVER" reload
```

The fragment starts with:

```toml
[[client]]
role = "planner"
key = "ed25519/<base64>"
```

`init` creates `W/.onlyne/keys/role.key` and `W/.onlyne/config.toml`. The operator owns the spec append and reload.

## Generate workspaces

Generate role workspaces from server templates, either top-level or through the server forward:

```bash
onlyne generate --root "$SERVER" --out "$OUT" > "$OUT/spec-frag.toml"
```

```bash
onlyne server generate --root "$SERVER" --out "$OUT" > "$OUT/spec-frag.toml"
```

The default output root is `<server-root>/.onlyne/ws`. Template selection uses role name basename matching under `template_root`. `--template` and `--role` select an intersection.

Closed placeholders:

```text
{{role}}
{{cluster}}
{{server_name}}
{{listen}}
{{cert_pin}}
{{admin}}
{{max_sessions}}
{{agent_package}}
```

Generation scans output bytes for absolute paths. A hit deletes the generated output and exits 4 with:

```text
onlyne: generated workspace embeds absolute path <path>
```

## Run a fake agent

Build first, then run the fake backend and fake agent script:

```bash
cargo build --workspace
ONLYNE_BACKEND=fake onlyne-client run --workspace "$WS" &
onlyne-agent-fake --workspace "$WS" --script \
  "$SRC/crates/onlyne-testkit/scripts/echo-complete.json" &
```

`echo-complete.json` waits for `assign`, reports ready, completes with `outcome = "done"` using the assign body as its head, and writes the received `assign.prose` to `prose.log` inside the workspace.

| Code | Meaning | Exact user-facing string |
|---|---|---|
| 0 | Success | one JSON line for message and admin verbs |
| 1 | Failed daemon answer or `wait-ready` bound hit | daemon `ok:false` answer or `onlyne: server not ready after <ms>ms` |
| 2 | Legacy workspace layout | `onlyne: legacy workspace layout; v1.0.0 does not migrate` |
| 2 | Local validation | `onlyne: --from is only valid on the admin surface`, `onlyne: --from is required on the admin surface`, `onlyne: --ttl requires --note` |
| 3 | Socket resolution failure | `onlyne: no onlyne socket found; pass --socket, --server-root, or --workspace` |
| 127 | Missing sibling binary | `onlyne: binary not found: <name>` |
| 4 | Existing output refusal | `onlyne: refusing to overwrite <path>; pass --force` |
| 4 | Ambiguous template | `onlyne: template for role <r> is ambiguous: <p1>, <p2>` |
| 4 | Missing template | `onlyne: no template directory named <r> under <template_root>` |
| 4 | Empty role/template intersection | `onlyne: no role matches the requested templates/roles` |
| 4 | Absolute path in generated output | `onlyne: generated workspace embeds absolute path <path>` |
| 4 | Missing `agent_package` for placeholder use | `onlyne: agent_package not set in spec.toml [server]` |

Other hard failures:

```text
spec.toml:<line>: <message>
onlyne: unsupported schema; v1.0.0 does not migrate
onlyne: binary not found: <name>
```

Frame error codes are closed: `invalid`, `unknown_op`, `acl_denied`, `unknown_role`, `recipient_offline`, `duplicate`, `conflict`, `unauthorized`, `forbidden`, `not_admin`, `frame_too_large`, `bad_frame`, `protocol_version`, and `internal`.

## Smoke

Verification case 1 local task run (the landed script drives server, client, fake agent, `ledger`, and `sessions`):

1. Build the workspace.
   ```bash
   cargo build --workspace
   ```
2. Set source and temp roots.
   ```bash
   SRC=$(pwd); tmp=$(mktemp -d)
   ```
3. Initialize the server root.
   ```bash
   "$SRC/target/debug/onlyne-server" init --root "$tmp/server" --listen 127.0.0.1:7899
   ```
4. Run the server.
   ```bash
   "$SRC/target/debug/onlyne-server" run --root "$tmp/server" &
   ```
5. Wait for readiness.
   ```bash
   "$SRC/target/debug/onlyne" --server-root "$tmp/server" wait-ready
   ```
6. Initialize the planner role and capture the registration fragment.
   ```bash
   "$SRC/target/debug/onlyne-client" init --workspace "$tmp/planner" --role planner \
     --server-root "$tmp/server" > "$tmp/planner.spec.toml"
   ```
7. Append the fragment and reload the server spec.
   ```bash
   cat "$tmp/planner.spec.toml" >> "$tmp/server/.onlyne/spec.toml"
   "$SRC/target/debug/onlyne" --server-root "$tmp/server" reload
   ```
8. Run the planner client.
   ```bash
   "$SRC/target/debug/onlyne-client" run --workspace "$tmp/planner" &
   ```
9. Run the fake agent.
   ```bash
   "$SRC/target/debug/onlyne-agent-fake" --workspace "$tmp/planner" --script \
     "$SRC/crates/onlyne-testkit/scripts/echo-complete.json" &
   ```
10. Send the local task and inspect the resulting task in `ledger` and `sessions`.
   ```bash
   "$SRC/target/debug/onlyne" --server-root "$tmp/server" send --from planner --to planner --text "hello v1"
   ```
