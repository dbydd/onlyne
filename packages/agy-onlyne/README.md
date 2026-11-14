# @onlyne/agy-onlyne

Decentralized **Onlyne v3** node integration, **Antigravity plugin**, **MCP App GUI panel**, **lifecycle wake hooks**, and **standalone 4-column TUI** (`onlyne-agy`).

## Features

- **Single Agent-Facing Entry**: Exposes `msg({ delivery_kind, msg, meta })` interpreted by SystemOne (`JudgeDecider`), plus read-only views via `onlyne_read` (`onlyne://` URLs).
- **Antigravity MCP App GUI (`onlyne_panel`)**: Renders an interactive 4-column Discord/Onlyne-style web panel (`ui://onlyne/panel`) inside Antigravity (`aux_pane` & `global`) and over direct authenticated loopback HTTP+SSE (`/ui?token=...`).
- **Standalone Terminal UI (`onlyne-agy`)**: Full keyboard-driven 4-column TUI (`Domains | Sidebar | Flow | Peers`) with compact `< 96 col` responsive tabs, bilingual (`en` / `zh-CN`) UI, interactive fingerprint confirmation modals, and an 11-field `.onlyne/config.toml` setup wizard (`onlyne-agy init`).
- **Lifecycle Hooks & Wake Correlation**: Registers `SessionStart`, `PreInvocation`, `PostInvocation`, `PostToolUse`, and `Stop` hooks to inject inbound Onlyne messages via `[[onlyne:<token>]]` and confirm transcript consumption deterministically. Safe zero-spawn no-op in workspaces without `.onlyne/config.toml` or a running daemon.
- **Subagent Isolation**: `ActionGuard` enforces allocated context slices for Antigravity subagent conversations.

## Build & Install

From the repository root:

```bash
bun install --offline
bun run --filter '@onlyne/agy-onlyne' build
```

Validate the bundled Antigravity plugin:

```bash
agy plugin validate --strict --json packages/agy-onlyne/plugin
```

Install CLI shims (`onlyne-agy`, `onlyne-agy-mcp`, `onlyne-agy-hook`) into `~/.bun/bin`:

```bash
bun run devinstall
```

Install the local plugin in Antigravity:

```bash
agy plugin install ./packages/agy-onlyne/plugin
```

## Quick Usage

1. Initialize workspace node configuration interactively:
   ```bash
   onlyne-agy init --cwd /path/to/workspace
   ```
2. Open the standalone TUI or Web GUI:
   ```bash
   onlyne-agy --cwd /path/to/workspace
   onlyne-agy gui --open --cwd /path/to/workspace
   ```
3. Inside Antigravity, ask the assistant to check `onlyne://` or open `onlyne_panel`.

## Verification

```bash
bun run --filter '@onlyne/agy-onlyne' check
bun run --filter '@onlyne/agy-onlyne' test
```
