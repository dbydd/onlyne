---
name: onlyne-cluster
description: Deploy and verify an Onlyne v3 cluster. Covers agent nodes from the OMP plugin, IM gateway nodes, MCP gateway nodes, per-pair key exchange, resource assignment, the steps only a human can perform, and the failure codes that follow a missing pair or a wrong handler. Use when installing Onlyne, adding a node, pairing keys, or diagnosing a gateway that receives no events.
---

# Deploy an Onlyne cluster

## What a cluster is

One process is one node. A node keeps its own records. There is no central Onlyne server.

A cluster is a set of nodes that know each other by key exchange and address discovery. Three
kinds exist.

| Kind | Process | Serves |
|---|---|---|
| Agent node | an OMP session with the `omp-onlyne` plugin | messages, threads, and the `msg` tool |
| IM gateway | `onlyne-im-gateway <config.json>` | platform conversations, one resource each |
| MCP gateway | `onlyne-mcp-gateway <config.json>` | MCP servers, one resource each |

An agent reaches a gateway resource through the same `msg` path it uses for messages. No
second protocol and no side channel.

## Before you start

Install Bun 1.4.3 or newer and OMP 18.12.6 or newer.

Install the packages one of two ways.

```sh
npm install -g @onlyne/lib @onlyne/node-host @onlyne/im-gateway @onlyne/mcp-gateway
```

```sh
cd packages/omp-onlyne && bun link
cd packages/im-gateway && bun link
cd packages/mcp-gateway && bun link
```

The plugin belongs to OMP, so link it through OMP.

```sh
omp plugin link /path/to/onlyne/packages/omp-onlyne
omp plugin list --json
```

`omp plugin list --json` must show `omp-onlyne` with `enabled: true` and
`manifest.extensions: ["./index.ts"]`. Confirm both before you continue.

Both gateway binaries must answer on PATH.

```sh
onlyne-im-gateway    # prints usage and exits 2
onlyne-mcp-gateway   # prints usage and exits 2
```

## Agent node

Each agent node owns one project directory. The plugin writes `<project>/.onlyne/config.toml`.

```toml
language = "en"

[node]
region = "urn:onlyne:region:research"
id = "urn:onlyne:agent:alice"
key_scope = "project"

[server]
bind = "127.0.0.1"

[threads]
page_size = 50
```

Create it with `/onlyne init` in the OMP session. Keep one identity per directory. `key_scope`
names where the private key lives: `project` keeps it in `.onlyne/keys`, `global` in
`~/.config/onlyne/keys`, `none` runs the keyless loopback development binding.

A keyless node trusts any declared actor. Use it only for a loopback test. A real cluster needs
keys on every node.

## IM gateway node

```json
{
  "node": { "id": "urn:onlyne:gateway:im" },
  "server": { "bind": "127.0.0.1", "port": 3991 },
  "storage": { "path": "./var/im/store.json" },
  "credentials_dir": "./credentials",
  "media_dir": "./var/im/media",
  "connectors": [
    { "platform": "lark", "assignee": "urn:onlyne:agent:alice", "credential_file": "feishu" }
  ]
}
```

`assignee` becomes the resource's `binding.handler`. Events for a conversation go to that node.
`credential_file` resolves under `credentials_dir`. The file holds a `$VARIABLE` reference, and
the gateway reads the value from the environment.

Install the connector for each platform you serve. The gateway loads `@onlyne/im-<platform>` at
runtime and answers `unsupported_delivery` when the package is absent.

`ONLYNE_NODE_ID`, `ONLYNE_BIND`, and `ONLYNE_PORT` override the file for one host.
`ONLYNE_NO_TUI=1` keeps the JSON log on stdout.

## MCP gateway node

```json
{
  "node": { "id": "urn:onlyne:gateway:mcp" },
  "server": { "bind": "127.0.0.1", "port": 3992 },
  "storage": { "path": "./var/mcp/store.json" },
  "servers": [
    {
      "name": "hindsight",
      "url": "http://localhost:8888/mcp/example-bank/",
      "timeout_ms": 60000,
      "controllers": ["urn:onlyne:agent:alice"]
    }
  ]
}
```

The server's tool list becomes the resource's `operations`. `controllers` lists the nodes that
may call those tools. An MCP resource has no handler binding, so it has no handover and no event
log.

