// The pi side of the adapter: every effect the agent asks for, expressed
// through the pi extension API, with a probe for each optional member so an
// older pi degrades instead of throwing.
//
// Probed members (measured against pi 0.85.1):
//   wakeUser      pi.sendUserMessage(content, { deliverAs: "followUp" })
//   proseContext  pi.sendMessage({customType,...}, { deliverAs:"followUp", triggerTurn:false })
//   customEntry   pi.appendEntry(customType, data)
//   widget        ctx.ui.setWidget("onlyne", lines) / ctx.ui.setWidget("onlyne", undefined)
//   status        ctx.ui.setStatus("onlyne", text)
//   exit          ctx.shutdown()
//   isIdle        ctx.isIdle()
//   pending       ctx.hasPendingMessages()
//   toolNames     pi.getAllTools()          (background-work.mjs)
//   eventBus      pi.events                 (background-work.mjs)
//   registerTool / registerCommand are probed by index.ts itself.

import { WIDGET_KEY } from "./activity.mjs";
import { createBackgroundProbe } from "./background-work.mjs";

/**
 * @param {{ pi: any, log: (line: string) => void, context: () => any }} options
 */
export function createSurface({ pi, log, context }) {
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

  const available = {
    wakeUser: has(pi.sendUserMessage),
    proseContext: has(pi.sendMessage),
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

  /** One pi user message; images ride along as pi image content parts. */
  const wakeUser = (text, parts = []) => {
    if (!available.wakeUser) {
      log("pi has no sendUserMessage; the assignment reached the session log only");
      return false;
    }
    const content = parts.length === 0
      ? text
      : [
          { type: "text", text },
          ...parts.map((part) => ({
            type: "image",
            source: { type: "base64", mediaType: part.mime, data: part.data },
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
   * The role prose, once, as a custom message that joins the LLM context
   * without starting a turn of its own.
   */
  const proseContext = (text, welcome) => {
    if (!available.proseContext) return false;
    try {
      pi.sendMessage(
        {
          customType: "onlyne-role-prose",
          content: `[onlyne] role prose for ${welcome.role} (from the cluster spec, delivered with welcome):\n\n${text}`,
          display: true,
        },
        { deliverAs: "followUp", triggerTurn: false },
      );
      return true;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      log(`role prose injection refused: ${message}`);
      return false;
    }
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
    proseContext,
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
     * background-task extension holding live work keeps the session running
     * even while pi itself waits.
     */
    async waitingForInput() {
      if (!piWaitsForInput()) return false;
      return !(await background.running());
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
