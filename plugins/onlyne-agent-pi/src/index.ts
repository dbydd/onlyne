// pi-onlyne — the onlyne agent adapter for pi.
//
// The factory registers event handlers and starts nothing: pi loads extensions
// in invocations that never open a session, and this plugin must stay inert
// outside an onlyne-spawned session anyway. The session identity arrives in the
// environment (`ONLYNE_ROLE`, `ONLYNE_SESSION_ID`, `ONLYNE_TASK_ID`, injected by
// `crates/onlyne-client/src/dispatch.rs`); with any of the three missing this is
// a plain pi session and the extension stays silent rather than failing.
//
//   session_start      -> bind the env session id, read env + .pi/onlyne.json,
//                         connect, register tools; a resident plugin that is
//                         already connected only follows the switch
//   session_switch     -> /new, /resume and /fork on a host that names them;
//   session_branch       the same re-point a resident plugin does when current
//   session_tree         pi reports the switch through session_start, and
//                        after tree navigation
//   before_agent_start -> the role prose becomes one section of the system
//                         prompt the run is about to send (the instruction layer)
//   turn_start         -> heartbeat{running}
//   turn_end           -> one turn of a run ended; the phase is re-derived from pi
//   message_end        -> keep the last assistant text as the head fallback
//   agent_settled      -> heartbeat{idle} when the session waits for input
//   session_shutdown   -> detach{reason}
//
// The plugin keeps exactly one completion path: the model's explicit
// `onlyne_complete` (and `recycle{outcome}`). A turn that ends without one
// decides nothing here — the client's own turn-end rule owns it
// (`docs/v2-CONTRACT.md` §3c: one nudge, then the delivery settles `blocked`)
// and its sweeps own a session whose agent died. A turn error only supplies the
// head fallback, because pi retries a retryable provider error and stays inside
// the same run, and the client's nudge can restart a session a moment after it
// settles — a report sent on a turn error would race work that is still going.
//
// Host frames are dispatched in `agent.mjs`, not here: `assign` and `nudge` are
// injected as user messages, `probe` is answered with a heartbeat, and `recycle`
// settles the task and stops the plugin.
//
// Only the session whose id equals `ONLYNE_SESSION_ID` mounts: a pi process runs
// a subagent sidechain and `/new` sessions too, all inheriting this extension's
// environment, and a sidechain that called `onlyne_complete` would settle the
// parent's task. `session-scope.mjs` carries the rule, the guard behind it, and
// what a call from a foreign session is told.

import { defineTool, type ExtensionAPI, type ExtensionContext } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";

import { OnlyneAgent } from "./agent.mjs";
import { ASSIGN_MESSAGE_TYPE, createAssignMessageComponent } from "./activity.mjs";
import { loadConfig, residentIdentity, sessionIdentity } from "./config.mjs";
import { resolveResidentSocketPath, resolveSocketPath } from "./socket.mjs";
import { createSurface } from "./pi-surface.mjs";
import { foreignSessionRefusal, mountsSession, ownsSession, sessionIdOf } from "./session-scope.mjs";

/**
 * Node's process globals, declared here so the package needs no `@types/node`
 * and no unchecked cast to reach them.
 */
declare const process: {
  env: Record<string, string | undefined>;
  pid: number;
  stderr: { write(chunk: string): void };
};

/** One image part handed to the pi message surface. */
interface ImagePartInput {
  mime: string;
  data: string;
  name?: string | null;
}

/** What a `welcome` carries, as the surface needs it. */
interface WelcomeLike {
  role: string;
}

/**
 * The effect surface the agent drives (`pi-surface.mjs`): the contract between
 * the protocol core and pi's API.
 */
interface PiSurface {
  available: {
    wakeUser: boolean;
    customEntry: boolean;
    widget: boolean;
    status: boolean;
    exit: boolean;
    isIdle: boolean;
    registerTool: boolean;
    registerCommand: boolean;
  };
  wakeUser(text: string, parts?: ImagePartInput[], meta?: unknown): boolean;
  roleProse(text: string): boolean;
  applyRoleProse(event: { systemPromptOptions?: { sections?: Record<string, string> } }): boolean;
  customEntry(customType: string, data: unknown): boolean;
  widget(lines: string[] | undefined, activity?: unknown): void;
  status(text: string): void;
  welcome(welcome: WelcomeLike): void;
  isIdle(): boolean;
  /** The phase rule: true only while the session waits for user input. */
  waitingForInput(): Promise<boolean>;
  /** Drops the background-task probe's EventBus subscription. */
  closeBackground?(): void;
  exit(reason: string): void;
}

