// pi-onlyne — the onlyne agent adapter for pi.
//
// The factory registers event handlers and starts nothing: pi loads extensions
// in invocations that never open a session, and this plugin must stay inert
// outside an onlyne-spawned session anyway. The session identity arrives in the
// environment (`ONLYNE_ROLE`, `ONLYNE_SESSION_ID`, `ONLYNE_TASK_ID`, injected by
// `crates/onlyne-client/src/dispatch.rs`); with any of the three missing this is
// a plain pi session and the extension stays silent rather than failing.
//
//   session_start    -> read env + .pi/onlyne.json, connect, register tools
//   turn_start       -> heartbeat{running}
//   turn_end         -> heartbeat{idle}
//   message_end      -> keep the last assistant text; a failed turn is `failed`
//   agent_settled    -> completion exit: done|failed
//   session_shutdown -> detach{reason}

import { defineTool, type ExtensionAPI, type ExtensionContext } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";

import { OnlyneAgent } from "./agent.mjs";
import { loadConfig, sessionIdentity } from "./config.mjs";
import { loadRelay, relayEnabled } from "./relay.mjs";
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

/** Socket every role workspace serves; `crates/onlyne-client/src/adapter_socket.rs`. */
const SOCKET_RELATIVE_PATH = ".onlyne/run/s";

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
    proseContext: boolean;
    customEntry: boolean;
    status: boolean;
    exit: boolean;
    isIdle: boolean;
    registerTool: boolean;
    registerCommand: boolean;
  };
  wakeUser(text: string, parts?: ImagePartInput[]): boolean;
  proseContext(text: string, welcome: WelcomeLike): boolean;
  customEntry(customType: string, data: unknown): boolean;
  status(text: string): void;
  welcome(welcome: WelcomeLike): void;
  isIdle(): boolean;
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
          "Send one message to another role in this onlyne cluster. kind=note (default) is free text; kind=task hands work to the role and creates a session for it.",
        promptSnippet: "Send a note or a task to another onlyne role",
        promptGuidelines: [
          "Use onlyne_send when a task needs another onlyne role's work; it submits the envelope to the cluster and returns once the router has queued it.",
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
          return textResult(`queued ${result.kind} to ${result.to}`, result);
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
          "End this onlyne task with an explicit outcome. Call it once, when the assigned work is finished (outcome=done), provably impossible (outcome=failed), or withdrawn (outcome=cancelled). Without this call the session still completes on its own: done, or failed when the turn errored. In a workspace whose relay policy (relay.toml) names the handoffs this session owes, the call is refused until each one has gone out.",
        promptSnippet: "Finish the current onlyne task with an outcome and a one-line summary",
        promptGuidelines: [
          "Use onlyne_complete at the end of an onlyne task, naming the outcome and the result in one line; the summary becomes the ledger head.",
          "If onlyne_complete answers 'relay guard', the session still owes a downstream handoff: make it with onlyne_send and call onlyne_complete again. Close the session anyway only when the handoff is genuinely impossible, with force: true and a reason.",
        ],
        parameters: Type.Object({
          outcome: Type.Optional(Type.String({ description: '"done" (default), "failed", or "cancelled"' })),
          text: Type.Optional(Type.String({ description: "one-line result summary" })),
          force: Type.Optional(Type.Boolean({ description: "waive the relay guard; requires a non-empty reason" })),
          reason: Type.Optional(Type.String({ description: "why the relay guard is waived; stamped into the ledger head after `relay-guard-forced: `" })),
        }),
        async execute(_toolCallId, params) {
          if (!agent) throw new Error("onlyne: session is not connected");
          // The exit is not a tool-result flag: pi 0.85.1 has no tool-result
          // `terminate` handling. `agent.complete` asks the surface to shut the
          // process down once the client has acknowledged the report. A relay
          // refusal throws out of here as a tool error, which leaves the session
          // mounted for the handoff that clears it.
          const result = await agent.completeFromTool({
            outcome: params.outcome,
            text: params.text,
            force: params.force,
            reason: params.reason,
          });
          return textResult(`onlyne task ${result.taskId} -> ${result.outcome}`, result);
        },
      }));
    } catch (error) {
      log(`registerTool(onlyne_complete) refused: ${error instanceof Error ? error.message : String(error)}`);
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
    const socketPath = env.ONLYNE_SOCKET || `${ctx.cwd}/${SOCKET_RELATIVE_PATH}`;
    // The relay guard's policy travels with the plugin package rather than in
    // `.onlyne/config.toml`, which the client parses strictly (relay.mjs).
    const relay = loadRelay();
    if (relay.warning) log(relay.warning);
    if (relayEnabled(relay)) {
      log(
        `relay guard from ${relay.path}: required=${JSON.stringify(relay.required)} count=${relay.count ?? "-"}`,
      );
    }
    surface = createSurface({ pi, log, context: () => context });
    agent = new OnlyneAgent({
      socketPath,
      cwd: ctx.cwd,
      role: identity.role,
      sessionId: identity.sessionId,
      taskId: identity.taskId,
      surface,
      relay,
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
    agent = null;
    surface = null;
    registered = false;
    context = null;
  });
}
