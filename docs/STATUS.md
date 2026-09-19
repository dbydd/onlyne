# Onlyne Status

v1.1.0 is on crates.io and tagged `v1.1.0` (`e2d0e15`). `docs/v1-PLAN.md` is the settled spec. `docs/v1-CONTRACT.md` owns the work split. Root README files are the user manual.

## Three-process shape

- `onlyne-server` routes envelopes and holds the ledger.
- `onlyne-client` owns one role workspace and its session execution.
- `onlyne-gateway` translates one chat platform through a feature-gated plugin.
- Agent plugins and gateway plugins use one adapter protocol over two mount kinds.

## Crate state

Counts come from `cargo test --workspace` on 2026-09-15 (760 passed, 0 failed, 1 ignored: `herdr_live_probe`), one line per crate with its libraries and integration targets summed.

- [x] `onlyne-proto` green with envelope, frame variants, ops, errors, and events: 59 unit + 5 wire vectors + 2 sizes.
- [x] `onlyne-frame` green with length-prefixed codec: 9.
- [x] `onlyne-config` green with spec parse and reload: 11 template + 30 config contract + 17 ACL table + 3 spec example.
- [x] `onlyne-layout` green with legacy refusal exit 2 and the local-socket seam: 20.
- [x] `onlyne-store` green with ledger and local DB: 31 unit + 2 schema statements.
- [x] `onlyne-session` green with the lifecycle port and the session backends (zellij, Orca, exec, fake, herdr): 106 unit + 13 herdr.
- [x] `onlyne-net` green with TLS, handshake, ACL, and backoff: 25.
- [x] `onlyne-adapter` green with SDK and protocol schema: 5 unit + 3 conformance + 1 protocol doc.
- [x] `onlyne-server` green with router, relay, projection, faults, admin, and generate: 13 unit + 83 delivery + 27 generate + 2 stale.
- [x] `onlyne-client` green with runloop, intents, adapter socket, host detection, and dispatch: 35 unit + 41 scenarios.
- [x] `onlyne-tui` green with the role network graph, the swarm monitor, and the key table: 72.
- [x] `onlyne-gateway` green with shared kit: 47.
- [x] `onlyne-cli` green with entrypoint, socket resolution, and the admin verbs: 33 + 1.
- [x] `onlyne-testkit` green with fake agent, fake gateway, and conformance: 3 unit + 2 binaries + 11 conformance.
- [x] Four gateway plugins green behind `telegram`, `feishu`, `qqbot`, and `weixin` features: 11, 10, 10, 13.

## Wave plan status

- [x] Wave 1 closed: proto, frame, session kernel, config/layout/store, net.
- [x] Wave 2 closed: server runtime, client runtime, adapter SDK plus testkit, gateway kit.
- [x] Wave 3 closed: generate, federation path, legacy deletion, docs.

## Verification cases

Each case is a script under `crates/onlyne-testkit/e2e/`, run with `ONLYNE_BACKEND=fake BIN_DIR=target/debug` from the repository root. The directory holds seventeen scripts besides `lib.sh`. Thirteen fake-backend cases (1-7, 9, 12, 14, 15, 16, 17) exited 0, `running-lights` the long one and `gateway-mount` the quick one; case 16 `exec-headless` joined the set for 1.1.0, and case 17 `socket-path-length` joined on 2026-09-17 as the field fix for the deep-workspace socket. Case 18 `acp-session` joins as the ACP backend proof: a real server and client against a scripted ACP agent as the role's `session_command`, so it is neither a fake-backend case nor a live one. Cases 10 and 11 are live and last exited 0 on 2026-09-11 (2 seconds and 9 seconds). Case 13 is live against herdr session `onlyne-test` and exited 0 on 2026-09-14 in 3 seconds, its workspace closed and the session's workspace list back to the single `~` entry it started with. The pane-reclaim step that case 13 gained on 2026-09-17 is operator-pending: the script passed `bash -n`, and the next live run on a host with a reachable `onlyne-test` records its own green line. Cases 10, 11, and 13 override the backend: case 10 needs a shell inside an Orca tab with the app answering, case 11 needs a `pi` that answers a credential probe, and case 13 needs a reachable herdr session `onlyne-test`. On a host without its requirement, a live case prints `SKIP` and exits 0, so a green line says "passed here" and a skip says "not exercised here".

