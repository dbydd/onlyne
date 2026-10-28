# @onlyne/mcp-gateway

An Onlyne node that hosts MCP servers as resources. Each configured server registers as one
resource whose operations are that server's tools, so an agent reaches MCP through the same
resource path it uses for every other capability.

The node also runs persistent scheduled tasks. A task executes a locally allowed command or an
allowed MCP tool and sends a readable result to its controller through Onlyne. These results use
ordinary messages. They do not create a platform event stream or a resource event log.

## Run

```sh
bunx onlyne-mcp-gateway ./gateway.json
```

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
      "controllers": ["urn:onlyne:agent:main"]
    }
  ]
}
```

`headers` adds per-request headers such as an API key and is never logged. `controllers` lists
the nodes allowed to call a server's tools; an empty list leaves the node owner alone.
`language` accepts `en` or `zh-CN` and otherwise follows `ONLYNE_LANG`, `LC_ALL`, or `LANG`.

## Scheduled tasks

Mounts are disabled until the operator configures the `mounts` section:

```json
{
  "mounts": {
    "controllers": ["urn:onlyne:agent:main"],
    "commands": [
      { "id": "heartbeat", "command": "/bin/sh", "args": ["-c", "printf 'heartbeat\\n'"], "cwd": "/tmp" }
    ],
    "tools": [{ "server": "hindsight", "tool": "recall" }],
    "tick_ms": 1000
  }
}
```

The node operator defines executable commands and arguments in local configuration. A remote
mount selects a command id. Remote input cannot replace its executable, arguments, or working
directory. An MCP mount selects an allowed server and tool. Its controller must also appear in
that server's `controllers` list. The store records the actor and definition for each change.

Each configured controller owns one scheduler resource named `mounts:<controller node id>`.
The resource's handler is that controller, so another controller cannot operate its tasks. The
gateway owner can read the resource. Agents use the existing resource operation path through
`msg`; hosts may call the authenticated resource HTTP endpoint directly.

The resource exposes these operations:

| Operation | Data | Result |
|---|---|---|
| `mount` | `schedule`, `action`, optional `title` | New task |
| `list` | Optional `include_removed` | This controller's tasks |
| `inspect` | `task` | Definition, runs, and change records |
| `update` | `task`, optional `title`, `schedule`, `action` | Updated definition |
| `runs` | `task`, optional `limit` from 1 to 100 | Recent runs, newest first |
| `cancel` | `task` | Stops future runs and the scheduler's active command |
| `resume` | `task` | Re-arms the task |
| `remove` | `task` | Removes the mount and retains its records |

For example, mount a command every 30 seconds:

```json
{
  "operation": "mount",
  "data": {
    "title": "Heartbeat",
    "schedule": { "kind": "interval", "every_ms": 30000 },
    "action": { "kind": "command", "command": "heartbeat" }
  }
}
```

An interval fires immediately, then measures each interval from its previous start. A one-shot
schedule uses `{ "kind": "once", "at": "<future ISO timestamp>" }`. Cron uses
`{ "kind": "cron", "expression": "*/5 * * * *" }`, with five numeric UTC fields and the
standard `*`, comma, range, and step syntax. Named months and weekdays are rejected. An MCP
action uses `{ "kind": "mcp", "server": "hindsight", "tool": "recall", "arguments": {} }`.

The store retains each task, its next trigger time, and every run's start, finish, exit code,
output summary, and failure reason. A task runs once at a time. A missed trigger during an
active run records a skipped run. On restart, unfinished runs become interrupted records.
Overdue interval and one-shot tasks receive one catch-up run. Cron resumes at its next future
UTC occurrence. Updating an active task changes future runs and preserves the active run's
definition.

Each completed run has a durable push record. Its stable run id is also its message id, so a
retry reuses the same delivery identity. Results are sent to the controller with
`response: "none"` and `execution: "existing"`. Pending and uncertain deliveries retry with
backoff within a 24-hour window. Refused and expired deliveries retain their reason. An
accepted receipt means the controller saved the message; the controller host confirms model
consumption through its existing delivery path.

## Terminal

The shared trust terminal shows servers and tool counts, task state and next trigger times,
the pending push queue, and recent results. `ONLYNE_NO_TUI=1` keeps JSON logs on stdout,
including when the process runs in a terminal.

## Library use

```ts
import { createMcpGateway } from '@onlyne/mcp-gateway';
```

The result carries the node, the connections, and a `fetch` handler.

## License

MIT.
