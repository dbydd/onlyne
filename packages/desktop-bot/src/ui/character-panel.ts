import { DEFAULT_MAX_AUTO_TURNS } from '../contracts.ts';
import type {
  Bot,
  CharacterCard,
  Conversation,
  DesktopEvent,
  DesktopState,
  Lorebook,
  LorebookEntry,
} from '../contracts.ts';
import type { DesktopUiApi, WidgetContext } from './api.ts';

/**
 * Character and scene panel: the SillyTavern-shaped surfaces of a bot, wired to
 * the desktop controller.
 *
 * Two scopes live here. The bot scope owns the character card and its lorebook,
 * saved through `bot.save`; the conversation scope owns persona, author's note,
 * world-info budgets, turn-taking, greeting starters, regeneration and the
 * harness preview with per-block overrides.
 *
 * Every draft is kept per bot or per conversation, so switching context never
 * discards unsaved edits, and every async result lands in the store it was
 * started from.
 */

type PanelContext = WidgetContext;
type Role = 'system' | 'user' | 'assistant';
type Position =
  | 'before-character'
  | 'after-character'
  | 'before-example'
  | 'after-example'
  | 'at-depth'
  | 'after-history';

const PANEL_ID = 'character';
const PANEL_TITLE = 'Character & scene';
const CONTEXT_SEPARATOR = '\u0000';
const MAX_CARD_BYTES = 16 * 1024 * 1024;

const ROLES: readonly Role[] = ['system', 'user', 'assistant'];
const POSITIONS: readonly Position[] = [
  'before-character',
  'after-character',
  'before-example',
  'after-example',
  'at-depth',
  'after-history',
];
const ROLE_LABELS: Record<Role, string> = { system: 'System', user: 'User', assistant: 'Assistant' };
const POSITION_LABELS: Record<Position, string> = {
  'before-character': 'Before character',
  'after-character': 'After character',
  'before-example': 'Before examples',
  'after-example': 'After examples',
  'at-depth': 'At depth',
  'after-history': 'After history',
};

const COLOR = {
  muted: 'var(--muted)',
  warn: 'var(--warn)',
  danger: 'var(--err)',
} as const;

const STYLE = {
  panel: 'display:flex;flex-direction:column;gap:16px;width:100%;box-sizing:border-box;font:13px/1.5 var(--font);color:var(--fg);',
  section: 'display:flex;flex-direction:column;gap:12px;padding:16px;border:1px solid var(--glass-border-strong);border-radius:var(--radius-lg);background:var(--glass-surface);backdrop-filter:var(--glass-blur-sm);-webkit-backdrop-filter:var(--glass-blur-sm);box-shadow:var(--shadow-sm),var(--glass-specular);',
  row: 'display:flex;flex-wrap:wrap;gap:8px;align-items:center;',
  stack: 'display:flex;flex-direction:column;gap:8px;',
  fieldRow: 'display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:8px;',
  field: 'display:flex;flex-direction:column;gap:4px;min-width:0;',
  label: 'font-size:12px;font-weight:600;color:var(--fg-soft);',
  title: 'font-size:14px;font-weight:650;letter-spacing:-0.01em;',
  muted: 'font-size:12px;color:var(--muted);white-space:pre-wrap;overflow-wrap:anywhere;',
  mono: 'font:12px/1.45 var(--mono);white-space:pre-wrap;overflow-wrap:anywhere;',
  button: 'font:12px/1.2 var(--font);padding:6px 12px;border-radius:var(--radius-sm);border:1px solid var(--glass-border-strong);background:var(--glass-surface);color:inherit;cursor:pointer;box-shadow:var(--glass-specular);transition:all 140ms ease;',
  primary: 'font:12px/1.2 var(--font);padding:6px 12px;border-radius:var(--radius-sm);border:1px solid transparent;background:var(--accent);color:var(--accent-fg);font-weight:600;cursor:pointer;box-shadow:var(--accent-glow);transition:all 140ms ease;',
  danger: 'font:12px/1.2 var(--font);padding:6px 12px;border-radius:var(--radius-sm);border:1px solid var(--err);background:var(--err-soft);color:var(--err);cursor:pointer;transition:all 140ms ease;',
  input: 'font:12px/1.45 var(--mono);padding:7px 10px;border-radius:var(--radius-sm);border:1px solid var(--glass-border-strong);background:rgba(12,16,26,0.55);color:inherit;width:100%;box-sizing:border-box;resize:vertical;outline:none;box-shadow:inset 0 1px 2px rgba(0,0,0,0.35);',
  number: 'font:12px/1.45 var(--mono);padding:7px 10px;border-radius:var(--radius-sm);border:1px solid var(--glass-border-strong);background:rgba(12,16,26,0.55);color:inherit;width:96px;box-sizing:border-box;outline:none;box-shadow:inset 0 1px 2px rgba(0,0,0,0.35);',
  card: 'display:flex;flex-direction:column;gap:8px;padding:12px;border:1px solid var(--glass-border);border-radius:var(--radius-sm);background:var(--glass-surface);box-shadow:var(--shadow-sm);',
  check: 'display:inline-flex;align-items:center;gap:6px;font-size:12px;',
  chip: 'font-size:11px;padding:2px 8px;border-radius:999px;border:1px solid var(--glass-border);color:var(--muted);background:var(--panel-3);',
  badge: 'font-size:11px;padding:2px 9px;border-radius:999px;border:1px solid var(--glass-border);background:var(--panel-3);color:var(--fg-soft);',
  pre: 'margin:0;padding:12px;border:1px solid var(--glass-border-strong);border-radius:var(--radius-sm);background:rgba(10,14,24,0.65);font:12px/1.5 var(--mono);white-space:pre-wrap;overflow-wrap:anywhere;max-height:320px;overflow:auto;',
} as const;

