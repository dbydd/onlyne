// onlyne-sessions — Orca plugin worker (pluginApi v1, EXPERIMENTAL).
//
// Read-only supervisor board for Onlyne, built from two independent axes:
//   tabs  — the Orca tabs of the panes the connected sessions report running in
//   roots — each configured onlyne server root's admin surface
//           (`<root>/.onlyne/run/s`, queried through `sessions` + `roles`)
// joined by the weak `onlyne:<task_id>` title convention, and exposed as a
// board (notifications + plugin log pane + command results) plus four commands:
// push the board, rescan, focus a tab, and read the agent context of a tab.
//
// Orca is the supervisor's management port only: the backend no longer
// registers one worktree per role, so every session tab lands flat in the host
// worktree's list, and session identity lives in the adapter/pi plugin
// protocol — never in this board. A session's own process names its Orca pane
// there (`observed.host.orca.pane_key`), which is the whole authority for the
// tab axis: no pane reported, no tab listed.
//
// Boundaries this plugin deliberately keeps:
//   * it never creates, closes, or renames a tab — lifecycle belongs to the
//     onlyne backend;
//   * it never writes an onlyne file, and never writes a content-addressed
//     install; in a dev tree it rewrites exactly one file of its own, panel.html,
//     because that document is the only channel into the panel
//     (src/panel-document.mjs);
//   * the only mutating Orca call it ever makes is `orca terminal switch`, and
//     only while the user runs the focus command.

import { dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { createRunner, resolveBinaries } from "./src/runner.mjs";
import { createOrcaCli } from "./src/orca-cli.mjs";
import { createOnlyneCli } from "./src/onlyne-cli.mjs";
import { collectBoard } from "./src/board.mjs";
import { createBoardState } from "./src/board-state.mjs";
import { createCommands } from "./src/commands.mjs";
import { createPanelPublisher } from "./src/panel-document.mjs";


/** The plugin root: this file's own directory, in a dev tree and an install. */
export const PLUGIN_ROOT = dirname(fileURLToPath(import.meta.url));

export const COMMAND_IDS = [
  "onlyne-sessions.refresh",
  "onlyne-sessions.board",
  "onlyne-sessions.debug-board",
  "onlyne-sessions.focus",
  "onlyne-sessions.copy-agent-context",
];

export const SUBSCRIBED_EVENTS = ["worktree.created", "worktree.removed", "agent.status.changed"];

const NOTIFICATION_TITLE_LIMIT = 120;
const NOTIFICATION_BODY_LIMIT = 1000;
const LOG_LINE_LIMIT = 2000;

export function createPlugin({
  orca,
  runner = createRunner(),
  binaries = resolveBinaries(),
  pluginRoot = PLUGIN_ROOT,
} = {}) {
  const granted = new Set(orca?.grantedCapabilities ?? []);
  const log = (message) => {
    try {
      orca?.log?.(String(message).slice(0, LOG_LINE_LIMIT));
    } catch {
      /* logging must never break a scan */
    }
  };

  const notify = async (title, body) => {
    if (!granted.has("notifications:show")) {
      log(`notification suppressed (capability not granted): ${title}`);
      return { delivered: false };
    }
    try {
      return await orca.host.call("notifications.show", {
        title: String(title).slice(0, NOTIFICATION_TITLE_LIMIT),
        body: String(body ?? "").slice(0, NOTIFICATION_BODY_LIMIT),
      });
    } catch (error) {
      log(`notification failed: ${error?.message ?? error}`);
      return { delivered: false };
    }
  };

  const orcaCli = createOrcaCli({ runner, binary: binaries.orcaBin });
  const onlyneCli = createOnlyneCli({ runner, binary: binaries.onlyneBin });
  const serverRoots = binaries.serverRoots ?? [];

  // The panel reads this file, not this worker: see src/panel-document.mjs for

  const panel = createPanelPublisher({ rootDir: pluginRoot, log });
  let installedTreeLogged = false;

  const boardState = createBoardState({
    collect: (options) => collectBoard({ orca: orcaCli, onlyne: onlyneCli, serverRoots, ...options }),
    notify,
    log,
  });
  boardState.onBoard((board) => {
    const result = panel.publish(board);
    if (result.written) {
      log(`panel document updated (${result.reason}, ${result.bytes} B) → ${result.path}`);
      return;
    }
    if (result.reason === "installed-tree" && !installedTreeLogged) {
      installedTreeLogged = true;
      log(
        "content-addressed install: the panel document is not regenerated, so the panel keeps the " +
          "snapshot it was installed with; the board still goes to notifications and this log"
      );
    }
  });

  const commands = createCommands({
    getBoard: boardState.getBoard,
    refreshBoard: boardState.refresh,
    orca: orcaCli,
    notify,
    log,
    panelInfo: () => ({ target: panel.target }),
  });

  orca.commands.register("onlyne-sessions.refresh", (args) => commands.refresh(args ?? {}));
  orca.commands.register("onlyne-sessions.board", (args) => commands.board(args ?? {}));
  orca.commands.register("onlyne-sessions.debug-board", (args) => commands.debugBoard(args ?? {}));
  orca.commands.register("onlyne-sessions.focus", (args) => commands.focus(args ?? {}));
  orca.commands.register("onlyne-sessions.copy-agent-context", (args) =>
    commands.copyAgentContext(args ?? {})
  );

  const subscribed = granted.has("events:subscribe");
  if (subscribed) {
    orca.events.on("worktree.created", (payload) => {
      log(`worktree created: ${payload?.path ?? payload?.worktreeId ?? "unknown"}`);
      boardState.scheduleRefresh({ reason: "worktree.created" });
    });
    orca.events.on("worktree.removed", (payload) => {
      log(`worktree removed: ${payload?.path ?? payload?.worktreeId ?? "unknown"}`);
      boardState.scheduleRefresh({ reason: "worktree.removed" });
    });
    orca.events.on("agent.status.changed", (payload) => {
      if (payload?.state) log(`agent status: ${payload.state} in ${payload.worktreeId ?? "?"}`);
      boardState.scheduleRefresh({ reason: "agent.status.changed" });
    });
  } else {
    log("events:subscribe not granted — event-driven rescans disabled (commands still work)");
  }

  boardState.start();
  log(
    `onlyne-sessions active · orca=${binaries.orcaBin} · onlyne=${binaries.onlyneBin}` +
      ` · roots=${serverRoots.length}` +
      ` · panel=${panel.target ?? "(content-addressed install: not regenerated)"}` +
      (binaries.configLoaded ? ` · config=${binaries.configPath}` : "")
  );
  if (binaries.configError) log(`config ignored: ${binaries.configError}`);

  return { boardState, commands, notify, subscribed, panel };
}

let active = null;

export default function activate(orca) {
  active = createPlugin({ orca });
  return {
    commands: COMMAND_IDS,
    subscribedEvents: active.subscribed ? SUBSCRIBED_EVENTS : [],
  };
}

/** The worker calls this on shutdown; the fallback cadence must not outlive it. */
export function deactivate() {
  active?.boardState.stop();
  active = null;
}
