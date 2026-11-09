import type { DesktopEvent, DesktopState, RpcFrame } from '../contracts.ts';
import type { DesktopUiApi, WidgetContext } from './api.ts';

/**
 * Built-in renderer panels for the OMP workflow surfaces a desktop host needs:
 * session history/tree, subagents, goal mode, queued input, and the per-bot
 * `.omp` configuration tree.
 *
 * Every runtime read goes through `api.command('runtime.command', ...)`, using
 * the context captured when the action started; a response never lands in a
 * conversation the user switched to afterwards.
 */

type PanelContext = WidgetContext;

const CONTEXT_SEPARATOR = '\u0000';
const RUNTIME_COMMAND = 'runtime.command';
const ROOT_REFRESH_MS = 5000;
const REFRESH_DEBOUNCE_MS = 250;

const COLOR = {
  fg: 'var(--onlyne-fg, light-dark(#1f2328, #e6e8eb))',
  muted: 'var(--onlyne-muted, light-dark(#667085, #98a2b3))',
  surface: 'var(--onlyne-surface, light-dark(#ffffff, #171a1f))',
  subtle: 'var(--onlyne-subtle, light-dark(#f5f6f8, #20242b))',
  border: 'var(--onlyne-border, light-dark(#d9dde5, #323844))',
  accent: 'var(--onlyne-accent, #3b82f6)',
  danger: 'var(--onlyne-danger, #d92d20)',
} as const;

const STYLE = {
  panel: `display:flex;flex-direction:column;gap:16px;padding:0;box-sizing:border-box;width:100%;height:100%;overflow:auto;font:13px/1.45 var(--font),sans-serif;color:${COLOR.fg};`,
  section: `display:flex;flex-direction:column;gap:8px;padding:10px;border:1px solid ${COLOR.border};border-radius:10px;background:${COLOR.surface};`,
  row: 'display:flex;flex-wrap:wrap;gap:6px;align-items:center;',
  stack: 'display:flex;flex-direction:column;gap:8px;',
  title: 'font-size:13px;font-weight:650;',
  muted: `font-size:12px;color:${COLOR.muted};white-space:pre-wrap;overflow-wrap:anywhere;`,
  text: 'white-space:pre-wrap;overflow-wrap:anywhere;',
  monospace: 'font:12px/1.45 ui-monospace,SFMono-Regular,Menlo,monospace;',
  button: `font:12px/1.2 system-ui;padding:4px 8px;border-radius:6px;border:1px solid ${COLOR.border};background:${COLOR.subtle};color:inherit;cursor:pointer;`,
  primary: `font:12px/1.2 system-ui;padding:4px 8px;border-radius:6px;border:1px solid ${COLOR.accent};background:transparent;color:${COLOR.accent};font-weight:600;cursor:pointer;`,
  danger: `font:12px/1.2 system-ui;padding:4px 8px;border-radius:6px;border:1px solid ${COLOR.danger};background:transparent;color:${COLOR.danger};cursor:pointer;`,
  input: `font:12px/1.45 var(--mono),monospace;padding:6px 8px;border-radius:6px;border:1px solid ${COLOR.border};background:var(--field);color:var(--fg);width:100%;box-sizing:border-box;resize:vertical;`,
  grid: 'display:grid;grid-template-columns:max-content minmax(0,1fr);gap:4px 10px;font-size:12px;align-items:baseline;',
  card: `display:flex;flex-direction:column;gap:6px;padding:8px;border:1px solid ${COLOR.border};border-radius:8px;background:${COLOR.subtle};`,
} as const;

interface OmpPanel {
  render(state: DesktopState): HTMLElement;
  syncContext(): void;
  notify(event: DesktopEvent): void;
  dispose(): void;
}

/* ------------------------------------------------------------------ */
/* shared helpers                                                       */
/* ------------------------------------------------------------------ */

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function stringField(value: unknown, key: string): string | undefined {
  if (!isRecord(value)) return undefined;
  const field = value[key];
  return typeof field === 'string' && field.length > 0 ? field : undefined;
}

function asArray(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function contextKey(context: PanelContext): string {
  return `${context.conversationId}${CONTEXT_SEPARATOR}${context.botId}`;
}

function eventContext(event: DesktopEvent): PanelContext | undefined {
  if (typeof event.conversationId !== 'string' || typeof event.botId !== 'string') return undefined;
  return { conversationId: event.conversationId, botId: event.botId };
}

function eventFrame(event: DesktopEvent): RpcFrame | undefined {
  return isRecord(event.frame) ? (event.frame as RpcFrame) : undefined;
}

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  style: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (style.length > 0) node.setAttribute('style', style);
  if (text !== undefined) node.textContent = text;
  return node;
}

interface ActionOptions {
  primary?: boolean;
  danger?: boolean;
  disabled?: boolean;
  reason?: string;
}

function action(label: string, onClick: () => void, options: ActionOptions = {}): HTMLButtonElement {
  const node = el('button', options.danger === true ? STYLE.danger : options.primary === true ? STYLE.primary : STYLE.button, label);
  node.type = 'button';
  node.disabled = options.disabled === true;
  if (options.reason !== undefined) node.title = options.reason;
  if (!node.disabled) node.addEventListener('click', onClick);
  return node;
}

function sectionHead(title: string, actions: HTMLElement[]): HTMLElement {
  const head = el('div', STYLE.row);
  head.append(el('div', STYLE.title, title), el('span', 'flex:1;'));
  for (const node of actions) head.append(node);
  return head;
}

const OMP_LAUNCH = 'omp --mode rpc-ui';

interface StatusLine {
  node: HTMLElement;
  show(message: string | undefined, tone?: 'muted' | 'error', missing?: boolean): void;
}

function createStatus(): StatusLine {
  const node = el('div', '');
  node.className = 'desk-status';
  node.setAttribute('aria-live', 'polite');
  return {
    node,
    show(message, tone = 'muted', missing = false) {
      node.replaceChildren();
      if (message === undefined) {
        node.hidden = true;
        return;
      }
      node.hidden = false;
      if (!missing) {
        node.className = tone === 'error' ? 'desk-status err' : 'desk-status';
        node.textContent = message;
        return;
      }
      node.className = 'runtime-notice';
      node.setAttribute('role', 'note');
      const copy = el('button', '', 'Copy command');
      copy.type = 'button';
      copy.className = 'btn sm';
      copy.addEventListener('click', () => {
        void navigator.clipboard?.writeText(OMP_LAUNCH).then(
          () => { copy.textContent = 'Copied'; },
          () => { copy.textContent = 'Copy failed'; },
        );
      });
      const command = el('code', '', OMP_LAUNCH);
      command.className = 'runtime-command';
      node.append(
        el('strong', '', 'No model runtime'),
        el('p', '', 'Install OMP so the command below is on PATH, then refresh this panel. The controls stay off until a runtime can start.'),
        command,
        copy,
      );
    },
  };
}

function panelStatus(state: { busy?: string; error?: string; notice?: string; loading?: boolean }): {
  text: string | undefined;
  error: boolean;
  missing: boolean;
} {
  if (state.error !== undefined) {
    const missing = /executable not found|ENOENT|\bomp\b/i.test(state.error);
    if (missing) return { text: 'No model runtime is available. Install OMP and put omp on PATH. These controls stay off until a runtime can start.', error: false, missing: true };
    return { text: state.error, error: true, missing: false };
  }
  if (state.busy !== undefined) return { text: state.busy, error: false, missing: false };
  if (state.loading === true) return { text: 'Loading…', error: false, missing: false };
  if (state.notice !== undefined) return { text: state.notice, error: false, missing: false };
  return { text: undefined, error: false, missing: false };
}

function lockRuntime(root: HTMLElement, missing: boolean): void {
  if (!missing) return;
  for (const control of root.querySelectorAll<HTMLButtonElement | HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>('button, input, textarea, select')) {
    if (control.closest('.runtime-notice')) continue;
    control.disabled = true;
    control.style.setProperty('color', 'var(--fg-soft)', 'important');
    control.style.setProperty('background', 'var(--surface-3)', 'important');
    control.style.setProperty('border-color', 'var(--line)', 'important');
    control.style.setProperty('opacity', '1', 'important');
    control.style.setProperty('cursor', 'not-allowed', 'important');
    control.style.setProperty('box-shadow', 'none', 'important');
  }
}

function messageSummary(value: unknown): HistoryMessage {
  if (!isRecord(value)) return { role: 'unknown', text: '', raw: value };
  const role = typeof value.role === 'string' ? value.role : 'unknown';
  const parts: string[] = [];
  const content = value.content;
  if (typeof content === 'string') parts.push(content);
  else if (Array.isArray(content)) {
    for (const part of content) {
      if (typeof part === 'string') {
        parts.push(part);
        continue;
      }
      if (!isRecord(part)) continue;
      if (part.type === 'text' && typeof part.text === 'string') parts.push(part.text);
      else if (part.type === 'thinking' && typeof part.thinking === 'string') parts.push(`[thinking] ${part.thinking}`);
      else if (part.type === 'toolCall') {
        const name = stringField(part, 'name') ?? '?';
        parts.push(`[tool ${name}] ${JSON.stringify(part.arguments ?? {})}`);
      } else if (part.type === 'image') parts.push('[image]');
    }
  }
  if (role === 'toolResult') {
    const name = stringField(value, 'toolName');
    parts.unshift(`[tool result${name === undefined ? '' : ` ${name}`}]`);
  }
  const at = formatClock(value.createdAt ?? value.at);
  return { role, text: parts.join('\n'), at, raw: value };
}

function truncate(text: string, limit: number): string {
  return text.length <= limit ? text : `${text.slice(0, limit)}…`;
}

function formatClock(value: unknown): string {
  const time = typeof value === 'number' ? value : typeof value === 'string' ? Date.parse(value) : Number.NaN;
  if (!Number.isFinite(time)) return '';
  const date = new Date(time);
  return Number.isNaN(date.getTime()) ? '' : date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
}

function humanizeKey(key: string): string {
  const spaced = key.replace(/[_-]+/g, ' ').replace(/([a-z0-9])([A-Z])/g, '$1 $2');
  return spaced.charAt(0).toUpperCase() + spaced.slice(1);
}

function scalarPairs(source: Record<string, unknown> | undefined, skip: Record<string, true>): Array<[string, string]> {
  const pairs: Array<[string, string]> = [];
  if (source === undefined) return pairs;
  for (const [key, value] of Object.entries(source)) {
    if (skip[key] === true) continue;
    if (typeof value === 'string') {
      if (value.length === 0 || value.length > 200) continue;
      pairs.push([humanizeKey(key), value]);
    } else if (typeof value === 'number' || typeof value === 'boolean') {
      pairs.push([humanizeKey(key), String(value)]);
    }
    if (pairs.length >= 12) break;
  }
  return pairs;
}

interface DraftImage {
  mimeType: string;
  data: string;
}

function readImages(value: unknown): DraftImage[] {
  const images: DraftImage[] = [];
  for (const item of asArray(value)) {
    if (!isRecord(item) || item.type !== 'image') continue;
    const data = stringField(item, 'data');
    if (data === undefined) continue;
    images.push({ mimeType: stringField(item, 'mimeType') ?? 'image/png', data });
  }
  return images;
}

function imagePayload(images: DraftImage[]): Array<Record<string, unknown>> {
  return images.map(image => ({ type: 'image', mimeType: image.mimeType, data: image.data }));
}

function imageStrip(images: DraftImage[], onRemove?: (index: number) => void): HTMLElement {
  const strip = el('div', STYLE.row);
  images.forEach((image, index) => {
    const chip = el('span', `display:inline-flex;align-items:center;gap:6px;padding:3px 6px;border:1px solid ${COLOR.border};border-radius:6px;background:${COLOR.surface};`);
    const preview = el('img', 'width:28px;height:28px;object-fit:cover;border-radius:4px;');
    preview.src = `data:${image.mimeType};base64,${image.data}`;
    preview.alt = 'attachment preview';
    const bytes = Math.round((image.data.length * 3) / 4);
    chip.append(preview, el('span', STYLE.muted, `${image.mimeType} · ${bytes} B`));
    if (onRemove !== undefined) chip.append(action('Remove', () => onRemove(index)));
    strip.append(chip);
  });
  return strip;
}

function pairGrid(pairs: Array<[string, string]>): HTMLElement {
  const grid = el('div', STYLE.grid);
  for (const [label, value] of pairs) {
    grid.append(el('span', STYLE.muted, label), el('span', STYLE.text, value));
  }
  return grid;
}