/**
 * Last assistant text inside one message.
 *
 * Read field by field rather than by type assertion: pi's `AgentMessage` union
 * also admits extension-defined custom messages, so nothing about the shape is
 * guaranteed until `role` says `assistant`.
 */
function assistantTextOf(message: unknown): string {
  if (!message || typeof message !== "object") return "";
  if (!("role" in message) || message.role !== "assistant") return "";
  if (!("content" in message)) return "";
  const content = message.content;
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  const parts: string[] = [];
  for (const part of content) {
    if (!part || typeof part !== "object") continue;
    if (!("type" in part) || part.type !== "text") continue;
    if (!("text" in part) || typeof part.text !== "string") continue;
    parts.push(part.text);
  }
  return parts.join("\n");
}

/** One line of a failed assistant message, when it carries one. */
function failureOf(message: unknown): string | null {
  if (!message || typeof message !== "object") return null;
  if (!("stopReason" in message) || message.stopReason !== "error") return null;
  if ("errorMessage" in message && typeof message.errorMessage === "string" && message.errorMessage) {
    return message.errorMessage;
  }
  return "turn failed";
}

/**
 * Result shape every tool returns to pi. A named helper because three call
 * sites must produce the identical envelope.
 */
const textResult = (text: string, details?: unknown) => ({
  content: [{ type: "text" as const, text }],
  details,
});

