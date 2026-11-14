---
name: onlyne-tui
description: Launch, configure, and navigate the Onlyne v3 Antigravity standalone terminal panel (`onlyne-agy`), setup wizard (`onlyne-agy init`), local Web GUI launcher (`onlyne-agy gui`), and background daemon.
---

# Onlyne Antigravity TUI & CLI (`onlyne-agy`)

Use `onlyne-agy` in a terminal for a full keyboard-driven 4-column TUI or Web GUI management outside the MCP host.

## CLI Subcommands

- `onlyne-agy` (or `onlyne-agy tui [--cwd <dir>]`) — Launch the interactive 4-column TUI (`Domains | Sidebar | Flow | Peers`, collapsing into tabs below 96 columns).
- `onlyne-agy init [--cwd <dir>]` (or `onlyne-agy --init`) — Open the 11-field `.onlyne/config.toml` Setup Wizard directly (`region`, `node.id`, `server.bind`, `key_scope`, `page_size`, `seeds`, `policy`, `allow`, `judge.mode`, `judge.model`, `judge.base_url`).
- `onlyne-agy gui [--open] [--cwd <dir>]` — Start/connect the workspace daemon and print the authenticated `http://.../ui?token=...` Web GUI URL (passing `--open` launches the OS browser).
- `onlyne-agy status [--cwd <dir>]` — Print live node JSON status.
- `onlyne-agy read <onlyne://...> [--cwd <dir>]` — Render any `onlyne://` URL to stdout.
- `onlyne-agy msg <kind> <msg> [--meta <json>] [--cwd <dir>]` — Invoke `msg({ delivery_kind, msg, meta })` from CLI.
- `onlyne-agy daemon <run|start|stop|status> [--cwd <dir>]` — Control the per-workspace background Onlyne daemon.

## TUI Keybindings

| Key | Action |
| :--- | :--- |
| `1`-`4`, `Tab`, `h`/`l` | Switch active column (`Domains` / `Sidebar` / `Flow` / `Peers`) |
| `↑`/`↓` or `j`/`k` | Move cursor / scroll flow |
| `Enter` / `Space` | Open selected thread/peer/item, read full message, or cycle todo state |
| `c` | Compose message in active thread or to selected peer (`Ctrl+S` sends, `Tab` cycles response mode) |
| `y` | Reply to latest or currently opened message |
| `s` | Connect a seed peer by URL |
| `r` | Refresh discovery and snapshot |
| `k` | Start interactive fingerprint key exchange with selected peer |
| `a` / `x` | Accept / Reject (or revoke) key exchange |
| `e` | Edit acquisition policy (`all` / `segment` / `none`) and CIDR allow list |
| `K` | Rotate local node key |
| `i` | Open `.onlyne/config.toml` Setup Wizard modal |
| `L` | Toggle UI language (`en` / `zh-CN`) |
| `?` / `q` | Toggle help / quit TUI (background daemon stays running) |

## Troubleshooting

- **Daemon won't connect or stale lock**: check `onlyne-agy daemon status --cwd <dir>`, or restart via `onlyne-agy daemon stop && onlyne-agy daemon start`.
- **Unconfigured workspace**: TUI shows an unconfigured banner and hints `[i]` to run the Setup Wizard cleanly without writing unwanted `.onlyne/config.toml` files into unrelated repos.
- **No inbound events visible in Antigravity**: ensure `.onlyne/config.toml` exists in the workspace, peer trust is `exchanged`, and Antigravity's `onlyne` plugin is enabled.
