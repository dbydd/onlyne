# Onlyne Desktop Bot

Onlyne Desktop Bot runs persistent bots and group conversations in a native Electron window. OMP RPC runs the models and tools. The application embeds Onlyne nodes and handles pairing, messages and resources without a separate Onlyne installation.

## Run

Install the workspace dependencies and use an existing OMP installation with configured models.

```sh
bun install
bun run desktop
```

Choose a data directory for a separate bot team.

```sh
bun run desktop --data-dir /path/to/team
```

The default directory is `~/Library/Application Support/Onlyne Desktop/node` on macOS, `%LOCALAPPDATA%/Onlyne Desktop/node` on Windows, and `$XDG_DATA_HOME/onlyne/desktop` on Linux. Linux uses `~/.local/share` when `XDG_DATA_HOME` is unset.

The backend can run without the native window.

```sh
bun packages/desktop-bot/src/main.ts --serve --data-dir /path/to/team --port 0
```

The backend emits a private `desktop.ready` handshake with a one-use launch URL. The native launcher consumes this URL. Each backend holds an exclusive lock on its data directory.

## A bot owns its configuration

Each bot owns `bots/<bot-id>/.omp/` under the team directory. Its private chat and group sessions share this configuration. Each bot and conversation pair has a separate OMP session directory.

```text
team/
  state.json
  bots/<bot-id>/
    .omp/
      bot.json
      config.yml
      mcp.json
      extensions/
      skills/
      memory/MEMORY.md
      onlyne/
  sessions/<bot-id>/<conversation-id>/
```

`bot.json` holds the bot name, instructions, workspace, model, thinking level, selected capabilities, runtime options, host and renderer plugins, and Onlyne identity. `memory/MEMORY.md` holds personal memory. Native OMP extensions keep their own configuration beside these files. The desktop host preserves those extension files.

The bot itself lives in `bots/<bot-id>/`: that directory is its portable identity and configuration. Its default writable workspace is `workspaces/<bot-id>/`; a bot can point its `workspace` elsewhere, for example at a project repository.

The application stores the bot catalog, conversation membership and displayed messages in `state.json`. Application settings control the window interface and common appearance. Bot settings control runtime behavior.

`bot.reload` reloads the bot configuration through the controller API. Profile changes mark existing sessions for renewal after their current work settles. The next dispatch uses the updated profile.

One bot is the team administrator. `state.json` records its id in `teamAdminId`, and a legacy state without that field promotes the first bot. The administrator creates and edits other bots, creates groups, and configures them; every bot may still edit itself and manage groups it administers. The Members panel transfers the role with `team.admin`; the current administrator cannot be deleted until the role moves.

Each bot has one stable Onlyne node, key and protocol store. The desktop host owns these nodes. RPC sessions use the host's `msg` tool. A bot's conversations do not create competing network listeners or reuse another bot's key.

## Models, capabilities and memory

OMP supplies authentication and model discovery. Selecting a bot model changes that bot's session. The desktop host does not change OMP's global model defaults.

Capabilities attach MCP servers, skills and OMP extensions to a bot. The bot's `.omp/` directory is the configuration boundary. Runtime command arguments and environment overrides are available for custom RPC launchers. Environment values remain host configuration and do not belong in messages.

The three memory layers have separate purposes.

| Layer | Data | Use |
|---|---|---|
| Conversation | OMP sessions and shared group transcript | Continue work, inspect history and branch a conversation |
| Personal and group memory | The bot's Markdown memory and the group's memory field | Keep instructions and conclusions that later turns need |
| Knowledge library | An optional OMP memory extension such as Hindsight | Recall and retain knowledge across conversations |

The installed Hindsight extension uses `.omp/omp-hindsight.json`. A bot can select its own bank and disable inherited banks. Hindsight runs through its native OMP extension and tool. Its configuration reads once at session startup, so changing banks requires renewing that bot's runtime.

## Extend the host

Host plugins export a `DesktopPluginFactory` as their default export. The factory creates one instance per bot. A bot lists plugin paths in `plugins.host` in its `bot.json`. Relative paths resolve from that bot's `.omp/` directory.

```ts
import type { DesktopPluginFactory } from '@onlyne/desktop-bot/contracts';

export default (() => ({
  id: 'project-tools',
  setup(api) {
    api.registerTool({
      name: 'project_status',
      label: 'Project status',
      description: 'Read the configured project status.',
      parameters: { type: 'object', properties: {} },
      async execute(_args, context) {
        return { content: [{ type: 'text', text: context.bot.workspace }] };
      },
    });
  },
})) satisfies DesktopPluginFactory;
```

The host API registers runtime drivers, tools and URI schemes. A runtime driver supplies `connect(context)`, then `request(frame)`, `send(frame)` and `close()`. Each connection owns its request IDs. Host tool and URI calls carry the bot, conversation, abort signal and progress callback.

Native OMP extensions run inside OMP. Desktop host plugins run in the Bun host. Renderer plugins run in the isolated web interface. These contracts have different responsibilities and different module APIs.

## Characters and scenes