- [x] Case 1 `local-task.sh`: single-machine fake-backend task reaches `acked`.
- [x] Case 2 `acl-reject.sh`: ACL refusal emits `acl_denied`.
- [x] Case 3 `idempotency.sh`: repeated `op_id` emits `duplicate`; changed body emits `conflict`.
- [x] Case 4 `reconnect-requeue.sh`: disconnect keeps queue state and reconnect flushes in order.
- [x] Case 5 `two-cluster.sh`: aggregate-role federation preserves the parent ledger boundary.
- [x] Case 6 `gateway-mount.sh`: gateway mount delivers platform traffic.
- [x] Case 7 `legacy-layout.sh`: legacy workspace exits 2.
- [x] Case 8: formatting, lint, workspace tests, and binary firewall checks pass. `.github/workflows/ci.yml` splits this across two jobs: linux fmt/clippy/`cargo test --workspace`; windows `cargo test` on the core crate subset. Dual-platform CI is green (run 34977562567).
- [x] Case 9 `generate-relocate.sh`: generate produces relocatable workspaces.
- [x] Case 10 `orca-live.sh`: the Orca backend against the live app. The tab lands flat in the supervisor's own worktree under the `host` policy, probe inputs come in four parts, the tab map carries that identity, SIGTERM drains back to the tab count it started with, and no Orca registration is created.
- [x] Case 11 `pi-live.sh`: the `plugins/onlyne-agent-pi` pi extension against a real `onlyne-client`. `ONLYNE_BACKEND=exec` spawns the workspace's `session_command` as a child of the client with stdin held open. pi loads the plugin, the task text reaches pi's context, the plugin's `report.complete` settles the ledger to `acked` with the model's answer in `out_head`, and the session projects `exited`/`done`. SKIP semantics: pi not on PATH, or a one-turn credential probe that does not answer, prints `SKIP pi-live` and exits 0. A host without a model must not read as a product failure. The plugin's own protocol path is covered without a model by `node --test` in `plugins/onlyne-agent-pi`: framing, protocol vocabulary, the agent state machine against a fake host, and a live handshake against a really-running `onlyne-client`.
- [x] Case 12 `running-lights.sh`: six roles in a closed ring, one token. A `send` starts it on `light1`. Each role runs `onlyne handoff` to pass a child task to its neighbour and settles its own. The agent that meets hop 11 keeps the task and does not pass it on, so the ledger ends with twelve `task` rows all `acked` at hops `0..11`, each naming its parent, plus the twelve `completion` receipts. `onlyne-tui --page 2 --once` is sampled mid-run: two frames three seconds apart put the working session on different roles, and each sighting pairs the role's `working` row, with the hop it travels on in its `in-flight` cell, with the same `in_flight` edge in the ledger.
- [x] Case 13 `herdr-live.sh`: the herdr backend against the live herdr session `onlyne-test`. `ONLYNE_BACKEND=herdr` with `HERDR_ENV=1` and `HERDR_SESSION` set, `session_command = ["sleep", "600"]` on the pane-run track, `max_sessions` left at the seed value `1` so the role is saturated by its own session. The case polls `herdr workspace list` `.result.workspaces[]` for the label built from the spec it generated (`onlyne:<[server] name>`, the value the client passes to each pane as `ONLYNE_CLUSTER`), then `herdr tab list --workspace W` `.result.tabs[]` for tab label `planner`, then that tab's `pane_count` reaching 2 with the two ids from `herdr pane list` `.result.panes[]`. The 1-pane moment is left unasserted: the split lands within a few hundred ms of the tab's creation, so the poller would race the product. `backend_ref` from `client.db` `sessions` names the workspace, the tab, and the session's pane; the pane in that tab beside it is the tab's root. Then `onlyne control --from planner focus --task` (the saturated-role delivery `pull{control_only}` is what carries it) and `herdr pane get` `.result.pane.focused` to true. The next step reads the session pane's shell pid through `herdr pane process-info` with `kill -0` proving that pid live, then drives `onlyne control --from planner recycle --task`, and waits for the reclaim: `herdr pane list` drops that pane id and `kill -0` finds the recorded pid gone. `pane get` on the closed id answers rc 1 with `pane_not_found`, `pane_count` is back to 1, and the surviving pane equals the recorded root. SIGTERM then drains the client with that session's host resource already released. `workspace close` returns the session's workspace list to the pre-run snapshot, and cleanup closes any workspace that appeared after that snapshot, so a failing assertion leaves nothing behind. SKIP: missing herdr binary or an unreachable `HERDR_SESSION` prints `SKIP herdr-live` and exits 0.
- [x] Case 14 `heartbeat-watch.sh`: the server's own heartbeat watch against real processes. The spec's `[server]` carries `stale_watch_secs = 2` and `heartbeat_grace_secs = 4`, a scripted agent reports `ready`, lands one heartbeat, and then sleeps inside the assignment. The `sessions` answer reads `working` with `heartbeat_stale` absent while beats flow, the `faults` table gains `heartbeat_missing` for the task once the grace passes, and the same row keeps `working` all the way: the server flags, the supervisor decides.
- [x] Case 15 `requeue-claim.sh`: the hello claim across a server restart on real processes. A task sits `in_flight` in a live client session when `kill -9` takes the server; the restarted server answers `wait-ready`, the client reconnects and declares its live slot at `hello`. The row keeps `in_flight` through adoption, the task's ledger arc counts one delivery event and zero requeues, the sessions axis still holds exactly one `working` row, and the scripted completion lands the same row `acked`.
- [x] Case 16 `exec-headless.sh`: the exec backend with workspace `backend = "headless"` (parse alias) and env `ONLYNE_BACKEND=exec`. Fake agent as `session_command` writes a banner into `.onlyne/logs/session-<task>.log`, the ledger settles `acked`, the session projects `exited`/`done`, and `client.db` stores backend `exec`.
- [x] Case 17 `socket-path-length.sh`: the deep-workspace socket. The case pads a workspace path until the canonical `<workspace>/.onlyne/run/s` spelling passes 103 bytes, then runs the client and the fake agent there. It asserts the served path is short, published in `run/socket`, and holding a bound socket, with the canonical path left bare and the client log naming the served path; `onlyne --workspace <deep ws> who` answers `planner` through the marker, and one task settles `acked` with the session `exited`/`done`.
- [x] Case 18 `acp-session.sh`: the ACP session backend against a scripted ACP v1 agent the case installs as the role's `session_command`. The workspace config names `backend = "acp"` plus an `[acp]` table whose mode, model and reasoning effort the agent's own trace proves it received. The agent holds its one turn at a gate, so while it is held the case asserts the journal carries the client's dispatch record alone; releasing the gate settles the ledger `acked` and the session `exited`/`done`, and leaves the events journal, the content index, and the rendered log in agreement — one turn journalled three ways under `.onlyne/logs/`, with the ACP child gone once its client drains.

## CI

`.github/workflows/ci.yml` defines two jobs on `push` to `main`, pull requests, and `workflow_dispatch`.

- `linux` (`ubuntu-latest`): `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- `windows` (`windows-latest`): `cargo test --no-fail-fast` on `onlyne-proto`, `onlyne-frame`, `onlyne-config`, `onlyne-layout`, `onlyne-store`, `onlyne-session`, `onlyne-net`, `onlyne-adapter`, `onlyne-server`, `onlyne-client`, `onlyne-gateway`, `onlyne-cli`, `onlyne-tui`. Dual-platform CI is green (run 34977562567 @ `e2d0e15`).
