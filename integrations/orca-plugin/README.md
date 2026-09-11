# Onlyne Sessions (Orca plugin)

A **read-only supervisor board** for the Orca desktop app. It joins two independent
axes into one board:

```
   orca terminal list --json                        ──┐   every tab of every worktree,
   (one flat call, no --worktree)                     │   in Orca's own order
                                                      │
   each configured serverRoots[i]:                    ├─→  board: root → role → task → tab
   onlyne --server-root <S> sessions --json           │   (a tab whose title is
   onlyne --server-root <S> roles    --json         ──┘    `onlyne:<task_id>` is annotated
                                                            onto that task's row)
```

- **pluginApi v1** (Orca 1.4.198+); the API is still marked EXPERIMENTAL.
- **Zero npm dependencies** — Node built-ins only.
- **Read-only discipline**: it never creates, closes or renames a tab and never writes any onlyne
  file. Its only Orca mutation is `orca terminal switch`, run solely while you invoke the focus
  command; its only file write is `panel.html` inside a dev-installed tree, which *is* the panel's
  data channel (§3). A content-addressed install is never written to.
- **The panel is the board** in a dev install (Settings → Plugins → Development), refreshed by the
  same 2 s debounce / 5 s cadence the notifications use. A packaged install shows the snapshot it
  was installed with and keeps the live board in notifications + the plugin log.

**Where authority lives.** Session identity belongs to the onlyne adapter / pi plugin protocol.
This board is a supervisor convenience: it mirrors what a server root's admin surface reports and
annotates tabs through a title convention *any process in a pane can steal* (see §4). A wrong or
missing annotation is never evidence about a session.

The plugin no longer discovers role workspaces and no longer reads any backend cache file: the
backend registers no per-role Orca worktree any more, so every session tab lands flat in the host
worktree's list and the only session source is the admin surface.

---

## 1. Install (manual — there is no CLI install surface for plugins)

1. **Add the plugin**, picking one of the two surfaces (the first is what makes the
   panel live):

   - **Development (recommended)** — Settings → Plugins → Development → add the absolute path to
     this directory (for example `<repo>/integrations/orca-plugin`). Hot reload is the panel's data
     channel: this worker writes the board into `panel.html` in that tree, the dev watcher notices,
     and the panel reloads (see §3, *The panel is the board*).
   - **Install plugin (degraded)** — Settings → Plugins → Install plugin → *Local path* → the same
     directory. Orca copies the tree to `<userData>/plugins/onlyne.onlyne-sessions/<content-hash>/`
     and writes the `current` pointer, lock entry and provenance; that tree is content-addressed and
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

   Degradation when not granted: without `events:subscribe` no events are subscribed (commands
   still work, the reason is logged); without `notifications:show` notifications are suppressed
   into the plugin log. The plugin never asks for `terminal:send` and never asks for `storage`,
   `secrets` or `settings:own` — it keeps no state of its own.

