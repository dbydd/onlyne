// The pi side of the adapter: every effect the agent asks for, expressed
// through the pi extension API, with a probe for each optional member so an
// older pi degrades instead of throwing.
//
// Probed members (measured against pi 0.87.1):
//   wakeUser      pi.sendUserMessage(content, { deliverAs: "followUp" })
//   roleProse     pi.on("before_agent_start") -> event.systemPromptOptions.sections
//                 (index.ts owns the subscription; this module holds the prose and
//                  writes the section it becomes)
//   customEntry   pi.appendEntry(customType, data)
//   widget        ctx.ui.setWidget("onlyne", lines) / ctx.ui.setWidget("onlyne", undefined)
//   status        ctx.ui.setStatus("onlyne", text)
//   exit          ctx.shutdown()
//   isIdle        ctx.isIdle()
//   pending       ctx.hasPendingMessages()
//   toolNames     pi.getAllTools()          (background-work.mjs)
//   eventBus      pi.events                 (background-work.mjs)
//   subagentRegistry  ~/.pi/subagents/missions  (background-subagents.mjs)
//   registerTool / registerCommand are probed by index.ts itself.

import { WIDGET_KEY } from "./activity.mjs";
import { createSubagentProbe } from "./background-subagents.mjs";
import { createBackgroundProbe } from "./background-work.mjs";

/**
 * The system-prompt section the role prose occupies. pi wraps a section in a tag
 * of the same name and records it under that name in the transcript, which is
 * also where the live case reads the prose back from
 * (`crates/onlyne-testkit/e2e/pi-live.sh`), so the name is stable.
 */
export const PROSE_SECTION = "onlyne-role-prose";

/**
 * @param {{ pi: any, log: (line: string) => void, context: () => any, sessionId?: string | null }} options
 */