export default function onlyne(pi: ExtensionAPI) {
  const env: Record<string, string | undefined> = typeof process === "undefined" ? {} : process.env;
  let context: ExtensionContext | null = null;
  let agent: OnlyneAgent | null = null;
  let surface: PiSurface | null = null;
  let registered = false;
  /**
   * The session id that mounted this plugin. A pi process can host more than
   * one session — a `task` subagent runs a sidechain in the same process and
   * inherits `ONLYNE_*`, and `/new` starts another — and every one of them
   * loads this extension. The env names the one session that is the onlyne
   * session, so the tools belong to it alone; an unbound session must not be
   * able to settle this task by calling `onlyne_complete` from its sidechain.
   */
  let boundSessionId: string | null = null;

  const log = (line: string, data?: unknown) => {
    const detail = data === undefined ? "" : ` ${JSON.stringify(data)}`;
    process?.stderr?.write(`[pi-onlyne] ${line}${detail}\n`);
  };

  /**
   * Guard one tool call: the caller must be the session that mounted this
   * plugin (`session-scope.mjs` carries the rule and the reason).
   */
  const refuseForeignSession = (caller: string | null): never => {
    throw new Error(foreignSessionRefusal(boundSessionId, caller));
  };

  /** Tools and the `/onlyne` command exist only inside an onlyne session. */
  const registerSurface = () => {
    if (registered || !surface) return;
    registered = true;
    if ("registerMessageRenderer" in pi && typeof pi.registerMessageRenderer === "function") {
      try {
        pi.registerMessageRenderer(ASSIGN_MESSAGE_TYPE, (message, options, theme) => {
          if (!message?.details || typeof message.details !== "object") return undefined;
          return createAssignMessageComponent(message.details, message.content, theme, !options?.expanded);
        });
      } catch (error) {
        log(`registerMessageRenderer(${ASSIGN_MESSAGE_TYPE}) refused: ${error instanceof Error ? error.message : String(error)}`);
      }
    }
    try {
      pi.registerTool(defineTool({
        name: "onlyne_send",
        label: "Onlyne send",
        description:
          "Send one message to another role. kind=note (default) is free text; kind=task hands work to that role and opens a task for it.",
        promptSnippet: "Send a note or a task to another role",
        promptGuidelines: [
          "Use onlyne_send when something has to reach another role; the call returns once the message is queued.",
        ],
        parameters: Type.Object({
          to: Type.String({ description: "target role name, e.g. builder" }),
          text: Type.String({ description: "message body" }),
          kind: Type.Optional(Type.String({ description: '"note" (default) or "task"' })),
          image: Type.Optional(Type.String({ description: "absolute path to a png/jpeg/gif/webp image to attach" })),
        }),
        async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
          const caller = sessionIdOf(ctx?.sessionManager);
          if (!ownsSession(boundSessionId, caller)) refuseForeignSession(caller);
          if (!agent) throw new Error("onlyne: session is not connected");
          const result = await agent.sendFromTool({
            to: params.to,
            text: params.text,
            kind: params.kind,
            imagePath: params.image ?? null,
          });
          // The recipient and nothing else: a tool result is model-visible, and
          // there is no fact about this send the model needs beyond where it went.
          return textResult(`sent to ${result.to}`, { to: result.to });
        },
      }));
    } catch (error) {
      log(`registerTool(onlyne_send) refused: ${error instanceof Error ? error.message : String(error)}`);
    }
    try {
      pi.registerTool(defineTool({
        name: "onlyne_complete",
        label: "Onlyne complete",
        description:
          "End the current task with an explicit outcome: done (the work is finished), failed (it is provably impossible), cancelled (it was withdrawn), or blocked (something outside this session stops it). summary is the one-line result and details is the full one; files names the paths the result rests on. If the workspace requires a handoff before the task may end, the call is refused until that handoff has gone out.",
        promptSnippet: "Finish the current task with an outcome and a one-line summary",
        promptGuidelines: [
          "Use onlyne_complete at the end of the current task, naming the outcome and the result in one line.",
        ],
        parameters: Type.Object({
          outcome: Type.String({ description: '"done", "failed", "cancelled", or "blocked"' }),
          summary: Type.String({ description: "one-line result summary" }),
          details: Type.Optional(Type.String({ description: "the full result, delivered as it stands" })),
          files: Type.Optional(Type.Array(Type.String(), { description: "absolute paths of the files the result names" })),
        }),
        async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
          const caller = sessionIdOf(ctx?.sessionManager);
          if (!ownsSession(boundSessionId, caller)) refuseForeignSession(caller);
          if (!agent) throw new Error("onlyne: session is not connected");
          // The exit is not a tool-result flag: pi 0.85.1 has no tool-result
          // `terminate` handling. `agent.complete` asks the surface to shut the
          // process down once the client has acknowledged the report. A refusal
          // from the client throws out of here as a tool error, so the model
          // reads the host's own sentence.
          const result = await agent.completeFromTool({
            outcome: params.outcome,
            summary: params.summary,
            details: params.details,
            files: params.files,
          });
          // The client that had already settled this task answers with its own
          // record, and the sentence says so rather than claiming a completion
          // this call did not cause.
          if (result.alreadySettled) {
            const settled = result.alreadySettled;
            const head = settled.head ? `: ${settled.head}` : "";
            return textResult(
              `task already settled as ${settled.state} at ${settled.settled_at}${head} — this call changed nothing`,
              { outcome: result.outcome, alreadySettled: settled }
            );
          }
          // The outcome and nothing else: the ledger's head stays a display
          // field (docs/v2-CONTRACT.md §3c), and the task's identity is not a
          // fact the model is meant to hold.
          return textResult(`reported ${result.outcome}`, { outcome: result.outcome });
        },
      }));
    } catch (error) {
      log(`registerTool(onlyne_complete) refused: ${error instanceof Error ? error.message : String(error)}`);
    }
    try {
      pi.registerTool(defineTool({
        name: "onlyne_handoff",
        label: "Onlyne handoff",
        description:
          "Hand the current task on to another role, which continues it. Call it when this task's work goes on to another role.",
        promptSnippet: "Hand the current task on to another role",
        promptGuidelines: [
          "Use onlyne_handoff when this task's work goes on to another role; the receiving role continues it.",
        ],
        parameters: Type.Object({
          to: Type.String({ description: "target role name, e.g. builder" }),
          text: Type.String({ description: "handoff text for the receiving role" }),
          image: Type.Optional(Type.String({ description: "absolute path to a png/jpeg/gif/webp image to attach" })),
        }),
        async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
          const caller = sessionIdOf(ctx?.sessionManager);
          if (!ownsSession(boundSessionId, caller)) refuseForeignSession(caller);
          if (!agent) throw new Error("onlyne: session is not connected");
          const result = await agent.handoffFromTool({
            to: params.to,
            text: params.text,
            imagePath: params.image ?? null,
          });
          // The recipient and nothing else: the child's id and the hop are the
          // host's bookkeeping, and a result naming them would teach the model
          // to read itself as one node of a numbered chain.
          return textResult(`handed on to ${result.to}`, { to: result.to });
        },
      }));
    } catch (error) {
      log(`registerTool(onlyne_handoff) refused: ${error instanceof Error ? error.message : String(error)}`);
    }
    try {
      pi.registerCommand("onlyne", {
        description: "Onlyne session status: connection, task, reports",
        handler: async (argLine, commandContext) => {
          // The command drives this plugin's connection, so it belongs to the
          // bound session for the same reason the tools do: a sidechain that
          // issued `/onlyne disconnect` would detach the real session.
          const caller = sessionIdOf(commandContext?.sessionManager);
          if (!ownsSession(boundSessionId, caller)) {
            commandContext.ui.notify(
              foreignSessionRefusal(boundSessionId, caller),
              "info"
            );
            return;
          }
          if (!agent) {
            commandContext.ui.notify("onlyne: no session (ONLYNE_* env is absent)", "info");
            return;
          }
          const verb = String(argLine ?? "").trim();
          if (verb === "disconnect") {
            agent.stop("operator");
            commandContext.ui.notify("onlyne: detached", "info");
            return;
          }
          if (verb === "connect") {
            agent.start();
            commandContext.ui.notify(`onlyne: connecting to ${agent.status().socket}`, "info");
            return;
          }
          commandContext.ui.notify(`onlyne ${JSON.stringify(agent.status())}`, "info");
        },
      });
    } catch (error) {
      log(`registerCommand refused: ${error instanceof Error ? error.message : String(error)}`);
    }
  };

  /**
   * Re-point the plugin at the session pi made current. The socket belongs to
   * the role, not to the session file that mounted it: /new, /resume, /fork and
   * a tree jump replace the live session in this process, and the tools must
   * follow it instead of refusing every call as foreign.
   */
  const syncSessionContext = (_event: unknown, ctx: ExtensionContext) => {
    if (!agent) return;
    context = ctx;
    const currentId = sessionIdOf(ctx?.sessionManager);
    if (currentId) {
      boundSessionId = currentId;
    }
    if (surface?.available.widget) {
      surface.widget(agent.activity.lines(), agent.activity);
    }
    surface?.status(agent.activity.lines()[0]);
  };

  pi.on("session_start", async (event, ctx) => {
    const spawned = sessionIdentity(env);
    const resident = spawned ? null : residentIdentity(env, ctx.cwd);
    const identity = spawned ?? resident;
    if (!identity) return; // not an onlyne session: stay out of the way
    // Only the session the client names may mount. A pi process runs more than
    // one session — a `task` subagent's sidechain, `/new` — and each of them
    // loads this extension with the same inherited environment, so without this
    // check a subagent's `onlyne_complete` settles the parent's task (observed:
    // librarian task cfa73386). An unknown id means an older pi that does not
    // expose one; the env-bound mount is then the only candidate and stays the
    // behaviour, with no line to explain.
    const starting = sessionIdOf(ctx.sessionManager);
    // A resident plugin already holds its connection, so another session_start
    // in this process is a re-point, not a mount: /new, /resume and /fork
    // replace the live session, and pi can re-bind this same session's
    // extensions. The sidechain a `task` subagent runs re-binds every extension
    // with reason "startup" and a different id, and stays inert.
    if (!spawned && agent !== null && (event.reason !== "startup" || starting === boundSessionId)) {
      syncSessionContext(event, ctx);
      return;
    }
    if (spawned) {
      if (!mountsSession(spawned.sessionId, starting)) {
        log(`session ${starting} is not the onlyne session ${spawned.sessionId}; staying inert`);
        return;
      }
    } else if (boundSessionId !== null && starting !== null && starting !== boundSessionId) {
      log(`session ${starting} is not the resident onlyne session ${boundSessionId}; staying inert`);
      return;
    }
    const wireSessionId = identity.sessionId ?? starting;
    context = ctx;
    boundSessionId = spawned ? spawned.sessionId : (starting ?? wireSessionId);
    const config = loadConfig(ctx.cwd);
    if (config.warning) log(config.warning);
    if (!config.enabled) {
      log(`disabled by ${config.path}`);
      return;
    }
    // The path the client injected, or the client the runtime directory's
    // registration files name for this workspace (socket.mjs). A session with
    // neither has no socket to dial, and saying so is the whole answer: this
    // stays a plain pi session instead of retrying a path nothing serves.
    let socketPath: string | null = null;
    const targetWorkspace = resident ? resident.workspaceRoot : ctx.cwd;
    try {
      socketPath = resident
        ? resolveResidentSocketPath(env, targetWorkspace, identity.role)
        : resolveSocketPath(env, ctx.cwd);
    } catch (error) {
      log(`socket unresolved: ${error instanceof Error ? error.message : String(error)}`);
    }
    if (socketPath === null) return;
    surface = createSurface({ pi, log, context: () => context, sessionId: () => boundSessionId });
    agent = new OnlyneAgent({
      socketPath,
      resolveSocket: resident
        ? () => resolveResidentSocketPath(env, targetWorkspace, identity.role)
        : undefined,
      cwd: ctx.cwd,
      role: identity.role,
      sessionId: wireSessionId,
      taskId: identity.taskId,
      resident: Boolean(resident),
      surface,
      log,
    });
    log(`session ${wireSessionId ?? "resident"} role=${identity.role} socket=${socketPath}`);
    registerSurface();
    if (!surface.available.wakeUser) {
      // Without an injection surface the payload travels through config_get,
      // and the hello capability list has to say so.
      log("no wakeUser surface: dropping the inject capability");
      agent.capabilities = agent.capabilities.filter((name) => name !== "inject");
    }
    if (config.autoStart) agent.start();
  });

  // `session_switch` and `session_branch` name the same transitions in the omp
  // lineage of this API; current pi reports them as `session_start` with a
  // reason and keeps `session_tree` for navigation, so one helper serves all.
  // The installed typings no longer declare the first two, but the host stores
  // handlers by name, which this narrow cast is the whole boundary of.
  const legacySessionEvents = pi as unknown as {
    on(event: "session_switch" | "session_branch", handler: (event: unknown, ctx: ExtensionContext) => void): void;
  };
  legacySessionEvents.on("session_switch", syncSessionContext);
  legacySessionEvents.on("session_branch", syncSessionContext);
  pi.on("session_tree", async (event, ctx) => syncSessionContext(event, ctx));

  // The role prose is instruction-layer text: `before_agent_start` hands the
  // handler the prompt options the run is about to render, and a section written
  // there is part of the system prompt rather than one more message the model has
  // to read as an utterance. Outside an onlyne session there is no surface and
  // nothing to add.
  pi.on("before_agent_start", async (event) => {
    surface?.applyRoleProse(event);
  });

  pi.on("turn_start", async () => {
    agent?.onTurnStart();
  });

  pi.on("turn_end", async (event) => {
    const failure = failureOf(event.message);
    if (failure) agent?.onTurnError(failure);
    else agent?.onTurnEnd();
  });

  pi.on("message_end", async (event) => {
    const failure = failureOf(event.message);
    if (failure) {
      agent?.onTurnError(failure);
      return;
    }
    agent?.noteAssistantText(assistantTextOf(event.message));
  });

  pi.on("agent_settled", async () => {
    agent?.onSettled();
  });

  pi.on("session_shutdown", async (event) => {
    agent?.stop(`pi:${event.reason ?? "quit"}`);
    surface?.closeBackground?.();
    surface?.widget?.(undefined);
    agent = null;
    surface = null;
    registered = false;
    context = null;
    boundSessionId = null;
  });
}