async function runtimeCommand(
  api: DesktopUiApi,
  context: PanelContext,
  frame: RpcFrame,
): Promise<Record<string, unknown>> {
  const response = await api.command<RpcFrame>(RUNTIME_COMMAND, {
    conversationId: context.conversationId,
    botId: context.botId,
    command: frame,
  });
  return isRecord(response.data) ? response.data : {};
}

async function appCommand(
  api: DesktopUiApi,
  actionName: string,
  payload: Record<string, unknown>,
): Promise<Record<string, unknown>> {
  const data = await api.command<unknown>(actionName, payload);
  return isRecord(data) ? data : {};
}

/* ------------------------------------------------------------------ */
/* session history and tree                                             */
/* ------------------------------------------------------------------ */

interface HistoryMessage {
  role: string;
  text: string;
  at?: string;
  raw?: unknown;
}

interface SessionTreeNode {
  entry: Record<string, unknown> | undefined;
  label: string | undefined;
  children: SessionTreeNode[];
}

interface HistoryState {
  context: PanelContext;
  loaded: boolean;
  loading: boolean;
  loadedAt: number;
  session: Record<string, unknown> | undefined;
  stats: Record<string, unknown> | undefined;
  tree: SessionTreeNode[];
  entries: Array<Record<string, unknown>>;
  leafId: string | undefined;
  messages: HistoryMessage[];
  totalMessages: number | undefined;
  nextCursor: string | undefined;
  selected: string | undefined;
  draftText: string;
  draftImages: DraftImage[];
  summarize: boolean;
  armedNewSession: boolean;
  busy: string | undefined;
  error: string | undefined;
  notice: string | undefined;
  deskOnly?: boolean;
}

const HISTORY_REFRESH: Record<string, true> = {
  agent_end: true,
  session_tree: true,
  session_info_update: true,
  model_changed: true,
};

function parseTree(value: unknown, depth = 0): SessionTreeNode[] {
  if (!Array.isArray(value) || depth > 256) return [];
  const nodes: SessionTreeNode[] = [];
  for (const item of value) {
    if (!isRecord(item)) continue;
    nodes.push({
      entry: isRecord(item.entry) ? item.entry : undefined,
      label: stringField(item, 'label'),
      children: parseTree(item.children, depth + 1),
    });
  }
  return nodes;
}

function entryLine(entry: Record<string, unknown>): { kind: string; text: string } {
  const type = typeof entry.type === 'string' ? entry.type : 'entry';
  if (type === 'message') {
    const summary = messageSummary(entry.message);
    return { kind: summary.role, text: summary.text };
  }
  for (const key of ['summary', 'shortSummary', 'label', 'model', 'customType', 'name']) {
    const value = stringField(entry, key);
    if (value !== undefined) return { kind: type, text: value };
  }
  return { kind: type, text: '' };
}

function activePath(entries: Array<Record<string, unknown>>, leafId: string | undefined): Set<string> {
  const parents = new Map<string, string | undefined>();
  for (const entry of entries) {
    const id = stringField(entry, 'id');
    if (id === undefined) continue;
    parents.set(id, stringField(entry, 'parentId'));
  }
  const path = new Set<string>();
  let cursor = leafId;
  while (cursor !== undefined && !path.has(cursor)) {
    path.add(cursor);
    cursor = parents.get(cursor);
  }
  return path;
}

function sessionPairs(
  session: Record<string, unknown> | undefined,
  stats: Record<string, unknown> | undefined,
  total: number | undefined,
): Array<[string, string]> {
  const pairs: Array<[string, string]> = [];
  if (session !== undefined) {
    const model = isRecord(session.model) ? session.model : undefined;
    if (model !== undefined) pairs.push(['Model', `${stringField(model, 'provider') ?? '?'}/${stringField(model, 'id') ?? '?'}`]);
    const thinking = stringField(session, 'thinkingLevel');
    if (thinking !== undefined) pairs.push(['Thinking', thinking]);
    if (total !== undefined) pairs.push(['Transcript messages', String(total)]);
    else if (typeof session.messageCount === 'number') pairs.push(['Messages', String(session.messageCount)]);
    const usage = isRecord(session.contextUsage) ? session.contextUsage : undefined;
    if (usage !== undefined) {
      const tokens = typeof usage.tokens === 'number' ? usage.tokens : undefined;
      const window = typeof usage.contextWindow === 'number' ? usage.contextWindow : undefined;
      const percent = typeof usage.percent === 'number' ? usage.percent : undefined;
      if (tokens !== undefined) {
        pairs.push(['Context', `${tokens}${window === undefined ? '' : ` / ${window}`}${percent === undefined ? '' : ` (${percent}%)`}`]);
      }
    }
    if (typeof session.isStreaming === 'boolean') pairs.push(['Turn', session.isStreaming ? 'streaming' : session.isSettled === false ? 'busy' : 'idle']);
    if (typeof session.queuedMessageCount === 'number') pairs.push(['Queued input', String(session.queuedMessageCount)]);
    const name = stringField(session, 'sessionName');
    if (name !== undefined) pairs.push(['Session', name]);
    const id = stringField(session, 'sessionId');
    if (id !== undefined) pairs.push(['Session id', id]);
    const file = stringField(session, 'sessionFile');
    if (file !== undefined) pairs.push(['File', file.split(/[\\/]/).at(-1) ?? file]);
  }
  pairs.push(...scalarPairs(stats, { model: true, usage: true, cost: true, goal: true, contextUsage: true }));
  return pairs;
}

