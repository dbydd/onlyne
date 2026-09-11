# Onlyne Sessions (Orca plugin)

A **read-only role session manager** for the Orca desktop app. It discovers, with zero
configuration, every Orca worktree carrying `<role workspace>/.onlyne/cache/orca-tabs.jsonl`
and joins four sources into one board:

```
Orca worktree list   ──┐
mapping jsonl file   ──┼─→ board: role → task → tab liveness → session state
orca terminal list   ──┤
onlyne client socket ──┘
```

- **pluginApi v1** (Orca 1.4.198+); the API is still marked EXPERIMENTAL.
- **Zero npm dependencies** — Node built-ins only.
- **Read-only discipline**: it never creates, closes or renames a tab, never writes any onlyne
  file, and never writes inside its own install directory. Its only mutation is
  `orca terminal switch`, run solely while you invoke the focus command.

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
   | `workspace:read` | Read the name, branch, and terminal list of your focused worktree | the panel's "read current workspace" button |
   | `notifications:show` | Show desktop notifications labeled with the plugin name | board pushes, focus results, context triple |
   | `events:subscribe` | Get notified when worktrees are created or removed and when agent status changes | event-driven rescans (2s debounce) |

   Degradation when not granted: without `events:subscribe` no events are subscribed (commands
   still work, the reason is logged); without `notifications:show` notifications are suppressed
   into the plugin log. The plugin never asks for `terminal:send`.

4. Optional config (everything works without it): `~/.config/onlyne-sessions/config.json`

   ```json
   { "orcaBin": "/opt/homebrew/bin/orca", "onlyneBin": "/path/to/v1.0.0/onlyne" }
   ```

   Why this may be needed: the plugin worker environment is scrubbed to a 16-variable allowlist
   (`PATH`, `HOME`, …), and an Orca launched from the Dock often has no homebrew `PATH`. The
   plugin resolves binaries via `PATH → /opt/homebrew/bin → /usr/local/bin → ~/.local/bin →
   ~/bin → ~/.cargo/bin`, and logs a clear degradation when it cannot. `BIN_DIR` (the repository's
   e2e convention) wins over discovery when the file exists, so a smoke run against a fresh build
   is `BIN_DIR=target/debug node tools/smoke.mjs`. Note that the **legacy `onlyne` 0.6.0 does not
   know the `sessions` verb** and degrades as `cli_surface_mismatch`; point `onlyneBin` at the
   v1.0.0 binary (usually `target/debug/onlyne`).

## 2. Commands (command palette: search “Onlyne Sessions”)

| command | effect | argument |
| --- | --- | --- |
| `onlyne-sessions.board` | push the board now (notification + plugin log) | — |
| `onlyne-sessions.refresh` | rescan, then push | — |
| `onlyne-sessions.focus` | `orca terminal switch` on a unique match | `args.task`: task prefix |
| `onlyne-sessions.copy-agent-context` | emit the `pane_key`/`handle`/`orca selector` triple | `args.task`: task prefix |

**Argument boundary (measured)**: the palette passes no arguments to plugin commands
(`plugin-command-execution.ts` sends only `pluginKey`/`commandId`). Therefore:

- without a prefix, `focus` / `copy-agent-context` act only when there is **exactly one live
  tab**; zero or many matches produce a notification listing candidates instead of a guess.
- to pass a prefix, call it through the RPC/IPC surface (it returns a structured result):

  ```json
  { "pluginKey": "onlyne.onlyne-sessions",
    "commandId": "onlyne-sessions.focus",
    "args": { "task": "task8" } }
  ```

- `focus` moves foreground focus (the `orca terminal switch` side effect), and only when you ask.
- `copy-agent-context` "copies" by notification: pluginApi v1 has **no clipboard host method**,
  so the notification carries the triple as text and the structured result carries it for RPC
  callers.

## 3. What the board looks like, and where it shows up

```
Onlyne sessions · 2 roles · 3 live tabs · 1 working
planner  (2/3 live)
  ● task8a1b · working/running · 12s · 45e603f7:b6d067b6
  ○ task9c2d · idle/gone · 3m · 970e3c58:17fc0aac
builder  (1/1 live)
  ● task7f3e · idle · 5s · 24d62468:4ce346ca
```

- summary: `N roles · M live tabs · K working` (working = `public_lifecycle=working` or
  `agent=running`).
- row: `task short · lifecycle/agent (falls back to mapping state when the client socket is
  unreachable) · relative lastOutputAt · short pane_key`.
- legend: `●` live tab (`connected` and mapping `state=spawned`) · `○` mapping row only (tab is
  gone) · `✕` worktree removed (kept 10 minutes for orientation).
- empty state, one sentence: **no role workspace yet — waiting for the supervisor to bring up a
  role client** (`.onlyne/cache/orca-tabs.jsonl` must appear in the role workspace).

It surfaces in three places:

1. **Desktop notifications** on structural change (roles / tasks / tab liveness / worktree
   removal), with a 30s cooldown;
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
to run. **Live mapping is delivered through notifications and the plugin log**, which is a
pluginApi v1 boundary rather than a shortcut — and the panel says so on its face.

## 4. Data contract as implemented

`<role workspace>/.onlyne/cache/orca-tabs.jsonl` (append-only, one JSON object per line):