A bot stores its roleplay material in `bot.json` beside the OMP configuration. `character` holds a SillyTavern-compatible card: description, personality, scenario, system prompt, example dialogue, first message, alternate greetings and post-history instructions. `lorebook` holds world-info entries with keys, secondary keys, order, depth and a constant flag.

Import a SillyTavern V2/V3 JSON card or a PNG with an embedded `chara` or `ccv3` chunk with `character.import`. Export the current card as JSON with `character.export`.

A conversation carries the scene: user persona, author's note, world-info budget and scan depth, history limit, and group turn mode. `harness.preview` compiles the next turn without calling a model and returns the prompt, matched lore entries, block list and token estimate. `conversation.greet` shows a chosen greeting and marks the scene as opened; `message.regenerate` replaces the last reply from one bot.

A group scene can advance on its own: `group.auto` with `maxAutoTurns` makes the next member answer after a settled reply, in `group.order` or member order. Any message from a person resets the count; stopping a runtime, a prompt error or the limit pauses the chain until the next message. The turn counter lives in memory, so restarting the application never resumes an unattended conversation.

Media is first class. `media.upload` stores a file and `message.send` accepts `mediaIds`, so a message can be an image, GIF, video, audio clip or sticker with no text. The bot's `show_media` tool displays a local file, inline SVG, a sticker from `.omp/stickers`, or an http(s) URL; generated pictures stay in the workspace and are copied into the media store when shown. `/api/media/<id>` serves files to the session-cookie-authenticated renderer with Range support, and the imported character PNG becomes the bot avatar.

Identifiers that a model may read back are short: media `m-…`, conversations `c-…`, bots `bot-…`, and node ids such as `urn:onlyne:agent:desktop:mira-z1uww3ag`. Long UUIDs are not used in URNs.

## Extend the interface

The renderer exposes `window.onlyne`. Its API registers widget protocols, panels, message renderers and themes. It also exposes the active bot and conversation, current state, events and controller commands.

Widget protocol registration returns a disposer. Removing an override restores the previous renderer. A protocol without a renderer displays its payload and an actionable error.

## RPC dialogs and widgets

The default driver starts `omp --mode rpc-ui`, negotiates transport version 2, enables the batched `ask` dialog and requests subagent events. The renderer handles every current serialized OMP UI method.

| Method | Behavior |
|---|---|
| `select` | Options with descriptions and exact-label responses |
| `confirm` | Explicit confirmation or rejection |
| `input` | Text input, including the masked-input hint |
| `editor` | Multiline editing with initial content |
| `ask` | Multiple questions, recommended options, multiple selection and free text |
| `cancel` | Close the matching pending request |
| `notify` | Display a notification |
| `setStatus` | Update or remove a keyed status |
| `setWidget` | Update or remove keyed text lines above or below the editor |
| `setTitle` | Set the conversation title surface |
| `set_editor_text` | Update the active conversation's draft |
| `open_url` | Show a clickable authorization link and copy action |

A dialog response stays bound to its originating bot, conversation and request. Startup dialogs can be answered before ordinary RPC initialization completes. A prompt acknowledgement reports admission. `prompt_result` reports that prompt's outcome. `session_settled` reports that background work and queued turns have settled.

OMP terminal component factories have no serialized RPC representation. The `show_view` host tool supplies a rich protocol for desktop widgets.

```json
{
  "protocol": "onlyne.view/v1",
  "title": "Choose a storage engine",
  "await_reply": true,
  "view": {
    "kind": "choices",
    "options": [
      { "label": "SQLite", "value": "sqlite" },
      { "label": "PostgreSQL", "value": "postgres" }
    ]
  }
}
```

The built-in rich protocol supports `text`, `markdown`, `metrics`, `table`, `chart`, `progress`, `choices`, `confirm`, `form`, `html` and `stack`. MIME adapters support `text/plain`, `text/markdown` and `text/html`. HTML cards run in a sandbox without same-origin access or network access. They answer through `onlyneWidget.submit(value)` or `onlyneWidget.cancel()`.

## Controller API

`createDesktopApp({ dataDir })` returns a controller with `snapshot`, `subscribe`, `command` and `close`. `startDesktopServer({ controller })` serves the authenticated loopback interface. Embedders can supply a store, runtime drivers, host plugins or an Onlyne bridge factory.

The controller groups commands by domain: `bot.*`, `conversation.*`, `message.send`, `runtime.*`, `desktop.health`, `desktop.read`, `team.admin`, `capability.*`, `memory.save`, `settings.save`, `widget.respond` and `onlyne.*`. `runtime.health` reports settled state, pending host calls and the last prompt error per bot; `runtime.restart` drops a runtime so the next message starts clean. `desktop.read` reads the same `desktop://` resources a bot sees. RPC command access remains available for OMP features and extensions that add their own workflows.

## References

[DS-Bot](https://github.com/Fei2411/DS-Bot) supplied the desktop bot, group and runtime-wrapper reference. This package implements its own TypeScript host and renderer. The Grok bot package supplied the existing Onlyne host and stable-conversation examples.