function createHistoryPanel(api: DesktopUiApi): OmpPanel {
  const stores = new Map<string, HistoryState>();
  let root: HTMLElement | undefined;
  let mounted: { key: string; element: HTMLElement; paint(): void } | undefined;
  let refreshTimer: ReturnType<typeof setTimeout> | undefined;

  const paintFor = (key: string): void => {
    if (mounted?.key === key) mounted.paint();
  };

  function storeFor(context: PanelContext): HistoryState {
    const key = contextKey(context);
    let state = stores.get(key);
    if (state === undefined) {
      state = {
        context, loaded: false, loading: false, loadedAt: 0, session: undefined, stats: undefined,
        tree: [], entries: [], leafId: undefined, messages: [], totalMessages: undefined, nextCursor: undefined,
        selected: undefined, draftText: '', draftImages: [], summarize: false, armedNewSession: false,
        busy: undefined, error: undefined, notice: undefined,
      };
      stores.set(key, state);
    }
    return state;
  }

  async function load(state: HistoryState): Promise<void> {
    if (state.loading) return;
    const key = contextKey(state.context);
    const context = state.context;
    state.loading = true;
    state.error = undefined;
    paintFor(key);
    const results = await Promise.all([
      runtimeCommand(api, context, { type: 'get_state' }).then(data => ({ label: 'State', data }), error => ({ label: 'State', error: errorText(error) })),
      runtimeCommand(api, context, { type: 'get_tree' }).then(data => ({ label: 'Tree', data }), error => ({ label: 'Tree', error: errorText(error) })),
      runtimeCommand(api, context, { type: 'get_entries' }).then(data => ({ label: 'Entries', data }), error => ({ label: 'Entries', error: errorText(error) })),
      runtimeCommand(api, context, { type: 'get_messages_page', limit: 60 }).then(data => ({ label: 'Messages', data }), error => ({ label: 'Messages', error: errorText(error) })),
      runtimeCommand(api, context, { type: 'get_session_stats' }).then(data => ({ label: 'Stats', data }), error => ({ label: 'Stats', error: errorText(error) })),
    ]);
    const errors: string[] = [];
    for (const result of results) {
      if ('error' in result) {
        const busySnapshot = result.label === 'Messages' && /busy|compacting/i.test(result.error);
        if (busySnapshot) state.notice ??= 'Transcript snapshot unavailable while the session is busy; refresh after the turn.';
        else if (result.label !== 'Stats') errors.push(`${result.label}: ${result.error}`);
        continue;
      }
      const data = result.data;
      if (result.label === 'State') state.session = data;
      else if (result.label === 'Stats') state.stats = data;
      else if (result.label === 'Tree') {
        state.tree = parseTree(data.tree);
        state.leafId = stringField(data, 'leafId');
      } else if (result.label === 'Entries') {
        state.entries = asArray(data.entries).filter(isRecord);
        if (state.leafId === undefined) state.leafId = stringField(data, 'leafId');
      } else if (result.label === 'Messages') {
        state.messages = asArray(data.messages).map(messageSummary);
        state.totalMessages = typeof data.totalMessages === 'number' ? data.totalMessages : undefined;
        state.nextCursor = stringField(data, 'nextCursor');
      }
    }
    const runtimeMissing = errors.some(line => /executable not found|ENOENT|omp/i.test(line));
    if (state.messages.length === 0) {
      const desk = api.state().messages.filter(item => item.conversationId === context.conversationId);
      if (desk.length > 0 && (runtimeMissing || errors.length > 0)) {
        state.messages = desk.map(item => ({ role: item.role, text: item.text, at: formatClock(item.createdAt), raw: { role: item.role, text: item.text, createdAt: item.createdAt } }));
        state.deskOnly = true;
        state.notice = 'Showing the desk transcript. A model session, with its tree, starts when OMP can run.';
        state.error = undefined;
        state.loading = false;
        state.loaded = true;
        state.loadedAt = Date.now();
        paintFor(key);
        return;
      }
    }
    state.error = errors.length > 0 ? errors.join('\n') : undefined;
    state.loading = false;
    state.loaded = true;
    state.loadedAt = Date.now();
    paintFor(key);
  }

  function scheduleRefresh(key: string): void {
    clearTimeout(refreshTimer);
    refreshTimer = setTimeout(() => {
      refreshTimer = undefined;
      const state = stores.get(key);
      if (state !== undefined) void load(state);
    }, REFRESH_DEBOUNCE_MS);
  }

  function build(context: PanelContext, state: HistoryState): { key: string; element: HTMLElement; paint(): void } {
    const key = contextKey(context);
    const element = el('div', STYLE.stack);
    const status = createStatus();

    const refreshButton = action('Refresh', () => void load(state));
    const sessionSection = el('section', STYLE.section);
    const sessionBody = el('div', STYLE.stack);
    sessionSection.append(sectionHead('Session', [refreshButton]), sessionBody);

    const loadMoreButton = action('Load more', () => void loadMore());
    const historySection = el('section', STYLE.section);
    const historyBody = el('div', STYLE.stack);
    historySection.append(sectionHead('Transcript', [loadMoreButton]), historyBody);

    const treeSection = el('section', STYLE.section);
    const treeBody = el('div', STYLE.stack);
    treeBody.setAttribute('role', 'tree');
    treeBody.setAttribute('aria-label', 'Session tree');
    treeSection.append(el('div', STYLE.title, 'Session tree'), treeBody);

    const entrySection = el('section', STYLE.section);
    const entryBody = el('div', STYLE.stack);
    const summarizeLabel = el('label', `display:flex;gap:6px;align-items:center;font-size:12px;color:${COLOR.muted};`);
    const summarizeInput = el('input', '');
    summarizeInput.type = 'checkbox';
    summarizeLabel.append(summarizeInput, el('span', '', 'Summarize the abandoned branch'));
    const navigateButton = action('Navigate here', () => void sessionAction('navigate_tree'));
    const branchButton = action('Branch to new session', () => void sessionAction('branch'));
    const forkButton = action('Fork session here', () => void sessionAction('fork'));
    const entryActions = el('div', STYLE.row);
    entryActions.append(navigateButton, branchButton, forkButton, summarizeLabel);
    entrySection.append(el('div', STYLE.title, 'Selected entry'), entryBody, entryActions);

    const draftSection = el('section', STYLE.section);
    const draftBody = el('div', STYLE.stack);
    const draftInput = el('textarea', STYLE.input);
    draftInput.rows = 3;
    draftInput.setAttribute('aria-label', 'Session draft');
    draftInput.placeholder = 'Draft restored from session navigation';
    const draftImagesBox = el('div', STYLE.row);
    const draftSend = action('Send draft', () => void sendDraft(), { primary: true });
    const draftDiscard = action('Discard', () => {
      state.draftText = '';
      state.draftImages = [];
      paint();
    });
    const draftActions = el('div', STYLE.row);
    draftActions.append(draftSend, draftDiscard);
    draftBody.append(
      el('div', STYLE.muted, 'Session navigation returns the prompt it rewound past. Nothing is sent until you choose to.'),
      draftInput, draftImagesBox, draftActions,
    );
    draftSection.append(el('div', STYLE.title, 'Draft'), draftBody);

    const newSessionButton = action('New session', () => onNewSession());
    const reloadButton = action('Reload history', () => void load(state));
    const sessionActionBody = el('div', STYLE.row);
    sessionActionBody.append(newSessionButton, reloadButton);
    const sessionActionSection = el('section', STYLE.section);
    sessionActionSection.append(el('div', STYLE.title, 'Session actions'), sessionActionBody);

    element.classList.add('library-stack');
    element.append(status.node, sessionSection, historySection, treeSection, entrySection, draftSection, sessionActionSection);

    function paint(): void {
      summarizeInput.checked = state.summarize;
      summarizeInput.disabled = state.busy !== undefined;
      if (draftInput.value !== state.draftText) draftInput.value = state.draftText;
      draftImagesBox.replaceChildren();
      if (state.draftImages.length > 0) {
        draftImagesBox.append(imageStrip(state.draftImages, index => {
          state.draftImages = state.draftImages.filter((_, position) => position !== index);
          paint();
        }));
      }
      draftSection.style.display = state.draftText.length > 0 || state.draftImages.length > 0 ? '' : 'none';
      draftSend.disabled = state.busy !== undefined || state.draftText.trim().length === 0;
      draftDiscard.disabled = state.busy !== undefined;

      sessionBody.replaceChildren();
      const pairs = sessionPairs(state.session, state.stats, state.totalMessages);
      if (state.deskOnly) sessionBody.append(el('div', STYLE.muted, 'This is the desk transcript. There is no model session yet, so there is no session tree.'));
      else sessionBody.append(pairs.length === 0 ? el('div', STYLE.muted, 'The model session has not reported state yet.') : pairGrid(pairs));

      historyBody.replaceChildren();
      if (state.messages.length === 0) {
        const empty = el('div', '');
        empty.className = 'desk-empty';
        empty.append(el('h2', '', 'No transcript yet'), el('p', '', 'Messages appear here after someone writes in this conversation.'));
        historyBody.append(empty);
      }
      for (const message of state.messages) {
        const card = el('article', '');
        card.className = 'timeline-item';
        const role = el('span', '', message.role);
        role.className = 'chip';
        const when = el('time', '', message.at ?? '');
        when.className = 'timeline-time';
        const text = el('p', '', message.text);
        text.className = 'timeline-text';
        const raw = el('pre', '', JSON.stringify(message.raw ?? message, null, 2));
        raw.className = 'desk-pre';
        raw.hidden = true;
        const toggle = el('button', '', 'Show raw');
        toggle.type = 'button';
        toggle.className = 'btn sm';
        toggle.addEventListener('click', () => {
          raw.hidden = !raw.hidden;
          toggle.textContent = raw.hidden ? 'Show raw' : 'Hide raw';
        });
        const head = el('div', '');
        head.className = 'timeline-head';
        head.append(role, when);
        card.append(head, text, toggle, raw);
        historyBody.append(card);
      }
      loadMoreButton.disabled = state.busy !== undefined || state.nextCursor === undefined;
      loadMoreButton.title = state.nextCursor === undefined ? 'No further pages' : 'Load the next page';

      treeBody.replaceChildren();
      if (state.deskOnly) treeBody.append(el('div', STYLE.muted, 'The model session has not started.'));
      else if (state.tree.length === 0) treeBody.append(el('div', STYLE.muted, 'The model session has not started, so there is no tree.'));
      else {
        const path = activePath(state.entries, state.leafId);
        treeBody.append(...treeRows(state.tree, 1, path));
      }

      const selectedEntry = state.selected === undefined
        ? undefined
        : state.entries.find(entry => stringField(entry, 'id') === state.selected) ?? findTreeEntry(state.tree, state.selected);
      entryBody.replaceChildren();
      if (selectedEntry === undefined) entryBody.append(el('div', STYLE.muted, 'Select an entry in the tree to navigate, branch or fork.'));
      else {
        const line = entryLine(selectedEntry);
        entryBody.append(pairGrid([
          ['Entry id', stringField(selectedEntry, 'id') ?? '?'],
          ['Kind', line.kind],
          ['Preview', truncate(line.text, 400) || '—'],
        ]));
      }
      const busy = state.busy !== undefined;
      navigateButton.disabled = busy || selectedEntry === undefined;
      branchButton.disabled = busy || selectedEntry === undefined;
      forkButton.disabled = busy || selectedEntry === undefined;
      navigateButton.title = 'Move the leaf in this session file (rewind)';
      branchButton.title = 'Create a branched session file at this entry';
      forkButton.title = 'Create a new session file from this entry';

      newSessionButton.textContent = state.armedNewSession ? 'Confirm new session' : 'New session';
      newSessionButton.setAttribute('style', state.armedNewSession ? STYLE.danger : STYLE.button);
      newSessionButton.disabled = busy;
      reloadButton.disabled = busy || state.loading;
      refreshButton.disabled = busy || state.loading;

      const line = panelStatus(state);
      status.show(line.text, line.error ? 'error' : 'muted', line.missing);
      const runtimeHost = status.node.parentElement;
      if (runtimeHost) lockRuntime(runtimeHost, line.missing);
    }

    function treeRows(nodes: SessionTreeNode[], level: number, path: Set<string>): HTMLElement[] {
      const rows: HTMLElement[] = [];
      for (const node of nodes) {
        const entry = node.entry;
        if (entry === undefined) continue;
        const id = stringField(entry, 'id');
        if (id === undefined) continue;
        const line = entryLine(entry);
        const selected = state.selected === id;
        const onPath = path.has(id);
        const row = el('div', `display:flex;gap:6px;align-items:baseline;padding:4px 6px;padding-left:${6 + (level - 1) * 14}px;border:1px solid ${selected ? COLOR.accent : 'transparent'};border-radius:6px;background:${selected ? COLOR.subtle : 'transparent'};cursor:pointer;${STYLE.monospace}${onPath ? 'font-weight:600;' : ''}`);
        row.setAttribute('role', 'treeitem');
        row.setAttribute('aria-level', String(level));
        row.setAttribute('aria-selected', selected ? 'true' : 'false');
        row.tabIndex = 0;
        row.append(el('span', `color:${COLOR.accent};`, `${onPath ? '•' : ' '}${id === state.leafId ? '›' : ' '}`));
        if (node.label !== undefined) row.append(el('span', `color:${COLOR.accent};`, `[${node.label}]`));
        row.append(
          el('span', `color:${COLOR.muted};`, `${line.kind}:`),
          el('span', 'overflow-wrap:anywhere;', truncate(line.text, 160) || '—'),
          el('span', `margin-left:auto;color:${COLOR.muted};`, formatClock(entry.timestamp)),
        );
        const select = (): void => {
          state.selected = id;
          paint();
        };
        row.addEventListener('click', select);
        row.addEventListener('keydown', event => {
          if (event.key !== 'Enter' && event.key !== ' ') return;
          event.preventDefault();
          select();
        });
        rows.push(row, ...treeRows(node.children, level + 1, path));
      }
      return rows;
    }

    function findTreeEntry(nodes: SessionTreeNode[], id: string): Record<string, unknown> | undefined {
      for (const node of nodes) {
        if (node.entry !== undefined && stringField(node.entry, 'id') === id) return node.entry;
        const nested = findTreeEntry(node.children, id);
        if (nested !== undefined) return nested;
      }
      return undefined;
    }

    async function loadMore(): Promise<void> {
      if (state.nextCursor === undefined || state.busy !== undefined) return;
      state.busy = 'Loading older messages…';
      state.error = undefined;
      paint();
      try {
        const data = await runtimeCommand(api, context, { type: 'get_messages_page', cursor: state.nextCursor, limit: 60 });
        state.messages = [...state.messages, ...asArray(data.messages).map(messageSummary)];
        if (typeof data.totalMessages === 'number') state.totalMessages = data.totalMessages;
        state.nextCursor = stringField(data, 'nextCursor');
      } catch (error) {
        state.error = errorText(error);
      } finally {
        state.busy = undefined;
        paintFor(key);
      }
    }

    async function sendDraft(): Promise<void> {
      const message = state.draftText.trim();
      if (message.length === 0) {
        state.error = 'Draft is empty.';
        paintFor(key);
        return;
      }
      state.busy = 'Sending draft…';
      state.error = undefined;
      state.notice = undefined;
      paint();
      const images = state.draftImages;
      try {
        await runtimeCommand(api, context, { type: 'prompt', message, ...(images.length > 0 ? { images: imagePayload(images) } : {}) });
        state.draftText = '';
        state.draftImages = [];
        state.notice = 'Draft sent.';
      } catch (error) {
        state.error = errorText(error);
      } finally {
        state.busy = undefined;
        paintFor(key);
      }
    }

    async function sessionAction(kind: 'navigate_tree' | 'branch' | 'fork'): Promise<void> {
      const entryId = state.selected;
      if (entryId === undefined || state.busy !== undefined) return;
      const selectedEntry = state.entries.find(entry => stringField(entry, 'id') === entryId) ?? findTreeEntry(state.tree, entryId);
      const rewoundUserMessage = selectedEntry !== undefined && entryLine(selectedEntry).kind === 'user';
      state.busy = kind === 'navigate_tree' ? 'Navigating…' : kind === 'branch' ? 'Branching…' : 'Forking…';
      state.error = undefined;
      state.notice = undefined;
      paint();
      try {
        const frame: RpcFrame = { type: kind, entryId };
        if (kind === 'navigate_tree') frame.summarize = state.summarize;
        const data = await runtimeCommand(api, context, frame);
        if (data.cancelled === true) {
          state.notice = data.aborted === true ? 'Branch summary aborted; nothing moved.' : 'The runtime cancelled this session change.';
        } else if (kind === 'navigate_tree') {
          const editorText = stringField(data, 'editorText');
          if (editorText !== undefined) {
            state.draftText = editorText;
            state.notice = 'Rewound; the earlier prompt is in the draft below.';
          } else if (rewoundUserMessage) {
            state.notice = 'Rewound past a user message; the runtime returned no draft text to restore.';
          } else {
            state.notice = 'Session leaf moved.';
          }
        } else {
          state.notice = kind === 'branch' ? 'Branched into a new session file.' : 'Forked into a new session file.';
        }
        state.selected = undefined;
      } catch (error) {
        state.error = errorText(error);
      } finally {
        state.busy = undefined;
        paintFor(key);
        if (state.error === undefined) void load(state);
      }
    }

    function onNewSession(): void {
      if (state.busy !== undefined) return;
      if (!state.armedNewSession) {
        state.armedNewSession = true;
        state.notice = 'Press again to start a fresh session in this conversation.';
        paint();
        return;
      }
      state.armedNewSession = false;
      void (async () => {
        state.busy = 'Starting a new session…';
        state.error = undefined;
        state.notice = undefined;
        paint();
        try {
          const data = await runtimeCommand(api, context, { type: 'new_session' });
          state.notice = data.cancelled === true ? 'The runtime cancelled the new session.' : 'New session started.';
          state.selected = undefined;
          state.tree = [];
          state.entries = [];
          state.messages = [];
          state.nextCursor = undefined;
          await load(state);
        } catch (error) {
          state.error = errorText(error);
        } finally {
          state.busy = undefined;
          paintFor(key);
        }
      })();
    }

    draftInput.addEventListener('input', () => {
      state.draftText = draftInput.value;
      draftSend.disabled = state.draftText.trim().length === 0;
    });

    return { key, element, paint };
  }

  function render(state: DesktopState): HTMLElement {
    root ??= el('div', STYLE.panel);
    const context = api.activeContext();
    if (context === undefined) {
      mounted = undefined;
      root.replaceChildren(el('div', STYLE.muted, state.conversations.length === 0
        ? 'Create a conversation to use the session panel.'
        : 'Select a conversation to use the session panel.'));
      return root;
    }
    const key = contextKey(context);
    if (mounted?.key === key) {
      mounted.paint();
      return root;
    }
    const store = storeFor(context);
    mounted = build(context, store);
    root.replaceChildren(mounted.element);
    if (!store.loaded || Date.now() - store.loadedAt > ROOT_REFRESH_MS) void load(store);
    else mounted.paint();
    return root;
  }

  return {
    render,
    syncContext(): void {
      if (root !== undefined && mounted !== undefined) render(api.state());
    },
    notify(event: DesktopEvent): void {
      if (event.type !== 'rpc') return;
      const context = eventContext(event);
      const frame = eventFrame(event);
      if (context === undefined || frame === undefined) return;
      const key = contextKey(context);
      if (mounted?.key !== key) return;
      if (typeof frame.type === 'string' && HISTORY_REFRESH[frame.type] === true) scheduleRefresh(key);
    },
    dispose(): void {
      clearTimeout(refreshTimer);
      refreshTimer = undefined;
      stores.clear();
      mounted = undefined;
      root = undefined;
    },
  };
}