export function createSurface({ pi, log, context, sessionId = null }) {
  const has = (value) => typeof value === "function";
  const ctx = () => {
    try {
      return context();
    } catch {
      return null;
    }
  };

  /**
   * The one question behind every phase the plugin reports. pi answers it; a
   * probe that is missing, throws, or arrives without a context answers `false`
   * because an unwitnessed session is a running one as far as this plugin can
   * prove (`background-work.mjs` carries the second half of the question).
   */
  const piWaitsForInput = () => {
    const current = ctx();
    if (!current) return false;
    try {
      if (!has(current.isIdle) || !current.isIdle()) return false;
      if (has(current.hasPendingMessages) && current.hasPendingMessages()) return false;
      return true;
    } catch {
      return false;
    }
  };

  const background = createBackgroundProbe({
    events: pi.events ?? null,
    getToolNames: has(pi.getAllTools) ? () => (pi.getAllTools() ?? []).map((tool) => tool?.name) : null,
    log,
  });

  // The second family of off-loop work: `Agent` calls, which return a handle and
  // leave a subagent running with no tool event to bracket it. Scoped to this
  // session's own missions when the plugin knows its id.
  const subagents = createSubagentProbe({
    getSessionId: () => sessionId,
    log,
  });

  /**
   * Live work in either extension holds the turn open. Both probes fail safe on
   * their own, and a throw from either is caught here, so a broken probe still
   * resolves to "not running" and the session proceeds.
   * @returns {Promise<boolean>}
   */
  const backgroundRunning = async () => {
    try {
      const answers = await Promise.all([background.running(), subagents.running()]);
      return answers.some(Boolean);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      log(`background work probe failed: ${message}`);
      return false;
    }
  };

  const available = {
    wakeUser: has(pi.sendUserMessage),
    customEntry: has(pi.appendEntry),
    widget: has(ctx()?.ui?.setWidget),
    status: true,
    exit: true,
    isIdle: true,
    registerTool: has(pi.registerTool),
    registerCommand: has(pi.registerCommand),
  };

  const widget = (lines) => {
    try {
      ctx()?.ui?.setWidget?.(WIDGET_KEY, lines);
    } catch {
      /* widget is decoration; never let it break the protocol */
    }
  };

  const status = (text) => {
    try {
      ctx()?.ui?.setStatus?.("onlyne", text);
    } catch {
      /* status is decoration; never let it break the protocol */
    }
  };

  /**
   * One pi user message; images ride along as pi image content parts.
   *
   * pi reads an image part as the flat `ImageContent` of its message types —
   * `data` plus `mimeType` — and normalizes every part before the message is
   * built, so a part missing either string stops the whole delivery inside pi.
   * pi reports that failure in its own pane and hands nothing back to this
   * plugin, so an unusable part is dropped here and the assignment still
   * travels: the delivery text already names the path the client wrote.
   */
  const wakeUser = (text, parts = []) => {
    if (!available.wakeUser) {
      log("pi has no sendUserMessage; the assignment reached the session log only");
      return false;
    }
    const images = parts.filter(
      (part) => typeof part?.data === "string" && typeof part?.mime === "string" && part.mime,
    );
    if (images.length !== parts.length) {
      log(`attachment carried no base64 data or no media type; ${parts.length - images.length} dropped, the task text still went`);
    }
    const content = images.length === 0
      ? text
      : [
          { type: "text", text },
          ...images.map((part) => ({
            type: "image",
            data: part.data,
            mimeType: part.mime,
          })),
        ];
    try {
      pi.sendUserMessage(content, { deliverAs: "followUp" });
      return true;
    } catch (error) {
      try {
        pi.sendUserMessage(content);
        return true;
      } catch (fallbackError) {
        const message = fallbackError instanceof Error ? fallbackError.message : String(fallbackError);
        log(`sendUserMessage refused: ${message}`);
        return false;
      }
    }
  };

  /**
   * The role prose this session was handed, held for the instruction layer.
   *
   * The client rendered it and this module adds nothing to it: no prefix, no
   * label, no formatting. It reaches the model as a system-prompt section and
   * never as a conversation message: a message would file the spec's prose in
   * the transcript's message stream beside the delivery the model was asked to
   * act on, and the model would read both as the same kind of thing.
   */
  let roleProseText = "";

  /** One line for a pi without sectioned prompts, not one per run. */
  let warnedNoSections = false;

  /**
   * Hand the role prose over once.
   * @param {string} text
   * @returns {boolean} whether there is prose to carry
   */
  const roleProse = (text) => {
    roleProseText = typeof text === "string" ? text : "";
    return roleProseText.length > 0;
  };

  /**
   * Put that prose into the run that is starting: one section of pi's system
   * prompt, whose value is the client's bytes exactly.
   *
   * `before_agent_start` is pi's only instruction-layer point, and it hands the
   * handler the prompt options it is about to render (`prompt-customizer.ts` in
   * pi's own examples does this). pi rebuilds those options for every run, so the
   * section is written again rather than once — the write is idempotent, and the
   * first run records it in the transcript's system message.
   * @param {{ systemPromptOptions?: { sections?: Record<string, string> } } | null} event
   * @returns {boolean} whether a section was written
   */
  const applyRoleProse = (event) => {
    if (roleProseText.length === 0) return false;
    const sections = event?.systemPromptOptions?.sections;
    if (!sections) {
      if (!warnedNoSections) {
        warnedNoSections = true;
        log("before_agent_start carries no prompt sections; the role prose reached no instruction layer");
      }
      return false;
    }
    sections[PROSE_SECTION] = roleProseText;
    return true;
  };

  const customEntry = (customType, data) => {
    if (!available.customEntry) return false;
    try {
      pi.appendEntry(customType, data);
      return true;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      log(`appendEntry refused: ${message}`);
      return false;
    }
  };

  return {
    available,
    wakeUser,
    roleProse,
    applyRoleProse,
    customEntry,
    widget,
    status,
    welcome(welcome) {
      status(`onlyne: ${welcome.role}`);
    },
    /** `true` when pi is not processing a run, retry, compaction, or continuation. */
    isIdle() {
      const current = ctx();
      try {
        return has(current?.isIdle) ? current.isIdle() : true;
      } catch {
        return true;
      }
    },
    /**
     * The phase rule in one place: idle means waiting for user input, and a
     * background extension holding live work — a `bg_*` task or a subagent
     * mission — keeps the session running even while pi itself waits.
     */
    async waitingForInput() {
      if (!piWaitsForInput()) return false;
      return !(await backgroundRunning());
    },
    closeBackground() {
      background.close();
    },
    exit(reason) {
      log(`exiting pi: ${reason}`);
      try {
        const current = ctx();
        if (has(current?.shutdown)) current.shutdown();
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        log(`shutdown refused: ${message}`);
      }
    },
  };
}
