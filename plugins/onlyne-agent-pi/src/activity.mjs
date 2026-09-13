/** The widget key shared with pi-surface. */
export const WIDGET_KEY = "onlyne";
/** Render budget below pi's own ten-line cap. */
export const MAX_LINES = 8;
/** Line budget that keeps the panel inside one terminal row. */
export const MAX_WIDTH = 96;
/** Event text budget before the final line-width cap. */
export const MAX_TEXT = 72;
/** Retained newest-first activity history. */
export const KEEP_EVENTS = 64;
/** Default count of activity rows below the header. */
export const SHOW_EVENTS = 6;

const MARKERS = {
  in: "<=",
  dup: "~~",
  out: "=>",
  warn: "!!",
  state: "..",
};

const clean = (value) => String(value ?? "").replace(/\s+/g, " ").trim();
const points = (value) => Array.from(String(value ?? ""));

function cut(value, width, mark = true) {
  const chars = points(value);
  if (chars.length <= width) return chars.join("");
  if (width <= 0) return "";
  if (!mark || width === 1) return chars.slice(0, width).join("");
  return `${chars.slice(0, width - 1).join("")}…`;
}

function noteText(value) {
  const text = clean(value);
  const chars = points(text);
  return chars.length > MAX_TEXT ? `${chars.slice(0, MAX_TEXT).join("")}…` : text;
}

function timeOf(value) {
  const date = value instanceof Date ? value : new Date(value);
  const two = (number) => String(number).padStart(2, "0");
  return `${two(date.getHours())}:${two(date.getMinutes())}:${two(date.getSeconds())}`;
}

function headerLine(state) {
  const role = clean(state.role);
  const connection = clean(state.connection) || "connecting";
  const first = role ? `onlyne ${cut(role, 16, false)}` : "onlyne";
  const parts = [first, connection];
  if (state.generation !== undefined && state.generation !== null) parts.push(`gen ${state.generation}`);
  const taskId = clean(state.taskId);
  if (taskId) {
    const phase = clean(state.phase);
    parts.push(`task ${cut(taskId, 8, false)}${phase ? ` ${phase}` : ""}`);
  }
  return cut(parts.join(" · "), MAX_WIDTH);
}

function eventLine(event) {
  const marker = MARKERS[event.kind] ?? MARKERS.state;
  const count = event.repeats > 0 ? ` x${event.repeats + 1}` : "";
  return cut(`${timeOf(event.at)}  ${marker} ${event.text}${count}`, MAX_WIDTH);
}

/**
 * Activity is a pure panel model: header state plus newest-first event history.
 * `lines()` reads stored state only, so identical state gives identical output.
 */
export function createActivity({ maxEvents = SHOW_EVENTS, clock = () => new Date() } = {}) {
  const state = { connection: "connecting" };
  const events = [];
  const shown = Math.max(0, Math.min(Number(maxEvents) || 0, MAX_LINES - 1));
  const api = {
    note(kind, text) {
      const next = noteText(text);
      const current = events[0];
      if (current && current.kind === kind && current.text === next) {
        current.repeats += 1;
        current.at = clock();
        return api;
      }
      events.unshift({ at: clock(), kind, text: next, repeats: 0 });
      if (events.length > KEEP_EVENTS) events.length = KEEP_EVENTS;
      return api;
    },
    set(patch = {}) {
      Object.assign(state, patch);
      return api;
    },
    lines() {
      return [headerLine(state), ...events.slice(0, shown).map(eventLine)].slice(0, MAX_LINES);
    },
    events,
  };
  return api;
}