/* ------------------------------------------------------------------ */
/* subagents                                                            */
/* ------------------------------------------------------------------ */

interface AgentRecord {
  id: string;
  agent: string | undefined;
  status: string | undefined;
  detached: boolean | undefined;
  parentToolCallId: string | undefined;
  sessionFile: string | undefined;
  task: string | undefined;
}

interface TranscriptState {
  fromByte: number;
  messages: HistoryMessage[];
  error: string | undefined;
}

interface SubagentState {
  context: PanelContext;
  loadedAt: number;
  loading: boolean;
  agents: AgentRecord[];
  selected: string | undefined;
  steerText: string;
  transcripts: Map<string, TranscriptState>;
  busy: string | undefined;
  error: string | undefined;
  notice: string | undefined;
}

function readAgent(value: unknown): AgentRecord | undefined {
  if (!isRecord(value)) return undefined;
  const id = stringField(value, 'id');
  if (id === undefined) return undefined;
  return {
    id,
    agent: stringField(value, 'agent'),
    status: stringField(value, 'status'),
    detached: typeof value.detached === 'boolean' ? value.detached : undefined,
    parentToolCallId: stringField(value, 'parentToolCallId'),
    sessionFile: stringField(value, 'sessionFile'),
    task: stringField(value, 'task'),
  };
}

function mergeAgent(agents: AgentRecord[], payload: unknown): AgentRecord[] {
  const patch = readAgent(payload);
  if (patch === undefined) return agents;
  const index = agents.findIndex(agent => agent.id === patch.id);
  if (index < 0) return [...agents, patch];
  const previous = agents[index];
  if (previous === undefined) return agents;
  const merged: AgentRecord = {
    id: previous.id,
    agent: patch.agent ?? previous.agent,
    status: patch.status ?? previous.status,
    detached: patch.detached ?? previous.detached,
    parentToolCallId: patch.parentToolCallId ?? previous.parentToolCallId,
    sessionFile: patch.sessionFile ?? previous.sessionFile,
    task: patch.task ?? previous.task,
  };
  const next = [...agents];
  next[index] = merged;
  return next;
}

function createSubagentsPanel(api: DesktopUiApi): OmpPanel {
  const stores = new Map<string, SubagentState>();
  let root: HTMLElement | undefined;
  let mounted: { key: string; element: HTMLElement; paint(): void } | undefined;

  const paintFor = (key: string): void => {
    if (mounted?.key === key) mounted.paint();
  };

  function storeFor(context: PanelContext): SubagentState {
    const key = contextKey(context);
    let state = stores.get(key);
    if (state === undefined) {
      state = {
        context, loadedAt: 0, loading: false, agents: [], selected: undefined, steerText: '',
        transcripts: new Map(), busy: undefined, error: undefined, notice: undefined,
      };
      stores.set(key, state);
    }
    return state;
  }

  async function load(state: SubagentState): Promise<void> {
    if (state.loading) return;
    const key = contextKey(state.context);
    state.loading = true;
    state.error = undefined;
    paintFor(key);
    try {
      const data = await runtimeCommand(api, state.context, { type: 'get_subagents' });
      state.agents = asArray(data.subagents).map(readAgent).filter((agent): agent is AgentRecord => agent !== undefined);
      state.loadedAt = Date.now();
    } catch (error) {
      state.error = errorText(error);
    } finally {
      state.loading = false;
      paintFor(key);
    }
  }

  function build(context: PanelContext, state: SubagentState): { key: string; element: HTMLElement; paint(): void } {
    const key = contextKey(context);
    const element = el('div', STYLE.stack);
    const status = createStatus();

    const refreshButton = action('Refresh', () => void load(state));
    const listSection = el('section', STYLE.section);
    const listBody = el('div', STYLE.stack);
    listSection.append(sectionHead('Subagents', [refreshButton]), listBody);

    const detailSection = el('section', STYLE.section);
    const detailTitle = el('div', STYLE.title, 'Inspector');
    const detailMeta = el('div', STYLE.muted);
    const steerInput = el('textarea', STYLE.input);
    steerInput.rows = 2;
    steerInput.setAttribute('aria-label', 'Steer subagent');
    steerInput.placeholder = 'Message the selected subagent';
    const steerButton = action('Send steer', () => void steer(), { primary: true });
    const stopButton = action('Stop subagent', () => void stop(), { danger: true });
    const loadTranscriptButton = action('Load newer transcript', () => void loadTranscript());
    const steerRow = el('div', STYLE.row);
    steerRow.append(steerButton, stopButton, loadTranscriptButton);
    const transcriptBody = el('div', STYLE.stack);
    const detailBody = el('div', STYLE.stack);
    detailBody.append(detailTitle, detailMeta, steerInput, steerRow, transcriptBody);
    detailSection.append(detailBody);

    element.classList.add('library-stack');
    element.append(status.node, listSection, detailSection);

    let transcriptSignature = '';

    function selectedAgent(): AgentRecord | undefined {
      return state.selected === undefined ? undefined : state.agents.find(agent => agent.id === state.selected);
    }

    function paint(): void {
      listBody.replaceChildren();
      if (state.agents.length === 0) listBody.append(el('div', STYLE.muted, 'No subagents registered for this session.'));
      for (const agent of state.agents) {
        const card = el('div', STYLE.card);
        const head = el('div', STYLE.row);
        head.append(
          el('span', `width:8px;height:8px;border-radius:50%;background:${agent.status === 'running' ? COLOR.accent : agent.status === 'aborted' ? COLOR.danger : COLOR.muted};`),
          el('span', `${STYLE.monospace}font-weight:600;`, agent.id),
        );
        if (agent.agent !== undefined) head.append(el('span', STYLE.muted, `agent ${agent.agent}`));
        if (agent.status !== undefined) head.append(el('span', `font-size:12px;color:${agent.status === 'aborted' ? COLOR.danger : COLOR.muted};`, agent.status));
        if (agent.detached === true) head.append(el('span', STYLE.muted, 'detached'));
        head.append(el('span', 'flex:1;'), action(state.selected === agent.id ? 'Selected' : 'Inspect', () => {
          state.selected = agent.id;
          state.steerText = '';
          paint();
          void loadTranscript();
        }));
        card.append(head);
        if (agent.task !== undefined) card.append(el('div', STYLE.text, truncate(agent.task, 300)));
        const lineage: string[] = [];
        if (agent.parentToolCallId !== undefined) lineage.push(`tool ${agent.parentToolCallId}`);
        if (agent.sessionFile !== undefined) lineage.push(agent.sessionFile.split(/[\\/]/).at(-1) ?? agent.sessionFile);
        if (lineage.length > 0) card.append(el('div', STYLE.muted, lineage.join(' · ')));
        listBody.append(card);
      }

      const selected = selectedAgent();
      detailSection.style.display = selected === undefined ? 'none' : '';
      if (selected !== undefined) {
        detailTitle.textContent = `Inspector · ${selected.id}`;
        const facts: string[] = [];
        if (selected.status !== undefined) facts.push(`status ${selected.status}`);
        if (selected.agent !== undefined) facts.push(`agent ${selected.agent}`);
        if (selected.detached === true) facts.push('detached');
        detailMeta.textContent = facts.join(' · ');
        if (steerInput.value !== state.steerText) steerInput.value = state.steerText;
        steerButton.disabled = state.busy !== undefined || state.steerText.trim().length === 0;
        stopButton.disabled = state.busy !== undefined;
        loadTranscriptButton.disabled = state.busy !== undefined;
      }

      const transcript = state.selected === undefined ? undefined : state.transcripts.get(state.selected);
      const signature = `${state.selected ?? ''}:${transcript?.messages.length ?? 0}:${transcript?.error ?? ''}`;
      if (signature !== transcriptSignature) {
        transcriptSignature = signature;
        transcriptBody.replaceChildren();
        if (transcript === undefined || transcript.messages.length === 0) {
          transcriptBody.append(el('div', STYLE.muted, 'No transcript records read yet.'));
        } else {
          for (const message of transcript.messages) {
            const card = el('div', STYLE.card);
            card.append(
              el('div', `${STYLE.monospace}font-weight:600;`, message.role),
              el('div', `${STYLE.text}${STYLE.monospace}`, truncate(message.text, 1500)),
            );
            transcriptBody.append(card);
          }
        }
        if (transcript?.error !== undefined) transcriptBody.append(el('div', `font-size:12px;color:${COLOR.danger};${STYLE.text}`, transcript.error));
      }
      refreshButton.disabled = state.busy !== undefined || state.loading;

      const line = panelStatus(state);
      status.show(line.text, line.error ? 'error' : 'muted', line.missing);
      const runtimeHost = status.node.parentElement;
      if (runtimeHost) lockRuntime(runtimeHost, line.missing);
    }

    async function stop(): Promise<void> {
      const agent = selectedAgent();
      if (agent === undefined || state.busy !== undefined) return;
      state.busy = `Stopping ${agent.id}…`;
      state.error = undefined;
      state.notice = undefined;
      paint();
      try {
        const data = await runtimeCommand(api, context, { type: 'cancel_subagent', subagentId: agent.id });
        state.notice = data.cancelled === true ? `${agent.id} stopped.` : `${agent.id} was not running any more.`;
      } catch (error) {
        state.error = errorText(error);
      } finally {
        state.busy = undefined;
        paintFor(key);
        void load(state);
      }
    }

    async function steer(): Promise<void> {
      const agent = selectedAgent();
      const message = state.steerText.trim();
      if (agent === undefined || state.busy !== undefined) return;
      if (message.length === 0) {
        state.error = 'Steer message is empty.';
        paint();
        return;
      }
      state.busy = `Steering ${agent.id}…`;
      state.error = undefined;
      state.notice = undefined;
      paint();
      try {
        await runtimeCommand(api, context, { type: 'steer_subagent', subagentId: agent.id, message });
        state.steerText = '';
        state.notice = `Message accepted by ${agent.id}.`;
      } catch (error) {
        state.error = errorText(error);
      } finally {
        state.busy = undefined;
        paintFor(key);
      }
    }

    async function loadTranscript(): Promise<void> {
      const agent = selectedAgent();
      if (agent === undefined || state.busy !== undefined) return;
      let transcript = state.transcripts.get(agent.id);
      if (transcript === undefined) {
        transcript = { fromByte: 0, messages: [], error: undefined };
        state.transcripts.set(agent.id, transcript);
      }
      const record = transcript;
      state.busy = `Reading ${agent.id} transcript…`;
      state.error = undefined;
      paint();
      try {
        const data = await runtimeCommand(api, context, { type: 'get_subagent_messages', subagentId: agent.id, fromByte: record.fromByte });
        if (data.reset === true) record.messages = [];
        record.messages = [...record.messages, ...asArray(data.messages).map(messageSummary)];
        if (typeof data.nextByte === 'number') record.fromByte = data.nextByte;
        record.error = undefined;
      } catch (error) {
        record.error = errorText(error);
      } finally {
        state.busy = undefined;
        paintFor(key);
      }
    }

    steerInput.addEventListener('input', () => {
      state.steerText = steerInput.value;
      steerButton.disabled = state.steerText.trim().length === 0;
    });

    return { key, element, paint };
  }

  function render(state: DesktopState): HTMLElement {
    root ??= el('div', STYLE.panel);
    const context = api.activeContext();
    if (context === undefined) {
      mounted = undefined;
      root.replaceChildren(el('div', STYLE.muted, state.conversations.length === 0
        ? 'Create a conversation to inspect subagents.'
        : 'Select a conversation to inspect subagents.'));
      return root;
    }
    const key = contextKey(context);
    if (mounted?.key === key) {
      mounted.paint();
      return root;
    }
    const store = storeFor(context);
    mounted = build(context, store);
    root.replaceChildren(mounted.element);
    if (Date.now() - store.loadedAt > ROOT_REFRESH_MS) void load(store);
    else mounted.paint();
    return root;
  }

  return {
    render,
    syncContext(): void {
      if (root !== undefined && mounted !== undefined) render(api.state());
    },
    notify(event: DesktopEvent): void {
      if (event.type !== 'rpc') return;
      const context = eventContext(event);
      const frame = eventFrame(event);
      if (context === undefined || frame === undefined) return;
      const key = contextKey(context);
      const state = stores.get(key);
      if (state === undefined) return;
      if (frame.type === 'subagent_lifecycle' || frame.type === 'subagent_progress') {
        state.agents = mergeAgent(state.agents, isRecord(frame.payload) ? frame.payload : frame);
        paintFor(key);
        return;
      }
      if (frame.type === 'agent_end' && mounted?.key === key) void load(state);
    },
    dispose(): void {
      stores.clear();
      mounted = undefined;
      root = undefined;
    },
  };
}

