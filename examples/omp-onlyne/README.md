# omp-onlyne cluster example

Four real OMP sessions, modelled on the local regions and roles:

| region | role |
|---|---|
| `alexandria` | `caerulea` |
| `alexandria` | `cyanea` |
| `daily-workbench` | `dpdp` |
| `research-chamber` | `lazuli` |

Each role is one OMP process with its own Onlyne node. The role directories are
empty: the first run creates the project config through `/onlyne init`. There is
no central server and no fake peer. The role that sends the first message starts
the thread and owns it for the rest of the conversation; the other roles join as
members and read it from that owner. The example predefines no thread.

`AGENTS.md` in this directory is the contract every role session reads: `msg` is
the only entry, a send's `admission` is the delivery fact, and a reply arrives on
its own as an inbound message. OMP loads it from a parent directory, so starting
any role below this path is enough; `--no-rules` disables rule files, not this.

## Launch one role

From a role directory, for example `regions/research-chamber/lazuli`:

```sh
omp --no-extensions --no-skills --no-rules \
  -e ../../../../../packages/omp-onlyne/index.ts
```

`--no-extensions` disables discovery; the single `-e` flag is the complete
extension set. `--no-skills` and `--no-rules` keep the test free of the machine's
usual agent configuration. OMP must start in the role directory so its project
config lives in that directory.

If `omp-todo-more` is installed and you want its richer todo surface, add
`-e ~/.omp/plugins/node_modules/omp-todo-more/index.ts` before the plugin flag.
The plugin works without it and falls back to OMP's native todo.

Run `/onlyne init`. The form asks for:

- region: required; a Tern session name is only a suggestion, and the saved
  value is an absolute `urn:onlyne:region:...` URI;
- node id: required, initially empty; a short name such as `lazuli` is saved as
  `urn:onlyne:agent:lazuli`;
- bind: default `127.0.0.1`;
- key ref: optional; leave empty for the first loopback E2E;
- page size: default 50;
- initial node address: optional; used when the seed node is already running.

Saving starts the node, joins the region and begins network discovery. Cancel
writes nothing. `/onlyne settings` edits the same fields later.

The status row above the editor shows
`region | nodes N | neighbors M | alert A | unread U` for the running node.

## Join the other roles

Start the first role, complete `/onlyne init`, and note the endpoint it reports.
For each later role, run the same command in its directory and complete
`/onlyne init`; put that endpoint in the optional **initial node address** field.
The new node directly checks the seed's service description, reads its HTTP
candidate directory, and stores the candidates in `.onlyne/omp.db`.

DNS-SD is optional. If `bonjour-service` is installed, the same-link browser may
add candidates; a missing package or blocked multicast only disables that source.
The seed and HTTP directory path keep working.

Without Tern, fill region by hand and use the same seed flow. Tern only supplies
a suggested region and a native rendering surface.

## What to look at

1. In `dpdp`, ask the agent to send a review request to `lazuli` with the `msg`
   tool. `dpdp` starts the thread and owns it; `lazuli` becomes a member and
   receives an attention row.
2. `lazuli` receives an `onlyne.inbound` custom message and starts a turn. Its
   `/onlyne` menu lists the thread in the sidebar and shows the message flow in
   the middle column.
3. Reply from `lazuli` with `in_reply_to`. The reply is stored in `dpdp`'s
   thread at the next seq, so both roles read one list from one owner.
4. `/onlyne settings` edits that role's project config in place. Cancel writes
   nothing.
5. `/onlyne` shows four columns: the domain rail, the thread sidebar with the
   pending key requests and the inbox, the conversation with its composer, and
   the current domain's nodes with the selected node's detail. Each long panel
   scrolls on its own. Different regions still appear in the rail; region
   membership only affects domain-scoped routing.

Each node stores protocol records in `.onlyne/omp.db` beside its config. That
file is local test state and is ignored by git.

## Authentication boundary

This example uses the loopback development binding. Requests carry an
`Onlyne-Actor` header that names the sender, and the receiver rejects a claim of
its own node identity. This binding is not authenticated: it exists to exercise
the full message path before the credential proposal. Do not bind it to a remote
interface.