4. Optional config (the board also works without it):
   `~/.config/onlyne-sessions/config.json`

   ```json
   {
     "serverRoots": ["/abs/path/to/server-root", "/abs/path/to/second-cluster"],
     "piWorkspaces": ["/abs/path/to/role-workspace", "/abs/path/to/another-role-workspace"],
     "orcaBin": "/opt/homebrew/bin/orca",
     "onlyneBin": "/path/to/v1.0.0/onlyne"
   }
   ```

   - `serverRoots` is the session axis: one entry per onlyne server root, addressed as
     `onlyne --server-root <S> …` (`<S>/.onlyne/run/s` is that root's admin socket).
     **Absent or empty is a valid state** — the board then renders the flat tab list only and
     never calls `onlyne` at all. Entries are trimmed and de-duplicated.
  - `piWorkspaces` is the swarm-membership authority (§2 axis A): one entry per role workspace
    whose pi adapter may publish a pane claim under `<workspace>/.onlyne/cache/pi-panes/`. It is
    **not** the same list as `serverRoots` — a workspace is where `onlyne client run` runs, and the
    board cannot derive it from a server root. Absent or empty is a valid state: the tab axis then
    falls back to the worktree heuristic. Same trimming and de-duplication.
   - Why the binaries may need pinning: the plugin worker environment is scrubbed to a 16-variable
     allowlist (`PATH`, `HOME`, …), and an Orca launched from the Dock often has no homebrew
     `PATH`. The plugin resolves binaries via `PATH → /opt/homebrew/bin → /usr/local/bin →
     ~/.local/bin → ~/bin`, and logs a clear degradation when it cannot. `BIN_DIR` (the
     repository's e2e convention) wins over discovery when the file exists, so a smoke run
     against a fresh build is `BIN_DIR=target/debug node tools/smoke.mjs`.
   - **The legacy `onlyne` 0.6.0 does not know `--server-root`/`sessions`** and degrades as
     `cli_surface_mismatch`; point `onlyneBin` at the v1.0.0 binary (usually
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
  tab**; zero or many matches produce a notification listing candidates instead of a guess.
- the accepted prefixes are a **task id** (`task8a1b…`) or a **pane prefix** `<tabId>:<leafId>`;
  the shortened `tab8:leaf8` form the board prints is accepted too, so what you copy off the
  board works verbatim.
- to pass a prefix, call it through the RPC/IPC surface (it returns a structured result):

  ```json
  { "pluginKey": "onlyne.onlyne-sessions",
    "commandId": "onlyne-sessions.focus",
    "args": { "task": "task8" } }
  ```

- `focus` moves foreground focus (the `orca terminal switch` side effect), and only when you ask.
- `copy-agent-context` "copies" by notification: pluginApi v1 has **no clipboard host method**,
  so the notification carries the triple as text and the structured result carries it for RPC
  callers. `orca selector` comes from the tab row's own `worktreeId`, and that line is omitted
  when Orca did not report one.

## 3. What the board looks like, and where it shows up

```
Onlyne sessions · 2 roots · 3 roles · 7 tabs (4 live) · 9 sessions (2 working)
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

(the shipped board text is Chinese: `!` lines are that root's own failures, `无 tab` marks a task
row the tab axis does not list, `未 join 的 tab` is the stray-tab section.)

- summary: `N roots · N roles · N tabs (M live) · N sessions (K working)`; working means
  `public_lifecycle=working` or `agent=running`.
- root line: that root's role sections, session count and working count; a dead root shows zeros
  plus its own `!` lines instead of taking the board down.
- role section: the role's presence (`online` / `offline` / `draining`, or `no role row` when only
  a session named it) plus `N tasks · M live`.
- row: `task short · lifecycle/agent (with the outcome when the server reports one) · relative
  lastOutputAt (falling back to the session's updatedAt) · short pane_key`. A task row with no tab
  adds `无 tab` and dashes the pane.
- stray-tab row: `tab · title=<raw title> · relative lastOutputAt · short pane_key · wt <worktree>`.
- legend: `●` live (`connected=true`) · `○` not connected, or a row the tab axis does not list.
- empty state, one sentence: **no `serverRoots` and no Orca tab** — how to add `serverRoots` to the
  config file.

It surfaces in four places:

1. **The panel** (`Onlyne Sessions` in the right sidebar) — the live board in a dev install, see
   below. Rows carry `✕` when a session ended badly (`outcome=fault/cancelled`), and a snapshot
   older than the cadence dims its ages;
2. **Desktop notifications** on structural change (roots, roles, tasks, tab liveness, a root going
   down or coming back), with a 30s cooldown;
3. **Settings → Plugins → this plugin's logs**: one summary line per change, the whole board for
   the `board` command, and the panel-write outcome;
4. **Command results**: RPC callers get the structured board (the palette discards return values).

### The panel is the board (and how data gets in)

An Orca 1.4.198 plugin panel is a sandboxed `srcdoc` document: CSP
`default-src 'none'; connect-src 'none'` (no fetch), and it may call exactly three host methods
(`workspace.readContext`, `terminal.sendText`, `notifications.show`) — `PLUGIN_PANEL_ACTIONS` in
`src/shared/plugins/plugin-host-api.ts:263`, enforced again by the schema refine in
`plugin-panel-bridge.ts:42` and the capability gate. The host posts nothing into the frame but
watchdog pings and action results. **There is no worker→panel channel**, in either direction of
the bridge, and none is planned in v1.

So the board reaches the panel the only way available: **the document itself**. The worker renders
the snapshot into the panel entry file, and Orca reads that file from the plugin root every time it
opens or refreshes the panel (`src/main/plugins/plugin-panel-controller.ts:142-148`). Two Orca
behaviours turn a file write into a live panel:

- **dev install (primary path)** — the dev watcher watches the configured plugin paths and a change
  schedules the 300 ms debounced refresh (`plugin-dev-watcher.ts:106-114`); the renderer re-reads
  the entry and remounts the frame when the HTML changed (`PluginPanel.tsx:143-147`). Writing is
  explicitly allowed here: `verifyHashAddressedPluginContent` returns ok when `contentHash === null`
  — *“Dev trees are intentionally mutable; installed hash-addressed trees are not”*
  (`plugin-content-integrity.ts`).
- **packaged install (degraded)** — the tree is content-addressed (`<plugins>/<key>/<sha256>/`) and
  re-hashed per panel load, so the worker never writes there. The installed document is whatever
  `panel.html` was when the plugin was installed; this repository commits the **placeholder**
  version, and the live board stays in notifications + the plugin log.

The worker rewrites the document only when the board's *structure* or a session's state changes
(see `panelFingerprint`), never on a timer: ages are `data-ts` attributes ticked by the document's
own script, and a rewrite remounts the panel. Past ~15 s without a new scan the document marks itself
stale and dims its ages — the numbers stay exact (`now - data-ts`), the dimming is the signal that
the scan loop stopped. Rewriting `panel.html` inside
a dev tree is a normal working-tree modification — that file *is* the panel's data channel.

## 4. Data contract as implemented

### Axis A — tabs (one call, flat)

`orca terminal list --json`, with **no** `--worktree` selector: one call answers every tab of every
worktree, and the plugin never walks worktrees. Per row the plugin keeps `handle`, `tabId`,
`leafId`, `paneKey`, `title`, `connected`, `writable`, `lastOutputAt`, `worktreeId`.

- liveness is exactly the row's own `connected` flag; nothing else is consulted.
- a row whose `handle` is missing is dropped (it cannot be addressed).
- **measured**: Orca 1.4.198 rows carry no `paneKey` field, so the plugin derives
  `${tabId}:${leafId}` (newer builds may carry it; it wins when present).
- `worktreeId` is the only selector the plugin keeps — it is what `orca terminal list --worktree`
  would need, and it is what `copy-agent-context` emits.

#### Scoping the tab axis to one swarm: pi claims first, worktree second

An Orca worktree can hold tabs that are not onlyne sessions, so the tab axis is filtered by, in order:

1. **Adapter claims — authoritative.** The pi adapter inherits `ORCA_PANE_KEY`,
   `ORCA_TAB_ID`, `ORCA_TERMINAL_HANDLE` and `ORCA_WORKTREE_ID` from the pane it was spawned in
   (**measured 2026-09-11, Orca 1.4.198**: `orca terminal create --command …` exports all four into
   the command's process), so it is the one component that knows, from the inside, which pane is an
   onlyne session. It publishes that binding as one file per pane —
   `<workspace>/.onlyne/cache/pi-panes/<pane_key, ':' flattened to '-'>.json`, where `<workspace>`
   is where the operator runs `onlyne client run` and the client spawns its plugin with `cwd` =
   workspace. One file per pane, not per workspace: the client admits one client per workspace, but
   one role slot of it runs up to `max_sessions` sessions, each in its own pane and its own pi
   process — a single workspace-wide file would be last-writer-wins, and the board would hide every
   pane but the one that mounted last. A tab is in scope iff its `paneKey` is claimed; a claim whose
   pane Orca no longer lists hides the rest of the axis, so a stale claim never resurrects a tab. A
   claim left behind by a pane that died without clearing is harmless in exactly the same way: it
   matches no row.
2. **Worktree heuristic — fallback.** While no claim is published anywhere, a tab is in scope iff a
   configured root lives inside that tab's own `worktreePath` (both sides `realpath`-resolved). A
   root that resolves into no tab's worktree leaves the axis unscoped, and the note says so.

The plugin can compute neither path on its own: a workspace is where the *client* lives, the board's
session axis is configured by *server root*, and the `welcome.server` triple carries no path — so
claims are read from the workspaces listed in `piWorkspaces` (step 4 above), independent of
`serverRoots`. Empty is a normal state: no claims, worktree heuristic only.

`board.scope` records which decided: `{ derived, source: "adapter" | "worktree" | "none",
worktrees, hidden, claimed? }`, alongside `summary.hiddenTabs`. A missing, unreadable or malformed
claim file is a normal state, never an error: it means that pane has published nothing, and a
malformed file costs its own claim and no other's. A workspace whose `pi-panes/` holds no readable
claim has published nothing either, directory or not.

The pre-v1 single file `<workspace>/.onlyne/cache/pi-pane.json` is read as well, through the
transition: a pi process that was already running when this reader was upgraded keeps writing it
while its pane is live, and ignoring it would hide that pane. It is a transition read, not a second
authority — for one pane key the newest claim wins (`updated_at`, or the file's own mtime when the
claim carries no stamp, which is the case for every legacy file), so a stale legacy file loses to a
fresh per-pane claim and disappears on the next restart. Nothing writes it any more.

### Axis B — sessions (one call per root per verb)

For each configured server root, in config order:

```
onlyne --server-root <S> sessions --json   -> {ok:true, data:{sessions:[…]}}
onlyne --server-root <S> roles    --json   -> {ok:true, data:{roles:[…]}}
```

- Session rows are normalized to `task_id`, `role`, `session_id`, `public_lifecycle` (falling back
  to `projection.lifecycle`), `projection.agent` / `delivery` / `resource`, `outcome`, `updated_at`,
  `seq`. The shape is pinned by the repository's own wire vector
  `crates/onlyne-proto/tests/wire_vectors/res_session_row.json`.
- Role rows are normalized to `name` → `role`, `admin`, `max_sessions` → `maxSessions`, `state` →
  `presence` (`online` / `offline` / `draining`), `sessions`. Pinned by `res_role_info.json`.
- The role list is the section skeleton: a role with zero sessions still renders (that is how you
  see an offline role), and a session whose role has no role row lands under `(unknown role)`.
- **measured 2026-09-11** with `target/debug/onlyne` against a root whose socket is absent: exit 3,
  `onlyne: no onlyne socket found; pass --socket, --server-root, or --workspace` on stderr and
  nothing on stdout, which the plugin reports as `cli_error` for that root only. A refused but
  existing socket answers the JSON error body instead, and that body's own code is reported.

### The join (weak, display-only)

A tab is annotated onto a task row when its **trimmed title is exactly** `onlyne:<task_id>`; a tab
that matches nothing renders in the stray-tab section. Nothing more is inferred.

- **This is not identity.** Any process in the pane can set the title (OSC 0/2), so the prefix can
  be stolen, drop the tab, or point at the wrong task. The adapter / pi plugin protocol owns
  identity; this board is a convenience for a supervisor, and a supervisor who needs certainty must
  read the session rows, not the annotation.
- **Measured 2026-09-11, Orca 1.4.198, one live `sleep 600` session in the host worktree**: the
  create-time title survives about a second — the operator's own login shell (zsh + prompt) takes
  the title over immediately after — while the session row reaches a server root only once the
  agent reports. The two therefore rarely coincide, so in practice the board usually renders the
  two axes side by side and the annotation stays empty. That is the accepted cost of a supervisor
  view with no second discovery axis; `joined` is a bonus, never the reason a row exists.
- Claims are consumed once, in config-root order and Orca row order: with two tabs carrying one
  title the first joins and the rest render stray, and with the same task id on two roots the
  earlier root in `serverRoots` takes the tab while the later row stays unjoined. That keeps
  `live tabs` from counting one physical tab twice; which root truly owns it is not knowable here.

### Degradation matrix (never fatal; reported in the log/notification)

| situation | result |
| --- | --- |
| `serverRoots` absent/empty | valid: tab axis only, `onlyne` is never invoked |
| a `piWorkspaces` entry publishes no claim | not an error: it is listed in `board.claims.unpublished` and named in the panel's scope note; the tab axis falls back to the worktree heuristic |
| `orca terminal list` fails (e.g. `missing_binary`) | `ok:false` + the error code; the session axis is still rendered, unjoined |
| a root's socket is absent | that root reports `cli_error` with the canonical no-socket hint; other roots and the tab axis still render |
| onlyne CLI without `--server-root`/`sessions` (e.g. 0.6.0) | `cli_surface_mismatch` on that verb; a failing verb never hides the other one |
| `onlyne` binary missing | `missing_binary`, same per-verb degradation |
| an answer without the verb's rows | `unexpected_shape` |
| a tab title stolen or absent | no annotation: the task row shows `无 tab`, the tab shows up stray |

## 5. Known boundaries

- **Manual install**: pluginApi v1 has no CLI install surface; install/enable/consent happen in
  the desktop UI.
- **Experimental API**: `pluginApi` is not frozen; after an Orca upgrade run “rescan” once to
  confirm the join still holds.
- **Workers get reaped**: the 5s fallback cadence runs only while the worker lives; the next
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
    claims.mjs         the pi adapter's pane claims (swarm membership authority)
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
`worktree create|rm` call and exits 1 — so running the smoke run is itself evidence that nothing
touched your tabs. It generates the panel document into a temp root, so a smoke run never rewrites
the plugin tree it is testing.

**Verifying the panel without the UI**: run `onlyne-sessions.debug-board` (palette or
`plugins.invokeCommand`) — it writes the very payload the panel document embeds to
`/tmp/onlyne-board.json` and notifies a one-line summary, so a JSON dump and the panel can be
compared directly. A dev-installed panel updates within the watcher's 300 ms debounce plus one
scan (≤5 s with no events).

## 8. Contract notes for the onlyne backend

Implemented against the surfaces as measured; these are the points the implementation surfaced
(reported, not renegotiated):

1. **The title convention is not a contract.** The plugin joins on `onlyne:<task_id>` because that
   is the only tab-side hint available, but OSC title writes can replace it at any moment. If the
   backend starts depending on that prefix, it must also publish the pane/handle binding some other
   way — the adapter / pi plugin protocol is the authority, and this board only mirrors it.
2. **The plugin reads no backend file.** Sessions come from `sessions` and roles from `roles`,
   nothing else. Anything a supervisor needs must be answerable through those two verbs.
3. **`sessions` must stay answerable without a role workspace.** With no per-role worktree
   registration, a role's tab is indistinguishable from any other tab on the Orca side, so the
  `task_id` ↔ tab binding is recovered from the pi adapter instead (`pi-panes/`, §2 axis A):
   the adapter inherits `ORCA_PANE_KEY` from the pane it runs in, so the binding measured on the
   wire is the backend's own `pane_key` for spawned sessions.
4. **Session keys used**: `task_id` (identity), `session_id` (display), `role` (section),
   `public_lifecycle` / `projection.lifecycle`, `projection.agent`, `projection.outcome`,
   `updated_at`, `seq`. Anything else is ignored.
5. **Role row fields used**: `name`, `admin`, `max_sessions`, `state`, `sessions`. The presence
   vocabulary is the server's own (`online` / `offline` / `draining`) and is rendered verbatim.
6. **Per-root failures are expected.** A root with no live server is a normal state for the board;
   the plugin reports each failing verb with its own code and keeps going. It never merges roots:
   identical task ids on two roots stay two rows.
7. **The plugin never passes `--quiet` or `--socket`.** It reads the whole answer body
   (`{ok, data:{…}}`), so a change in that envelope shape is a breaking change for the plugin.