/* ------------------------------------------------------------------ */
/* goal mode                                                            */
/* ------------------------------------------------------------------ */

interface GoalPanelState {
  context: PanelContext;
  loadedAt: number;
  goal: Record<string, unknown> | undefined;
  mode: Record<string, unknown> | undefined;
  objective: string;
  budget: string;
  armedDrop: boolean;
  busy: string | undefined;
  error: string | undefined;
  notice: string | undefined;
}

function goalStatus(state: GoalPanelState): string | undefined {
  for (const source of [state.mode, state.goal]) {
    if (source === undefined) continue;
    for (const key of ['status', 'state', 'mode', 'goal']) {
      const value = source[key];
      if (typeof value === 'string') return value;
    }
  }
  return state.goal === undefined ? undefined : 'active';
}

function createGoalPanel(api: DesktopUiApi): OmpPanel {
  const stores = new Map<string, GoalPanelState>();
  let root: HTMLElement | undefined;
  let mounted: { key: string; element: HTMLElement; paint(): void } | undefined;

  const paintFor = (key: string): void => {
    if (mounted?.key === key) mounted.paint();
  };

  function storeFor(context: PanelContext): GoalPanelState {
    const key = contextKey(context);
    let state = stores.get(key);
    if (state === undefined) {
      state = {
        context, loadedAt: 0, goal: undefined, mode: undefined, objective: '', budget: '',
        armedDrop: false, busy: undefined, error: undefined, notice: undefined,
      };
      stores.set(key, state);
    }
    return state;
  }

  function apply(state: GoalPanelState, data: Record<string, unknown>): void {
    state.goal = isRecord(data.goal) ? data.goal : undefined;
    state.mode = isRecord(data.state) ? data.state : undefined;
  }

  async function run(state: GoalPanelState, op: 'get' | 'create' | 'resume' | 'pause' | 'drop'): Promise<void> {
    if (state.busy !== undefined) return;
    const key = contextKey(state.context);
    state.busy = op === 'get' ? 'Reading goal…' : `Running ${op}…`;
    state.error = undefined;
    state.notice = undefined;
    paintFor(key);
    try {
      const frame: RpcFrame = { type: 'goal', op };
      if (op === 'create') {
        const objective = state.objective.trim();
        if (objective.length === 0) throw new Error('Objective is required.');
        frame.objective = objective;
        const budget = state.budget.trim();
        if (budget.length > 0) {
          const tokens = Number(budget);
          if (!Number.isInteger(tokens) || tokens <= 0) throw new Error('Token budget must be a positive integer.');
          frame.token_budget = tokens;
        }
      }
      apply(state, await runtimeCommand(api, state.context, frame));
      if (op !== 'get') state.notice = `Goal ${op === 'create' ? 'created' : op === 'resume' ? 'resumed' : op === 'pause' ? 'paused' : 'dropped'}.`;
      state.armedDrop = false;
      state.loadedAt = Date.now();
    } catch (error) {
      state.error = errorText(error);
      state.armedDrop = false;
    } finally {
      state.busy = undefined;
      paintFor(key);
    }
  }

  function build(context: PanelContext, state: GoalPanelState): { key: string; element: HTMLElement; paint(): void } {
    const key = contextKey(context);
    const element = el('div', STYLE.stack);
    const status = createStatus();

    const summaryBody = el('div', STYLE.stack);
    const summarySection = el('section', STYLE.section);
    summarySection.append(sectionHead('Goal', [action('Refresh', () => void run(state, 'get'))]), summaryBody);

    const objectiveInput = el('textarea', STYLE.input);
    objectiveInput.rows = 3;
    objectiveInput.placeholder = 'What should the agent keep working toward?';
    objectiveInput.setAttribute('aria-label', 'Goal objective');
    const budgetInput = el('input', STYLE.input);
    budgetInput.type = 'number';
    budgetInput.min = '1';
    budgetInput.step = '1';
    budgetInput.placeholder = 'e.g. 200000';
    budgetInput.setAttribute('aria-label', 'Goal token budget');
    const createButton = action('Create goal', () => void run(state, 'create'), { primary: true });
    const resumeButton = action('Resume', () => void run(state, 'resume'));
    const pauseButton = action('Pause', () => void run(state, 'pause'));
    const dropButton = action('Drop goal', () => onDrop(), { danger: true });
    const buttonRow = el('div', STYLE.row);
    buttonRow.append(createButton, resumeButton, pauseButton, dropButton);
    const formBody = el('div', STYLE.stack);
    formBody.append(
      el('label', STYLE.stack, 'Objective'),
      objectiveInput,
      el('label', STYLE.stack, 'Token budget (optional)'),
      budgetInput,
      buttonRow,
    );
    const formSection = el('section', STYLE.section);
    formSection.append(el('div', STYLE.title, 'Goal controls'), formBody);

    element.classList.add('library-stack');
    element.append(status.node, summarySection, formSection);

    function paint(): void {
      const value = goalStatus(state);
      summaryBody.replaceChildren();
      summaryBody.append(pairGrid([
        ['Mode', value ?? 'inactive'],
        ['Objective', state.goal === undefined ? '—' : stringField(state.goal, 'objective') ?? stringField(state.mode, 'objective') ?? '—'],
      ]));
      const details = [
        ...scalarPairs(state.goal, { objective: true, status: true, state: true, mode: true }),
        ...scalarPairs(state.mode, { objective: true, status: true, state: true, mode: true, goal: true }),
      ];
      if (details.length > 0) summaryBody.append(pairGrid(details));
      if (state.goal === undefined) summaryBody.append(el('div', STYLE.muted, 'No goal is active or paused in this session.'));

      if (document.activeElement !== objectiveInput && objectiveInput.value !== state.objective) objectiveInput.value = state.objective;
      if (document.activeElement !== budgetInput && budgetInput.value !== state.budget) budgetInput.value = state.budget;
      const busy = state.busy !== undefined;
      const active = value === 'active';
      const paused = value === 'paused';
      const exists = state.goal !== undefined || active || paused;
      createButton.disabled = busy || active || paused || state.objective.trim().length === 0;
      createButton.title = state.objective.trim().length === 0
        ? 'Enter an objective first'
        : active || paused ? 'Pause or drop the current goal before creating another' : 'Start working toward this objective';
      resumeButton.disabled = busy || !paused;
      resumeButton.title = paused ? 'Resume the paused goal' : 'Only a paused goal can resume';
      pauseButton.disabled = busy || !active;
      pauseButton.title = active ? 'Pause the active goal' : 'Only an active goal can pause';
      dropButton.disabled = busy || !exists;
      dropButton.textContent = state.armedDrop ? 'Confirm drop' : 'Drop goal';
      dropButton.setAttribute('style', state.armedDrop ? STYLE.danger : STYLE.button);

      const line = panelStatus(state);
      status.show(line.text, line.error ? 'error' : 'muted', line.missing);
      const runtimeHost = status.node.parentElement;
      if (runtimeHost) lockRuntime(runtimeHost, line.missing);
    }

    function onDrop(): void {
      if (state.busy !== undefined) return;
      if (!state.armedDrop) {
        state.armedDrop = true;
        state.notice = 'Press again to drop the goal and its journal.';
        paint();
        return;
      }
      void run(state, 'drop');
    }

    objectiveInput.addEventListener('input', () => {
      state.objective = objectiveInput.value;
      createButton.disabled = state.busy !== undefined || state.objective.trim().length === 0;
    });
    budgetInput.addEventListener('input', () => {
      state.budget = budgetInput.value;
    });

    return { key, element, paint };
  }

  function render(state: DesktopState): HTMLElement {
    root ??= el('div', STYLE.panel);
    const context = api.activeContext();
    if (context === undefined) {
      mounted = undefined;
      root.replaceChildren(el('div', STYLE.muted, state.conversations.length === 0
        ? 'Create a conversation to manage a goal.'
        : 'Select a conversation to manage a goal.'));
      return root;
    }
    const key = contextKey(context);
    if (mounted?.key === key) {
      mounted.paint();
      return root;
    }
    const store = storeFor(context);
    mounted = build(context, store);
    root.replaceChildren(mounted.element);
    if (Date.now() - store.loadedAt > ROOT_REFRESH_MS) void run(store, 'get');
    else mounted.paint();
    return root;
  }

  return {
    render,
    syncContext(): void {
      if (root !== undefined && mounted !== undefined) render(api.state());
    },
    notify(event: DesktopEvent): void {
      if (event.type !== 'rpc') return;
      const context = eventContext(event);
      const frame = eventFrame(event);
      if (context === undefined || frame === undefined || frame.type !== 'goal_updated') return;
      const key = contextKey(context);
      const state = stores.get(key);
      if (state === undefined) return;
      apply(state, frame);
      state.loadedAt = Date.now();
      paintFor(key);
    },
    dispose(): void {
      stores.clear();
      mounted = undefined;
      root = undefined;
    },
  };
}

/* ------------------------------------------------------------------ */
/* queued input                                                         */
/* ------------------------------------------------------------------ */

type QueueName = 'steering' | 'followUp';

interface RestoredItem {
  key: string;
  text: string;
  images: DraftImage[];
  queue: QueueName;
}

interface QueuePanelState {
  context: PanelContext;
  loadedAt: number;
  steering: string[];
  followUp: string[];
  draftText: string;
  draftImages: DraftImage[];
  restored: RestoredItem[];
  busy: string | undefined;
  error: string | undefined;
  notice: string | undefined;
}

function readQueue(value: unknown): { steering: string[]; followUp: string[] } | undefined {
  if (!isRecord(value)) return undefined;
  const text = (items: unknown): string[] => asArray(items).filter((item): item is string => typeof item === 'string');
  return { steering: text(value.steering), followUp: text(value.followUp) };
}

