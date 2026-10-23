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
//
// One process also holds at most one resident claim. A resident pi is started
// by hand rather than spawned by the client, so a `task` subagent's sidechain
// inherits nothing that names a task and reaches the resident branch through a
// second call of this factory with fresh closure state; the claim is what stops
// it from opening a connection that impersonates the parent's resident alias
// (`docs/v2.11-LIFECYCLE-IS-DECLARED-NOT-INFERRED.md` §4.2). Where the host
// itself declares what the session is — omp marks every subagent context
// `agent.kind === "sub"` — that declaration decides first, on both identity
// paths, and the claim is the reading left for a host that declares nothing.
//
import { defineTool, type ExtensionAPI, type ExtensionContext } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";

import { OnlyneAgent } from "./agent.mjs";
import { ASSIGN_MESSAGE_TYPE, createAssignMessageComponent } from "./activity.mjs";
import { loadConfig, residentIdentity, sessionIdentity } from "./config.mjs";
import { resolveResidentSocketPath, resolveSocketPath } from "./socket.mjs";
import { createSurface, PROSE_SECTION } from "./pi-surface.mjs";
import { isAbsolute, resolve } from "node:path";
import { handoffRole, mountLabel, resolveTarget, resolveTaskMount, soleActiveMount } from "./mounts.mjs";
import {
  declaredSubagent,
  foreignSessionRefusal,
  isSessionReplacement,
  mountsSession,
  ownsSession,
  sessionIdOf,
} from "./session-scope.mjs";

/** One client link this process holds, under its own role and cluster. */
interface Mount {
  agent: OnlyneAgent;
  cluster: string | null;
  role: string;
}

/** What one mount has to dial, before any connection exists. */
interface MountSpec {
  role: string;
  cluster: string | null;
  workspace: string;
  sessionId: string | null;
}

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
  applyRoleProse(event: { systemPromptOptions?: { sections?: Record<string, string> } }, fallback?: string): boolean;
  applyRoleProseSection(
    event: { systemPromptOptions?: { sections?: Record<string, string> } },
    key: string,
    text: string
  ): boolean;
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

/**
 * The session this process has already taken as its resident, or `null` while
 * no resident mount holds it.
 *
 * Module scope is the point: pi hosts a `task` subagent's sidechain in the same
 * process by loading this extension again, so a guard that read the factory's
 * own `boundSessionId` compared `null` against a name and let the child open a
 * connection for the parent's resident alias
 * (`docs/v2.11-LIFECYCLE-IS-DECLARED-NOT-INFERRED.md` §4.2).
 */
let residentClaim: string | null = null;

