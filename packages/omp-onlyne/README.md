# omp-onlyne

Onlyne v3 for OMP. One model entry, `msg({ delivery_kind, msg, meta })`, backed by
thread-aware message admission, reply correlation and OMP's `judge` model role.

The plugin owns the OMP integration: a local Onlyne HTTP node, project-scoped
configuration, `onlyne://` reads, custom inbound messages and the `msg` tool.
The protocol library is [`@onlyne/lib`](../lib).

## Configuration

Configuration belongs to the project at `<project>/.onlyne/config.toml`. It is
not read from user-level plugin settings, so two projects on one machine keep
separate identities. `/onlyne init` creates it; no file is required before the
first run.

```toml
[node]
region = "urn:onlyne:region:research-chamber"
id = "urn:onlyne:agent:lazuli"
key_scope = "project"

[server]
bind = "127.0.0.1"

[threads]
page_size = 50
```

`region` is the required protocol membership and `id` is the stable node
identity. They are independent; moving region does not change existing message
references. `/onlyne init` suggests region from the current Tern session name
and leaves the node id empty for the user. A short region is saved as
`urn:onlyne:region:<encoded name>`.

The listening port is allocated by the operating system at startup; the runtime
derives the endpoint and publishes it through discovery. Discovery uses a seed
address supplied during bootstrap and an HTTP candidate exchange stored in
`.onlyne/omp.db`. DNS-SD is an optional same-link accelerator: install
`bonjour-service` to enable it. The configuration does not contain endpoint,
port, peers, task-spawn policy or supplement windows.

`node.key_scope` names where the identity key lives. `project` keeps it in
`<project>/.onlyne/keys`, which a git work tree must ignore; the node refuses to
create it otherwise and names both escapes. `global` keeps it in
`~/.config/onlyne/keys`, shared by every project on the machine. `none` runs the
keyless loopback development binding, which refuses a non-loopback `bind`. A
configuration without the key keeps reading the global store.

`key_ref` is parsed but not enforced. The current development binding refuses a
non-empty value and a non-loopback `bind`, because it authenticates with
`Onlyne-Actor` only. Credential exchange and proof of possession remain a
separate security proposal; leave both at their defaults for the first E2E.

## Surface

- `msg` — send, reply, read, manage threads and operate resources. The judge
  role chooses the action and delivery plan; the result reports the plan that
  ran. A new execution context belongs to the receiver; the sender never mints
  one.
- `onlyne://threads`, `onlyne://threads/<alias>`, `onlyne://threads/<alias>/<seq>`,
  `onlyne://contexts`, `onlyne://inbox`, `onlyne://outbox` — read-only views.
- Inbound messages arrive as `onlyne.inbound` custom messages. A supplement for
  an existing context uses `onlyne.supplement` and `sendCustomMessage`.
- Tern renders these messages as mailing-list letters: an `[onlyne]` subject,
  sender, date, thread and reply headers, Markdown body, and a link to the
  original entry. Other terminals show the same fields as text. Consumption
  markers stay in the model payload and are hidden from the letter.
- Consumption is confirmed only after the host observes the entry in an actual
  provider request; submitting a custom message or tool result does not advance
  the record.
- `/onlyne` opens the queue/chat viewer; `/onlyne settings` edits the project
  configuration. `/onlyne init` writes a first config and starts the node; with
  no config, `/onlyne` and `/onlyne settings` open the same bootstrap.

Subagent sessions may read only the context slice attached to their own agent
identity and may not send; the main agent owns the node identity and the reply
path.

`AGENTS.sample.md` is the agent-facing half of that surface. Copy it into the
host project's `AGENTS.md`: it states that `msg` is the only entry, that a send's
`admission` is the delivery fact rather than the wait, and that replies arrive as
inbound messages so a send needs no polling.

On the discovery page, request a key exchange with a peer. The viewer reads the
peer's public key and shows both fingerprints before sending. Confirmation sends
the signed introduction and records local approval. The peer's contacts page shows
the pending request. Both nodes become trusted after the peer approves. Committed
trust changes notify the viewer immediately, including introductions received over
HTTP, so an open viewer does not wait for maintenance to show a request or approval.

## Run

```sh
bun install
bun run --filter omp-onlyne check
bun run --filter omp-onlyne test
```

The package is loaded by OMP through its `omp.extensions` field. It requires
OMP 18.12.6 or newer.