/* ------------------------------------------------------------------ */
/* shared helpers                                                       */
/* ------------------------------------------------------------------ */

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function asArray(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function extraNumber(source: unknown, key: string): number | undefined {
  if (!isRecord(source)) return undefined;
  const value = source[key];
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

function contextKey(context: PanelContext): string {
  return `${context.conversationId}${CONTEXT_SEPARATOR}${context.botId}`;
}

function isPosition(value: unknown): value is Position {
  return typeof value === 'string' && (POSITIONS as readonly string[]).includes(value);
}

function isRole(value: unknown): value is Role {
  return value === 'system' || value === 'user' || value === 'assistant';
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

let fieldCounter = 0;

function field(labelText: string, control: HTMLElement, hint?: string): HTMLElement {
  fieldCounter += 1;
  const id = `onlyne-character-field-${fieldCounter}`;
  control.id = id;
  const wrapper = el('div', STYLE.field);
  const caption = el('label', STYLE.label, labelText);
  caption.htmlFor = id;
  wrapper.append(caption, control);
  if (hint !== undefined) wrapper.append(el('span', STYLE.muted, hint));
  return wrapper;
}

function inlineField(labelText: string, control: HTMLElement, ariaLabel: string): HTMLElement {
  control.setAttribute('aria-label', ariaLabel);
  const wrapper = el('label', STYLE.check);
  wrapper.append(el('span', STYLE.muted, labelText), control);
  return wrapper;
}

interface ActionOptions {
  primary?: boolean;
  danger?: boolean;
}

function action(label: string, onClick: () => void, options: ActionOptions = {}): HTMLButtonElement {
  const node = el('button', options.danger === true ? STYLE.danger : options.primary === true ? STYLE.primary : STYLE.button, label);
  node.type = 'button';
  node.addEventListener('click', onClick);
  return node;
}

function sectionHead(title: string, actions: HTMLElement[]): HTMLElement {
  const head = el('div', STYLE.row);
  head.append(el('div', STYLE.title, title), el('span', 'flex:1;'));
  for (const node of actions) head.append(node);
  return head;
}

interface StatusLine {
  node: HTMLElement;
  show(message: string | undefined, tone?: 'muted' | 'error'): void;
}

function createStatus(): StatusLine {
  const node = el('div', 'font-size:12px;white-space:pre-wrap;overflow-wrap:anywhere;');
  node.setAttribute('aria-live', 'polite');
  return {
    node,
    show(message, tone = 'muted') {
      if (message === undefined) {
        node.textContent = '';
        node.style.display = 'none';
        return;
      }
      node.style.display = '';
      node.textContent = message;
      node.style.color = tone === 'error' ? COLOR.danger : COLOR.muted;
    },
  };
}

function textInput(value: string, placeholder?: string): HTMLInputElement {
  const input = el('input', STYLE.input);
  input.type = 'text';
  input.value = value;
  input.spellcheck = false;
  if (placeholder !== undefined) input.placeholder = placeholder;
  return input;
}

function numberInput(value: string, min: number): HTMLInputElement {
  const input = el('input', STYLE.number);
  input.type = 'number';
  input.min = String(min);
  input.step = '1';
  input.value = value;
  return input;
}

function textArea(value: string, rows: number, placeholder?: string): HTMLTextAreaElement {
  const input = el('textarea', STYLE.input);
  input.rows = rows;
  input.value = value;
  input.spellcheck = false;
  if (placeholder !== undefined) input.placeholder = placeholder;
  return input;
}

function optionSelect(options: Array<{ value: string; label: string }>, value: string): HTMLSelectElement {
  const select = el('select', STYLE.input);
  for (const option of options) {
    const node = el('option', '', option.label);
    node.value = option.value;
    select.append(node);
  }
  select.value = value;
  return select;
}

function checkboxField(labelText: string, checked: boolean): { wrapper: HTMLElement; input: HTMLInputElement } {
  const wrapper = el('label', STYLE.check);
  const input = el('input', 'margin:0;');
  input.type = 'checkbox';
  input.checked = checked;
  wrapper.append(input, el('span', '', labelText));
  return { wrapper, input };
}

function syncInput(control: HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement, value: string): void {
  if (control === document.activeElement) return;
  if (control.value !== value) control.value = value;
}

function truncate(text: string, limit: number): string {
  return text.length <= limit ? text : `${text.slice(0, limit)}…`;
}

function splitCommas(value: string): string[] {
  return value
    .split(',')
    .map(part => part.trim())
    .filter(part => part.length > 0);
}

function splitLines(value: string): string[] {
  return value
    .split('\n')
    .map(line => line.trim())
    .filter(line => line.length > 0);
}

function withText(value: string): string | undefined {
  return value.trim().length === 0 ? undefined : value;
}

function integer(text: string, label: string, min: number): number {
  const trimmed = text.trim();
  if (trimmed.length === 0) throw new Error(`${label} is required.`);
  const value = Number(trimmed);
  if (!Number.isInteger(value) || value < min) {
    throw new Error(`${label} must be a whole number of at least ${min}.`);
  }
  return value;
}

function safeFileName(name: string): string {
  const cleaned = name.replace(/[^\w.-]+/g, '-').replace(/^-+|-+$/g, '');
  return cleaned.length === 0 ? 'character' : cleaned;
}

function toBase64(bytes: Uint8Array): string {
  let binary = '';
  const chunk = 0x8000;
  for (let index = 0; index < bytes.length; index += chunk) {
    binary += String.fromCharCode(...bytes.subarray(index, index + chunk));
  }
  return btoa(binary);
}

/* ------------------------------------------------------------------ */
/* drafts                                                              */
/* ------------------------------------------------------------------ */

interface EntryDraft {
  id: string;
  keys: string;
  secondaryKeys: string;
  content: string;
  enabled: boolean;
  constant: boolean;
  order: string;
  depth: string;
  role: Role;
}

interface CharacterDraft {
  name: string;
  description: string;
  personality: string;
  scenario: string;
  systemPrompt: string;
  postHistoryInstructions: string;
  exampleDialogue: string;
  firstMessage: string;
  alternateGreetings: string;
  creatorNotes: string;
  tags: string;
  lorebookName: string;
  entries: EntryDraft[];
  avatar?: string;
  source?: 'sillytavern' | 'manual';
  seed: string;
  dirty: boolean;
  edits: number;
  revision: number;
}

interface SceneDraft {
  personaName: string;
  personaDescription: string;
  authorNote: string;
  authorNoteDepth: string;
  worldInfoBudget: string;
  worldInfoScanDepth: string;
  historyLimit: string;
  promptBudget: string;
  groupMode: 'manual' | 'round-robin';
  groupOrder: string;
  groupAuto: boolean;
  groupMaxAutoTurns: string;
  seed: string;
  dirty: boolean;
  edits: number;
}

type SceneValues = Omit<SceneDraft, 'seed' | 'dirty' | 'edits'>;

function emptyCharacterDraft(): CharacterDraft {
  return {
    name: '',
    description: '',
    personality: '',
    scenario: '',
    systemPrompt: '',
    postHistoryInstructions: '',
    exampleDialogue: '',
    firstMessage: '',
    alternateGreetings: '',
    creatorNotes: '',
    tags: '',
    lorebookName: '',
    entries: [],
    seed: '',
    dirty: false,
    edits: 0,
    revision: 0,
  };
}

function characterSignature(card: CharacterCard | undefined, lorebook: Lorebook | undefined): string {
  return JSON.stringify({ card: card ?? null, lorebook: lorebook ?? null });
}

function entryToDraft(entry: LorebookEntry): EntryDraft {
  return {
    id: entry.id,
    keys: entry.keys.join(', '),
    secondaryKeys: (entry.secondaryKeys ?? []).join(', '),
    content: entry.content,
    enabled: entry.enabled ?? true,
    constant: entry.constant ?? false,
    order: String(entry.order ?? 100),
    depth: String(entry.depth ?? 4),
    role: entry.role ?? 'system',
  };
}

function seedCharacterDraft(
  draft: CharacterDraft,
  card: CharacterCard | undefined,
  lorebook: Lorebook | undefined,
): void {
  draft.name = card?.name ?? '';
  draft.description = card?.description ?? '';
  draft.personality = card?.personality ?? '';
  draft.scenario = card?.scenario ?? '';
  draft.systemPrompt = card?.systemPrompt ?? '';
  draft.postHistoryInstructions = card?.postHistoryInstructions ?? '';
  draft.exampleDialogue = card?.exampleDialogue ?? '';
  draft.firstMessage = card?.firstMessage ?? '';
  draft.alternateGreetings = (card?.alternateGreetings ?? []).join('\n');
  draft.creatorNotes = card?.creatorNotes ?? '';
  draft.tags = (card?.tags ?? []).join(', ');
  draft.avatar = card?.avatar;
  draft.source = card?.source;
  draft.lorebookName = lorebook?.name ?? '';
  draft.entries = (lorebook?.entries ?? []).map(entryToDraft);
  draft.seed = characterSignature(card, lorebook);
  draft.dirty = false;
  draft.revision += 1;
}

function sceneValues(conversation: Conversation): SceneValues {
  const scene = conversation.scene;
  const budget = extraNumber(scene, 'promptBudget');
  const order = scene?.group?.order ?? [];
  return {
    personaName: conversation.personaName ?? '',
    personaDescription: conversation.personaDescription ?? '',
    authorNote: conversation.authorNote ?? '',
    authorNoteDepth: String(scene?.authorNoteDepth ?? 4),
    worldInfoBudget: String(scene?.worldInfoBudget ?? 4000),
    worldInfoScanDepth: String(scene?.worldInfoScanDepth ?? 6),
    historyLimit: String(scene?.historyLimit ?? 30),
    promptBudget: budget === undefined ? '' : String(budget),
    groupMode: scene?.group?.mode === 'round-robin' ? 'round-robin' : 'manual',
    groupOrder: (order.length > 0 ? order : conversation.botIds).join(', '),
    groupAuto: scene?.group?.auto === true,
    groupMaxAutoTurns: String(scene?.group?.maxAutoTurns ?? DEFAULT_MAX_AUTO_TURNS),
  };
}

function sceneDraftValues(draft: SceneDraft): SceneValues {
  return {
    personaName: draft.personaName,
    personaDescription: draft.personaDescription,
    authorNote: draft.authorNote,
    authorNoteDepth: draft.authorNoteDepth,
    worldInfoBudget: draft.worldInfoBudget,
    worldInfoScanDepth: draft.worldInfoScanDepth,
    historyLimit: draft.historyLimit,
    promptBudget: draft.promptBudget,
    groupMode: draft.groupMode,
    groupOrder: draft.groupOrder,
    groupAuto: draft.groupAuto,
    groupMaxAutoTurns: draft.groupMaxAutoTurns,
  };
}

function sceneSignature(values: SceneValues): string {
  return JSON.stringify(values);
}

function seedSceneDraft(draft: SceneDraft, conversation: Conversation): void {
  const values = sceneValues(conversation);
  Object.assign(draft, values);
  draft.seed = sceneSignature(values);
  draft.dirty = false;
}

function characterPayload(draft: CharacterDraft): { character: CharacterCard; lorebook: Lorebook } {
  const name = withText(draft.name);
  const personality = withText(draft.personality);
  const scenario = withText(draft.scenario);
  const systemPrompt = withText(draft.systemPrompt);
  const postHistoryInstructions = withText(draft.postHistoryInstructions);
  const exampleDialogue = withText(draft.exampleDialogue);
  const firstMessage = withText(draft.firstMessage);
  const creatorNotes = withText(draft.creatorNotes);
  const greetings = splitLines(draft.alternateGreetings);
  const tags = splitCommas(draft.tags);
  const lorebookName = withText(draft.lorebookName);
  const character: CharacterCard = {
    description: draft.description,
    ...(name === undefined ? {} : { name: name.trim() }),
    ...(personality === undefined ? {} : { personality }),
    ...(scenario === undefined ? {} : { scenario }),
    ...(systemPrompt === undefined ? {} : { systemPrompt }),
    ...(postHistoryInstructions === undefined ? {} : { postHistoryInstructions }),
    ...(exampleDialogue === undefined ? {} : { exampleDialogue }),
    ...(firstMessage === undefined ? {} : { firstMessage }),
    ...(greetings.length === 0 ? {} : { alternateGreetings: greetings }),
    ...(creatorNotes === undefined ? {} : { creatorNotes }),
    ...(tags.length === 0 ? {} : { tags }),
    ...(draft.avatar === undefined ? {} : { avatar: draft.avatar }),
    ...(draft.source === undefined ? {} : { source: draft.source }),
  };
  const entries = draft.entries.map((entry, index) => entryPayload(entry, index));
  return { character, lorebook: { ...(lorebookName === undefined ? {} : { name: lorebookName }), entries } };
}

function entryPayload(entry: EntryDraft, index: number): LorebookEntry {
  if (entry.content.trim().length === 0) {
    throw new Error(`Lorebook entry ${index + 1} needs content.`);
  }
  const secondaryKeys = splitCommas(entry.secondaryKeys);
  return {
    id: entry.id,
    keys: splitCommas(entry.keys),
    ...(secondaryKeys.length === 0 ? {} : { secondaryKeys }),
    content: entry.content,
    enabled: entry.enabled,
    constant: entry.constant,
    order: integer(entry.order, `Lorebook entry ${index + 1} order`, 0),
    depth: integer(entry.depth, `Lorebook entry ${index + 1} depth`, 0),
    role: entry.role,
  };
}

function scenePayload(draft: SceneDraft, conversation: Conversation): Record<string, unknown> {
  const members = new Set(conversation.botIds);
  const order: string[] = [];
  for (const id of splitCommas(draft.groupOrder)) {
    if (!members.has(id)) throw new Error(`Group order names a bot that is not in this conversation: ${id}`);
    if (!order.includes(id)) order.push(id);
  }
  const scene: Record<string, unknown> = { ...(conversation.scene ?? {}) };
  scene.authorNoteDepth = integer(draft.authorNoteDepth, "Author's note depth", 0);
  scene.worldInfoBudget = integer(draft.worldInfoBudget, 'World info budget', 1);
  scene.worldInfoScanDepth = integer(draft.worldInfoScanDepth, 'World info scan depth', 0);
  scene.historyLimit = integer(draft.historyLimit, 'History limit', 1);
  const budget = draft.promptBudget.trim();
  if (budget.length === 0) delete scene.promptBudget;
  else scene.promptBudget = integer(budget, 'Prompt budget', 1);
  const existingGroup = conversation.scene?.group;
  scene.group = {
    ...(existingGroup === undefined ? {} : existingGroup),
    mode: draft.groupMode,
    order,
    auto: draft.groupAuto,
    maxAutoTurns: integer(draft.groupMaxAutoTurns, 'Automatic turn limit', 1),
  };
  return {
    id: conversation.id,
    personaName: draft.personaName,
    personaDescription: draft.personaDescription,
    authorNote: draft.authorNote,
    scene,
  };
}

/* ------------------------------------------------------------------ */
/* preview and block overrides                                          */
/* ------------------------------------------------------------------ */

interface PreviewBlock {
  id: string;
  title: string;
  source: string;
  enabled: boolean;
  order: number;
  position: Position;
  depth: number;
  content: string;
}

interface PreviewStats {
  promptTokens?: number;
  loreTokens?: number;
  historyMessages?: number;
  droppedLore?: number;
  droppedForBudget?: number;
}

interface PreviewResult {
  system: string;
  prompt: string;
  blocks: PreviewBlock[];
  matchedLore: string[];
  stats: PreviewStats;
}

interface BlockBase {
  enabled: boolean;
  order: number;
  position: Position;
  depth: number;
  content: string;
}

interface BlockDraft {
  id: string;
  title: string;
  source: string;
  enabled: boolean;
  order: string;
  position: Position;
  depth: string;
  content: string;
  base: BlockBase;
}

function parsePreviewBlock(value: unknown): PreviewBlock | undefined {
  if (!isRecord(value) || typeof value.id !== 'string') return undefined;
  return {
    id: value.id,
    title: typeof value.title === 'string' && value.title.length > 0 ? value.title : value.id,
    source: typeof value.source === 'string' ? value.source : '',
    enabled: typeof value.enabled === 'boolean' ? value.enabled : true,
    order: extraNumber(value, 'order') ?? 100,
    position: isPosition(value.position) ? value.position : 'after-character',
    depth: extraNumber(value, 'depth') ?? 4,
    content: typeof value.content === 'string' ? value.content : '',
  };
}

function parsePreview(value: unknown): PreviewResult | undefined {
  if (!isRecord(value)) return undefined;
  if (typeof value.system !== 'string' && typeof value.prompt !== 'string') return undefined;
  const blocks: PreviewBlock[] = [];
  for (const item of asArray(value.blocks)) {
    const block = parsePreviewBlock(item);
    if (block !== undefined) blocks.push(block);
  }
  const matchedLore = asArray(value.matchedLore).filter((item): item is string => typeof item === 'string');
  const stats: PreviewStats = {};
  if (isRecord(value.stats)) {
    for (const key of ['promptTokens', 'loreTokens', 'historyMessages', 'droppedLore', 'droppedForBudget'] as const) {
      const number = extraNumber(value.stats, key);
      if (number !== undefined) stats[key] = number;
    }
  }
  return {
    system: typeof value.system === 'string' ? value.system : '',
    prompt: typeof value.prompt === 'string' ? value.prompt : '',
    blocks,
    matchedLore,
    stats,
  };
}

function buildBlockDraft(input: {
  id: string;
  title: string;
  source: string;
  enabled: boolean;
  order: number;
  position: Position;
  depth: number;
  content: string;
}): BlockDraft {
  return {
    id: input.id,
    title: input.title,
    source: input.source,
    enabled: input.enabled,
    order: String(input.order),
    position: input.position,
    depth: String(input.depth),
    content: input.content,
    base: {
      enabled: input.enabled,
      order: input.order,
      position: input.position,
      depth: input.depth,
      content: input.content,
    },
  };
}

function blockChanged(block: BlockDraft): boolean {
  return (
    block.enabled !== block.base.enabled ||
    Number(block.order) !== block.base.order ||
    block.position !== block.base.position ||
    Number(block.depth) !== block.base.depth ||
    block.content !== block.base.content
  );
}

function badgeRow(stats: PreviewStats): HTMLElement {
  const row = el('div', STYLE.row);
  const items: Array<{ label: string; value: number | undefined; approximate: boolean }> = [
    { label: 'prompt tokens', value: stats.promptTokens, approximate: true },
    { label: 'lore tokens', value: stats.loreTokens, approximate: true },
    { label: 'history messages', value: stats.historyMessages, approximate: false },
    { label: 'lore dropped', value: stats.droppedLore, approximate: false },
    { label: 'dropped for budget', value: stats.droppedForBudget, approximate: false },
  ];
  for (const item of items) {
    if (item.value === undefined) continue;
    row.append(el('span', STYLE.badge, `${item.approximate ? '~' : ''}${item.value} ${item.label}`));
  }
  if (row.childElementCount === 0) row.append(el('span', STYLE.muted, 'The host reported no statistics.'));
  return row;
}

/* ------------------------------------------------------------------ */
/* panel state                                                          */
/* ------------------------------------------------------------------ */

interface PanelStore {
  key: string;
  context: PanelContext;
  previewText: string;
  preview: PreviewResult | undefined;
  blocks: Map<string, BlockDraft>;
  revision: number;
  busy: string | undefined;
  error: string | undefined;
  notice: string | undefined;
}

interface Section {
  node: HTMLElement;
  paint(busy: boolean): void;
}

interface Mounted {
  key: string;
  element: HTMLElement;
  paint(): void;
}

interface Panel {
  render(state: DesktopState): HTMLElement;
  syncContext(): void;
  notify(event: DesktopEvent): void;
  dispose(): void;
}

/* ------------------------------------------------------------------ */
/* panel                                                                */
/* ------------------------------------------------------------------ */

export function registerCharacterPanel(api: DesktopUiApi): () => void {
  const panel = createCharacterPanel(api);
  const unregister = api.registerPanel(PANEL_ID, PANEL_TITLE, state => panel.render(state));

  let lastKey: string | undefined;
  const unsubscribe = api.subscribe(event => {
    const context = api.activeContext();
    const key = context === undefined ? undefined : contextKey(context);
    if (key !== lastKey) {
      lastKey = key;
      panel.syncContext();
    }
    panel.notify(event);
  });

  let disposed = false;
  return () => {
    if (disposed) return;
    disposed = true;
    unsubscribe();
    unregister();
    panel.dispose();
  };
}

function createCharacterPanel(api: DesktopUiApi): Panel {
  const characters = new Map<string, CharacterDraft>();
  const scenes = new Map<string, SceneDraft>();
  const stores = new Map<string, PanelStore>();
  let root: HTMLElement | undefined;
  let mounted: Mounted | undefined;

  function requestPaint(store: PanelStore): void {
    if (mounted?.key === store.key) mounted.paint();
  }

  function beginOp(store: PanelStore, message: string): boolean {
    if (store.busy !== undefined) return false;
    store.busy = message;
    store.error = undefined;
    store.notice = undefined;
    requestPaint(store);
    return true;
  }

  function endOp(store: PanelStore): void {
    store.busy = undefined;
    requestPaint(store);
  }

  function storeFor(key: string, context: PanelContext): PanelStore {
    let store = stores.get(key);
    if (store === undefined) {
      store = {
        key,
        context,
        previewText: '',
        preview: undefined,
        blocks: new Map(),
        revision: 0,
        busy: undefined,
        error: undefined,
        notice: undefined,
      };
      stores.set(key, store);
    } else {
      store.context = context;
    }
    return store;
  }

  function characterDraftFor(bot: Bot): CharacterDraft {
    let draft = characters.get(bot.id);
    if (draft === undefined) {
      draft = emptyCharacterDraft();
      characters.set(bot.id, draft);
      seedCharacterDraft(draft, bot.character, bot.lorebook);
      return draft;
    }
    if (!draft.dirty && characterSignature(bot.character, bot.lorebook) !== draft.seed) {
      seedCharacterDraft(draft, bot.character, bot.lorebook);
    }
    return draft;
  }

  function sceneDraftFor(conversation: Conversation): SceneDraft {
    let draft = scenes.get(conversation.id);
    if (draft === undefined) {
      draft = {
        personaName: '',
        personaDescription: '',
        authorNote: '',
        authorNoteDepth: '4',
        worldInfoBudget: '4000',
        worldInfoScanDepth: '6',
        historyLimit: '30',
        promptBudget: '',
        groupMode: 'manual',
        groupOrder: '',
        groupAuto: false,
        groupMaxAutoTurns: String(DEFAULT_MAX_AUTO_TURNS),
        seed: '',
        dirty: false,
        edits: 0,
      };
      scenes.set(conversation.id, draft);
      seedSceneDraft(draft, conversation);
      return draft;
    }
    if (!draft.dirty && sceneSignature(sceneValues(conversation)) !== draft.seed) {
      seedSceneDraft(draft, conversation);
    }
    return draft;
  }

  /* ---------------------------------------------------------------- */
  /* character card                                                   */
  /* ---------------------------------------------------------------- */

  function buildCharacterSection(store: PanelStore, draft: CharacterDraft): Section {
    const name = textInput(draft.name, 'e.g. Aria');
    const tags = textInput(draft.tags, 'tags, comma separated');
    const description = textArea(draft.description, 6, 'Who the character is.');
    const personality = textArea(draft.personality, 3);
    const scenario = textArea(draft.scenario, 3);
    const systemPrompt = textArea(draft.systemPrompt, 4);
    const postHistory = textArea(draft.postHistoryInstructions, 3);
    const example = textArea(draft.exampleDialogue, 4);
    const firstMessage = textArea(draft.firstMessage, 4);
    const greetings = textArea(draft.alternateGreetings, 5, 'One greeting per line.');
    const creatorNotes = textArea(draft.creatorNotes, 3);

    const dirtyLabel = el('span', STYLE.muted, '');
    const saveButton = action('Save character', () => void runSave(), { primary: true });
    const discardButton = action('Discard edits', () => discard());
    const importButton = action('Import card', () => fileInput.click());
    const exportButton = action('Export card', () => void runExport());

    const fileInput = el('input', 'display:none;');
    fileInput.type = 'file';
    fileInput.accept = '.json,.png,application/json,image/png';
    fileInput.setAttribute('aria-label', 'Import a character card');
    fileInput.addEventListener('change', () => {
      const file = fileInput.files?.[0];
      fileInput.value = '';
      if (file !== undefined) void runImport(file);
    });

    const head = el('div', STYLE.row);
    head.append(
      el('div', STYLE.title, 'Character card'),
      dirtyLabel,
      el('span', 'flex:1;'),
      importButton,
      exportButton,
      discardButton,
      saveButton,
    );

    const identity = el('div', STYLE.fieldRow);
    identity.append(field('Name', name), field('Tags', tags, 'Comma separated.'));
    const body = el('div', STYLE.stack);
    body.append(
      identity,
      field('Description', description),
      field('Personality', personality),
      field('Scenario', scenario),
      field('System prompt', systemPrompt),
      field('Post-history instructions', postHistory),
      field('Example dialogue', example),
      field('First message', firstMessage),
      field('Alternate greetings', greetings, 'Each line is one alternate greeting, starting at alternate 1.'),
      field('Creator notes', creatorNotes),
    );

    const node = el('section', STYLE.section);
    node.append(head, body, fileInput);

    function markDirty(): void {
      draft.dirty = true;
      draft.edits += 1;
      updateDirty();
    }

    function updateDirty(): void {
      dirtyLabel.textContent = draft.dirty ? 'Unsaved changes' : '';
      dirtyLabel.style.color = draft.dirty ? COLOR.warn : COLOR.muted;
    }

    function bindText(input: HTMLInputElement | HTMLTextAreaElement, apply: (value: string) => void): void {
      input.addEventListener('input', () => {
        apply(input.value);
        markDirty();
      });
    }

    bindText(name, value => { draft.name = value; });
    bindText(tags, value => { draft.tags = value; });
    bindText(description, value => { draft.description = value; });
    bindText(personality, value => { draft.personality = value; });
    bindText(scenario, value => { draft.scenario = value; });
    bindText(systemPrompt, value => { draft.systemPrompt = value; });
    bindText(postHistory, value => { draft.postHistoryInstructions = value; });
    bindText(example, value => { draft.exampleDialogue = value; });
    bindText(firstMessage, value => { draft.firstMessage = value; });
    bindText(greetings, value => { draft.alternateGreetings = value; });
    bindText(creatorNotes, value => { draft.creatorNotes = value; });

    function discard(): void {
      if (store.busy !== undefined) return;
      const bot = api.state().bots.find(item => item.id === store.context.botId);
      if (bot === undefined) return;
      seedCharacterDraft(draft, bot.character, bot.lorebook);
      store.error = undefined;
      store.notice = 'Unsaved character edits discarded.';
      requestPaint(store);
    }

    async function runSave(): Promise<void> {
      if (!beginOp(store, 'Saving the character card…')) return;
      const edits = draft.edits;
      try {
        const { character, lorebook } = characterPayload(draft);
        await api.command('bot.save', {
          bot: { id: store.context.botId, character, lorebook },
        });
        if (draft.edits === edits) {
          seedCharacterDraft(draft, character, lorebook);
          store.notice = 'Character card and lorebook saved.';
        } else {
          draft.dirty = true;
          store.notice = 'Saved, but newer edits are still unsaved.';
        }
      } catch (error) {
        draft.dirty = true;
        store.error = errorText(error);
      } finally {
        endOp(store);
      }
    }

    async function runExport(): Promise<void> {
      if (!beginOp(store, 'Preparing the card export…')) return;
      try {
        const result = await api.command<unknown>('character.export', { botId: store.context.botId });
        const record = isRecord(result) ? result : {};
        const exportName = typeof record.name === 'string' && record.name.trim().length > 0
          ? record.name.trim()
          : store.context.botId;
        const card = record.card ?? {};
        const blob = new Blob([JSON.stringify(card, null, 2)], { type: 'application/json' });
        const url = URL.createObjectURL(blob);
        const link = el('a', 'display:none;');
        link.href = url;
        link.download = `${safeFileName(exportName)}.json`;
        document.body.append(link);
        link.click();
        link.remove();
        window.setTimeout(() => URL.revokeObjectURL(url), 0);
        store.notice = `Exported ${exportName} as JSON.`;
      } catch (error) {
        store.error = errorText(error);
      } finally {
        endOp(store);
      }
    }

    async function runImport(file: File): Promise<void> {
      if (!beginOp(store, `Importing ${file.name}…`)) return;
      try {
        const bytes = new Uint8Array(await file.arrayBuffer());
        if (bytes.length > MAX_CARD_BYTES) throw new Error('The character file is larger than 16 MB.');
        const result = await api.command<unknown>('character.import', {
          botId: store.context.botId,
          content: toBase64(bytes),
          encoding: 'base64',
        });
        const record = isRecord(result) ? result : {};
        const card = isRecord(record.character) ? record.character : {};
        const importedName = typeof card.name === 'string' && card.name.trim().length > 0
          ? card.name.trim()
          : 'Unnamed card';
        const source = card.source === 'sillytavern' ? 'SillyTavern card' : 'JSON card';
        const lorebook = isRecord(record.lorebook) ? record.lorebook : {};
        const entries = Array.isArray(lorebook.entries) ? lorebook.entries.length : 0;
        draft.dirty = false;
        draft.seed = '';
        store.notice = `Imported ${importedName} (${source}) with ${entries} lore ${entries === 1 ? 'entry' : 'entries'}.`;
      } catch (error) {
        store.error = errorText(error);
      } finally {
        endOp(store);
      }
    }

    return {
      node,
      paint(busy: boolean): void {
        syncInput(name, draft.name);
        syncInput(tags, draft.tags);
        syncInput(description, draft.description);
        syncInput(personality, draft.personality);
        syncInput(scenario, draft.scenario);
        syncInput(systemPrompt, draft.systemPrompt);
        syncInput(postHistory, draft.postHistoryInstructions);
        syncInput(example, draft.exampleDialogue);
        syncInput(firstMessage, draft.firstMessage);
        syncInput(greetings, draft.alternateGreetings);
        syncInput(creatorNotes, draft.creatorNotes);
        saveButton.disabled = busy;
        importButton.disabled = busy;
        exportButton.disabled = busy;
        discardButton.disabled = busy || !draft.dirty;
        updateDirty();
      },
    };
  }

  /* ---------------------------------------------------------------- */
  /* lorebook                                                         */
  /* ---------------------------------------------------------------- */

  function buildLorebookSection(store: PanelStore, draft: CharacterDraft): Section {
    const addButton = action('Add entry', () => addEntry());
    const bookName = textInput(draft.lorebookName, 'Lorebook name');
    bookName.addEventListener('input', () => {
      draft.lorebookName = bookName.value;
      draft.dirty = true;
      draft.edits += 1;
    });
    const list = el('div', STYLE.stack);
    const node = el('section', STYLE.section);
    node.append(
      sectionHead('Lorebook', [addButton]),
      field('Name', bookName, 'Optional. Saved together with the character card.'),
      list,
    );

    let renderedRevision = -1;
    let removeButtons: HTMLButtonElement[] = [];

    function addEntry(): void {
      if (store.busy !== undefined) return;
      draft.entries.push({
        id: crypto.randomUUID(),
        keys: '',
        secondaryKeys: '',
        content: '',
        enabled: true,
        constant: false,
        order: '100',
        depth: '4',
        role: 'system',
      });
      draft.dirty = true;
      draft.edits += 1;
      draft.revision += 1;
      requestPaint(store);
    }

    function removeEntry(index: number): void {
      if (store.busy !== undefined) return;
      draft.entries.splice(index, 1);
      draft.dirty = true;
      draft.edits += 1;
      draft.revision += 1;
      requestPaint(store);
    }

    function renderEntries(): void {
      list.replaceChildren();
      removeButtons = [];
      if (draft.entries.length === 0) {
        list.append(el('div', STYLE.muted, 'No lorebook entries. Add one to give the scene world knowledge.'));
        return;
      }
      draft.entries.forEach((entry, index) => {
        const card = el('div', STYLE.card);
        const keys = textInput(entry.keys, 'trigger, words');
        const secondaryKeys = textInput(entry.secondaryKeys, 'second, filter');
        const content = textArea(entry.content, 4, 'What the character knows when the keys match.');
        const order = numberInput(entry.order, 0);
        const depth = numberInput(entry.depth, 0);
        const role = optionSelect(
          ROLES.map(value => ({ value, label: ROLE_LABELS[value] })),
          entry.role,
        );
        const enabled = checkboxField('Enabled', entry.enabled);
        const constant = checkboxField('Always on', entry.constant);
        const remove = action('Remove', () => removeEntry(index), { danger: true });
        removeButtons.push(remove);

        const header = el('div', STYLE.row);
        header.append(
          el('span', STYLE.title, `Entry ${index + 1}`),
          enabled.wrapper,
          constant.wrapper,
          el('span', 'flex:1;'),
          remove,
        );
        const controls = el('div', STYLE.row);
        controls.append(
          inlineField('Order', order, `Entry ${index + 1} order`),
          inlineField('Depth', depth, `Entry ${index + 1} depth`),
          inlineField('Role', role, `Entry ${index + 1} role`),
        );
        const keyRow = el('div', STYLE.fieldRow);
        keyRow.append(field('Keys', keys, 'Comma separated.'), field('Secondary keys', secondaryKeys, 'Comma separated, optional.'));

        keys.addEventListener('input', () => {
          entry.keys = keys.value;
          draft.dirty = true;
          draft.edits += 1;
        });
        secondaryKeys.addEventListener('input', () => {
          entry.secondaryKeys = secondaryKeys.value;
          draft.dirty = true;
          draft.edits += 1;
        });
        content.addEventListener('input', () => {
          entry.content = content.value;
          draft.dirty = true;
          draft.edits += 1;
        });
        order.addEventListener('input', () => {
          entry.order = order.value;
          draft.dirty = true;
          draft.edits += 1;
        });
        depth.addEventListener('input', () => {
          entry.depth = depth.value;
          draft.dirty = true;
          draft.edits += 1;
        });
        role.addEventListener('change', () => {
          if (isRole(role.value)) entry.role = role.value;
          draft.dirty = true;
          draft.edits += 1;
        });
        enabled.input.addEventListener('change', () => {
          entry.enabled = enabled.input.checked;
          draft.dirty = true;
          draft.edits += 1;
        });
        constant.input.addEventListener('change', () => {
          entry.constant = constant.input.checked;
          draft.dirty = true;
          draft.edits += 1;
        });

        card.append(header, keyRow, field('Content', content), controls);
        list.append(card);
      });
    }

    return {
      node,
      paint(busy: boolean): void {
        syncInput(bookName, draft.lorebookName);
        addButton.disabled = busy;
        if (renderedRevision !== draft.revision) {
          renderedRevision = draft.revision;
          renderEntries();
        }
        for (const button of removeButtons) button.disabled = busy;
      },
    };
  }

  /* ---------------------------------------------------------------- */
  /* scene                                                            */
  /* ---------------------------------------------------------------- */

  function buildSceneSection(store: PanelStore, draft: SceneDraft): Section {
    const personaName = textInput(draft.personaName, 'Your name in the scene');
    const personaDescription = textArea(draft.personaDescription, 2, 'How the bot should see you.');
    const authorNote = textArea(draft.authorNote, 3, 'Injected near the end of the prompt.');
    const authorNoteDepth = numberInput(draft.authorNoteDepth, 0);
    const worldInfoBudget = numberInput(draft.worldInfoBudget, 1);
    const worldInfoScanDepth = numberInput(draft.worldInfoScanDepth, 0);
    const historyLimit = numberInput(draft.historyLimit, 1);
    const promptBudget = numberInput(draft.promptBudget, 1);
    const groupMode = optionSelect(
      [
        { value: 'manual', label: 'Manual' },
        { value: 'round-robin', label: 'Round robin' },
      ],
      draft.groupMode,
    );
    const groupOrder = textInput(draft.groupOrder, 'bot-id, bot-id');
    const groupAuto = checkboxField('Keep the group talking automatically', draft.groupAuto);
    const groupMaxAutoTurns = numberInput(draft.groupMaxAutoTurns, 1);
    const dirtyLabel = el('span', STYLE.muted, '');
    const saveButton = action('Save scene', () => void runSave(), { primary: true });

    function markDirty(): void {
      draft.dirty = true;
      draft.edits += 1;
      updateDirty();
    }

    function updateDirty(): void {
      dirtyLabel.textContent = draft.dirty ? 'Unsaved changes' : '';
      dirtyLabel.style.color = draft.dirty ? COLOR.warn : COLOR.muted;
    }

    function bindText(input: HTMLInputElement | HTMLTextAreaElement, apply: (value: string) => void): void {
      input.addEventListener('input', () => {
        apply(input.value);
        markDirty();
      });
    }

    bindText(personaName, value => { draft.personaName = value; });
    bindText(personaDescription, value => { draft.personaDescription = value; });
    bindText(authorNote, value => { draft.authorNote = value; });
    bindText(authorNoteDepth, value => { draft.authorNoteDepth = value; });
    bindText(worldInfoBudget, value => { draft.worldInfoBudget = value; });
    bindText(worldInfoScanDepth, value => { draft.worldInfoScanDepth = value; });
    bindText(historyLimit, value => { draft.historyLimit = value; });
    bindText(promptBudget, value => { draft.promptBudget = value; });
    bindText(groupOrder, value => { draft.groupOrder = value; });
    groupMode.addEventListener('change', () => {
      draft.groupMode = groupMode.value === 'round-robin' ? 'round-robin' : 'manual';
      markDirty();
    });
    groupAuto.input.addEventListener('change', () => {
      draft.groupAuto = groupAuto.input.checked;
      markDirty();
    });
    bindText(groupMaxAutoTurns, value => { draft.groupMaxAutoTurns = value; });

    async function runSave(): Promise<void> {
      if (!beginOp(store, 'Saving the scene…')) return;
      const edits = draft.edits;
      try {
        const conversation = api.state().conversations.find(item => item.id === store.context.conversationId);
        if (conversation === undefined) throw new Error('Select a conversation first.');
        await api.command('conversation.update', scenePayload(draft, conversation));
        if (draft.edits === edits) {
          draft.seed = sceneSignature(sceneDraftValues(draft));
          draft.dirty = false;
          store.notice = 'Scene settings saved.';
        } else {
          draft.dirty = true;
          store.notice = 'Saved, but newer edits are still unsaved.';
        }
      } catch (error) {
        draft.dirty = true;
        store.error = errorText(error);
      } finally {
        endOp(store);
      }
    }

    const head = el('div', STYLE.row);
    head.append(el('div', STYLE.title, 'Scene'), dirtyLabel, el('span', 'flex:1;'), saveButton);
    const numbers = el('div', STYLE.fieldRow);
    numbers.append(
      field("Author's note depth", authorNoteDepth),
      field('World info budget (chars)', worldInfoBudget),
      field('World info scan depth', worldInfoScanDepth),
      field('History limit', historyLimit),
      field('Prompt budget (tokens)', promptBudget, 'Blank keeps no total budget.'),
    );
    const group = el('div', STYLE.fieldRow);
    group.append(
      field('Turn mode', groupMode, 'Group conversations only.'),
      field('Turn order', groupOrder, 'Comma separated bot ids; defaults to member order.'),
      field('Automatic turns', groupAuto.wrapper, 'After each reply the next member answers. Any message you send resets the count.'),
      field('Automatic turn limit', groupMaxAutoTurns, 'Consecutive bot replies before the group waits for you.'),
    );
    const body = el('div', STYLE.stack);
    body.append(
      field('Persona name', personaName),
      field('Persona description', personaDescription),
      field("Author's note", authorNote),
      numbers,
      group,
    );
    const node = el('section', STYLE.section);
    node.append(head, body);

    return {
      node,
      paint(busy: boolean): void {
        syncInput(personaName, draft.personaName);
        syncInput(personaDescription, draft.personaDescription);
        syncInput(authorNote, draft.authorNote);
        syncInput(authorNoteDepth, draft.authorNoteDepth);
        syncInput(worldInfoBudget, draft.worldInfoBudget);
        syncInput(worldInfoScanDepth, draft.worldInfoScanDepth);
        syncInput(historyLimit, draft.historyLimit);
        syncInput(promptBudget, draft.promptBudget);
        syncInput(groupMode, draft.groupMode);
        syncInput(groupOrder, draft.groupOrder);
        groupAuto.input.checked = draft.groupAuto;
        syncInput(groupMaxAutoTurns, draft.groupMaxAutoTurns);
        const conversation = api.state().conversations.find(item => item.id === store.context.conversationId);
        groupOrder.placeholder = conversation === undefined ? 'bot-id, bot-id' : conversation.botIds.join(', ');
        saveButton.disabled = busy;
        updateDirty();
      },
    };
  }

  /* ---------------------------------------------------------------- */
  /* greetings and regeneration                                       */
  /* ---------------------------------------------------------------- */

  function buildGreetingSection(store: PanelStore): Section {
    const list = el('div', STYLE.stack);
    const reset = checkboxField('Reset the scene before greeting (deletes existing messages)', false);
    const buttons: HTMLButtonElement[] = [];
    const regenerate = action('Regenerate last reply', () => void runRegenerate());
    const node = el('section', STYLE.section);
    node.append(sectionHead('Greeting starters', [regenerate]), reset.wrapper, list);

    let renderedCard: string | undefined;

    function refresh(): void {
      const bot = api.state().bots.find(item => item.id === store.context.botId);
      const signature = JSON.stringify(bot?.character ?? null);
      if (signature === renderedCard) return;
      renderedCard = signature;
      buttons.length = 0;
      list.replaceChildren();
      const card = bot?.character;
      const starters: Array<{ label: string; text: string; index: number }> = [];
      if (card?.firstMessage !== undefined && card.firstMessage.trim().length > 0) {
        starters.push({ label: 'First message', text: card.firstMessage, index: -1 });
      }
      for (const [index, text] of (card?.alternateGreetings ?? []).entries()) {
        if (text.trim().length > 0) starters.push({ label: `Alternate ${index + 1}`, text, index });
      }
      if (starters.length === 0) {
        list.append(el('div', STYLE.muted, 'This bot has no greeting yet. Fill in the first message above and save the card.'));
        return;
      }
      for (const starter of starters) {
        const cardNode = el('div', STYLE.card);
        const row = el('div', STYLE.row);
        const button = action('Start scene', () => void runGreet(starter.index), { primary: true });
        buttons.push(button);
        row.append(el('span', STYLE.title, starter.label), button);
        cardNode.append(row, el('div', STYLE.muted, truncate(starter.text.replace(/\s+/g, ' ').trim(), 160)));
        list.append(cardNode);
      }
    }

    async function runGreet(index: number): Promise<void> {
      if (!beginOp(store, 'Starting the scene…')) return;
      try {
        await api.command('conversation.greet', {
          conversationId: store.context.conversationId,
          botId: store.context.botId,
          index,
          reset: reset.input.checked,
        });
        store.notice = index < 0
          ? 'Scene started from the first message.'
          : `Scene started from alternate greeting ${index + 1}.`;
      } catch (error) {
        store.error = errorText(error);
      } finally {
        endOp(store);
      }
    }

    async function runRegenerate(): Promise<void> {
      if (!beginOp(store, 'Regenerating the last reply…')) return;
      try {
        const result = await api.command<unknown>('message.regenerate', {
          conversationId: store.context.conversationId,
          botId: store.context.botId,
        });
        const record = isRecord(result) ? result : {};
        const tokens = extraNumber(record.state, 'promptTokens');
        store.notice = tokens === undefined
          ? 'Regenerating the last reply.'
          : `Regenerating the last reply (~${tokens} prompt tokens).`;
      } catch (error) {
        store.error = errorText(error);
      } finally {
        endOp(store);
      }
    }

    return {
      node,
      paint(busy: boolean): void {
        refresh();
        regenerate.disabled = busy;
        for (const button of buttons) button.disabled = busy;
      },
    };
  }

  /* ---------------------------------------------------------------- */
  /* harness preview and block overrides                              */
  /* ---------------------------------------------------------------- */

  function buildPreviewSection(store: PanelStore, conversation: Conversation): Section {
    const previewButton = action('Preview', () => void runPreview(), { primary: true });
    const saveBlocksButton = action('Save block overrides', () => void runSaveBlocks());
    const textAreaNode = textArea(store.previewText, 2, 'The next user message to compile against.');
    textAreaNode.addEventListener('input', () => {
      store.previewText = textAreaNode.value;
    });
    const result = el('div', STYLE.stack);
    const node = el('section', STYLE.section);
    node.append(
      sectionHead('Harness preview', [previewButton, saveBlocksButton]),
      field('Next user message', textAreaNode, 'Leave empty to compile the default turn.'),
      result,
    );

    let renderedRevision = -1;
    if (store.blocks.size === 0) refreshBlocks(store, store.preview, conversation);

    function paintResult(): void {
      result.replaceChildren();
      const preview = store.preview;
      if (preview === undefined) {
        result.append(el('div', STYLE.muted, 'Run a preview to compile the system prompt, the prompt and this scene\'s blocks.'));
      } else {
        result.append(badgeRow(preview.stats));
        result.append(el('div', STYLE.muted, preview.matchedLore.length === 0
          ? 'No lorebook entries matched.'
          : `Matched lore: ${preview.matchedLore.join(', ')}`));
      }
      result.append(el('div', STYLE.label, 'Scene blocks'));
      const rows = el('div', STYLE.stack);
      if (store.blocks.size === 0) {
        rows.append(el('div', STYLE.muted, 'No blocks loaded. Run a preview or save block overrides from the editor below.'));
      } else {
        for (const row of store.blocks.values()) rows.append(blockRow(row));
      }
      result.append(rows);
      if (preview !== undefined) {
        result.append(el('div', STYLE.label, 'System prompt'));
        result.append(el('pre', STYLE.pre, preview.system.length === 0 ? '(empty)' : preview.system));
        result.append(el('div', STYLE.label, 'Prompt'));
        result.append(el('pre', STYLE.pre, preview.prompt.length === 0 ? '(empty)' : preview.prompt));
      }
    }

    async function runPreview(): Promise<void> {
      if (!beginOp(store, 'Compiling the preview…')) return;
      try {
        const text = textAreaNode.value.trim();
        store.previewText = textAreaNode.value;
        const response = await api.command<unknown>('harness.preview', {
          conversationId: store.context.conversationId,
          botId: store.context.botId,
          ...(text.length === 0 ? {} : { text }),
        });
        const preview = parsePreview(response);
        if (preview === undefined) throw new Error('The host returned no compiled scene.');
        store.preview = preview;
        const current = api.state().conversations.find(item => item.id === store.context.conversationId);
        refreshBlocks(store, preview, current);
        store.revision += 1;
        store.notice = `Preview compiled: ${preview.blocks.length} blocks, ${preview.matchedLore.length} matched lore entries.`;
      } catch (error) {
        store.error = errorText(error);
      } finally {
        endOp(store);
      }
    }

    async function runSaveBlocks(): Promise<void> {
      if (!beginOp(store, 'Saving block overrides…')) return;
      try {
        const current = api.state().conversations.find(item => item.id === store.context.conversationId);
        if (current === undefined) throw new Error('Select a conversation first.');
        const changed = [...store.blocks.values()].filter(blockChanged);
        if (changed.length === 0) {
          store.notice = 'No block overrides changed.';
          return;
        }
        const storedScene: Record<string, unknown> = { ...(current.scene ?? {}) };
        const overrides: Array<Record<string, unknown>> = [];
        for (const item of asArray(storedScene.blocks)) {
          if (isRecord(item) && typeof item.id === 'string') overrides.push({ ...item });
        }
        const positions = new Map(overrides.map((item, index) => [String(item.id), index]));
        for (const block of changed) {
          const entry: Record<string, unknown> = {
            id: block.id,
            enabled: block.enabled,
            order: integer(block.order, `${block.title} order`, 0),
            position: block.position,
            depth: integer(block.depth, `${block.title} depth`, 0),
            content: block.content,
          };
          const position = positions.get(block.id);
          if (position === undefined) {
            overrides.push(entry);
            positions.set(block.id, overrides.length - 1);
          } else {
            overrides[position] = { ...(overrides[position] ?? {}), ...entry };
          }
        }
        storedScene.blocks = overrides;
        await api.command('conversation.update', { id: current.id, scene: storedScene });
        for (const block of changed) {
          block.base = {
            enabled: block.enabled,
            order: Number(block.order),
            position: block.position,
            depth: Number(block.depth),
            content: block.content,
          };
        }
        store.revision += 1;
        store.notice = `Saved block overrides for ${changed.length} ${changed.length === 1 ? 'block' : 'blocks'}.`;
      } catch (error) {
        store.error = errorText(error);
      } finally {
        endOp(store);
      }
    }

    return {
      node,
      paint(busy: boolean): void {
        previewButton.disabled = busy;
        saveBlocksButton.disabled = busy;
        syncInput(textAreaNode, store.previewText);
        if (renderedRevision !== store.revision) {
          renderedRevision = store.revision;
          paintResult();
        }
      },
    };
  }

  /* ---------------------------------------------------------------- */
  /* mount                                                            */
  /* ---------------------------------------------------------------- */

  function build(
    key: string,
    context: PanelContext,
    draft: CharacterDraft,
    conversation: Conversation | undefined,
    sceneDraft: SceneDraft | undefined,
  ): Mounted {
    const store = storeFor(key, context);
    const status = createStatus();
    const element = el('div', STYLE.stack);
    const parts: Section[] = [
      buildCharacterSection(store, draft),
      buildLorebookSection(store, draft),
    ];
    if (conversation !== undefined && sceneDraft !== undefined) {
      parts.push(
        buildSceneSection(store, sceneDraft),
        buildGreetingSection(store),
        buildPreviewSection(store, conversation),
      );
    } else {
      const note = el('section', STYLE.section);
      note.append(el('div', STYLE.muted, 'Scene, greeting and preview controls need an active conversation. The character card above can still be edited.'));
      parts.push({ node: note, paint(): void {} });
    }
    element.append(status.node, ...parts.map(part => part.node));

    function paint(): void {
      const busy = store.busy !== undefined;
      const line = store.error !== undefined
        ? { text: store.error, error: true }
        : store.busy !== undefined
          ? { text: store.busy, error: false }
          : store.notice !== undefined
            ? { text: store.notice, error: false }
            : { text: undefined, error: false };
      status.show(line.text, line.error ? 'error' : 'muted');
      for (const part of parts) part.paint(busy);
    }

    paint();
    return { key, element, paint };
  }

  function render(state: DesktopState): HTMLElement {
    root ??= el('div', STYLE.panel);
    const context = api.activeContext();
    if (context === undefined) {
      mounted = undefined;
      const note = el('section', STYLE.section);
      note.append(el('div', STYLE.muted, state.conversations.length === 0
        ? 'Create a conversation to edit a character and scene.'
        : 'Select a conversation to edit its character and scene.'));
      root.replaceChildren(note);
      return root;
    }
    const bot = state.bots.find(item => item.id === context.botId);
    if (bot === undefined) {
      mounted = undefined;
      const note = el('section', STYLE.section);
      note.append(el('div', STYLE.muted, 'The selected bot is no longer available. Pick another conversation.'));
      root.replaceChildren(note);
      return root;
    }
    const draft = characterDraftFor(bot);
    const conversation = state.conversations.find(item => item.id === context.conversationId);
    const sceneDraft = conversation === undefined ? undefined : sceneDraftFor(conversation);
    const key = `${contextKey(context)}${conversation === undefined ? `${CONTEXT_SEPARATOR}no-conversation` : ''}`;
    if (mounted?.key === key) {
      mounted.paint();
      return root;
    }
    mounted = build(key, context, draft, conversation, sceneDraft);
    root.replaceChildren(mounted.element);
    return root;
  }

  return {
    render,
    syncContext(): void {
      if (root !== undefined) render(api.state());
    },
    notify(event: DesktopEvent): void {
      if (event.type !== 'context' || root === undefined) return;
      render(api.state());
    },
    dispose(): void {
      characters.clear();
      scenes.clear();
      stores.clear();
      mounted = undefined;
      root = undefined;
    },
  };
}

/* ------------------------------------------------------------------ */
/* block row rendering                                                  */
/* ------------------------------------------------------------------ */

function blockRow(row: BlockDraft): HTMLElement {
  const card = el('div', STYLE.card);
  const head = el('div', STYLE.row);
  head.append(
    el('span', STYLE.title, row.title),
    el('span', STYLE.chip, row.source.length === 0 ? 'block' : row.source),
    el('span', STYLE.muted, row.id),
  );
  const enabled = checkboxField('Enabled', row.enabled);
  enabled.input.addEventListener('change', () => {
    row.enabled = enabled.input.checked;
  });
  const order = numberInput(row.order, 0);
  order.addEventListener('input', () => {
    row.order = order.value;
  });
  const position = optionSelect(
    POSITIONS.map(value => ({ value, label: POSITION_LABELS[value] })),
    row.position,
  );
  position.addEventListener('change', () => {
    if (isPosition(position.value)) row.position = position.value;
  });
  const depth = numberInput(row.depth, 0);
  depth.addEventListener('input', () => {
    row.depth = depth.value;
  });
  const controls = el('div', STYLE.row);
  controls.append(
    enabled.wrapper,
    inlineField('Order', order, `${row.title} order`),
    inlineField('Position', position, `${row.title} position`),
    inlineField('Depth', depth, `${row.title} depth`),
  );
  const content = textArea(row.content, 3, 'Empty keeps the compiled content.');
  content.setAttribute('aria-label', `${row.title} content`);
  content.addEventListener('input', () => {
    row.content = content.value;
  });
  card.append(head, controls, content);
  return card;
}

function refreshBlocks(
  store: PanelStore,
  preview: PreviewResult | undefined,
  conversation: Conversation | undefined,
): void {
  const next = new Map<string, BlockDraft>();
  for (const block of preview?.blocks ?? []) {
    next.set(block.id, buildBlockDraft({
      id: block.id,
      title: block.title,
      source: block.source,
      enabled: block.enabled,
      order: block.order,
      position: block.position,
      depth: block.depth,
      content: block.content,
    }));
  }
  for (const override of conversation?.scene?.blocks ?? []) {
    if (next.has(override.id)) continue;
    next.set(override.id, buildBlockDraft({
      id: override.id,
      title: override.title ?? override.id,
      source: 'override',
      enabled: override.enabled ?? true,
      order: override.order ?? 100,
      position: isPosition(override.position) ? override.position : 'after-character',
      depth: override.depth ?? 4,
      content: override.content,
    }));
  }
  for (const [id, row] of next) {
    const previous = store.blocks.get(id);
    if (previous === undefined || !blockChanged(previous)) continue;
    next.set(id, { ...previous, title: row.title, source: row.source, base: row.base });
  }
  store.blocks = next;
}