export default function onlyne(pi: ExtensionAPI) {
  const env: Record<string, string | undefined> = typeof process === "undefined" ? {} : process.env;
  let context: ExtensionContext | null = null;
  /**
   * One entry per client link this process holds. A resident plugin may serve
   * the same role in more than one cluster (`docs/v2.12-ONE-SESSION-TWO-
   * CLUSTERS.md`): every mount dials its own client, keeps its own session
   * row and task table, and shares the one pi surface with the rest.
   */
  let mounts: Mount[] = [];
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

  /** Identity of a mount across a re-mount, for carrying its prose over. */
  const mountKey = (cluster: string | null, role: string): string => `${cluster ?? ""}\u0000${role}`;

  /** End the process once no mount is left; a shared process's last word. */
  const exitWhenNoMountRemains = (reason: string) => {
    if (mounts.every((mount) => !mount.agent.connected)) surface?.exit?.(reason);
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
          to: Type.String({
            description:
              'target role, e.g. builder, or "cluster:role" when this session serves several clusters',
          }),
          text: Type.String({ description: "message body" }),
          kind: Type.Optional(Type.String({ description: '"note" (default) or "task"' })),
          image: Type.Optional(Type.String({ description: "absolute path to a png/jpeg/gif/webp image to attach" })),
        }),
        async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
          const caller = sessionIdOf(ctx?.sessionManager);
          if (!ownsSession(boundSessionId, caller)) refuseForeignSession(caller);
          if (mounts.length === 0) throw new Error("onlyne: session is not connected");
          const { mount, to } = resolveTarget(mounts, params.to);
          const result = await mount.agent.sendFromTool({
            to,
            text: params.text,
            kind: params.kind,
            imagePath: params.image ?? null,
          });
          // Delivery and local parking are different facts and the model is told
          // which one happened: a client with a dead link answers `queued`, and
          // calling that "sent" is how a message that never left looked
          // delivered (docs/v2.12-ONE-SESSION-TWO-CLUSTERS.md §7).
          const queued = Boolean(result.data && result.data.queued);
          const where = result.to;
          return queued
            ? textResult(`queued locally for ${where}; the client sends it when its link returns`, {
                to: where,
                queued: true,
              })
            : textResult(`sent to ${where}`, { to: where });
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
          cluster: Type.Optional(Type.String({
            description:
              "the cluster this task came from, when this session serves several; the task's own mount answers without it",
          })),
        }),
        async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
          const caller = sessionIdOf(ctx?.sessionManager);
          if (!ownsSession(boundSessionId, caller)) refuseForeignSession(caller);
          if (mounts.length === 0) throw new Error("onlyne: session is not connected");
          // The reply goes back where the work came from, never to "the first
          // mount": two clusters in one process means two ledgers, and settling
          // the wrong one leaves the real task open and the other falsely done.
          const mount = resolveTaskMount(mounts, params.cluster);
          // The exit is not a tool-result flag: pi 0.85.1 has no tool-result
          // `terminate` handling. `agent.complete` asks the surface to shut the
          // process down once the client has acknowledged the report. A refusal
          // from the client throws out of here as a tool error, so the model
          // reads the host's own sentence.
          const result = await mount.agent.completeFromTool({
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
          to: Type.String({
            description:
              'target role, e.g. builder; "cluster:role" is accepted only for the task\'s own cluster',
          }),
          text: Type.String({ description: "handoff text for the receiving role" }),
          image: Type.Optional(Type.String({ description: "absolute path to a png/jpeg/gif/webp image to attach" })),
          cluster: Type.Optional(Type.String({
            description:
              "the cluster this task came from, when this session serves several; the task's own mount answers without it",
          })),
        }),
        async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
          const caller = sessionIdOf(ctx?.sessionManager);
          if (!ownsSession(boundSessionId, caller)) refuseForeignSession(caller);
          if (mounts.length === 0) throw new Error("onlyne: session is not connected");
          // A handoff continues the task, so it stays in the task's cluster: the
          // mount that holds the task is also the ledger the child is minted in.
          const mount = resolveTaskMount(mounts, params.cluster);
          const to = handoffRole(params.to, mount);
          const result = await mount.agent.handoffFromTool({
            to,
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
          if (mounts.length === 0) {
            commandContext.ui.notify("onlyne: no session (ONLYNE_* env is absent)", "info");
            return;
          }
          const verb = String(argLine ?? "").trim();
          if (verb === "disconnect") {
            for (const mount of mounts) mount.agent.stop("operator");
            commandContext.ui.notify("onlyne: detached", "info");
            return;
          }
          if (verb === "connect") {
            for (const mount of mounts) mount.agent.start();
            commandContext.ui.notify(
              `onlyne: connecting to ${mounts.map((mount) => mount.agent.status().socket).join(", ")}`,
              "info"
            );
            return;
          }
          commandContext.ui.notify(
            mounts.length === 1
              ? `onlyne ${JSON.stringify(mounts[0].agent.status())}`
              : `onlyne ${JSON.stringify(mounts.map((mount) => ({ mount: mountLabel(mount), ...mount.agent.status() })))}`,
            "info"
          );
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
    if (mounts.length === 0) return;
    // The same declaration the mount gate reads, on the other door: a context
    // the host declares a subagent must not re-point this plugin's connection
    // onto a child, whatever session id it carries.
    if (declaredSubagent(ctx)) return;
    context = ctx;
    const currentId = sessionIdOf(ctx?.sessionManager);
    if (currentId) {
      // `/new`, `/resume` and a tree jump replace the live session inside this
      // process rather than starting a second resident, so the claim moves with
      // it — but only the mount that holds the claim may move it. A factory that
      // never mounted returned above, and one whose mount is not the claim
      // holder leaves the claim where it is. The comparison reads the session
      // this mount held until now, so it goes before the rebind.
      if (boundSessionId === residentClaim) residentClaim = currentId;
      boundSessionId = currentId;
    }
    // The panel and the footer show the mount that owns the work, or the first
    // one when none does: one status line cannot name two clusters, and the
    // delivery card already carries the label the model needs.
    const shown = soleActiveMount(mounts) ?? mounts[0];
    if (surface?.available.widget) {
      surface.widget(shown.agent.activity.lines(), shown.agent.activity);
    }
    surface?.status(shown.agent.activity.lines()[0]);
    // A surface that was replaced starts with no prose of its own, and the run
    // after a switch still owes the model the role spec: every mount remembers
    // what its own welcome carried and hands it over here.
    for (const mount of mounts) mount.agent.syncRoleProse();
  }

  pi.on("session_start", async (event, ctx) => {
    const spawned = sessionIdentity(env);
    const config = loadConfig(ctx.cwd);
    const declared = Array.isArray(config.mounts) ? config.mounts : [];
    const resident = spawned ? null : residentIdentity(env, ctx.cwd);
    /**
     * A switch file that declares several mounts is the opt-in, and it names
     * its own roles: no single `role` key has to exist for a two-cluster
     * session to mount, and the first entry supplies the identity the session
     * gate below compares.
     */
    const multi = !spawned && declared.length > 1;
    const identity =
      spawned ??
      resident ??
      (!spawned && declared.length > 0
        ? {
            role: declared[0].role,
            sessionId: declared[0].sessionId,
            taskId: null,
            cluster: declared[0].cluster,
            resident: true as const,
            workspaceRoot: declared[0].workspace
              ? isAbsolute(declared[0].workspace)
                ? declared[0].workspace
                : resolve(ctx.cwd, declared[0].workspace)
              : ctx.cwd,
          }
        : null);
    if (!identity) return; // not an onlyne session: stay out of the way
    const starting = sessionIdOf(ctx.sessionManager);
    // The host's own declaration comes first, before either identity path: a
    // session it declares a subagent — a `task` subagent, an `eval` child, a
    // `/tan` clone, all re-bound through this factory in this process — is never
    // the session a role's tools belong to, even when it inherits the client's
    // environment or arrives with no session id at all
    // (docs/v2.11-LIFECYCLE-IS-DECLARED-NOT-INFERRED.md §2 rule 2).
    const subagent = declaredSubagent(ctx);
    if (subagent) {
      log(
        `session ${starting} is subagent ${subagent.id ?? "?"} of ${subagent.parentId ?? "?"}; staying inert`
      );
      return;
    }
    // A resident plugin already holds its connection, so another session_start
    // in this process is a re-point, not a mount: /new, /resume and /fork
    // replace the live session, and pi can re-bind this same session's
    // extensions. A subagent on a host that declares no agent identity re-binds
    // with reason "startup" and a different id, and is refused below.
    if (!spawned && mounts.length > 0 && (event.reason !== "startup" || starting === boundSessionId)) {
      syncSessionContext(event, ctx);
      return;
    }
    // Only the session the client names may mount. A pi process runs more than
    // one session — a `task` subagent's sidechain, `/new` — and each of them
    // loads this extension with the same inherited environment, so without this
    // check a subagent's `onlyne_complete` settles the parent's task (observed:
    // librarian task cfa73386). An unknown id means an older pi that does not
    // expose one; the env-bound mount is then the only candidate and stays the
    // behaviour, with no line to explain.
    if (spawned) {
      if (!mountsSession(spawned.sessionId, starting)) {
        log(`session ${starting} is not the onlyne session ${spawned.sessionId}; staying inert`);
        return;
      }
    } else {
      // The claim is process-wide, so it holds across factory calls: a `task`
      // subagent's sidechain loads this extension again with fresh closure state
      // and would otherwise mount the alias this process already serves. A
      // `session_start` the host calls a replacement is the other reading: the
      // live session is moving inside this process, so the claim moves with it
      // instead of refusing the session that is now the resident. An older pi
      // that names no session id is the one case with nothing to compare — it
      // mounts as it did before, and claims nothing.
      if (
        residentClaim !== null &&
        starting !== null &&
        starting !== residentClaim &&
        !isSessionReplacement(event)
      ) {
        log(`session ${starting} is not the resident session ${residentClaim}; staying inert`);
        return;
      }
      if (starting !== null) residentClaim = starting;
    }
    const wireSessionId = identity.sessionId ?? starting;
    context = ctx;
    boundSessionId = spawned ? spawned.sessionId : (starting ?? wireSessionId);
    if (config.warning) log(config.warning);
    if (!config.enabled) {
      log(`disabled by ${config.path}`);
      return;
    }
    // One descriptor per client link. A spawned session holds exactly one; a
    // resident plugin reads the switch file, and a file that declares none
    // keeps the single-mount shape it always had.
    const specs: MountSpec[] = multi
      ? declared.map((entry) => ({
          role: entry.role,
          cluster: entry.cluster,
          workspace: entry.workspace
            ? isAbsolute(entry.workspace)
              ? entry.workspace
              : resolve(ctx.cwd, entry.workspace)
            : resident
              ? resident.workspaceRoot
              : ctx.cwd,
          sessionId: entry.sessionId,
        }))
      : [
          {
            role: identity.role,
            cluster: resident ? resident.cluster : null,
            workspace: resident ? resident.workspaceRoot : ctx.cwd,
            sessionId: identity.sessionId ?? null,
          },
        ];
    // The prose each mount of this process already holds: the agents are
    // replaced below and the surface made now starts empty, so what a mount's
    // last welcome carried moves with it rather than waiting for a handshake.
    const carried: Record<string, string> = {};
    for (const mount of mounts) {
      carried[mountKey(mount.cluster, mount.role)] = mount.agent.latestProse ?? "";
    }
    for (const mount of mounts) mount.agent.stop("remount");
    surface = createSurface({ pi, log, context: () => context, sessionId: () => boundSessionId });
    const next: Mount[] = [];
    for (const spec of specs) {
      // The path the client injected, or the client the runtime directory's
      // registration files name for this workspace (socket.mjs). A mount with
      // neither has no socket to dial; the rest still mount, and the line names
      // which one could not.
      let socketPath: string | null = null;
      try {
        socketPath = spawned
          ? resolveSocketPath(env, ctx.cwd)
          : resolveResidentSocketPath(env, spec.workspace, spec.role, { cluster: spec.cluster });
      } catch (error) {
        log(
          `socket unresolved for ${spec.cluster ? `${spec.cluster}:` : ""}${spec.role}: ${error instanceof Error ? error.message : String(error)}`
        );
        continue;
      }
      if (socketPath === null) continue;
      const agent = new OnlyneAgent({
        socketPath,
        resolveSocket: spawned
          ? undefined
          : () => resolveResidentSocketPath(env, spec.workspace, spec.role, { cluster: spec.cluster }),
        cwd: ctx.cwd,
        role: spec.role,
        sessionId: spec.sessionId ?? wireSessionId,
        taskId: identity.taskId,
        resident: !spawned,
        surface,
        log,
        prose: carried[mountKey(spec.cluster, spec.role)] ?? "",
        cluster: spec.cluster,
        sharedProcess: specs.length > 1,
        recycleHandler: specs.length > 1 ? exitWhenNoMountRemains : undefined,
      });
      next.push({ agent, cluster: spec.cluster, role: spec.role });
      log(
        `session ${wireSessionId ?? "resident"} role=${spec.role}${spec.cluster ? ` cluster=${spec.cluster}` : ""} socket=${socketPath}`
      );
    }
    if (next.length === 0) return;
    mounts = next;
    registerSurface();
    if (!surface.available.wakeUser) {
      // Without an injection surface the payload travels through config_get,
      // and the hello capability list has to say so.
      log("no wakeUser surface: dropping the inject capability");
      for (const mount of mounts) {
        mount.agent.capabilities = mount.agent.capabilities.filter((name) => name !== "inject");
      }
    }
    if (config.autoStart) for (const mount of mounts) mount.agent.start();
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
    if (!surface) return;
    if (mounts.length <= 1) {
      // A surface created after the handshake holds no prose; the agent's copy
      // of the last welcome is what the run that is starting carries instead of
      // running without the role spec until the next connection.
      surface.applyRoleProse(event, mounts[0]?.agent.latestProse ?? "");
      return;
    }
    // Two layers, two role specs. Identical prose from both mounts is written
    // once — one fact repeated is not two facts — and different prose is
    // written once per cluster under its own key, so neither spec silently
    // replaces the other (docs/v2.12-ONE-SESSION-TWO-CLUSTERS.md §6).
    const written = new Set<string>();
    for (const mount of mounts) {
      const prose = mount.agent.latestProse ?? "";
      if (prose.length === 0 || written.has(prose)) continue;
      written.add(prose);
      const key = written.size === 1 ? PROSE_SECTION : `${PROSE_SECTION}.${mount.cluster ?? mount.role}`;
      surface.applyRoleProseSection(event, key, prose);
    }
  });

  pi.on("turn_start", async () => {
    for (const mount of mounts) mount.agent.onTurnStart();
  });

  pi.on("turn_end", async (event) => {
    const failure = failureOf(event.message);
    for (const mount of mounts) {
      if (failure) mount.agent.onTurnError(failure);
      else mount.agent.onTurnEnd();
    }
  });

  pi.on("message_end", async (event) => {
    const failure = failureOf(event.message);
    for (const mount of mounts) {
      if (failure) mount.agent.onTurnError(failure);
      else mount.agent.noteAssistantText(assistantTextOf(event.message));
    }
  });

  pi.on("agent_settled", async () => {
    for (const mount of mounts) mount.agent.onSettled();
  });

  pi.on("session_shutdown", async (event) => {
    for (const mount of mounts) mount.agent.stop(`pi:${event.reason ?? "quit"}`);
    surface?.closeBackground?.();
    surface?.widget?.(undefined);
    // Only the mount holding the claim may release it: a child factory that
    // never mounted has no `boundSessionId` of its own, and its shutdown must
    // leave the resident's claim where it is.
    if (boundSessionId !== null && boundSessionId === residentClaim) residentClaim = null;
    mounts = [];
    surface = null;
    registered = false;
    context = null;
    boundSessionId = null;
  });
}
