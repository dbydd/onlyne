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
- **Read-only discipline**: it never creates, closes or renames a tab, never writes any onlyne
  file, and never writes inside its own install directory. Its only mutation is
  `orca terminal switch`, run solely while you invoke the focus command.

**Where authority lives.** Session identity belongs to the onlyne adapter / pi plugin protocol.
This board is a supervisor convenience: it mirrors what a server root's admin surface reports and
annotates tabs through a title convention *any process in a pane can steal* (see §4). A wrong or
missing annotation is never evidence about a session.

The plugin no longer discovers role workspaces and no longer reads any backend cache file: the
backend registers no per-role Orca worktree any more, so every session tab lands flat in the host
worktree's list and the only session source is the admin surface.

---

## 1. Install (manual — there is no CLI install surface for plugins)

1. **Settings → Plugins → Install plugin**, choose the **Local path** tab, enter the absolute
   path to this directory (for example `<repo>/integrations/orca-plugin`), install.
   - Orca copies the tree to `<userData>/plugins/onlyne.onlyne-sessions/<content-hash>/` and
     writes the `current` pointer, lock entry and provenance. The content hash of that tree is
     re-verified on every plugin refresh, so **never edit files inside the install directory** —
     change the source directory and re-install instead.
   - Alternatively add the source directory under the **Development** section (hot reload, still
     requires permission review).
2. **Enable** the plugin in the list.
3. **Consent** — this plugin asks for three capabilities only:

   | capability | Orca's own wording | used for |
   | --- | --- | --- |
   | `workspace:read` | Read the name, branch, and terminal list of your focused worktree | the panel's "read current workspace" button (the worker itself never calls it) |
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
     "orcaBin": "/opt/homebrew/bin/orca",
     "onlyneBin": "/path/to/v1.0.0/onlyne"
   }
   ```

   - `serverRoots` is the session axis: one entry per onlyne server root, addressed as
     `onlyne --server-root <S> …` (`<S>/.onlyne/run/s` is that root's admin socket).
     **Absent or empty is a valid state** — the board then renders the flat tab list only and
     never calls `onlyne` at all. Entries are trimmed and de-duplicated.
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
| `onlyne-sessions.board` | push the board now (notification + plugin log) | — |
| `onlyne-sessions.refresh` | rescan, then push | — |
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

It surfaces in three places:

1. **Desktop notifications** on structural change (roots, roles, tasks, tab liveness, a root going
   down or coming back), with a 30s cooldown;
2. **Settings → Plugins → this plugin's logs**: one summary line per change, and the whole board
   for the `board` command;
3. **Command results**: RPC callers get the structured board (the palette discards return values).

### The panel is a static document (important)

An Orca 1.4.198 plugin panel is a sandboxed `srcdoc` document: CSP
`default-src 'none'; connect-src 'none'` (no fetch), and it may call exactly three host methods
(`workspace.readContext`, `terminal.sendText`, `notifications.show`). It cannot read worker
state, cannot invoke plugin commands, and the host never pushes plugin data into it
(`PLUGIN_PANEL_ACTIONS` in `plugin-host-api.ts`, the schema refine in `plugin-panel-bridge.ts:42`,
and the capability gate). The entry HTML is re-read only on mount and on plugin refresh events,
and it is an immutable hash-addressed file.

So this plugin's panel is a **static dashboard**: legend, install/consent checklist, boundaries,
one live “current workspace” read button, and four buttons that notify you which palette command
to run. **The live board is delivered through notifications and the plugin log**, which is a
pluginApi v1 boundary rather than a shortcut — and the panel says so on its face.

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
- **No file writes at all** — neither the plugin's own install directory (hash-checked) nor any
  onlyne file, cache or socket-side state. The only file the plugin reads is its own optional
  config.
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
  main.mjs             worker entry: activate/deactivate, four commands, three events
  panel.html           static panel (sandboxed document)
  src/
    runner.mjs         injectable exec, CLI JSON parsing (errors arrive on stdout), config
    orca-cli.mjs       terminal list (flat) / terminal switch / status
    onlyne-cli.mjs     --server-root <S> sessions|roles, normalized rows + failures
    board.mjs          the two axes, the weak title join, the board model
    render.mjs         text rendering shared by notifications and logs
    commands.mjs       refresh / board / focus / copy-agent-context
    board-state.mjs    debounce, 5s fallback, structural fingerprint, notify cooldown
    *.test.mjs         node:test suites (zero dependencies)
  tools/smoke.mjs      read-only smoke run against the live desktop (mutation guard)
```

```bash
node --test                     # the package's own test command
BIN_DIR=target/debug node tools/smoke.mjs
                                # live, read-only: orca status / flat tab list / board per root /
                                # join demo (synthetic sessions, nothing written)
```

The smoke runner intercepts any `terminal switch|create|close|rename|send` or
`worktree create|rm` call and exits 1 — so running the smoke run is itself evidence that nothing
touched your tabs.

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
   registration, a role's tab is indistinguishable from any other tab on the Orca side, so
   `task_id` ↔ tab binding cannot be recovered from Orca.
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
