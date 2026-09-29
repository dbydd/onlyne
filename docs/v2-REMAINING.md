# v2 remaining work — the state after slice 10

Written 2026-09-29 at the end of the wave that landed `onlyne-web`. This file exists so the
next context can recover every open task without re-deriving it. It is a status record, not
a design document: the design lives in `docs/v2-PLAN.md` and `docs/v2-CONTRACT.md`.

## Where the task list came from

The list is not mine. The pinned session **`onlynev2dev`**
(`~/.omp/agent/sessions/-Documents-progs-onlyne/2026-09-26T08-51-40-762Z_01a0dce9-bc5a-70f0-9b6f-9ec65b3ced75.jsonl`)
ends with a todo of **32 items across 5 phases**, and with the user's completion criterion:

> 按 docs/v2-PLAN.md 完成阶段一到三全部项。完成标准：todo 全部 done；
> `cargo fmt --check`、`clippy -D warnings`、`cargo test --workspace` 全绿；
> pi 真实运行时验收（axonhub/generic-writer）通过；DSH 以 external 放置接入并跑通；
> CHANGELOG 记录 v2。真正需要用户决定的事用 ask 阻塞提问。

That session's last todo state (2026-09-26T15:19) read **2/32 done, 30 open**. Its 95
thinking blocks (947 K chars) are the design record; the deepest are the earliest, written
on opus-5.5. Read in this order: `b00` (10:34) the audit and the runtime/session model,
`b07` (10:39) the anti-corruption analysis, `b24` (11:52) the crate/test/network design,
`b28` (12:30) the 落地顺序 and the decisions, `b86` (14:11) the execution plan and the DSH
model. `b10`–`b27` are the drafting of `docs/v2-PLAN.md`, whose content is on disk.

Two decisions the user took on 2026-09-29, which override the plan's `migrate` proposal:

- **migrate**: no `migrate` verb. A version-mismatch **warning** and a refusal to start.
- **DSH**: being customised on another line. This repository supplies the **protocol
  interface and the protocol spec** for that work, and no DSH implementation.

## Landed

| phase | items | evidence |
|---|---|---|
| zero | 6/6 | `4fad380` |
| one | 8/8 | crates 15→**11**; `onlyne-wire` present; runtime-dir sockets `837cc70`; forwarding layer deleted; gateway frozen (tag `frozen/gateway-v1`, `crates/onlyne-gateway` gone from the tree); test fns 1170→**1036**; `AGENTS.md` is the v2 contract; scenario suite 11 green + 1 ignore (federation) |
| two | 12/15 | see below |
| three | 3/3 | `da415f2` slice 8 reducer · `fa2fbe7` TUI · `c21889d` onlyne-web |
| 验收 | 0/3 | |

Phase two, landed: session tables rekeyed on `session_id` with `session_tasks` (`282c77a`,
`edf8f11`); drive/placement split with suspend and resume (`aebbe11`); adapter protocol v2;
one delivery template with role prose in the instruction layer; `onlyne mcp` and the
`summary`/`details`/`files` completion vocabulary (`a9b924d`); payload-v2 deleted; one
turn-end rule with `Outcome::Blocked`; declarative route edges replacing `relay_required`
with the budget check moved to the client; `SpecGet`/`SpecApply` with a streaming
subscribe (`e48f73d`); event hooks (`da415f2` slice 7); liveness in memory, one write many
reads, machine-local unix socket (`e48f73d` slice 6); **schema bumped** — client 2→3,
server 4→6 (`CLIENT_SCHEMA_VERSION` in `onlyne-store/src/client.rs:31`,
`SERVER_SCHEMA_VERSION` in `onlyne-store/src/server.rs:46`).

Also landed this wave: `9cd6925` (`spec_reloaded` now re-reads the registry in both front
ends, in the subscribe **replay page** as well as the live stream — the page was the arm
that mattered, and its absence was the real cause of a 1-in-8 drag-case flake), plus
`running-lights.sh` rewritten because it had been reading a `onlyne tui --page 2` flag
that no longer exists.

## Open, item by item

### A — reconnect jitter, retry classification, accept timeout (phase two)

The audit item is *partly* closed and the part that is open is the part that was the
finding.

- `onlyne_net::handshake::accept_with_timeout` exists. **Done.**
- `Backoff::with_jitter` (`onlyne-net/src/backoff.rs:43`) still has **no production
  caller** — its only references are `onlyne-net/src/lib.rs:46` and its own definition.
  Every client therefore redials on the same schedule after a server restart.
- Two functions named `is_permanent` still return **opposite answers** for
  `Unauthorized`: `onlyne-proto/src/frame.rs:181` puts it in the retryable set, while
  `onlyne-net/src/conn.rs:712-715` returns true (permanent) for `NetError::Unauthorized`.
  `onlyne-server/src/router.rs:69-71` documents the proto side as the intended one, so
  the client's own supervisor stops redialling where the server expects a plugin to keep
  trying. The plan's wording was "统一成一个重试分类函数".

Done means: one retry classification, one answer for `unauthorized`, and a reconnect
delay that actually varies per client.

### B — the v2 migration path (phase two)

Per the user's 2026-09-29 decision there is **no `migrate` verb**. The deliverable is a
refusal that is accurate and actionable:

- Today every refusal prints `onlyne: unsupported schema; v1.0.0 does not migrate`
  (`onlyne-proto/src/text.rs:25`, `onlyne-store/src/error.rs:3`). "v1.0.0" is the old
  product's version, so a v2 operator reads a v1 sentence.
