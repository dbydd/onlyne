// onlyne-sessions — Orca plugin worker (pluginApi v1, EXPERIMENTAL).
//
// Read-only supervisor board for Onlyne, built from two independent axes:
//   tabs  — every Orca tab, from one flat `orca terminal list --json` call
//   roots — each configured onlyne server root's admin surface
//           (`<root>/.onlyne/run/s`, queried through `sessions` + `roles`)
// joined by the weak `onlyne:<task_id>` title convention, and exposed as a
// board (notifications + plugin log pane + command results) plus four commands:
// push the board, rescan, focus a tab, and read the agent context of a tab.
//
// Orca is the supervisor's management port only: the backend no longer
// registers one worktree per role, so every session tab lands flat in the host
// worktree's list, and session identity lives in the adapter/pi plugin
// protocol — never in this board.
//
// Boundaries this plugin deliberately keeps:
//   * it never creates, closes, or renames a tab — lifecycle belongs to the
//     onlyne backend;
//   * it never writes any onlyne file, and never writes inside its own
//     hash-addressed install directory (Orca verifies that tree per refresh);
//   * the only mutating call it ever makes is `orca terminal switch`, and only
//     while the user runs the focus command.

import { createRunner, resolveBinaries } from "./src/runner.mjs";
import { createOrcaCli } from "./src/orca-cli.mjs";
import { createOnlyneCli } from "./src/onlyne-cli.mjs";
import { collectBoard } from "./src/board.mjs";
import { createBoardState } from "./src/board-state.mjs";
import { createCommands } from "./src/commands.mjs";

export const COMMAND_IDS = [
  "onlyne-sessions.refresh",
  "onlyne-sessions.board",
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

  const boardState = createBoardState({
    collect: (options) => collectBoard({ orca: orcaCli, onlyne: onlyneCli, serverRoots, ...options }),
    notify,
    log,
  });

  const commands = createCommands({
    getBoard: boardState.getBoard,
    refreshBoard: boardState.refresh,
    orca: orcaCli,
    notify,
    log,
  });

  orca.commands.register("onlyne-sessions.refresh", (args) => commands.refresh(args ?? {}));
  orca.commands.register("onlyne-sessions.board", (args) => commands.board(args ?? {}));
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
      (binaries.configLoaded ? ` · config=${binaries.configPath}` : "")
  );
  if (binaries.configError) log(`config ignored: ${binaries.configError}`);

  return { boardState, commands, notify, subscribed };
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
