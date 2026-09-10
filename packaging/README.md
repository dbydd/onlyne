# Packaging

Service definitions ship as examples for an operator or a package to install. The core binaries
never install or launch themselves as a service.

## Files

| file | target |
| --- | --- |
| `launchd/se.onlyne.server.plist` | `~/Library/LaunchAgents/` or `/Library/LaunchDaemons/` |
| `systemd/onlyne-server.service` | `/etc/systemd/system/` |
| `systemd/onlyne-client@.service` | `/etc/systemd/system/` |

## Paths to edit

Both shapes run one foreground command and name no supervisor logic of their own:

- server: `onlyne-server run --root <server-root>`, working directory `<server-root>`
- client: `onlyne-client run --workspace <workspace>`, working directory `<workspace>`

The units carry `/srv/onlyne` and `/srv/onlyne-ws/%i` as the two roots. Replace them with the real
roots before loading. Create a root first with `onlyne-server init --root <dir>`, which also makes
`.onlyne/logs/`, the directory the log files land in.

## Restart behavior

`KeepAlive` with `SuccessfulExit=false` on macOS and `Restart=on-failure` on Linux both restart the
daemon after an abnormal exit and leave a clean exit alone. `systemctl reload onlyne-server` sends
SIGHUP, which re-parses `spec.toml` and swaps it atomically.

## Logs

Daemon output goes to `.onlyne/logs/server.log` under the server root and `.onlyne/logs/client.log`
under the role workspace.

## Client instances

The client unit is a template. `systemctl start onlyne-client@planner` runs the client for
`/srv/onlyne-ws/planner`. A nested workspace name uses `systemd-escape` output as the instance name.
