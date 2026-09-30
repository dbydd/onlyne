// pi-onlyne — the onlyne agent adapter for pi.
//
// The factory registers event handlers and starts nothing: pi loads extensions
// in invocations that never open a session, and this plugin must stay inert
// outside an onlyne-spawned session anyway. The session identity arrives in the
// environment (`ONLYNE_ROLE`, `ONLYNE_SESSION_ID`, `ONLYNE_TASK_ID`, injected by
// `crates/onlyne-client/src/dispatch.rs`); with any of the three missing this is
// a plain pi session and the extension stays silent rather than failing.
//
//   session_start      -> read env + .pi/onlyne.json, connect, register tools
//   before_agent_start -> the role prose becomes one section of the system
//                         prompt the run is about to send (the instruction layer)
//   turn_start         -> heartbeat{running}
//   turn_end           -> one turn of a run ended; the phase is re-derived from pi
//                         and the fallback window for a witnessed failure opens
//   message_end        -> keep the last assistant text; a failed turn is `failed`
//   agent_settled      -> heartbeat{idle} when the session waits for input, and the
//                         report a failed turn owes is sent from here
//   session_shutdown   -> detach{reason}
//
// Host frames are dispatched in `agent.mjs`, not here: `assign` and `nudge` are
// injected as user messages, `probe` is answered with a heartbeat, and `recycle`
// settles the task and stops the plugin.

import { defineTool, type ExtensionAPI, type ExtensionContext } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";

import { OnlyneAgent } from "./agent.mjs";
import { loadConfig, sessionIdentity } from "./config.mjs";
import { resolveSocketPath } from "./socket.mjs";
import { createSurface } from "./pi-surface.mjs";

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
  wakeUser(text: string, parts?: ImagePartInput[]): boolean;
  roleProse(text: string): boolean;
  applyRoleProse(event: { systemPromptOptions?: { sections?: Record<string, string> } }): boolean;
  customEntry(customType: string, data: unknown): boolean;
  widget(lines: string[] | undefined): void;
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

  const log = (line: string, data?: unknown) => {
    const detail = data === undefined ? "" : ` ${JSON.stringify(data)}`;
    process?.stderr?.write(`[pi-onlyne] ${line}${detail}\n`);
  };

  /** Tools and the `/onlyne` command exist only inside an onlyne session. */
  const registerSurface = () => {
    if (registered || !surface) return;
    registered = true;
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
        async execute(_toolCallId, params) {
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
        async execute(_toolCallId, params) {
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
        async execute(_toolCallId, params) {
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

  pi.on("session_start", async (_event, ctx) => {
    context = ctx;
    const identity = sessionIdentity(env);
    if (!identity) return; // not an onlyne session: stay out of the way
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
    try {
      socketPath = resolveSocketPath(env, ctx.cwd);
    } catch (error) {
      log(`socket unresolved: ${error instanceof Error ? error.message : String(error)}`);
    }
    if (socketPath === null) return;
    surface = createSurface({ pi, log, context: () => context, sessionId: identity.sessionId });
    agent = new OnlyneAgent({
      socketPath,
      cwd: ctx.cwd,
      role: identity.role,
      sessionId: identity.sessionId,
      taskId: identity.taskId,
      surface,
      log,
    });
    log(`session ${identity.sessionId} role=${identity.role} socket=${socketPath}`);
    registerSurface();
    if (!surface.available.wakeUser) {
      // Without an injection surface the payload travels through config_get,
      // and the hello capability list has to say so.
      log("no wakeUser surface: dropping the inject capability");
      agent.capabilities = agent.capabilities.filter((name) => name !== "inject");
    }
    if (config.autoStart) agent.start();
  });

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
  });
}