function createQueuePanel(api: DesktopUiApi): OmpPanel {
  const stores = new Map<string, QueuePanelState>();
  let root: HTMLElement | undefined;
  let mounted: { key: string; element: HTMLElement; paint(): void } | undefined;

  const paintFor = (key: string): void => {
    if (mounted?.key === key) mounted.paint();
  };

  function storeFor(context: PanelContext): QueuePanelState {
    const key = contextKey(context);
    let state = stores.get(key);
    if (state === undefined) {
      state = {
        context, loadedAt: 0, steering: [], followUp: [], draftText: '', draftImages: [], restored: [],
        busy: undefined, error: undefined, notice: undefined,
      };
      stores.set(key, state);
    }
    return state;
  }

  async function refresh(state: QueuePanelState): Promise<void> {
    const key = contextKey(state.context);
    try {
      const data = await runtimeCommand(api, state.context, { type: 'get_state' });
      const queue = readQueue(data.queuedMessages);
      if (queue !== undefined) {
        state.steering = queue.steering;
        state.followUp = queue.followUp;
      }
      state.loadedAt = Date.now();
    } catch (error) {
      state.error = errorText(error);
    } finally {
      paintFor(key);
    }
  }

  function build(context: PanelContext, state: QueuePanelState): { key: string; element: HTMLElement; paint(): void } {
    const key = contextKey(context);
    const element = el('div', STYLE.stack);
    const status = createStatus();

    const refreshButton = action('Refresh', () => void refresh(state));
    const queueBody = el('div', STYLE.stack);
    const queueSection = el('section', STYLE.section);
    queueSection.append(sectionHead('Queued input', [refreshButton]), queueBody);

    const composer = el('textarea', STYLE.input);
    composer.rows = 3;
    composer.setAttribute('aria-label', 'Message to steer or queue');
    composer.placeholder = 'Type a message to steer the running turn or queue as a follow-up';
    const attachments = el('div', STYLE.row);
    const steerButton = action('Steer now', () => void send('steering'), { primary: true });
    const followButton = action('Queue follow-up', () => void send('followUp'));
    const abortButton = action('Abort turn & restore drafts', () => void abortAndRestore(), { danger: true });
    const composerActions = el('div', STYLE.row);
    composerActions.append(steerButton, followButton, abortButton);
    const composerBody = el('div', STYLE.stack);
    composerBody.append(composer, attachments, composerActions);
    const composerSection = el('section', STYLE.section);
    composerSection.append(el('div', STYLE.title, 'Compose'), composerBody);

    const restoredBody = el('div', STYLE.stack);
    const restoredSection = el('section', STYLE.section);
    restoredSection.append(el('div', STYLE.title, 'Restored drafts'), restoredBody);

    element.classList.add('library-stack');
    element.append(status.node, queueSection, composerSection, restoredSection);

    let restoredSignature = '';

    function paint(): void {
      if (composer.value !== state.draftText) composer.value = state.draftText;
      attachments.replaceChildren();
      if (state.draftImages.length > 0) {
        attachments.append(imageStrip(state.draftImages, index => {
          state.draftImages = state.draftImages.filter((_, position) => position !== index);
          paint();
        }));
      }
      const busy = state.busy !== undefined;
      steerButton.disabled = busy || state.draftText.trim().length === 0;
      followButton.disabled = busy || state.draftText.trim().length === 0;
      abortButton.disabled = busy;
      refreshButton.disabled = busy;

      queueBody.replaceChildren();
      const cards: HTMLElement[] = [];
      for (const text of state.steering) cards.push(queueCard(text, 'steering'));
      for (const text of state.followUp) cards.push(queueCard(text, 'followUp'));
      if (cards.length === 0) queueBody.append(el('div', STYLE.muted, 'Nothing is queued; the agent takes new input directly.'));
      else queueBody.append(...cards);

      const signature = state.restored.map(item => `${item.key}:${item.images.length}`).join('|');
      if (signature !== restoredSignature) {
        restoredSignature = signature;
        restoredBody.replaceChildren();
        if (state.restored.length === 0) {
          restoredBody.append(el('div', STYLE.muted, 'Aborted queued input appears here with its attachments, ready to edit or resend.'));
        }
        for (const item of state.restored) restoredBody.append(restoredCard(item));
      }
      restoredSection.style.display = state.restored.length > 0 ? '' : 'none';

      const line = panelStatus(state);
      status.show(line.text, line.error ? 'error' : 'muted', line.missing);
      const runtimeHost = status.node.parentElement;
      if (runtimeHost) lockRuntime(runtimeHost, line.missing);
    }

    function queueCard(text: string, queue: QueueName): HTMLElement {
      const card = el('div', STYLE.card);
      const head = el('div', STYLE.row);
      head.append(el('span', `${STYLE.monospace}font-weight:600;`, queue === 'steering' ? 'steering' : 'follow-up'), el('span', 'flex:1;'));
      const busy = state.busy !== undefined;
      head.append(
        action('Edit', () => void moveToEditor(text, queue), { disabled: busy }),
        action('Remove', () => void removeQueued(text, queue), { disabled: busy }),
      );
      if (queue === 'followUp') head.append(action('Promote', () => void promote(text), { disabled: busy }));
      card.append(head, el('div', STYLE.text, text));
      return card;
    }

    function restoredCard(item: RestoredItem): HTMLElement {
      const card = el('div', STYLE.card);
      const head = el('div', STYLE.row);
      head.append(el('div', STYLE.muted, item.queue === 'steering' ? 'restored steering' : 'restored follow-up'), el('span', 'flex:1;'));
      card.append(head);
      const area = el('textarea', STYLE.input);
      area.rows = 2;
      area.setAttribute('aria-label', 'Restored draft text');
      area.value = item.text;
      area.addEventListener('input', () => {
        item.text = area.value;
      });
      const imagesBox = el('div', STYLE.row);
      if (item.images.length > 0) {
        imagesBox.append(imageStrip(item.images, index => {
          item.images = item.images.filter((_, position) => position !== index);
          paint();
        }));
      }
      const row = el('div', STYLE.row);
      row.append(
        action('Send', () => void sendRestored(item), { primary: true }),
        action('Discard', () => {
          state.restored = state.restored.filter(candidate => candidate.key !== item.key);
          paint();
        }),
      );
      card.append(area);
      if (item.images.length > 0) card.append(imagesBox);
      card.append(row);
      return card;
    }

    async function send(queue: QueueName): Promise<void> {
      const message = state.draftText.trim();
      if (message.length === 0 || state.busy !== undefined) {
        state.error = 'Type a message first.';
        paint();
        return;
      }
      state.busy = queue === 'steering' ? 'Sending steering message…' : 'Queueing follow-up…';
      state.error = undefined;
      state.notice = undefined;
      paint();
      const images = state.draftImages;
      try {
        await runtimeCommand(api, context, {
          type: queue === 'steering' ? 'steer' : 'follow_up',
          message,
          ...(images.length > 0 ? { images: imagePayload(images) } : {}),
        });
        state.draftText = '';
        state.draftImages = [];
        state.notice = queue === 'steering' ? 'Steering message sent.' : 'Follow-up queued.';
      } catch (error) {
        state.error = errorText(error);
      } finally {
        state.busy = undefined;
        paintFor(key);
        void refresh(state);
      }
    }

    async function moveToEditor(text: string, queue: QueueName): Promise<void> {
      if (state.busy !== undefined) return;
      state.busy = 'Moving the message to the editor…';
      state.error = undefined;
      state.notice = undefined;
      paint();
      try {
        const data = await runtimeCommand(api, context, { type: 'remove_queued_message', message: text, queue });
        if (data.removed !== true) {
          state.notice = 'That message is no longer pending.';
        } else {
          const images = readImages(data.images);
          if (state.draftText.trim().length > 0 || state.draftImages.length > 0) {
            state.restored = [...state.restored, { key: crypto.randomUUID(), text: state.draftText, images: state.draftImages, queue }];
          }
          state.draftText = text;
          if (images.length > 0) state.draftImages = images;
          state.notice = images.length > 0 ? `Moved to the editor with ${images.length} attachment(s).` : 'Moved to the editor.';
          if (data.imagesDropped === true) state.error = 'The runtime dropped the message attachments; nothing else was lost.';
        }
      } catch (error) {
        state.error = errorText(error);
      } finally {
        state.busy = undefined;
        paintFor(key);
        void refresh(state);
      }
    }

    async function removeQueued(text: string, queue: QueueName): Promise<void> {
      if (state.busy !== undefined) return;
      state.busy = 'Removing the message…';
      state.error = undefined;
      state.notice = undefined;
      paint();
      try {
        const data = await runtimeCommand(api, context, { type: 'remove_queued_message', message: text, queue });
        if (data.removed !== true) {
          state.notice = 'That message is no longer pending.';
        } else {
          const images = readImages(data.images);
          if (images.length > 0) {
            state.restored = [...state.restored, { key: crypto.randomUUID(), text, images, queue }];
            state.notice = `Removed; the text and ${images.length} attachment(s) are kept in Restored drafts.`;
          } else {
            state.notice = 'Message removed.';
          }
          if (data.imagesDropped === true) state.error = 'The runtime dropped the message attachments; nothing else was lost.';
        }
      } catch (error) {
        state.error = errorText(error);
      } finally {
        state.busy = undefined;
        paintFor(key);
        void refresh(state);
      }
    }

    async function promote(text: string): Promise<void> {
      if (state.busy !== undefined) return;
      state.busy = 'Promoting the follow-up…';
      state.error = undefined;
      state.notice = undefined;
      paint();
      try {
        const data = await runtimeCommand(api, context, { type: 'promote_queued_message', message: text });
        state.notice = data.promoted === true ? 'Promoted into the steering queue.' : 'That follow-up is no longer pending.';
      } catch (error) {
        state.error = errorText(error);
      } finally {
        state.busy = undefined;
        paintFor(key);
        void refresh(state);
      }
    }

    async function abortAndRestore(): Promise<void> {
      if (state.busy !== undefined) return;
      state.busy = 'Aborting and taking queued input back…';
      state.error = undefined;
      state.notice = undefined;
      paint();
      try {
        const data = await runtimeCommand(api, context, { type: 'abort_and_restore_queue' });
        const restored: RestoredItem[] = [];
        for (const [queue, items] of [['steering', data.steering], ['followUp', data.followUp]] as const) {
          for (const item of asArray(items)) {
            const text = typeof item === 'string' ? item : stringField(item, 'text') ?? '';
            const images = typeof item === 'string' ? [] : readImages(isRecord(item) ? item.images : undefined);
            if (text.length === 0 && images.length === 0) continue;
            restored.push({ key: crypto.randomUUID(), text, images, queue });
          }
        }
        state.steering = [];
        state.followUp = [];
        state.restored = [...state.restored, ...restored];
        state.notice = restored.length > 0
          ? `Turn aborted; ${restored.length} queued draft(s) kept below.`
          : 'Turn aborted; nothing was queued.';
        void refresh(state);
      } catch (error) {
        state.error = errorText(error);
      } finally {
        state.busy = undefined;
        paintFor(key);
      }
    }

    async function sendRestored(item: RestoredItem): Promise<void> {
      if (state.busy !== undefined) return;
      const message = item.text.trim();
      if (message.length === 0) {
        state.error = 'The restored draft is empty.';
        paint();
        return;
      }
      state.busy = item.queue === 'steering' ? 'Sending restored steering…' : 'Queueing restored follow-up…';
      state.error = undefined;
      state.notice = undefined;
      paint();
      try {
        await runtimeCommand(api, context, {
          type: item.queue === 'steering' ? 'steer' : 'follow_up',
          message,
          ...(item.images.length > 0 ? { images: imagePayload(item.images) } : {}),
        });
        state.restored = state.restored.filter(candidate => candidate.key !== item.key);
        state.notice = item.queue === 'steering' ? 'Restored steering sent.' : 'Restored follow-up queued.';
      } catch (error) {
        state.error = errorText(error);
      } finally {
        state.busy = undefined;
        paintFor(key);
        void refresh(state);
      }
    }

    composer.addEventListener('input', () => {
      state.draftText = composer.value;
      steerButton.disabled = state.draftText.trim().length === 0;
      followButton.disabled = state.draftText.trim().length === 0;
    });

    return { key, element, paint };
  }

  function render(state: DesktopState): HTMLElement {
    root ??= el('div', STYLE.panel);
    const context = api.activeContext();
    if (context === undefined) {
      mounted = undefined;
      root.replaceChildren(el('div', STYLE.muted, state.conversations.length === 0
        ? 'Create a conversation to manage queued input.'
        : 'Select a conversation to manage queued input.'));
      return root;
    }
    const key = contextKey(context);
    if (mounted?.key === key) {
      mounted.paint();
      return root;
    }
    const store = storeFor(context);
    mounted = build(context, store);
    root.replaceChildren(mounted.element);
    if (Date.now() - store.loadedAt > ROOT_REFRESH_MS) void refresh(store);
    else mounted.paint();
    return root;
  }

  return {
    render,
    syncContext(): void {
      if (root !== undefined && mounted !== undefined) render(api.state());
    },
    notify(event: DesktopEvent): void {
      if (event.type !== 'rpc') return;
      const context = eventContext(event);
      const frame = eventFrame(event);
      if (context === undefined || frame === undefined || frame.type !== 'queue_update') return;
      const key = contextKey(context);
      const state = stores.get(key);
      if (state === undefined) return;
      const queue = readQueue(frame);
      if (queue === undefined) return;
      state.steering = queue.steering;
      state.followUp = queue.followUp;
      paintFor(key);
    },
    dispose(): void {
      stores.clear();
      mounted = undefined;
      root = undefined;
    },
  };
}

/* ------------------------------------------------------------------ */
/* per-bot .omp configuration                                           */
/* ------------------------------------------------------------------ */

interface ConfigEntry {
  name: string;
  path: string;
  kind: 'file' | 'directory';
}

interface OpenConfigFile {
  path: string;
  saved: string;
  text: string;
  restartRequired: boolean;
}

interface ConfigPanelState {
  context: PanelContext;
  loadedAt: number;
  directory: string;
  entries: ConfigEntry[];
  file: OpenConfigFile | undefined;
  busy: string | undefined;
  error: string | undefined;
  notice: string | undefined;
}

function readConfigEntry(value: unknown, directory: string): ConfigEntry | undefined {
  if (!isRecord(value)) return undefined;
  const name = stringField(value, 'name');
  if (name === undefined) return undefined;
  return {
    name,
    kind: value.kind === 'directory' ? 'directory' : 'file',
    path: stringField(value, 'path') ?? (directory.length === 0 ? name : `${directory}/${name}`),
  };
}

function hindsightBank(suffix: string): Record<string, unknown> {
  return {
    id: `bot-memory-${suffix}`,
    bankId: `bot-${suffix}`,
    description: "This bot's working memory: decisions, failures, project state.",
    access: 'read-write',
    recall: { enabled: true, maxTokens: 800 },
    retain: { mode: 'manual', scopes: 'per_tag', constantTags: ['harness:omp', `bot:${suffix}`] },
  };
}

