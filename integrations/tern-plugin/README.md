# onlyne-widget — Onlyne Board for Tern

A [Tern](https://docs.stencil.so/tern/) plugin that shows every local Onlyne
cluster on one native board: server roots, their roles, live sessions, open
faults, and the supervisor actions that reach them. The block runs
`onlyne` on the machine that runs your panes — the same CLI an operator
types — and draws its JSON answers as a board.

One block type, `onlyne.board`: no lenses, no routes, no chrome. The two
halves are `host.luau` (the block, the polling loop, the actions) and
`window.luau` (the palette commands and key). `onlyne.css` restyles a few
of the block's own nodes; it carries no colors of its own, only Tern's tone
variables, so both themes follow.

## Install

Tern 0.4.5 or newer, and an Onlyne build whose `onlyne` CLI carries the
admin verbs the board runs (`status`, `roles`, `sessions`, `faults`,
`control`, `repair`). From this repository:

```sh
tern plugin install integrations/tern-plugin
```

or, to load it in place and reload on every save:

```sh
tern plugin link integrations/tern-plugin
```

The block finds the CLI by name first, then in the usual Homebrew and
`/usr/local` places, so a daemon-launched Tern without those directories
on its `PATH` still finds an installed Onlyne.

## What it shows

One section per server root, each with:

- the cluster name, version, roles-online count, and the read's age;
- the role table: name (with an aggregate role's target), presence
  (`online`/`draining`/`offline`), session count, queued depth;
- the session table: role, task id (short), lifecycle/agent phase with a
  `+stale` mark when a working row is silent past heartbeat grace, and the
  age of the last heartbeat;
- the open-fault table: fault id, kind, role, reason, age.

Roots come from two sources. The plugin's `roots` list in `tern.kv`
(persisted, shared by every board) and **live discovery**: every
`kind: "server"` registration in the Onlyne runtime directory whose socket
file exists and whose pid answers. A root that leaves both sources keeps
its last good read on the board, marked `gone`, until the block closes.

## Keys

- `↑`/`↓` (or `k`/`j`), `Home`/`End`, `PageUp`/`PageDown` move the cursor;
  the marked row is the one actions aim at.
- `f` focus a session's pane · `p` probe · `t` snapshot · `r` recycle ·
  `x` cancel — on a session row.
- `a` acknowledge a fault · `f` focus its task · `r` retry its task — on a
  fault row.
- Every mutating act is armed by the first press and runs only when the
  same key (or `⏎`, or the **Confirm** badge) repeats it; `esc`, or moving
  the cursor, aborts. A poll that reshuffled the board between the two
  presses turns the second press into a fresh arm, never into the wrong
  target.
- `.` (or `f5`) refreshes now; the board also re-reads every 10 seconds.
- `D` forgets a configured root (removes it from the kv list). A
  discovered root cannot be forgotten this way — it leaves by itself when
  its server stops.

## How the actions run

The board runs the CLI's own supervisor verbs as one-shot processes on the
host, against the row's own server root:

```sh
# focus, probe, snapshot (refuse --reason):
onlyne --server-root <root> --from _supervisor control --task <task> focus \
  --force --yes-i-am-supervisor-not-other-role

# recycle, cancel (require --reason):
onlyne --server-root <root> --from _supervisor control --task <task> recycle \
  --reason "requested from the Onlyne Board block in Tern" \
  --force --yes-i-am-supervisor-not-other-role

# faults:
onlyne --server-root <root> repair ack --fault-id <id> --reason "…"
onlyne --server-root <root> repair retry --task <task> --reason "…"
```

`_supervisor` is Onlyne's reserved operator role; the admin surface accepts
a control frame from it across the ACL. Focus goes through `onlyne control
focus` — the daemon resolves the session's own `backend_ref` and brings its
pane forward; the board never touches pane ids. The reason strings are the
board's own fixed text; edit them in `host.luau` (`ACTION_REASON`,
`ACK_REASON`) if you want the ledger to say something else.

Every action that succeeds triggers an immediate refresh, so the board
shows the effect on the next paint.

## Polling, and why

Tern 0.4.5's process API captures a child's whole output; it cannot stream
`onlyne watch --follow`. The board therefore re-runs four one-shot reads
per root (`status`, `roles`, `sessions`, `faults --open-only`, each
`--quiet --timeout 8000`) on a 10-second cadence, one process at a time, so
a cluster with three roots costs at most twelve short-lived `onlyne`
processes per cycle. A failed or malformed read never drops the data it
replaces: the section keeps its last good answer and the problem line says
what failed and when.

## Configuring

The board reads two keys from `tern.kv` (the plugin's own `kv.json`, shared
by both halves and every window):

- `roots` — a JSON array of server-root paths to watch. Launch arguments
  add to the list: opening the block with arguments, or running **Watch
  this directory as an Onlyne server root**, appends normalized paths and
  remembers them.
- `sender` — the `--from` role control ops carry. Defaults to
  `_supervisor`, which always resolves; set it to one of your spec's roles
  if your cluster expects that spelling.

Tern stores the file under its plugin data directory
(`tern plugin dir`'s sibling data tree), so the list survives restarts and
plugin updates.

## Files

- `plugin.toml` — manifest: schema 1, id `onlyne`, block `board`.
- `host.luau` — the `board` block: root registry, polling chain, views,
  armed actions.
- `window.luau` — **Open Onlyne board** and **Watch this directory as an
  Onlyne server root**; `cmd+alt+shift+o` (`ctrl+alt+shift+o` elsewhere)
  opens the board.
- `onlyne.css` — hover/separator styling from Tern's tone variables.
