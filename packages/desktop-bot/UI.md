# Desk panels

The renderer exposes `window.onlyne`. A third-party script can add a panel without editing the shell. Register the panel, and register a workspace only when it should occupy the rail.

```ts
const panel = window.onlyne.registerPanel('weather', 'Weather', (state) => {
  const root = document.createElement('div');
  root.className = 'desk-page';
  root.textContent = `${state.bots.length} bots`;
  return root;
});

const place = window.onlyne.registerWorkspace({
  id: 'weather',
  title: 'Weather',
  hint: 'Local forecast',
  shortcut: 'w',
  group: 'studio',
});
```

`group` is `primary`, `network`, `studio` or `system`. Studio places stay in the library and the command palette. The other groups are rail icons unless the operator hides them. `shortcut` is the key after `G`.

`window.onlyne.layout` reads and writes the saved rail order, hidden ids and dock:

```ts
window.onlyne.layout.update((layout) => ({ ...layout, dock: 'weather' }));
window.onlyne.layout.reset();
```

The layout is stored in `localStorage` under `onlyne.desk.v1`. Resetting it does not touch the team.

Themes still use `registerTheme(id, css)` and `setTheme(id)`. Custom CSS in Settings is an escape hatch for a single desk, not a plugin. A theme should set the tokens `--bg`, `--surface`, `--fg`, `--muted`, `--accent`, `--line`, `--font` and the matching `--onlyne-*` aliases. Built-in themes are `dark`, `light` and `system`.

Message renderers and widget protocols are unchanged: `registerMessageRenderer` and `widgets.register`. Host plugins, which run in Bun and register tools, stay on `DesktopPluginFactory` as described in the package README.

## Places

| Place | How to open | What it reads |
|---|---|---|
| Conversations | Rail, `G C` | Team messages, composer, members, runtime |
| Inbox | Rail, `G I` | `desktop.read` `onlyne://inbox` |
| Neighbors | Rail, `G P` | `onlyne.status`, `onlyne.action` `connect` |
| Trust | Rail, `G T` | Pending fingerprints. Approval sends the typed value |
| Threads | Rail, `G D` | Owned and joined threads, `thread.read` |
| Resources | Rail, `G R` | `resource.list` |
| Health | Rail, `G H` | `desktop.health`, `runtime.health`, node status |
| Integrations | Rail, `G X` | Grok Bot threads (`grokbot:`) and MCP resources |
| Team | Rail, `G M` | Roster, `team.admin`, capability library |
| Settings | Rail, `G S` | Theme, layout, plugins |
| Library | Side column | Character, session, subagents, goal, queue, configuration, capabilities, activity, node record |

The node record is the earlier raw status view. The rail screens are the ones meant for daily use.