const HINDSIGHT_ACCESS: Record<string, true> = { read: true, 'read-write': true };
const HINDSIGHT_RETAIN_MODE: Record<string, true> = { manual: true, decision: true, auto: true, off: true };
const HINDSIGHT_SCOPES: Record<string, true> = { per_tag: true, combined: true, all_combinations: true, shared: true };
const HINDSIGHT_TAGS_MATCH: Record<string, true> = { any: true, any_strict: true, all: true, all_strict: true, exact: true };
const HINDSIGHT_COLD_START: Record<string, true> = { 'mental-models': true, recall: true, off: true };
const HINDSIGHT_RECALL_BUDGET: Record<string, true> = { low: true, mid: true, high: true };
const HINDSIGHT_RECALL_MODE: Record<string, true> = { 'every-turn': true, 'every-n-turns': true, 'explicit-only': true };
const HINDSIGHT_RECALL_TRIGGER: Record<string, true> = { prompt: true, 'turn-end': true, interval: true };
const HINDSIGHT_INJECT_POSITION: Record<string, true> = { append: true, prepend: true };
const HINDSIGHT_CONTEXT_STRATEGY: Record<string, true> = { adaptive: true, user_only: true };
const HINDSIGHT_LOG_LEVEL: Record<string, true> = { debug: true, info: true, warn: true, error: true };
const HINDSIGHT_ROOT_KEYS: Record<string, true> = {
  baseUrl: true, apiKeyEnvVar: true, enabled: true, timeoutMs: true, promptPack: true, banks: true,
  defaultRole: true, recallBudget: true, recallTriggers: true, recallInSubagents: true, recallMode: true,
  recallEveryNTurns: true, recallMinIntervalMs: true, recallContextTurns: true, recallContextRoles: true,
  recallContextStrategy: true, recallDebounceTurns: true, recallMaxQueryChars: true, recallIntentMaxChars: true,
  recallSignatureMaxChars: true, recallTurnSampleChars: true, recallTopK: true, recallRerankCandidateLimit: true,
  recallAntiPatternLimit: true, recallPreviewChars: true, recallTimeoutMs: true, injectTimeoutMs: true,
  autoRecallTypes: true, recallPreferObservations: true, recallQueryPreamble: true, recallAntiPatternGuard: true,
  recallInjectJudge: true, recallInjectMinConfidence: true, recallInjectPosition: true, persistDisplay: true,
  judgeAmbiguousConfidence: true, retryQueueMaxAttempts: true, decomposerRole: true, logLevel: true,
};
const HINDSIGHT_BANK_KEYS: Record<string, true> = {
  id: true, bankId: true, description: true, enabled: true, access: true, recall: true, retain: true,
};
const HINDSIGHT_RECALL_KEYS: Record<string, true> = { enabled: true, maxTokens: true, coldStart: true, tags: true, tagsMatch: true };
const HINDSIGHT_RETAIN_KEYS: Record<string, true> = { mode: true, scopes: true, constantTags: true, autoTarget: true };

function unknownKeys(source: Record<string, unknown>, allowed: Record<string, true>, at: string, problems: string[]): void {
  for (const key of Object.keys(source)) {
    if (allowed[key] !== true) problems.push(`${at}.${key} is unknown; the plugin rejects unknown keys`);
  }
}

/** Comments are legal in `.jsonc`; strip them without touching string contents. */
function stripJsonComments(text: string): string {
  let result = '';
  let inString = false;
  let escaped = false;
  for (let index = 0; index < text.length; index += 1) {
    const char = text[index];
    if (inString) {
      result += char;
      if (escaped) escaped = false;
      else if (char === '\\') escaped = true;
      else if (char === '"') inString = false;
      continue;
    }
    if (char === '"') {
      inString = true;
      result += char;
      continue;
    }
    if (char === '/' && text[index + 1] === '/') {
      while (index < text.length && text[index] !== '\n') index += 1;
      result += '\n';
      continue;
    }
    if (char === '/' && text[index + 1] === '*') {
      index += 2;
      while (index < text.length && !(text[index] === '*' && text[index + 1] === '/')) index += 1;
      index += 1;
      result += ' ';
      continue;
    }
    result += char;
  }
  return result;
}

function parseConfigText(text: string, path: string): { value: unknown; error: string | undefined } {
  try {
    return { value: JSON.parse(path.endsWith('.jsonc') ? stripJsonComments(text) : text), error: undefined };
  } catch (error) {
    return { value: undefined, error: errorText(error) };
  }
}

/**
 * Mirrors the plugin's load-time constraints so the panel never saves a config
 * that would go static. The plugin merges project banks over the global layer by
 * id before validating, so a project entry that names an inherited id may be a
 * partial override; `inheritedBankIds` carries that merge fact.
 */
function validateHindsight(value: unknown, inheritedBankIds?: Set<string>): string[] {
  const problems: string[] = [];
  if (!isRecord(value)) return ['top level must be a JSON object'];
  unknownKeys(value, HINDSIGHT_ROOT_KEYS, 'config', problems);
  if (typeof value.baseUrl !== 'string' || value.baseUrl.trim().length === 0) problems.push('baseUrl must be a non-empty string');
  if ('pathMapping' in value) problems.push('pathMapping is retired and always rejected');
  const defaultRole = typeof value.defaultRole === 'string' ? value.defaultRole : undefined;
  if (!Array.isArray(value.banks) || value.banks.length === 0) {
    problems.push('banks must be a non-empty array');
    return problems;
  }
  const seen = new Set<string>();
  value.banks.forEach((bank, index) => {
    const at = `banks[${index}]`;
    if (!isRecord(bank)) {
      problems.push(`${at} must be an object`);
      return;
    }
    unknownKeys(bank, HINDSIGHT_BANK_KEYS, at, problems);
    if (typeof bank.id !== 'string' || bank.id.length === 0) {
      problems.push(`${at}.id must be a non-empty string`);
      return;
    }
    if (seen.has(bank.id)) problems.push(`${at}.id duplicates "${bank.id}"`);
    seen.add(bank.id);
    const fromGlobal = inheritedBankIds?.has(bank.id) === true;
    if (bank.enabled === false) return;
    if (fromGlobal && bank.bankId === undefined) return;
    if (typeof bank.bankId !== 'string' || bank.bankId.length === 0) {
      problems.push(`${at}.bankId is required for a bank defined here`);
      return;
    }
    if (bank.bankId.includes('${ROLE}') && defaultRole === undefined) problems.push(`${at}.bankId uses \${ROLE} but defaultRole is unset`);
    if (isRecord(bank.retain)) unknownKeys(bank.retain, HINDSIGHT_RETAIN_KEYS, `${at}.retain`, problems);
    if (!isRecord(bank.retain) || bank.retain.scopes === undefined) {
      if (!fromGlobal) problems.push(`${at}.retain.scopes is required`);
    } else {
      const scopes = bank.retain.scopes;
      const validScopes = (typeof scopes === 'string' && HINDSIGHT_SCOPES[scopes] === true)
        || (Array.isArray(scopes) && scopes.length > 0
          && scopes.every(group => Array.isArray(group) && group.length > 0 && group.every(tag => typeof tag === 'string')));
      if (!validScopes) problems.push(`${at}.retain.scopes must be per_tag, combined, all_combinations, shared, or non-empty tag groups`);
      const mode = bank.retain.mode;
      if (typeof mode === 'string' && HINDSIGHT_RETAIN_MODE[mode] !== true) problems.push(`${at}.retain.mode "${mode}" is invalid`);
    }
    if (typeof bank.access === 'string' && HINDSIGHT_ACCESS[bank.access] !== true) problems.push(`${at}.access must be read or read-write`);
    if (isRecord(bank.recall)) {
      const recall = bank.recall;
      unknownKeys(recall, HINDSIGHT_RECALL_KEYS, `${at}.recall`, problems);
      if (typeof recall.tagsMatch === 'string' && HINDSIGHT_TAGS_MATCH[recall.tagsMatch] !== true) problems.push(`${at}.recall.tagsMatch "${recall.tagsMatch}" is invalid`);
      if (typeof recall.coldStart === 'string' && HINDSIGHT_COLD_START[recall.coldStart] !== true) problems.push(`${at}.recall.coldStart "${recall.coldStart}" is invalid`);
    }
  });
  if (typeof value.recallBudget === 'string' && HINDSIGHT_RECALL_BUDGET[value.recallBudget] !== true) problems.push(`recallBudget "${value.recallBudget}" is invalid`);
  if (typeof value.recallMode === 'string' && HINDSIGHT_RECALL_MODE[value.recallMode] !== true) problems.push(`recallMode "${value.recallMode}" is invalid`);
  if (Array.isArray(value.recallTriggers)) {
    for (const trigger of value.recallTriggers) {
      if (typeof trigger === 'string' && HINDSIGHT_RECALL_TRIGGER[trigger] !== true) problems.push(`recallTriggers contains "${trigger}", which is invalid`);
    }
  }
  if (typeof value.recallInjectPosition === 'string' && HINDSIGHT_INJECT_POSITION[value.recallInjectPosition] !== true) problems.push(`recallInjectPosition "${value.recallInjectPosition}" is invalid`);
  if (typeof value.recallContextStrategy === 'string' && HINDSIGHT_CONTEXT_STRATEGY[value.recallContextStrategy] !== true) problems.push(`recallContextStrategy "${value.recallContextStrategy}" is invalid`);
  if (typeof value.logLevel === 'string' && HINDSIGHT_LOG_LEVEL[value.logLevel] !== true) problems.push(`logLevel "${value.logLevel}" is invalid`);
  return problems;
}

