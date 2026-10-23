// The one question a background-task extension makes necessary: is this
// session's work still running somewhere the agent loop cannot see?
//
// `pi-background-tasks` and its relatives take a long command off the loop — the
// tool call returns a task id at once and the child process carries on. pi then
// waits for input while the work runs, so `ctx.isIdle()` alone would report a
// session as idle with a task in flight. This probe reads the extension's own
// live task list over the pi EventBus, and only when the extension is installed:
// without one of its tools there is nothing to recognise, nothing to query, and
// nothing to wait for.
//
// The contract is that package's documented `eventbus-v1` surface
// (`pi-background-tasks/docs/api/eventbus-v1.md`): one request frame in, one
// response frame out, both closed objects carrying a schema id. A response that
// never arrives, a frame that does not parse, an error response, and a host with
// no EventBus all read as "no background work known" and leave the plugin's own
// judgement untouched.

/** The tools that mark the extension as installed. */
export const BACKGROUND_TOOL_NAMES = Object.freeze([
  "bg_run",
  "bg_run_pi_attested",
  "bg_status",
  "bg_logs",
  "bg_kill",
  "bg_delegate",
  "bg_result",
  "fusion_reason",
  "fusion_investigate",
  "fusion_research",
  "fusion_validate",
  "fusion_web_fetch",
]);

const REQUEST_CHANNEL = "pi-background-tasks:request:v1";
const RESPONSE_CHANNEL = "pi-background-tasks:response:v1";
const REQUEST_SCHEMA = "pi-background-tasks.extension-request.v1";
const RESPONSE_SCHEMA = "pi-background-tasks.extension-response.v1";

/** Task statuses that mean the work is still going. */
const LIVE_TASK_STATUS = "running";

/** How long one status query waits for its answer. */
export const DEFAULT_STATUS_TIMEOUT_MS = 500;

/** @param {string} name */
export function isBackgroundTool(name) {
  return BACKGROUND_TOOL_NAMES.includes(name);
}

let requestCounter = 0;

function nextRequestId() {
  requestCounter += 1;
  return `onlyne-bg-status-${requestCounter}`;
}

function isResponseFor(frame, requestId) {
  return frame
    && typeof frame === "object"
    && frame.schema_version === RESPONSE_SCHEMA
    && frame.request_id === requestId;
}

/**
 * One live-task question, asked of the EventBus and answered by whatever is
 * listening. Every failure mode is inert: the probe reports `false` and says why
 * once, then lets the caller's own judgement stand.
 *
 * @param {{
 *   events?: { emit: (channel: string, data: unknown) => void, on: (channel: string, handler: (data: unknown) => void) => () => void } | null,
 *   getToolNames?: () => string[] | null,
 *   log?: (line: string) => void,
 *   timeoutMs?: number,
 * }} options
 */
export function createBackgroundProbe({
  events = null,
  getToolNames = null,
  log = () => {},
  timeoutMs = DEFAULT_STATUS_TIMEOUT_MS,
} = {}) {
  let installed = null;
  const warned = new Set();
  let unsubscribe = null;

  const warnOnce = (reason) => {
    if (warned.has(reason)) return;
    warned.add(reason);
    log(`background work: ${reason}`);
  };

  /** True once one of the extension's tools is registered; cached either way. */
  function isInstalled() {
    if (installed !== null) return installed;
    let names = null;
    try {
      names = typeof getToolNames === "function" ? getToolNames() : null;
    } catch (error) {
      warnOnce(`tool list unreadable: ${error.message}`);
      return false;
    }
    const list = Array.isArray(names) ? names : [];
    installed = list.some(isBackgroundTool);
    if (!installed) log("background work: no background-task extension in this session");
    return installed;
  }

  /**
   * One status round trip. The answer arrives on the response channel, so the
   * listener lives exactly as long as the wait and the timer bounds it.
   * @returns {Promise<boolean>}
   */
  function running() {
    if (!isInstalled()) return Promise.resolve(false);
    const bus = events;
    if (!bus || typeof bus.emit !== "function" || typeof bus.on !== "function") {
      warnOnce("event bus unavailable");
      return Promise.resolve(false);
    }
    return new Promise((resolve) => {
      const requestId = nextRequestId();
      let settled = false;
      let timer = null;
      const finish = (answer) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        if (typeof unsubscribe === "function") unsubscribe();
        unsubscribe = null;
        resolve(answer);
      };
      try {
        unsubscribe = bus.on(RESPONSE_CHANNEL, (frame) => {
          if (!isResponseFor(frame, requestId)) return;
          if (frame.ok !== true) {
            warnOnce(`status query refused: ${String(frame.error ?? "unknown")}`);
            finish(false);
            return;
          }
          const tasks = frame.result?.tasks;
          if (!Array.isArray(tasks)) {
            warnOnce("status answer carried no task list");
            finish(false);
            return;
          }
          finish(tasks.some((task) => task?.status === LIVE_TASK_STATUS));
        });
        bus.emit(REQUEST_CHANNEL, {
          schema_version: REQUEST_SCHEMA,
          request_id: requestId,
          operation: "status",
          payload: {},
        });
      } catch (error) {
        warnOnce(`status query failed: ${error.message}`);
        finish(false);
        return;
      }
      timer = setTimeout(() => {
        warnOnce("status query timed out");
        finish(false);
      }, timeoutMs);
      if (typeof timer?.unref === "function") timer.unref();
    });
  }

  return {
    installed: isInstalled,
    running,
    close() {
      if (typeof unsubscribe === "function") unsubscribe();
      unsubscribe = null;
    },
  };
}