- There is **no exit code for it**. `crates/onlyne-cli/src/runtime.rs:13-21` has
  `EXIT_OK 0`, `EXIT_ANSWER_FAILED 1`, `EXIT_VALIDATION 2`, `EXIT_NO_SOCKET 3`,
  `EXIT_REFUSAL 4`, `EXIT_NO_SIBLING 127`. The schema refusal surfaces from the daemon's
  store open, not from a verb, so a supervisor cannot tell "needs migration" from "bad
  config". The plan asked for a distinct code (b24:107).
- The refusal should name the revision found and the revision expected, and say what the
  operator does: v2 starts with an empty ledger, the old database stays where it is
  (b28:143, "ledger 历史确认不迁移"), and the old spec/config is edited by hand.

Also in scope for the migration, and deliberately **not** automated: the v1 workspace
`config.toml` carried `backend`; v2 splits it into `drive` in `spec.toml` and
`placement` in the workspace config, plus a new `[client.session]` block (b28:217). A v1
config that still says `backend` must be **refused clearly**, not silently defaulted.

Done means: a v1 database or a v1-shaped config produces one accurate sentence, a
dedicated exit code, and a documented manual path. No rewriting of anything.

### C — DSH (phase two, protocol only)

`plugins/` holds only `onlyne-agent-pi`. There is no DSH plugin, and per the user's
decision there will not be one here. `Placement::External` exists and the drive × placement
matrix accepts `plugin × external` while refusing `acp × external` (stdio is the ACP
channel) — `onlyne-config/src/client.rs:649-676`.

What to deliver instead is the interface that work plugs into, which the pinned session
already designed (b00:155-181, b86:93-95):

- The adapter connection is scoped per **(runtime, role)**, not per session. One
  connection multiplexes that role's sessions by id. This is the change that lets one
  hosting runtime serve many roles.
- The connection direction is **plugin dials client, universally**. A hosting runtime
  learns the sockets it serves from the registration files in the runtime directory,
  matching its own runtime name.
- Two runtime kinds, differing only in how a session comes into existence: **spawned**
  (the client starts the process, the plugin connects back and serves that one session)
  and **hosting** (the client asks the already-connected runtime to `open` one).
- A spawned runtime's persistence is an **opaque `resume_handle`** the plugin reports and
  the client substitutes into the launch command on resume. The client never replays a
  journal to rebuild history — that summary *is* the context rot the design is avoiding.
- The runtime mount declares `open` / `resume` / `suspend` / `close`; a runtime that does
  not declare them serves only the session that started it.

Done means: a written specification of that surface, checked against
`crates/onlyne-adapter/PROTOCOL.md` for what already exists, plus whatever protocol or
spec gaps it exposes. No implementation.

### 验收 1 — pi real-runtime acceptance

`crates/onlyne-testkit/e2e/pi-live.sh` exists and **has never been run in this session**.
The criterion names `axonhub/generic-writer`; that id is not in the agent roster
(`~/.omp/agent/agents/` has `generic-researcher-weak` / `-powerful`), so the model to
drive pi with needs confirming before the run.

### 验收 2 — DSH acceptance

Depends on C being a spec, and on the other line. Expected outcome is a stated gap in the
CHANGELOG rather than a green run.

### 验收 3 — full validation, CHANGELOG, wrap-up

- `CHANGELOG.md`'s newest entry is `1.4.1 release index`; there is no v2 section.
- Every crate is still versioned `1.4.1`.
- The gate must be run with the criterion's exact flags. `cargo fmt --check` and
  `cargo clippy --workspace --all-targets -- -D warnings` have not been run in that form;
  `cargo fmt --all` and a plain clippy have, and both were clean.
- All 11 workspace crates must also build and pass on their own, since `onlyne-web` is
  excluded from the workspace (`crates/onlyne-web/Cargo.toml`).

## Documentation debt

`AGENTS.md` §0 is stale against the tree and the file says to fix it in the same change:
the table still reads phase two *in progress* and phase three *not started*, and line 47
lists four items as outstanding that have landed (declarative route edges,
`SpecGet`/`SpecApply` with a streaming subscribe, event hooks, liveness in memory).
`docs/STATUS.md` was corrected by an earlier pass (`DocTruth`, `DocTruthWave2`) but has
not been re-checked after slice 10.

## Recorded follow-ups, not tasks

Deliberately kept out of the task list; they are decisions or nits, and the notebook
holds them. The reference material's producer (`details` travels upward only); the ACP
instruction filename (`AGENTS.md` is a choice, claude-code reads `CLAUDE.md`);
`onlyne-client init` printing a self-loop `allowed_targets`; `SpecGet`/`SpecApply` having
no CLI verb; the web bundle being 1.65 MB because of elkjs; the graph not auto-fitting
after elk resolves; a 401 for `/favicon.ico`.

One was fixed in passing: the web header counted the server's `[[route]]` table while the
graph drew `allowed_targets`, so it read "0 routes" above two drawn edges. The contract
(`docs/v2-CONTRACT.md` §Slice 10) calls `allowed_targets` the allowed route, so the
header now counts what the graph draws.

## Environment notes worth not rediscovering

- `onlyne-web` is **not a workspace member**; build and test it from its own directory, or
  the core build needs Node.
- A stale binary — or a stale case — makes an e2e case lie. `running-lights.sh` had been
  failing since the TUI rebuild and was not a product fault.
- The `browser` eval tool's read helpers all fail with "Failed to restore browser request
  interception" against a page holding an open `EventSource`. Drive a private Chromium
  from a self-terminating Bun script instead; a Chrome build is at
  `~/.cache/puppeteer/chrome/mac_arm-151.0.7922.77/chrome-mac-arm64/`.
- The todo tool mangles nested phase payloads. Keep it flat and phase-prefixed.