```json
{"pane_key":"<tabId:leafId>","handle":"term_<uuid>","task_id":"…","session_id":"…",
 "role":"planner","worktree_selector":"path:/abs/ws","title":"onlyne:<task_id>",
 "state":"spawned|closed","updated_at":"<rfc3339>"}
```

- Later lines win per `pane_key`; `state:"closed"` is a tombstone and never appears as a row.
- A missing file (fake/zellij backend, or simply not a role workspace) skips that worktree — it
  is not an error.
- Join rules:
  - `pane_key`: Orca 1.4.198 `terminal list` rows carry **no** `paneKey` field, so the plugin
    derives `${tabId}:${leafId}` and matches mapping rows on it (then falls back to `handle`).
  - session: matched by `task_id`, then `session_id`; **never guessed by role** (one role runs
    several tasks).
  - session state is read only from that workspace's own client socket, naming the surface
    explicitly: `onlyne --socket <ws>/.onlyne/run/s --as client sessions --json`. Measured on
    2026-09-11 against `target/debug/onlyne`: without `--as client` the CLI infers the admin
    surface from the canonical `.onlyne/run/s` suffix, sends an admin frame to the client socket,
    and the socket closes it (`{"ok":false,"error":{"code":"internal","message":"socket closed
    before an answer arrived"}}`, exit 1). With `--as client` the same command answers
    `{"ok":true,"data":{"sessions":[…]}}`, and a dead server link answers
    `{"ok":false,"error":{"code":"internal","message":"the connection is not ready"}}` — which
    degrades to the mapping `state`, like every other failure below.

### Degradation matrix (never fatal; reported in the log/notification)

| situation | result |
| --- | --- |
| no mapping file | not a role workspace: skipped |
| `orca terminal list` fails | rows kept with `terminal=null`, shown `○`, error logged |
| `<ws>/.onlyne/run/s` missing | onlyne is not called; the state column falls back to the mapping `state` |
| onlyne CLI without the `sessions` verb (e.g. 0.6.0) | `cli_surface_mismatch`, same fallback |
| `onlyne` binary missing | `missing_binary`, same fallback |
| `orca worktree list` fails | empty board plus the error code in the notification/log |

## 5. Known boundaries

- **Manual install**: pluginApi v1 has no CLI install surface; install/enable/consent happen in
  the desktop UI.
- **Experimental API**: `pluginApi` is not frozen; after an Orca upgrade run “rescan” once to
  confirm the join still holds.
- **Workers get reaped**: the 5s fallback cadence runs only while the worker lives; the next
  event or command restarts it, so event-driven refresh is not a hard real-time guarantee.
- **No lifecycle**: spawn/close/rename belong to the onlyne backend (no dual owner).
- **No file writes at all** — neither the plugin's own install directory (hash-checked) nor
  onlyne's cache.
- **JSON tolerance**: unknown mapping fields are ignored; `state` is interpreted only for
  `spawned`/`closed`, other values render verbatim and do not count as live.

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
    runner.mjs         injectable exec, CLI JSON parsing (errors arrive on stdout)
    orca-cli.mjs       worktree list / terminal list / terminal switch / status
    onlyne-cli.mjs     --socket <ws>/.onlyne/run/s --as client sessions, normalized failures
    mapping.mjs        append-only + override semantics + tombstones
    discover.mjs       discovery → join → board model (incl. graveyard greying)
    render.mjs         text rendering shared by notifications and logs
    commands.mjs       refresh / board / focus / copy-agent-context
    board-state.mjs    debounce, 5s fallback, structural fingerprint, notify cooldown
    *.test.mjs         node:test suites (75 cases, no dependencies)
  tools/smoke.mjs      read-only smoke run against the live desktop (mutation guard)
```

```bash
node --test                     # 75 pass
BIN_DIR=target/debug node tools/smoke.mjs
                                # live, read-only: orca status / worktree list / discovery / join demo
```

The smoke runner intercepts any `terminal switch|create|close|rename|send` or
`worktree create|rm` call and exits 1 — so running the smoke run is itself evidence that nothing
touched your tabs.

## 8. Contract notes for the onlyne backend

Implemented verbatim against the agreed contract; these are the points the implementation
surfaced (reported, not renegotiated):

1. **`pane_key` spelling**: Orca 1.4.198 `terminal list` rows have no `paneKey`, so the plugin
   derives `${tabId}:${leafId}`. If the backend writes a different spelling into the mapping
   file, the join silently breaks (every row renders `○`).
2. **`state` vocabulary**: only `spawned`/`closed` carry semantics here (liveness / tombstone);
   other values render verbatim. A future rename (e.g. `running`/`attached`) needs a matching
   change.
3. **Tombstone scope**: `state:"closed"` is treated as the *current* state of that `pane_key`,
   not as a historical event — a later `spawned` line for the same pane resurrects it, which is
   what append-only implies.
4. **Session match keys**: only `task_id` / `session_id` join; when both are missing or disagree
   with the server, the board degrades to the mapping state rather than guessing.
5. **`worktree_selector` is display-only**: addressing uses the Orca worktree row's own `path`
   (`path:<abs>`), because only registered worktrees can be located by `terminal list`.
6. **Extra fields**: `tab_id`/`leaf_id`/`worktree_id` and friends are ignored today (the join is
   unaffected).
7. **Missing role**: a mapping row without `role` is grouped under `(unknown role)`.
