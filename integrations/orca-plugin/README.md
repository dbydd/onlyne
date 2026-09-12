# Onlyne Sessions (Orca plugin)

A **read-only supervisor board** for the Orca desktop app. It joins two independent
axes into one board:

```
   orca terminal list --json                        ──┐   every tab of every worktree,
   (one flat call, no --worktree)                     │   cut to the panes a live session
                                                      │   reports it runs in (its own pane_key)
                                                      │
   each configured serverRoots[i]:                    ├─→  board: root → role → task → tab
   onlyne --server-root <S> sessions --json           │   (a tab whose title is
   onlyne --server-root <S> roles    --json         ──┘    `onlyne:<task_id>` is annotated
                                                            onto that task's row)
```

- **pluginApi v1** (Orca 1.4.198+). The API is still marked EXPERIMENTAL.
- **Zero npm dependencies**: Node built-ins only.
- **Read-only discipline**: the plugin never creates, closes or renames a tab, and never writes an
  onlyne file. Its only Orca mutation is `orca terminal switch`, and only while you invoke the
  focus command. It writes one file: `panel.html` inside a dev-installed tree, which *is* the
  panel's data channel (§3). A content-addressed install is never written to.
- **The panel is the board** in a dev install (Settings → Plugins → Development). It refreshes on
  the same 2 s debounce / 5 s cadence the notifications use. A packaged install shows the snapshot
  it was installed with, and the live board lives in notifications + the plugin log.

**Where authority lives.** Session identity belongs to the onlyne adapter / pi plugin protocol.
This board is a supervisor convenience. It mirrors what a server root's admin surface reports, and
it annotates tabs through a title convention *any process in a pane can steal* (see §4). A wrong or
missing annotation is never evidence about a session.

The plugin no longer discovers role workspaces, and it reads no backend cache file. The backend
registers no per-role Orca worktree any more, so every session tab lands flat in the host
worktree's list, and the admin surface is now the only session source. The same place — the session
row — says which of those tabs belong to a swarm. So the tab axis has one authority, not two
(§4, *Axis A*).

---

## 1. Install (manual — there is no CLI install surface for plugins)

1. **Add the plugin.** Pick one of the two surfaces (only the first makes the
   panel live):

   - **Development (recommended)** — Settings → Plugins → Development → add the absolute path to
     this directory (for example `<repo>/integrations/orca-plugin`). Hot reload is the panel's data
     channel: this worker writes the board into `panel.html` in that tree, the dev watcher notices,
     and the panel reloads (see §3, *The panel is the board*).
   - **Install plugin (degraded)** — Settings → Plugins → Install plugin → *Local path* → the same
     directory. Orca copies the tree to `<userData>/plugins/onlyne.onlyne-sessions/<content-hash>/`
     and writes the `current` pointer, lock entry and provenance. That tree is content-addressed and
     re-hashed on every panel load, so the worker **must not and does not** write inside it. The
     panel then shows the snapshot the plugin was installed with, and the live board lives in
     notifications and the plugin log. To move a packaged install forward, re-install; never edit
     the install tree.

2. **Enable** the plugin in the list.
3. **Consent** — this plugin asks for two capabilities only:

   | capability | Orca's own wording | used for |
   | --- | --- | --- |
   | `notifications:show` | Show desktop notifications labeled with the plugin name | board pushes, focus results, context triple |
   | `events:subscribe` | Get notified when worktrees are created or removed and when agent status changes | event-driven rescans (2s debounce) |

   Degradation when not granted: without `events:subscribe` the plugin subscribes to no events
   (commands still work; the reason is logged); without `notifications:show` notifications are
   suppressed into the plugin log. The plugin never asks for `terminal:send`, and never for
   `storage`, `secrets` or `settings:own`: it keeps no state of its own.