## Key exchange is per pair

This is the step that fails most often. Trust is recorded for one pair of nodes, never for a
node in general.

| Action | Pair that must complete an exchange |
|---|---|
| Agent A sends a message to agent B | A ↔ B |
| Gateway pushes a resource event to its handler | gateway ↔ handler |
| Gateway hands a resource to a target | gateway ↔ target |

An agent that already trusts the gateway gains nothing for the handover. The gateway itself must
trust the handover target, because the gateway signs the probe.

### Pair from the OMP side

1. Open `/onlyne` and select the peer.
2. Choose the request action (发现 → 请求交换密钥 / Discover → Request key exchange).
3. Read the fingerprint the card shows. Compare it with the gateway terminal out of band.
4. Confirm only the fingerprint you read.

### Pair from the gateway side

The gateway terminal lists discovered peers with an exchange stage and a delivery mark.

| Key | Action |
|---|---|
| `t` | request an exchange, show the peer fingerprint, then wait for `y` |
| `a` | accept a request this node received |
| `r` | revoke trust |
| `c` | connect to an address, then press Enter |
| `q` | close the screen and keep the gateway running |

The stage becomes `exchanged` (已认证) only after both sides approve. A peer that shows
`requested` (等对方) waits for the other side.

The delivery mark is separate. It says whether a resource binding points at that peer right now.
A peer can be exchanged and receive nothing, and a peer can be a delivery target with no
exchange, in which case every push is refused.

## What only a human can do

An agent cannot complete these steps. Ask the operator and wait.

- Compare fingerprints on both machines and approve the exchange.
- Supply the platform credential for a real IM application.
- Choose the `assignee` for a conversation, and the `controllers` for an MCP server.
- Choose the handover target and confirm the deadline.
- Decide whether a resource keeps a standing log reader after a handover.

An agent may write configuration files and start processes. An agent must not confirm a
fingerprint it did not show the operator.

## Verify a deployment

Check each claim against output. Do not assume a step succeeded.

| Claim | Evidence |
|---|---|
| the plugin is loaded | `omp plugin list --json` shows `omp-onlyne`, enabled |
| a gateway is listening | log line `im.gateway.listening` or `mcp.gateway.listening` with an endpoint |
| the pair completed | `/onlyne` neighbor row shows `exchanged` (已认证) |
| events have a destination | the same row shows the delivery mark |
| the agent can read the resource | `onlyne://resources` lists it, and `onlyne://resources/<owner>/<id>` returns JSON |
| the event log is readable | `onlyne://resources/<owner>/<id>/log` returns events in id order |

The same reads are available through `msg`.

```json
{ "delivery_kind": "list the resources", "msg": "", "meta": { "scope": "resources", "op": "list" } }
{ "delivery_kind": "read the event log", "msg": "", "meta": { "scope": "resources", "op": "log", "resource": { "owner": "urn:onlyne:gateway:im", "id": "lark-oc_example" }, "since": "0" } }
```

Resource reads belong to the main OMP session. A subagent is refused.

## Failure codes

| Symptom | Cause | Fix |
|---|---|---|
| `untrusted_target` on a handover, with `details.required_action` | the gateway has no exchange with the handover target | pair the gateway with the target |
| `im.push.refused` with `not_found` repeating | the resource binding names a handler that does not hold a read grant, or the resource is not registered | check `assignee`, then re-register |
| `not_found` on a resource read | the caller is neither the handler nor a log reader | add the caller to `log_readers`, or read through the handler |
| `unsupported_delivery` for a connector | the platform package is not installed | install `@onlyne/im-<platform>` |
| `decision_unavailable` | no OMP session is bound to the decision | call `msg` from an OMP session, not from a script |
| a reply never arrives | the pair is incomplete in one direction | check both rows, not one |

## Stop a cluster

Send `Ctrl-C` to each gateway. The log line `im.gateway.stopped` or `mcp.gateway.stopped`
confirms the exit, and the listening port closes.

`q` in the gateway terminal closes only the screen. The gateway keeps running.

Stop each OMP session with `Ctrl-D` or its own exit command.

## Install this skill

The skill ships in the Onlyne repository at `.agents/skills/onlyne-cluster/`. Install it from
the repository URL with the skill CLI, or copy that directory into
`~/.agents/skills/onlyne-cluster/`.
