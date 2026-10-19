/** The widget key shared with pi-surface. */
export const WIDGET_KEY = "onlyne";
/** The custom message type registered for visible inbound delivery cards. */
export const ASSIGN_MESSAGE_TYPE = "onlyne_assign";
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

const EVENT_STYLES = {
  in: { fg: "accent" },
  dup: { fg: "dim" },
  out: { fg: "success" },
  warn: { fg: "warning" },
  state: { fg: "muted" },
};

const CONNECTION_STYLES = {
  connected: { fg: "success", status: "done" },
  reconnecting: { fg: "warning", status: "running" },
  detached: { fg: "muted", status: "cancelled" },
  connecting: { fg: "accent", status: "pending" },
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

function toneFg(theme, color, text) {
  if (!theme || typeof theme.fg !== "function") return text;
  try {
    return theme.fg(color, text);
  } catch {
    return text;
  }
}

function toneBold(theme, text) {
  if (!theme || typeof theme.bold !== "function") return text;
  try {
    return theme.bold(text);
  } catch {
    return text;
  }
}

function styledHeaderLine(state, theme, width) {
  if (!theme || typeof theme.fg !== "function") return headerLine(state);
  const role = clean(state.role);
  const connection = clean(state.connection) || "connecting";
  const connStyle = CONNECTION_STYLES[connection] ?? CONNECTION_STYLES.connecting;
  const sep = toneFg(theme, "dim", " · ");
  const head = role
    ? `${toneBold(theme, toneFg(theme, "accent", "onlyne"))} ${toneBold(theme, cut(role, 16, false))}`
    : toneBold(theme, toneFg(theme, "accent", "onlyne"));
  const parts = [head, toneFg(theme, connStyle.fg, connection)];
  if (state.generation !== undefined && state.generation !== null) {
    parts.push(toneFg(theme, "muted", `gen ${state.generation}`));
  }
  const taskId = clean(state.taskId);
  if (taskId) {
    const phase = clean(state.phase);
    const phaseTone = phase === "done"
      ? "success"
      : phase === "failed" || phase === "blocked"
        ? "error"
        : phase.includes("queued") || phase === "running"
          ? "warning"
          : "muted";
    const taskPart = `${toneFg(theme, "muted", "task")} ${toneFg(theme, "accent", cut(taskId, 8, false))}${
      phase ? ` ${toneFg(theme, phaseTone, phase)}` : ""
    }`;
    parts.push(taskPart);
  }
  return parts.join(sep);
}

function styledEventLine(event, theme, maxTextWidth) {
  if (!theme || typeof theme.fg !== "function") return eventLine(event);
  const marker = MARKERS[event.kind] ?? MARKERS.state;
  const style = EVENT_STYLES[event.kind] ?? EVENT_STYLES.state;
  const count = event.repeats > 0 ? toneFg(theme, "warning", ` ×${event.repeats + 1}`) : "";
  const bodyBudget = Math.max(12, maxTextWidth - 14 - (event.repeats > 0 ? String(event.repeats + 1).length + 2 : 0));
  const body = cut(event.text, bodyBudget);
  const bodyStyled = event.kind === "warn"
    ? toneFg(theme, "warning", body)
    : event.kind === "dup"
      ? toneFg(theme, "dim", body)
      : body;
  return `${toneFg(theme, "dim", timeOf(event.at))}  ${toneFg(theme, style.fg, marker)} ${bodyStyled}${count}`;
}

function describeHeaderSpans(state) {
  const role = clean(state.role);
  const connection = clean(state.connection) || "connecting";
  const connStyle = CONNECTION_STYLES[connection] ?? CONNECTION_STYLES.connecting;
  const spans = [{ t: "onlyne", s: "accent strong" }];
  if (role) spans.push({ t: ` ${cut(role, 16, false)}`, s: "strong" });
  spans.push({ t: " · ", s: "dim" }, { t: connection, s: connStyle.fg });
  if (state.generation !== undefined && state.generation !== null) {
    spans.push({ t: " · ", s: "dim" }, { t: `gen ${state.generation}`, s: "muted" });
  }
  const taskId = clean(state.taskId);
  if (taskId) {
    const phase = clean(state.phase);
    const phaseTone = phase === "done"
      ? "success"
      : phase === "failed" || phase === "blocked"
        ? "error"
        : phase.includes("queued") || phase === "running"
          ? "warning"
          : "muted";
    spans.push(
      { t: " · ", s: "dim" },
      { t: "task ", s: "muted" },
      { t: cut(taskId, 8, false), s: "accent mono" },
    );
    if (phase) spans.push({ t: ` ${phase}`, s: phaseTone });
  }
  return spans;
}

function describeEventRow(event, index) {
  const marker = MARKERS[event.kind] ?? MARKERS.state;
  const style = EVENT_STYLES[event.kind] ?? EVENT_STYLES.state;
  const bodyTone = event.kind === "warn" ? "warning" : event.kind === "dup" ? "dim" : undefined;
  const spans = [
    { t: timeOf(event.at), s: "dim mono" },
    { t: `  ${marker} `, s: `${style.fg} mono strong` },
    bodyTone ? { t: event.text, s: bodyTone } : { t: event.text },
  ];
  if (event.repeats > 0) {
    spans.push({ t: ` ×${event.repeats + 1}`, s: "warning mono" });
  }
  return {
    k: "text",
    p: { spans, truncate: "end", role: `onlyne.event.${event.kind}` },
    key: `ev-${index}`,
  };
}

/**
 * Build a TSP `NativeNode` card for Tern surfaces (`Component.describe()`).
 */
export function describeActivityNode(state, events, shown = SHOW_EVENTS, collapsed = false) {
  const connection = clean(state.connection) || "connecting";
  const connStyle = CONNECTION_STYLES[connection] ?? CONNECTION_STYLES.connecting;
  const visible = events.slice(0, shown);
  return {
    k: "card",
    p: {
      role: "omp.widget.onlyne",
      variant: "bare",
      status: connStyle.status,
      head: describeHeaderSpans(state),
      collapsible: visible.length > 0,
      collapsed: visible.length > 0 ? Boolean(collapsed) : false,
    },
    c: visible.map((ev, index) => describeEventRow(ev, index)),
  };
}

/**
 * Render an inbound Onlyne delivery (`customType: "onlyne_assign"`) as a
 * framed transcript block on ANSI terminals and a native TSP card on Tern.
 */
export function createAssignMessageComponent(details, messageContent, theme, collapsed = false) {
  const from = clean(details?.from) || "cluster";
  const kind = clean(details?.kind) || "task";
  const taskId = clean(details?.taskId);
  const shortId = taskId ? cut(taskId, 8, false) : "";
  const rawBody = clean(details?.bodyText)
    || (typeof messageContent === "string"
      ? messageContent
      : Array.isArray(messageContent)
        ? messageContent.filter((p) => p?.type === "text" && typeof p.text === "string").map((p) => p.text).join("\n")
        : "");
  const bodyLines = String(details?.bodyText ?? (typeof messageContent === "string" ? messageContent : rawBody))
    .replace(/\r\n/g, "\n")
    .split("\n");
  const headLabel = `From ${from} · ${kind}${shortId ? ` · ${shortId}` : ""}`;
  const kindTone = kind === "note" ? "info" : kind === "nudge" ? "warning" : "accent";
  let cachedWidth = -1;
  let cachedRows = null;
  let cachedNode = null;
  let isCollapsed = Boolean(collapsed);

  const buildNode = () => {
    const headSpans = [
      { t: `From ${from}`, s: "accent strong" },
      { t: " · ", s: "dim" },
      { t: kind, s: `${kindTone} mono` },
    ];
    if (shortId) {
      headSpans.push({ t: " · ", s: "dim" }, { t: shortId, s: "muted mono" });
    }
    const mdText = String(details?.bodyText ?? (typeof messageContent === "string" ? messageContent : rawBody)).trim();
    return {
      k: "card",
      p: {
        role: `omp.custom.onlyne.${kind}`,
        tone: kind === "nudge" ? "warning" : "accent",
        head: headSpans,
        collapsible: true,
        collapsed: isCollapsed,
      },
      c: mdText ? [{ k: "md", p: { text: mdText }, key: "body" }] : [],
    };
  };

  const buildRows = (width = MAX_WIDTH) => {
    const w = Math.max(16, Math.min(width, MAX_WIDTH));
    const inner = Math.max(12, w - 4);
    const topText = ` ${cut(headLabel, inner - 2)} `;
    const topFill = "─".repeat(Math.max(0, w - 2 - points(topText).length));
    const rows = [
      `${toneFg(theme, "muted", "╭")}${toneBold(theme, toneFg(theme, "accent", topText))}${toneFg(theme, "muted", `${topFill}╮`)}`,
    ];
    const visibleBody = isCollapsed
      ? [cut(clean(rawBody), inner)]
      : bodyLines.flatMap((line) => {
          const trimmed = line.replace(/\t/g, "  ");
          return trimmed.length === 0 ? [""] : [cut(trimmed, inner)];
        });
    for (const line of visibleBody) {
      const pad = " ".repeat(Math.max(0, inner - points(line).length));
      rows.push(`${toneFg(theme, "muted", "│")} ${toneFg(theme, "text", line)}${pad} ${toneFg(theme, "muted", "│")}`);
    }
    rows.push(`${toneFg(theme, "muted", `╰${"─".repeat(Math.max(0, w - 2))}╯`)}`);
    return rows;
  };

  return {
    render(width = MAX_WIDTH) {
      if (cachedRows && cachedWidth === width) return cachedRows;
      cachedWidth = width;
      cachedRows = buildRows(width);
      return cachedRows;
    },
    describe() {
      cachedNode ??= buildNode();
      return cachedNode;
    },
    handleNativeEvent(event) {
      if (event?.type === "toggle" && typeof event.collapsed === "boolean") {
        isCollapsed = event.collapsed;
        cachedRows = null;
        cachedNode = null;
      }
    },
    invalidate() {
      cachedWidth = -1;
      cachedRows = null;
      cachedNode = null;
    },
    dispose() {},
  };
}

/**
 * Activity is a pure panel model: header state plus newest-first event history.
 * `lines()` reads stored state only, so identical state gives identical output.
 */
export function createActivity({ maxEvents = SHOW_EVENTS, clock = () => new Date() } = {}) {
  const state = { connection: "connecting" };
  const events = [];
  const shown = Math.max(0, Math.min(Number(maxEvents) || 0, MAX_LINES - 1));
  let collapsed = false;
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
    ansiLines(theme, width = MAX_WIDTH) {
      const padX = width >= 4 ? " " : "";
      const innerWidth = Math.max(24, Math.min(MAX_WIDTH, width - padX.length * 2));
      if (collapsed) {
        return [`${padX}${styledHeaderLine(state, theme, innerWidth)}`];
      }
      return [
        `${padX}${styledHeaderLine(state, theme, innerWidth)}`,
        ...events.slice(0, shown).map((ev) => `${padX}${styledEventLine(ev, theme, innerWidth)}`),
      ].slice(0, MAX_LINES);
    },
    describe() {
      return describeActivityNode(state, events, shown, collapsed);
    },
    component(theme, tui = null) {
      let cachedWidth = -1;
      let cachedRows = null;
      let cachedNode = null;
      return {
        render(width = MAX_WIDTH) {
          if (cachedRows && cachedWidth === width) return cachedRows;
          cachedWidth = width;
          cachedRows = api.ansiLines(theme, width);
          return cachedRows;
        },
        describe() {
          cachedNode ??= api.describe();
          return cachedNode;
        },
        handleNativeEvent(event) {
          if (event?.type === "toggle" && typeof event.collapsed === "boolean") {
            collapsed = event.collapsed;
            cachedRows = null;
            cachedNode = null;
            tui?.requestRender?.();
          }
        },
        invalidate() {
          cachedWidth = -1;
          cachedRows = null;
          cachedNode = null;
        },
        dispose() {},
      };
    },
    events,
  };
  return api;
}