4. Optional config (the board also works without it):
   `~/.config/onlyne-sessions/config.json`

   ```json
   {
     "serverRoots": ["/abs/path/to/server-root", "/abs/path/to/second-cluster"],
     "orcaBin": "/opt/homebrew/bin/orca",
     "onlyneBin": "/path/to/v1.0.0/onlyne"
   }
   ```

   - `serverRoots` is the session axis: one entry per onlyne server root, addressed as
     `onlyne --server-root <S> …` (`<S>/.onlyne/run/s` is that root's admin socket).
     **Absent or empty is a valid state.** The board then renders the flat tab list only and
     never calls `onlyne` at all. Entries are trimmed and de-duplicated.
   - **Nothing else scopes the tab axis.** The sessions themselves decide which tabs are listed
     (§4, *Axis A*), so there is no workspace list to configure and no path for the board to
     resolve. A leftover `piWorkspaces` key in an existing config file is ignored.
   - Why the binaries may need pinning: Orca scrubs the plugin worker environment to a 16-variable
     allowlist (`PATH`, `HOME`, …), and an Orca launched from the Dock often has no homebrew
     `PATH`. The plugin resolves binaries via `PATH → /opt/homebrew/bin → /usr/local/bin →
     ~/.local/bin → ~/bin`, and logs a clear degradation when it cannot. `BIN_DIR` (the
     repository's e2e convention) wins over discovery when the file exists, so a smoke run
     against a fresh build is `BIN_DIR=target/debug node tools/smoke.mjs`.
   - **The legacy `onlyne` 0.6.0 does not know `--server-root`/`sessions`** and degrades as
     `cli_surface_mismatch`. Point `onlyneBin` at the v1.0.0 binary (usually
     `target/debug/onlyne`).

## 2. Commands (command palette: search “Onlyne Sessions”)

| command | effect | argument |
| --- | --- | --- |
| `onlyne-sessions.refresh` | rescan, then push | — |
| `onlyne-sessions.board` | push the board now (notification + plugin log) | — |
| `onlyne-sessions.debug-board` | write the board JSON the panel embeds to `/tmp/onlyne-board.json`, then notify a one-line summary | `args.path`: destination, default `/tmp/onlyne-board.json` |
| `onlyne-sessions.focus` | `orca terminal switch` on a unique match | `args.task`: task id or pane prefix |
| `onlyne-sessions.copy-agent-context` | emit the `pane_key` / `handle` / `orca selector` triple | `args.task`: task id or pane prefix |

**Argument boundary (measured)**: the palette passes no arguments to plugin commands
(`plugin-command-execution.ts` sends only `pluginKey`/`commandId`). Therefore:

- without a prefix, `focus` / `copy-agent-context` act only when there is **exactly one live
  tab**. Zero or many matches produce a notification listing candidates instead of a guess.
- the accepted prefixes are a **task id** (`task8a1b…`) or a **pane prefix** `<tabId>:<leafId>`.
  The board also prints a shortened `tab8:leaf8` form, and the plugin accepts that too, so what
  you copy off the board works verbatim.
- to pass a prefix, call it through the RPC/IPC surface (it returns a structured result):

  ```json
  { "pluginKey": "onlyne.onlyne-sessions",
    "commandId": "onlyne-sessions.focus",
    "args": { "task": "task8" } }
  ```

- `focus` moves foreground focus (the `orca terminal switch` side effect), and only when you ask.
- `copy-agent-context` "copies" by notification. pluginApi v1 has **no clipboard host method**,
  so the notification carries the triple as text, and the structured result carries it for RPC
  callers. `orca selector` comes from the tab row's own `worktreeId`; the plugin omits that line
  when Orca did not report one.

## 3. What the board looks like, and where it shows up

```
Onlyne sessions · 2 roots · 3 roles · 7 tabs (4 live) · 3 hidden · 9 sessions (2 working)
tab 轴：只列 7 个连着 adapter 的 pi pane（session 上报的 host.orca.pane_key），其余 3 个 tab 不计入
/srv/onlyne-a  (3 roles · 9 sessions · 2 working)
  planner  (online · 2 tasks · 1 live)
    ● task8a1b · working/running · 12s · 45e603f7:b6d067b6
    ○ task9c2d · idle/gone · 无 tab · 3m · —
  builder  (offline · 0 tasks · 0 live)
/srv/onlyne-b  (0 roles · 0 sessions · 0 working)
  ! sessions: cli_error — onlyne: no onlyne socket found; pass --socket, --server-root, or --workspace
  ! roles: cli_error — onlyne: no onlyne socket found; pass --socket, --server-root, or --workspace
未 join 的 tab (2)
  ● tab · title=Pi ready · 5s · 470e41ba:31f6d4b1 · wt 53e59790
  ○ tab · title=zsh · 3m · 794041dc:7c648f42 · wt 2ea2fe23
```

(the shipped board text is Chinese. `!` lines are that root's own failures; `无 tab` marks a task
row the tab axis does not list; `未 join 的 tab` is the stray-tab section.)

- summary: `N roots · N roles · N tabs (M live) [· H hidden] · N sessions (K working)`; `hidden`
  appears only when something was dropped. Working means `public_lifecycle=working` or
  `agent=running`.
- scope line (present only when the cut hid something): `tab 轴：只列 N 个连着 adapter 的 pi
  pane（session 上报的 host.orca.pane_key），其余 H 个 tab 不计入`. With no pane reported anywhere
  it instead reads `tab 轴：等 pi-onlyne 连上——没有任何 live session 报告它所在的 Orca pane，H 个
  tab 全部不计入`. That second wording says the board is waiting for a session, not that it is
  misconfigured.
- root line: that root's role sections, session count and working count. A dead root shows zeros
  plus its own `!` lines instead of taking the board down.
- role section: the role's presence (`online` / `offline` / `draining`, or `no role row` when only
  a session named it) plus `N tasks · M live`.
- row: `task short · lifecycle/agent (with the outcome when the server reports one) · relative
  lastOutputAt (falling back to the session's updatedAt) · short pane_key`. A task row with no tab
  adds `无 tab` and dashes the pane.
- stray-tab row: `tab · title=<raw title> · relative lastOutputAt · short pane_key · wt <worktree>`.
- legend: `●` live (`connected=true`) · `○` not connected, or a row the tab axis does not list.
- empty state, one sentence: **no `serverRoots` and no Orca tab at all** — how to add `serverRoots`
  to the config file. A board that has tabs but no reported pane shows the scope line instead;
  claiming "no Orca tab" while Orca lists several would be false.

It surfaces in four places:

1. **The panel** (`Onlyne Sessions` in the right sidebar) — the live board in a dev install, see
   below. Rows carry `✕` when a session ended badly (`outcome=fault/cancelled`); a snapshot older
   than the cadence dims its ages;
2. **Desktop notifications** on structural change (roots, roles, tasks, tab liveness, a root going
   down or coming back), with a 30s cooldown;
3. **Settings → Plugins → this plugin's logs**: one summary line per change, the whole board for
   the `board` command, and the panel-write outcome;
4. **Command results**: RPC callers get the structured board (the palette discards return values).

### The panel is the board (and how data gets in)

An Orca 1.4.198 plugin panel is a sandboxed `srcdoc` document. Its CSP is
`default-src 'none'; connect-src 'none'` (no fetch), and it may call exactly three host methods:
`workspace.readContext`, `terminal.sendText`, `notifications.show`. Those three are the
`PLUGIN_PANEL_ACTIONS` in `src/shared/plugins/plugin-host-api.ts:263`; the schema refine in
`plugin-panel-bridge.ts:42` and the capability gate enforce them again. The host posts nothing into
the frame but watchdog pings and action results. **There is no worker→panel channel**, in either
direction of the bridge, and none is planned in v1.

So the board reaches the panel the only way available: **the document itself**. The worker renders
the snapshot into the panel entry file. Orca reads that file from the plugin root every time it
opens or refreshes the panel (`src/main/plugins/plugin-panel-controller.ts:142-148`). Two Orca
behaviours turn a file write into a live panel:

- **dev install (primary path)** — the dev watcher watches the configured plugin paths; a change
  schedules the 300 ms debounced refresh (`plugin-dev-watcher.ts:106-114`). The renderer re-reads
  the entry and remounts the frame when the HTML changed (`PluginPanel.tsx:143-147`). Writing is
  explicitly allowed here: `verifyHashAddressedPluginContent` returns ok when `contentHash === null`
  — *“Dev trees are intentionally mutable; installed hash-addressed trees are not”*
  (`plugin-content-integrity.ts`).
- **packaged install (degraded)** — the tree is content-addressed (`<plugins>/<key>/<sha256>/`) and
  re-hashed per panel load, so the worker never writes there. The installed document is whatever
  `panel.html` was when the plugin was installed. This repository commits the **placeholder**
  version, and the live board stays in notifications + the plugin log.

The worker rewrites the document only when the board's *structure* or a session's state changes
(see `panelFingerprint`), never on a timer. Ages are `data-ts` attributes ticked by the document's
own script, and a rewrite remounts the panel. Past ~15 s without a new scan the document marks
itself stale and dims its ages: the numbers stay exact (`now - data-ts`), and the dimming is the
signal that the scan loop stopped. Rewriting `panel.html` inside a dev tree is a normal
working-tree modification — that file *is* the panel's data channel.

## 4. Data contract as implemented

### Axis A — tabs (one call, flat)

`orca terminal list --json`, with **no** `--worktree` selector: one call answers every tab of every
worktree, and the plugin never walks worktrees. Per row the plugin keeps `handle`, `tabId`,
`leafId`, `paneKey`, `title`, `connected`, `writable`, `lastOutputAt`, `worktreeId`.

- liveness is exactly the row's own `connected` flag; the plugin consults nothing else.
- a row whose `handle` is missing is dropped: it cannot be addressed.
- **measured**: Orca 1.4.198 rows carry no `paneKey` field, so the plugin derives
  `${tabId}:${leafId}` (newer builds may carry it; it wins when present). The scope cut compares
  that derived key against the pane a session reports.
- `worktreeId` (and the row's `worktreePath`) is kept as addressing information: it is what
  `orca terminal list --worktree` would need and what `copy-agent-context` emits, never what
  decides scope.

#### Scoping the tab axis to real sessions: the pane the session reports

An Orca worktree can hold tabs that are not onlyne sessions, so exactly one rule filters the tab
axis: **a tab is on the axis iff a live session reports that tab's pane.**

A session's own process states where it runs, on every heartbeat, as `observed.host.orca.pane_key`
in the adapter protocol (`crates/onlyne-session/src/host.rs`). It can: it was spawned inside the
pane and inherits `ORCA_PANE_KEY` (beside `ORCA_TAB_ID`, `ORCA_LEAF_ID` and
`ORCA_TERMINAL_HANDLE`) from it (**measured 2026-09-11, Orca 1.4.198**: `orca terminal create
--command …` exports them into the command's process). The key is `<tab_id>:<leaf_id>` on both
sides, so the cut is the set intersection of that report with Orca's own flat tab list — a plain
comparison, with nothing derived and nothing guessed.

- **No pane reported means no tab listed.** A board that cannot name a pane lists nothing rather
  than everything; `scope.source: "none"` and the scope line both say the board is waiting for pi.
  That is the deliberate cost of a rule with no guessing in it: a swarm whose adapter predates the
  report shows an empty tab axis until that adapter is upgraded.
- **Liveness is the projection's own verdict.** A session binds a pane while its
  `public_lifecycle` is not `exited`. The client's reconcile loop is what turns a dead pane into
  `exited`, so the board forms no second opinion about one fact.
- **A binding survives the session that made it.** `report.complete` carries `host` forward, so a
  finished session's row still says where it ran. The liveness rule above, not a vanished binding,
  is what keeps its tab off the axis.
- **A pane matching no tab binds nothing.** Either the tab is gone or the key belongs to another
  machine's Orca; the cut hides the row instead of falling back to a guess.
- **The board reads no file for this, and needs no workspace list.** There is no claim file, no
  cache read and no `piWorkspaces` to configure: the authority is the session axis the board
  already reads. `board.scope` is `{ source: "connected" | "none", panes, hidden }`, alongside
  `summary.hiddenTabs`.
- **`worktreePath` is not consulted.** It cannot be: a workspace is where the *client* lives, while
  the session axis is configured by *server root*, and the `welcome.server` triple carries no path.
  So the worktree heuristic this board used to fall back on was resolving a different thing, and
  it is gone along with the pre-v1 single-file claim read.

### Axis B — sessions (one call per root per verb)

For each configured server root, in config order:

```
onlyne --server-root <S> sessions --json   -> {ok:true, data:{sessions:[…]}}
onlyne --server-root <S> roles    --json   -> {ok:true, data:{roles:[…]}}
```

- Session rows are normalized to `task_id`, `role`, `session_id`, `public_lifecycle` (falling back
  to `projection.lifecycle`), `projection.agent` / `delivery` / `resource`, `outcome`, `updated_at`,
  `seq`, and the reported pane out of `projection.observed.host.orca` (`pane_key`, `tab_id`,
  `leaf_id`, `handle` — only `pane_key` is required; the rest are absent when the environment did
  not name them). The repository's own wire vector
  `crates/onlyne-proto/tests/wire_vectors/res_session_row.json` pins the base shape. The pane rides
  inside the projection's observation, because the observation is exactly what the client mirrors.
- Role rows are normalized to `name` → `role`, `admin`, `max_sessions` → `maxSessions`, `state` →
  `presence` (`online` / `offline` / `draining`), `sessions`. `res_role_info.json` pins them.
- The role list is the section skeleton: a role with zero sessions still renders (that is how you
  see an offline role), and a session whose role has no role row lands under `(unknown role)`.
- **measured 2026-09-11** with `target/debug/onlyne` against a root whose socket is absent: exit 3,
  `onlyne: no onlyne socket found; pass --socket, --server-root, or --workspace` on stderr and
  nothing on stdout. The plugin reports that as `cli_error` for that root only. A refused but
  existing socket answers the JSON error body instead, and the plugin reports that body's own code.

### The join (weak, display-only)

A tab is annotated onto a task row when its **trimmed title is exactly** `onlyne:<task_id>`; a tab
that matches nothing renders in the stray-tab section. Nothing more is inferred.

- **This is not identity.** Any process in the pane can set the title (OSC 0/2), so the prefix can
  be stolen, drop the tab, or point at the wrong task. Identity belongs to the adapter / pi plugin
  protocol. This board is a convenience for a supervisor, and a supervisor who needs certainty must
  read the session rows, not the annotation.
- **Measured 2026-09-11, Orca 1.4.198, one live `sleep 600` session in the host worktree**: the
  create-time title survives about a second — the operator's own login shell (zsh + prompt) takes
  the title over immediately after. The session row, meanwhile, reaches a server root only once the
  agent reports. The two therefore rarely coincide, so in practice the board usually renders the
  two axes side by side and the annotation stays empty. That is the accepted cost of a supervisor
  view with no second discovery axis; `joined` is a bonus, never the reason a row exists.
- Tabs are consumed once, in config-root order and Orca row order. With two tabs carrying one
  title the first joins and the rest render stray; with the same task id on two roots, the earlier
  root in `serverRoots` takes the tab while the later row stays unjoined. That keeps `live tabs`
  from counting one physical tab twice; which root truly owns it is not knowable here.

### Degradation matrix (never fatal; reported in the log/notification)

| situation | result |
| --- | --- |
| `serverRoots` absent/empty | valid: tab axis only, `onlyne` is never invoked |
| no live session reports a pane | not an error: the tab axis is empty (`scope.source: "none"`), the scope line says the board is waiting for pi, and the session axis renders normally |
| `orca terminal list` fails (e.g. `missing_binary`) | `ok:false` + the error code; the session axis is still rendered, unjoined |
| a root's socket is absent | that root reports `cli_error` with the canonical no-socket hint; other roots and the tab axis still render |
| onlyne CLI without `--server-root`/`sessions` (e.g. 0.6.0) | `cli_surface_mismatch` on that verb; a failing verb never hides the other one |
| `onlyne` binary missing | `missing_binary`, same per-verb degradation |
| an answer without the verb's rows | `unexpected_shape` |
| a tab title stolen or absent | no annotation: the task row shows `无 tab`, the tab shows up stray |

## 5. Known boundaries

- **Manual install**: pluginApi v1 has no CLI install surface; install/enable/consent happen in
  the desktop UI.
- **Experimental API**: `pluginApi` is not frozen; after an Orca upgrade, run “rescan” once to
  confirm the join still holds.
- **Workers get reaped**: the 5s fallback cadence runs only while the worker lives. The next
  event or command restarts it, so event-driven refresh is not a hard real-time guarantee.
- **No lifecycle**: spawn/close/rename belong to the onlyne backend (no dual owner).
- **Writes**: in a dev install, the worker rewrites exactly one file — its own `panel.html` (the
  panel's data channel; §3). In a content-addressed install it writes nothing at all. It never
  touches an onlyne file, cache or socket-side state; the only other file it reads is its own
  optional config, and `debug-board` writes only to the path you name.
- **Cost per scan**: one `orca` call plus two `onlyne` calls per configured root. Roots are
  queried independently, so one dead root costs its own two failures, never the board.

## 6. Disable / uninstall / upgrade

- **Disable**: the worker stops — no scans, no notifications, no event subscriptions (the panel
  remains and reports errors).
- **Uninstall**: remove the plugin in Settings → Plugins; Orca deletes
  `<userData>/plugins/onlyne.onlyne-sessions` (hash dir + `current` pointer). The plugin keeps no
  state elsewhere (no storage, no secrets, no settings writes).
- **Upgrade**: edit the source directory, install again (new hash dir); consent only needs
  re-approval when the capability fingerprint changes.
- **Upgrading from a build that used pane claims** (anything before the board read the pane from
  the session row): neither this plugin nor the pi adapter reads
  `<workspace>/.onlyne/cache/pi-panes/` or a legacy `<workspace>/.onlyne/cache/pi-pane.json` any
  more, so both are dead weight — delete the directory and the file from each workspace at your
  convenience. A leftover `piWorkspaces` key in the config file is ignored. Until the pi adapters
  on a machine are upgraded too, that machine's tab axis stays empty: the old adapters publish
  only the file.

## 7. Development and verification

```
integrations/orca-plugin/
  orca-plugin.json     manifest (pluginApi 1)
  main.mjs             worker entry: activate/deactivate, five commands, three events
  panel.html           panel document: the committed placeholder (a dev install replaces it)
  src/
    runner.mjs         injectable exec, CLI JSON parsing (errors arrive on stdout), config
    orca-cli.mjs       terminal list (flat) / terminal switch / status
    onlyne-cli.mjs     --server-root <S> sessions|roles, normalized rows + failures
    board.mjs          the two axes, the weak title join, the board model
    render.mjs         text rendering shared by notifications and logs
    panel-document.mjs the panel document: snapshot render, fingerprint, dev-only publisher
    commands.mjs       refresh / board / debug-board / focus / copy-agent-context
    board-state.mjs    debounce, 5s fallback, structural fingerprint, notify cooldown
    *.test.mjs         node:test suites (zero dependencies)
  tools/smoke.mjs      read-only smoke run against the live desktop (mutation guard)
```

```bash
node --test                     # the package's own test command
BIN_DIR=target/debug node tools/smoke.mjs
                                # live, read-only: orca status / flat tab list / board per root /
                                # join demo (synthetic sessions) / panel document in a temp root
```

The smoke runner intercepts any `terminal switch|create|close|rename|send` or
`worktree create|rm` call and exits 1. So running the smoke run is itself evidence that nothing
touched your tabs. It generates the panel document into a temp root, so a smoke run never rewrites
the plugin tree it is testing.

**Verifying the panel without the UI**: run `onlyne-sessions.debug-board` (palette or
`plugins.invokeCommand`). It writes the very payload the panel document embeds to
`/tmp/onlyne-board.json`, then notifies a one-line summary, so a JSON dump and the panel can be
compared directly. A dev-installed panel updates within the watcher's 300 ms debounce plus one
scan (≤5 s with no events).

## 8. Contract notes for the onlyne backend

Implemented against the surfaces as measured; these are the points the implementation surfaced
(reported, not renegotiated):

1. **The title convention is not a contract.** The plugin joins on `onlyne:<task_id>` because that
   is the only tab-side hint available, but OSC title writes can replace it at any moment. If the
   backend starts depending on that prefix, it must also publish the pane/handle binding some other
   way. The adapter / pi plugin protocol is the authority, and this board only mirrors it. The
   board's *scope* no longer depends on it: that comes from the session row (§4 axis A).
2. **The plugin reads no backend file.** Sessions come from `sessions` and roles from `roles`,
   nothing else. Anything a supervisor needs must be answerable through those two verbs.
3. **`sessions` must keep reporting the pane.** With no per-role worktree registration, a role's tab
   is indistinguishable from any other tab on the Orca side, so the pane each session reports scopes
   the tab axis (`observed.host.orca.pane_key`, `crates/onlyne-session/src/host.rs`), not any
   workspace path. Two properties matter to this board. The binding must survive
   `report.complete` (so a finished session still says where it ran), and its `pane_key` must be the
   same `<tab_id>:<leaf_id>` spelling Orca's own `terminal list` uses. If it is ever renamed or
   dropped from the observation, the tab axis goes empty rather than wrong.
4. **Session keys used**: `task_id` (identity), `session_id` (display), `role` (section),
   `public_lifecycle` / `projection.lifecycle`, `projection.agent`, `projection.outcome`,
   `projection.observed.host.orca.pane_key`, `updated_at`, `seq`. Anything else is ignored.
5. **Role row fields used**: `name`, `admin`, `max_sessions`, `state`, `sessions`. The presence
   vocabulary is the server's own (`online` / `offline` / `draining`), rendered as-is.
6. **Per-root failures are expected.** A root with no live server is a normal state for the board.
   The plugin reports each failing verb with its own code and keeps going. It never merges roots:
   identical task ids on two roots stay two rows.
7. **The plugin never passes `--quiet` or `--socket`.** It reads the whole answer body
   (`{ok, data:{…}}`), so a change in that envelope shape is a breaking change for the plugin.