function createConfigPanel(api: DesktopUiApi): OmpPanel {
  const stores = new Map<string, ConfigPanelState>();
  let root: HTMLElement | undefined;
  let mounted: { key: string; element: HTMLElement; paint(): void } | undefined;

  const paintFor = (key: string): void => {
    if (mounted?.key === key) mounted.paint();
  };

  function storeFor(context: PanelContext): ConfigPanelState {
    const key = contextKey(context);
    let state = stores.get(key);
    if (state === undefined) {
      state = {
        context, loadedAt: 0, directory: '', entries: [], file: undefined,
        busy: undefined, error: undefined, notice: undefined,
      };
      stores.set(key, state);
    }
    return state;
  }

  async function list(state: ConfigPanelState, directory: string): Promise<void> {
    if (state.busy !== undefined) return;
    const key = contextKey(state.context);
    state.busy = 'Reading configuration tree…';
    state.error = undefined;
    state.notice = undefined;
    paintFor(key);
    try {
      const data = await appCommand(api, 'config.list', {
        botId: state.context.botId,
        ...(directory.length > 0 ? { path: directory } : {}),
      });
      state.entries = asArray(data.entries)
        .map(entry => readConfigEntry(entry, directory))
        .filter((entry): entry is ConfigEntry => entry !== undefined)
        .sort((left, right) => {
          if (left.kind !== right.kind) return left.kind === 'directory' ? -1 : 1;
          return left.name.localeCompare(right.name);
        });
      state.directory = directory;
      state.loadedAt = Date.now();
    } catch (error) {
      state.error = errorText(error);
    } finally {
      state.busy = undefined;
      paintFor(key);
    }
  }

  async function openFile(state: ConfigPanelState, path: string): Promise<void> {
    if (state.busy !== undefined) return;
    const key = contextKey(state.context);
    state.busy = `Reading ${path}…`;
    state.error = undefined;
    state.notice = undefined;
    paintFor(key);
    try {
      const data = await appCommand(api, 'config.read', { botId: state.context.botId, path });
      const content = typeof data.content === 'string' ? data.content : '';
      state.file = { path: stringField(data, 'path') ?? path, saved: content, text: content, restartRequired: false };
    } catch (error) {
      state.error = errorText(error);
    } finally {
      state.busy = undefined;
      paintFor(key);
    }
  }

  async function validationError(state: ConfigPanelState, path: string, content: string): Promise<string | undefined> {
    const name = path.split('/').at(-1);
    if (name === 'omp-hindsight.json' || name === 'omp-hindsight.jsonc') {
      const parsed = parseConfigText(content, path);
      if (parsed.error !== undefined) return `Not saved; the file does not parse. ${parsed.error}`;
      let inherited: Set<string>;
      try {
        const defaults = await appCommand(api, 'config.hindsightDefaults', { botId: state.context.botId });
        inherited = new Set(asArray(defaults.globalBankIds).filter((value): value is string => typeof value === 'string' && value.length > 0));
      } catch (error) {
        return `Not saved; inherited Hindsight banks are unavailable, so the config cannot be validated. ${errorText(error)}`;
      }
      const problems = validateHindsight(parsed.value, inherited);
      return problems.length > 0 ? `Not saved; the plugin would reject this config:\n${problems.join('\n')}` : undefined;
    }
    if (path.endsWith('.json')) {
      try {
        JSON.parse(content);
      } catch (error) {
        return `Invalid JSON; nothing was saved. ${errorText(error)}`;
      }
    }
    return undefined;
  }

  async function save(state: ConfigPanelState): Promise<void> {
    const file = state.file;
    if (file === undefined || state.busy !== undefined) return;
    const key = contextKey(state.context);
    const path = file.path;
    const content = file.text;
    state.busy = 'Validating the file…';
    paintFor(key);
    let problem: string | undefined;
    try {
      problem = await validationError(state, path, content);
    } catch (error) {
      problem = errorText(error);
    } finally {
      state.busy = undefined;
    }
    if (problem !== undefined) {
      state.error = problem;
      paintFor(key);
      return;
    }
    state.busy = `Saving ${path}…`;
    state.error = undefined;
    state.notice = undefined;
    paintFor(key);
    try {
      const data = await appCommand(api, 'config.write', { botId: state.context.botId, path, content });
      file.saved = content;
      file.restartRequired = data.restartRequired === true;
      state.notice = file.restartRequired
        ? 'Saved. Reload the bot profile to apply this change.'
        : 'Saved.';
    } catch (error) {
      state.error = errorText(error);
    } finally {
      state.busy = undefined;
      paintFor(key);
    }
  }

  async function reloadBot(state: ConfigPanelState): Promise<void> {
    if (state.busy !== undefined) return;
    const key = contextKey(state.context);
    state.busy = 'Reloading the bot profile…';
    state.error = undefined;
    state.notice = undefined;
    paintFor(key);
    try {
      await appCommand(api, 'bot.reload', { id: state.context.botId });
      if (state.file !== undefined) state.file.restartRequired = false;
      state.notice = 'Profile reload requested; runtime configuration renews at the next settled dispatch.';
    } catch (error) {
      state.error = errorText(error);
    } finally {
      state.busy = undefined;
      paintFor(key);
    }
  }

  async function createHindsight(state: ConfigPanelState): Promise<void> {
    if (state.busy !== undefined) return;
    const key = contextKey(state.context);
    const botId = state.context.botId;
    state.busy = 'Preparing the Hindsight config…';
    state.error = undefined;
    state.notice = undefined;
    paintFor(key);
    try {
      const defaults = await appCommand(api, 'config.hindsightDefaults', { botId });
      const path = stringField(defaults, 'path');
      if (path === undefined) throw new Error('config.hindsightDefaults did not name the config file to write.');
      const globalProblem = stringField(defaults, 'globalProblem');
      if (globalProblem !== undefined) {
        throw new Error(`Inherited Hindsight banks cannot be enumerated, so isolation cannot be guaranteed: ${globalProblem}`);
      }
      const globalIds = asArray(defaults.globalBankIds).filter((value): value is string => typeof value === 'string' && value.length > 0);
      const content = typeof defaults.content === 'string' ? defaults.content : '';
      if (defaults.exists === true && content.trim().length === 0) {
        throw new Error(`${path} exists but the backend returned no content; refusing to overwrite it.`);
      }
      let config: Record<string, unknown> = {
        baseUrl: 'http://localhost:8888',
        apiKeyEnvVar: 'HINDSIGHT_API_KEY',
        enabled: true,
      };
      if (defaults.exists === true) {
        const parsed = parseConfigText(content, path);
        if (parsed.error !== undefined || !isRecord(parsed.value)) {
          state.file = { path, saved: content, text: content, restartRequired: false };
          throw new Error(`${path} already exists but does not parse; fix it in the editor. ${parsed.error ?? 'top level is not an object'}`);
        }
        config = { ...parsed.value };
        if (config.baseUrl === undefined) config.baseUrl = 'http://localhost:8888';
        if (config.apiKeyEnvVar === undefined) config.apiKeyEnvVar = 'HINDSIGHT_API_KEY';
      }
      if (config.banks !== undefined && !Array.isArray(config.banks)) {
        state.file = { path, saved: content, text: content, restartRequired: false };
        throw new Error(`${path} has a banks value that is not an array; fix it in the editor.`);
      }
      const banks = Array.isArray(config.banks) ? [...config.banks] : [];
      const known = new Set<string>();
      for (const bank of banks) {
        if (!isRecord(bank)) continue;
        if (typeof bank.id === 'string') known.add(bank.id);
      }
      const bot = api.state().bots.find(item => item.id === botId);
      const suffix = (bot?.onlyne?.nodeId ?? crypto.randomUUID()).replace(/[^a-zA-Z0-9_-]+/g, '-');
      const existingOwn = banks.find(bank => isRecord(bank) && typeof bank.id === 'string' && bank.id.startsWith('bot-memory-'));
      const ownId = isRecord(existingOwn) && typeof existingOwn.id === 'string' ? existingOwn.id : `bot-memory-${suffix}`;
      const additions: Array<Record<string, unknown>> = [];
      if (existingOwn === undefined) additions.push(hindsightBank(suffix));
      for (const id of globalIds) {
        if (id === ownId || known.has(id)) continue;
        known.add(id);
        additions.push({ id, enabled: false });
      }
      if (additions.length === 0) {
        state.file = { path, saved: content, text: content, restartRequired: false };
        state.notice = `This bot already defines the ${ownId} bank and disables every inherited bank; nothing was written.`;
        return;
      }
      config.banks = [...banks, ...additions];
      const problems = validateHindsight(config, new Set(globalIds));
      if (problems.length > 0) {
        state.file = { path, saved: content, text: content, restartRequired: false };
        throw new Error(`The merged template would be rejected by the plugin:\n${problems.join('\n')}`);
      }
      const next = `${JSON.stringify(config, null, 2)}\n`;
      const written = await appCommand(api, 'config.write', { botId, path, content: next });
      state.file = { path, saved: next, text: next, restartRequired: written.restartRequired === true };
      await list(state, state.directory);
      state.notice = [
        `Wrote ${path} with the ${ownId} bank; inherited global banks are listed as {id, enabled:false}.`,
        "The bank is created on the bot's next start if it does not exist yet; the plugin aligns banks at session start.",
        path.endsWith('.jsonc') ? 'This is a .jsonc file: saving from the panel rewrites it as plain JSON and drops comments.' : '',
      ].filter(part => part.length > 0).join(' ');
    } catch (error) {
      state.error = errorText(error);
    } finally {
      state.busy = undefined;
      paintFor(key);
    }
  }

  function build(context: PanelContext, state: ConfigPanelState): { key: string; element: HTMLElement; paint(): void } {
    const key = contextKey(context);
    const element = el('div', STYLE.stack);
    const status = createStatus();

    const upButton = action('Up', () => void list(state, state.directory.length === 0 ? '' : state.directory.split('/').slice(0, -1).join('/')));
    const refreshButton = action('Refresh', () => void list(state, state.directory));
    const hindsightButton = action('Create Hindsight config', () => void createHindsight(state));
    const browserBody = el('div', STYLE.stack);
    const crumbsRow = el('div', STYLE.row);
    const filesBody = el('div', STYLE.stack);
    const browserActions = el('div', STYLE.row);
    browserActions.append(upButton, refreshButton, hindsightButton);
    browserBody.append(crumbsRow, browserActions, filesBody);
    const browserSection = el('section', STYLE.section);
    browserSection.append(el('div', STYLE.title, 'Configuration'), browserBody);

    const editorTitle = el('div', STYLE.title, 'Editor');
    const editor = el('textarea', STYLE.input);
    editor.rows = 18;
    editor.spellcheck = false;
    editor.setAttribute('aria-label', 'Configuration file content');
    const saveButton = action('Save', () => void save(state), { primary: true });
    const reloadButton = action('Reload bot profile', () => void reloadBot(state));
    const closeButton = action('Close file', () => {
      state.file = undefined;
      paint();
    });
    const editorActions = el('div', STYLE.row);
    editorActions.append(saveButton, reloadButton, closeButton);
    const editorBody = el('div', STYLE.stack);
    editorBody.append(editorTitle, editor, editorActions);
    const editorSection = el('section', STYLE.section);
    editorSection.append(editorBody);

    element.append(status.node, browserSection, editorSection);

    function paint(): void {
      const busy = state.busy !== undefined;
      crumbsRow.replaceChildren(el('div', STYLE.muted, `.omp${state.directory.length === 0 ? ' /' : ` / ${state.directory}`}`));
      upButton.disabled = busy || state.directory.length === 0;
      filesBody.replaceChildren();
      if (state.entries.length === 0) filesBody.append(el('div', STYLE.muted, 'This directory is empty.'));
      for (const entry of state.entries) {
        const openButton = action(`${entry.kind === 'directory' ? '▸' : '·'} ${entry.name}`, () => {
          if (entry.kind === 'directory') void list(state, entry.path);
          else void openFile(state, entry.path);
        });
        openButton.setAttribute('style', `display:flex;gap:6px;align-items:center;justify-content:flex-start;width:100%;text-align:left;padding:4px 6px;border-radius:6px;border:1px solid ${COLOR.border};background:${COLOR.subtle};color:inherit;cursor:pointer;${STYLE.monospace}`);
        openButton.disabled = busy;
        filesBody.append(openButton);
      }

      const file = state.file;
      editorSection.style.display = file === undefined ? 'none' : '';
      if (file !== undefined) {
        editorTitle.textContent = `.omp/${file.path}${file.text === file.saved ? '' : ' · unsaved'}`;
        if (document.activeElement !== editor && editor.value !== file.text) editor.value = file.text;
        saveButton.disabled = busy;
        closeButton.disabled = busy;
        reloadButton.disabled = busy || !file.restartRequired;
        reloadButton.title = file.restartRequired
          ? 'Apply the saved configuration to this bot'
          : 'Save a configuration that requires a restart first';
      }
      refreshButton.disabled = busy;
      hindsightButton.disabled = busy;

      const line = panelStatus(state);
      status.show(line.text, line.error ? 'error' : 'muted', line.missing);
      const runtimeHost = status.node.parentElement;
      if (runtimeHost) lockRuntime(runtimeHost, line.missing);
    }

    editor.addEventListener('input', () => {
      if (state.file === undefined) return;
      state.file.text = editor.value;
      paint();
    });

    return { key, element, paint };
  }

  function render(state: DesktopState): HTMLElement {
    root ??= el('div', STYLE.panel);
    const context = api.activeContext();
    if (context === undefined) {
      mounted = undefined;
      root.replaceChildren(el('div', STYLE.muted, state.conversations.length === 0
        ? 'Create a conversation to edit per-bot configuration.'
        : 'Select a conversation to edit per-bot configuration.'));
      return root;
    }
    const key = contextKey(context);
    if (mounted?.key === key) {
      mounted.paint();
      return root;
    }
    const store = storeFor(context);
    mounted = build(context, store);
    root.replaceChildren(mounted.element);
    if (Date.now() - store.loadedAt > ROOT_REFRESH_MS) void list(store, store.directory);
    else mounted.paint();
    return root;
  }

  return {
    render,
    syncContext(): void {
      if (root !== undefined && mounted !== undefined) render(api.state());
    },
    notify(): void {},
    dispose(): void {
      stores.clear();
      mounted = undefined;
      root = undefined;
    },
  };
}

/* ------------------------------------------------------------------ */
/* registration                                                         */
/* ------------------------------------------------------------------ */

export function registerOmpPanels(api: DesktopUiApi): () => void {
  const panels: Array<{ panel: OmpPanel; id: string; title: string }> = [
    { panel: createHistoryPanel(api), id: 'omp.session', title: 'Session' },
    { panel: createSubagentsPanel(api), id: 'omp.subagents', title: 'Subagents' },
    { panel: createGoalPanel(api), id: 'omp.goal', title: 'Goal' },
    { panel: createQueuePanel(api), id: 'omp.queue', title: 'Queue' },
    { panel: createConfigPanel(api), id: 'omp.config', title: 'Configuration' },
  ];
  const unregister = panels.map(({ panel, id, title }) =>
    api.registerPanel(id, title, state => panel.render(state)));

  let lastKey: string | undefined;
  const unsubscribe = api.subscribe(event => {
    const context = api.activeContext();
    const key = context === undefined ? undefined : contextKey(context);
    if (key !== lastKey) {
      lastKey = key;
      for (const { panel } of panels) panel.syncContext();
    }
    for (const { panel } of panels) panel.notify(event);
  });

  let disposed = false;
  return () => {
    if (disposed) return;
    disposed = true;
    unsubscribe();
    for (const disposePanel of unregister) disposePanel();
    for (const { panel } of panels) panel.dispose();
  };
}
