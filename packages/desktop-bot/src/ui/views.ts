import type { Json } from '../contracts.ts';
import type { WidgetRegistry, WidgetRenderContext } from './api.ts';

/**
 * Declarative widget card protocol `onlyne.view/v1`.
 *
 * Content is one card object, or a `stack` whose children are cards. Every card
 * may carry a `title`. Unknown kinds render a visible error card instead of
 * breaking the conversation.
 */
export type ViewCardKind =
  | 'text'
  | 'markdown'
  | 'metrics'
  | 'table'
  | 'chart'
  | 'progress'
  | 'choices'
  | 'confirm'
  | 'form'
  | 'html'
  | 'stack';

export type ViewCardBase = {
  title?: string;
};

export type TextCard = ViewCardBase & {
  kind: 'text';
  text: string;
};

export type MarkdownCard = ViewCardBase & {
  kind: 'markdown';
  text: string;
};

export type MetricItem = {
  label: string;
  value: string | number;
  unit?: string;
};

export type MetricsCard = ViewCardBase & {
  kind: 'metrics';
  items: MetricItem[];
};

export type TableColumn = {
  key: string;
  label: string;
};

export type TableRow = { [key: string]: Json };

export type TableCard = ViewCardBase & {
  kind: 'table';
  columns: TableColumn[];
  rows: TableRow[];
};

export type ChartSeries = {
  label: string;
  values: number[];
};

export type ChartCard = ViewCardBase & {
  kind: 'chart';
  series: ChartSeries[];
  labels: string[];
  chartType: 'bar' | 'line';
};

export type ProgressCard = ViewCardBase & {
  kind: 'progress';
  value: number;
  max?: number;
  label?: string;
};

export type ChoiceOption = {
  label: string;
  value: Json;
  description?: string;
};

export type ChoicesCard = ViewCardBase & {
  kind: 'choices';
  options: ChoiceOption[];
  multi?: boolean;
};

export type ConfirmCard = ViewCardBase & {
  kind: 'confirm';
  message: string;
};

export type FormFieldType = 'text' | 'number' | 'checkbox' | 'select' | 'textarea';

export type FormField = {
  name: string;
  label: string;
  type: FormFieldType;
  required?: boolean;
  options?: string[];
  value?: string | number | boolean;
};

export type FormCard = ViewCardBase & {
  kind: 'form';
  fields: FormField[];
  submitLabel?: string;
};

export type HtmlCard = ViewCardBase & {
  kind: 'html';
  html: string;
};

export type StackCard = ViewCardBase & {
  kind: 'stack';
  children: ViewCard[];
};

export type ViewCard =
  | TextCard
  | MarkdownCard
  | MetricsCard
  | TableCard
  | ChartCard
  | ProgressCard
  | ChoicesCard
  | ConfirmCard
  | FormCard
  | HtmlCard
  | StackCard;

/** The `content` of a `WidgetEnvelope` using `onlyne.view/v1`. */
export type ViewContent = ViewCard;

/** Answers of a form card: one entry per named field. */
export type FormAnswers = { [key: string]: string | number | boolean | null };

const STYLE_ID = 'onlyne-builtin-view-styles';
let stylesInstalled = false;

const STYLES = `
.ov-card {
  --ov-fg: var(--onlyne-fg, light-dark(#1f2328, #e6e8eb));
  --ov-muted: var(--onlyne-muted, light-dark(#667085, #98a2b3));
  --ov-bg: var(--onlyne-surface, light-dark(#ffffff, #171a1f));
  --ov-subtle: var(--onlyne-subtle, light-dark(#f5f6f8, #20242b));
  --ov-border: var(--onlyne-border, light-dark(#d9dde5, #323844));
  --ov-accent: var(--onlyne-accent, #3b82f6);
  --ov-danger: var(--onlyne-danger, #d92d20);
  box-sizing: border-box;
  color-scheme: light dark;
  border: 1px solid var(--ov-border);
  border-radius: 10px;
  padding: 12px;
  background: var(--ov-bg);
  color: var(--ov-fg);
  font: 13px/1.45 system-ui, -apple-system, "Segoe UI", sans-serif;
  max-width: 100%;
}
.ov-card__title {
  font-size: 13px;
  font-weight: 650;
  margin-bottom: 8px;
}
.ov-stack {
  --ov-fg: var(--onlyne-fg, light-dark(#1f2328, #e6e8eb));
  --ov-muted: var(--onlyne-muted, light-dark(#667085, #98a2b3));
  --ov-bg: var(--onlyne-surface, light-dark(#ffffff, #171a1f));
  --ov-subtle: var(--onlyne-subtle, light-dark(#f5f6f8, #20242b));
  --ov-border: var(--onlyne-border, light-dark(#d9dde5, #323844));
  --ov-accent: var(--onlyne-accent, #3b82f6);
  color-scheme: light dark;
  display: flex;
  flex-direction: column;
  gap: 10px;
  color: var(--ov-fg);
  font: 13px/1.45 system-ui, -apple-system, "Segoe UI", sans-serif;
  max-width: 100%;
}
.ov-stack__title { font-size: 13px; font-weight: 650; color: var(--ov-fg); }
.ov-text { white-space: pre-wrap; overflow-wrap: anywhere; }
.ov-markdown > :first-child { margin-top: 0; }
.ov-markdown > :last-child { margin-bottom: 0; }
.ov-markdown p { margin: 0 0 8px; }
.ov-markdown h1, .ov-markdown h2, .ov-markdown h3,
.ov-markdown h4, .ov-markdown h5, .ov-markdown h6 {
  margin: 12px 0 6px;
  line-height: 1.3;
}
.ov-markdown h1 { font-size: 18px; }
.ov-markdown h2 { font-size: 16px; }
.ov-markdown h3 { font-size: 14px; }
.ov-markdown h4, .ov-markdown h5, .ov-markdown h6 { font-size: 13px; }
.ov-markdown ul, .ov-markdown ol { margin: 0 0 8px; padding-left: 22px; }
.ov-markdown li { margin: 2px 0; }
.ov-markdown blockquote {
  margin: 0 0 8px;
  padding-left: 10px;
  border-left: 3px solid var(--ov-border);
  color: var(--ov-muted);
}
.ov-markdown pre {
  margin: 0 0 8px;
  padding: 10px;
  overflow-x: auto;
  border-radius: 7px;
  background: var(--ov-subtle);
}
.ov-markdown code {
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 12px;
}
.ov-markdown :not(pre) > code {
  padding: 1px 4px;
  border-radius: 4px;
  background: var(--ov-subtle);
}
.ov-markdown hr { border: 0; border-top: 1px solid var(--ov-border); margin: 10px 0; }
.ov-markdown a { color: var(--ov-accent); }
.ov-markdown del { color: var(--ov-muted); }
.ov-metrics {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(110px, 1fr));
  gap: 8px;
}
.ov-metric { border: 1px solid var(--ov-border); border-radius: 8px; padding: 8px 10px; }
.ov-metric__label { font-size: 11px; color: var(--ov-muted); overflow-wrap: anywhere; }
.ov-metric__value { font-size: 20px; font-weight: 650; line-height: 1.2; overflow-wrap: anywhere; }
.ov-metric__unit { font-size: 11px; color: var(--ov-muted); margin-left: 2px; }
.ov-table-wrap { overflow-x: auto; }
.ov-table { border-collapse: collapse; width: 100%; }
.ov-table th, .ov-table td {
  border-bottom: 1px solid var(--ov-border);
  padding: 6px 8px;
  text-align: left;
  vertical-align: top;
  overflow-wrap: anywhere;
}
.ov-table th { font-size: 11px; color: var(--ov-muted); font-weight: 600; white-space: nowrap; }
.ov-table td { font-size: 12px; }
.ov-chart svg { display: block; width: 100%; height: auto; overflow: visible; }
.ov-chart text { fill: var(--ov-muted); font-size: 10px; font-family: inherit; }
.ov-chart .ov-chart__value { fill: var(--ov-fg); font-size: 9px; }
.ov-chart .ov-chart__grid { stroke: var(--ov-border); stroke-width: 1; }
.ov-chart .ov-chart__axis { stroke: var(--ov-border); stroke-width: 1; }
.ov-legend { display: flex; flex-wrap: wrap; gap: 4px 12px; margin-top: 6px; }
.ov-legend__item { display: inline-flex; align-items: center; gap: 5px; font-size: 11px; color: var(--ov-muted); }
.ov-legend__swatch { width: 9px; height: 9px; border-radius: 2px; flex: none; }
.ov-progress__head { display: flex; justify-content: space-between; gap: 10px; margin-bottom: 5px; font-size: 12px; }
.ov-progress__label { overflow-wrap: anywhere; }
.ov-progress__value { color: var(--ov-muted); white-space: nowrap; font-variant-numeric: tabular-nums; }
.ov-progress__track {
  height: 8px;
  border-radius: 999px;
  background: var(--ov-subtle);
  overflow: hidden;
}
.ov-progress__fill { height: 100%; border-radius: inherit; background: var(--ov-accent); transition: width 120ms ease; }
.ov-choices { display: flex; flex-direction: column; gap: 6px; }
.ov-choice {
  display: flex;
  align-items: flex-start;
  gap: 8px;
  padding: 7px 9px;
  border: 1px solid var(--ov-border);
  border-radius: 8px;
  cursor: pointer;
}
.ov-choice:hover { border-color: var(--ov-accent); }
.ov-choice input { margin: 2px 0 0; flex: none; accent-color: var(--ov-accent); }
.ov-choice__text { min-width: 0; }
.ov-choice__label { font-weight: 550; overflow-wrap: anywhere; }
.ov-choice__description { font-size: 11px; color: var(--ov-muted); margin-top: 2px; }
.ov-actions { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 10px; }
.ov-btn {
  font: inherit;
  font-size: 12px;
  padding: 5px 12px;
  border-radius: 7px;
  border: 1px solid var(--ov-border);
  background: var(--ov-subtle);
  color: var(--ov-fg);
  cursor: pointer;
}
.ov-btn:hover:not(:disabled) { border-color: var(--ov-accent); }
.ov-btn:disabled { opacity: 0.5; cursor: default; }
.ov-btn--primary { background: var(--ov-accent); border-color: var(--ov-accent); color: #fff; }
.ov-hint { min-height: 1em; margin-top: 6px; font-size: 11px; color: var(--ov-danger); }
.ov-form { display: flex; flex-direction: column; gap: 9px; }
.ov-field { display: flex; flex-direction: column; gap: 4px; }
.ov-field__label { font-size: 11px; color: var(--ov-muted); }
.ov-field__required { color: var(--ov-danger); margin-left: 3px; }
.ov-field input[type="text"], .ov-field input[type="number"], .ov-field select, .ov-field textarea {
  font: inherit;
  font-size: 12px;
  box-sizing: border-box;
  width: 100%;
  padding: 6px 8px;
  border: 1px solid var(--ov-border);
  border-radius: 7px;
  background: var(--ov-bg);
  color: var(--ov-fg);
}
.ov-field textarea { min-height: 70px; resize: vertical; }
.ov-field input:focus, .ov-field select:focus, .ov-field textarea:focus {
  outline: 2px solid var(--ov-accent);
  outline-offset: 1px;
}
.ov-field input[aria-invalid="true"], .ov-field select[aria-invalid="true"], .ov-field textarea[aria-invalid="true"] {
  border-color: var(--ov-danger);
}
.ov-field__error { font-size: 11px; color: var(--ov-danger); }
.ov-field--check { gap: 3px; }
.ov-field--check label { display: inline-flex; align-items: center; gap: 7px; cursor: pointer; }
.ov-field--check input { accent-color: var(--ov-accent); }
.ov-html-frame {
  display: block;
  width: 100%;
  height: 320px;
  box-sizing: border-box;
  border: 1px solid var(--ov-border);
  border-radius: 8px;
  background: #fff;
}
.ov-empty, .ov-error-note { font-size: 12px; color: var(--ov-muted); }
.ov-error-note { color: var(--ov-danger); white-space: pre-wrap; overflow-wrap: anywhere; }
`;

function ensureStyles(): void {
  if (stylesInstalled) return;
  if (typeof document === 'undefined') return;
  const existing = document.getElementById(STYLE_ID);
  if (existing) {
    existing.textContent = STYLES;
    stylesInstalled = true;
    return;
  }
  const style = document.createElement('style');
  style.id = STYLE_ID;
  style.textContent = STYLES;
  document.head.append(style);
  stylesInstalled = true;
}

function element(tag: string, className?: string): HTMLElement {
  const node = document.createElement(tag);
  if (className) node.className = className;
  return node;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isJsonValue(value: unknown, seen = new Set<object>()): value is Json {
  if (value === null) return true;
  const type = typeof value;
  if (type === 'string' || type === 'boolean') return true;
  if (type === 'number') return Number.isFinite(value as number);
  if (Array.isArray(value)) {
    if (seen.has(value)) return false;
    seen.add(value);
    const valid = value.every(entry => isJsonValue(entry, seen));
    seen.delete(value);
    return valid;
  }
  if (isRecord(value)) {
    if (seen.has(value)) return false;
    seen.add(value);
    const valid = Object.values(value).every(entry => isJsonValue(entry, seen));
    seen.delete(value);
    return valid;
  }
  return false;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function requireString(record: Record<string, unknown>, key: string, where: string): string {
  const value = record[key];
  if (typeof value !== 'string') throw new Error(`${where}.${key} must be a string.`);
  return value;
}

function optionalString(record: Record<string, unknown>, key: string, where: string): string | undefined {
  const value = record[key];
  if (value === undefined) return undefined;
  if (typeof value !== 'string') throw new Error(`${where}.${key} must be a string when present.`);
  return value;
}

function optionalBoolean(record: Record<string, unknown>, key: string, where: string): boolean | undefined {
  const value = record[key];
  if (value === undefined) return undefined;
  if (typeof value !== 'boolean') throw new Error(`${where}.${key} must be a boolean when present.`);
  return value;
}

function requireNumber(record: Record<string, unknown>, key: string, where: string): number {
  const value = record[key];
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    throw new Error(`${where}.${key} must be a finite number.`);
  }
  return value;
}

function optionalNumber(record: Record<string, unknown>, key: string, where: string): number | undefined {
  const value = record[key];
  if (value === undefined) return undefined;
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    throw new Error(`${where}.${key} must be a finite number when present.`);
  }
  return value;
}

function requireArray(record: Record<string, unknown>, key: string, where: string): unknown[] {
  const value = record[key];
  if (!Array.isArray(value)) throw new Error(`${where}.${key} must be an array.`);
  return value;
}

function requireRecord(value: unknown, where: string): Record<string, unknown> {
  if (!isRecord(value)) throw new Error(`${where} must be an object.`);
  return value;
}

function requireJson(value: unknown, where: string): Json {
  if (!isJsonValue(value)) throw new Error(`${where} must be a JSON value.`);
  return value;
}

function displayValue(value: Json | undefined): string {
  if (value === undefined || value === null) return '';
  if (typeof value === 'object') return JSON.stringify(value);
  return String(value);
}

let viewSerial = 0;

function humanNumber(value: number): string {
  if (Number.isInteger(value)) return String(value);
  return String(Number(value.toFixed(3)));
}

function createCard(title?: string): HTMLElement {
  const card = element('div', 'ov-card');
  if (title) {
    const heading = element('div', 'ov-card__title');
    heading.textContent = title;
    card.append(heading);
  }
  return card;
}

function errorCard(message: string): HTMLElement {
  ensureStyles();
  const card = element('div', 'ov-card');
  const note = element('div', 'ov-error-note');
  note.textContent = message;
  card.append(note);
  return card;
}

function emptyNote(text: string): HTMLElement {
  const note = element('div', 'ov-empty');
  note.textContent = text;
  return note;
}

function button(label: string, primary = false): HTMLButtonElement {
  const node = document.createElement('button');
  node.type = 'button';
  node.className = primary ? 'ov-btn ov-btn--primary' : 'ov-btn';
  node.textContent = label;
  return node;
}

interface Settler {
  readonly interactive: boolean;
  readonly signal: AbortSignal;
  readonly settled: boolean;
  answer(value: Json): boolean;
  cancel(): boolean;
  onSettle(callback: () => void): void;
  dispose(): void;
}

function createSettler(context: WidgetRenderContext): Settler {
  const controller = new AbortController();
  let settled = false;
  let disposed = false;
  const interactive = context.view.state === 'open' && context.view.interactive !== false;
  const settleCallbacks: Array<() => void> = [];
  const deliver = (cancelled: boolean, value?: Json): boolean => {
    if (disposed || settled || !interactive || context.view.state !== 'open') return false;
    settled = true;
    controller.abort();
    for (const callback of settleCallbacks) {
      try {
        callback();
      } catch (error) {
        console.error('Onlyne widget cleanup failed', error);
      }
    }
    const pending = cancelled ? context.cancel() : context.respond(value as Json);
    void pending.catch(error => {
      console.error('Onlyne widget could not deliver its response', error);
    });
    return true;
  };
  return {
    interactive,
    signal: controller.signal,
    get settled() {
      return settled;
    },
    answer(value: Json) {
      return deliver(false, value);
    },
    cancel() {
      return deliver(true);
    },
    onSettle(callback: () => void) {
      settleCallbacks.push(callback);
    },
    dispose() {
      disposed = true;
      controller.abort();
    },
  };
}

function renderViewContent(content: Json, context: WidgetRenderContext): HTMLElement {
  ensureStyles();
  try {
    if (isRecord(content) && content.kind === 'stack') return renderStack(content, context);
    return renderCard(content, context);
  } catch (error) {
    return errorCard(errorMessage(error));
  }
}

function renderStack(content: Record<string, unknown>, context: WidgetRenderContext): HTMLElement {
  const title = optionalString(content, 'title', 'stack');
  const children = requireArray(content, 'children', 'stack');
  const stack = element('div', 'ov-stack');
  if (title) {
    const heading = element('div', 'ov-stack__title');
    heading.textContent = title;
    stack.append(heading);
  }
  if (children.length === 0) stack.append(emptyNote('This stack has no cards.'));
  for (const child of children) stack.append(renderCard(child, context));
  return stack;
}

function renderCard(content: unknown, context: WidgetRenderContext): HTMLElement {
  ensureStyles();
  try {
    const card = requireRecord(content, 'view card');
    const kind = requireString(card, 'kind', 'view card');
    const title = optionalString(card, 'title', 'view card');
    switch (kind) {
      case 'text':
        return textCard(requireString(card, 'text', 'text card'), title);
      case 'markdown':
        return markdownCard(requireString(card, 'text', 'markdown card'), title);
      case 'metrics':
        return metricsCard(requireArray(card, 'items', 'metrics card'), title);
      case 'table':
        return tableCard(card, title);
      case 'chart':
        return chartCard(card, title);
      case 'progress':
        return progressCard(card, title);
      case 'choices':
        return choicesCard(card, title, context);
      case 'confirm':
        return confirmCard(requireString(card, 'message', 'confirm card'), title, context);
      case 'form':
        return formCard(card, title, context);
      case 'html':
        return htmlCard(requireString(card, 'html', 'html card'), title, context);
      case 'stack':
        return renderStack(card, context);
      default:
        return errorCard(`Unsupported view kind "${kind}".`);
    }
  } catch (error) {
    return errorCard(errorMessage(error));
  }
}

function textCard(text: string, title?: string): HTMLElement {
  ensureStyles();
  const card = createCard(title);
  const body = element('div', 'ov-text');
  body.textContent = text;
  card.append(body);
  return card;
}

function metricsCard(items: unknown[], title?: string): HTMLElement {
  const card = createCard(title);
  if (items.length === 0) {
    card.append(emptyNote('No metrics.'));
    return card;
  }
  const grid = element('div', 'ov-metrics');
  items.forEach((raw, index) => {
    const item = requireRecord(raw, `metrics.items[${index}]`);
    const label = requireString(item, 'label', `metrics.items[${index}]`);
    const value = item.value;
    if (typeof value !== 'string' && (typeof value !== 'number' || !Number.isFinite(value))) {
      throw new Error(`metrics.items[${index}].value must be a string or a number.`);
    }
    const unit = optionalString(item, 'unit', `metrics.items[${index}]`);
    const cell = element('div', 'ov-metric');
    const labelNode = element('div', 'ov-metric__label');
    labelNode.textContent = label;
    const valueNode = element('div', 'ov-metric__value');
    const valueText = document.createElement('span');
    valueText.textContent = String(value);
    valueNode.append(valueText);
    if (unit) {
      const unitNode = element('span', 'ov-metric__unit');
      unitNode.textContent = unit;
      valueNode.append(unitNode);
    }
    cell.append(labelNode, valueNode);
    grid.append(cell);
  });
  card.append(grid);
  return card;
}

interface ParsedColumn {
  key: string;
  label: string;
}

function tableCard(content: Record<string, unknown>, title?: string): HTMLElement {
  const columns: ParsedColumn[] = requireArray(content, 'columns', 'table card').map((raw, index) => {
    const column = requireRecord(raw, `table.columns[${index}]`);
    return {
      key: requireString(column, 'key', `table.columns[${index}]`),
      label: requireString(column, 'label', `table.columns[${index}]`),
    };
  });
  const rows = requireArray(content, 'rows', 'table card').map((raw, index) =>
    requireRecord(raw, `table.rows[${index}]`),
  );
  const card = createCard(title);
  if (columns.length === 0) {
    card.append(emptyNote('This table has no columns.'));
    return card;
  }
  const wrap = element('div', 'ov-table-wrap');
  const table = document.createElement('table');
  table.className = 'ov-table';
  const head = document.createElement('thead');
  const headRow = document.createElement('tr');
  for (const column of columns) {
    const th = document.createElement('th');
    th.scope = 'col';
    th.textContent = column.label;
    headRow.append(th);
  }
  head.append(headRow);
  const body = document.createElement('tbody');
  if (rows.length === 0) {
    const emptyRow = document.createElement('tr');
    const cell = document.createElement('td');
    cell.colSpan = columns.length;
    cell.textContent = 'No rows.';
    emptyRow.append(cell);
    body.append(emptyRow);
  }
  for (const row of rows) {
    const tr = document.createElement('tr');
    for (const column of columns) {
      const td = document.createElement('td');
      td.textContent = displayValue(row[column.key] as Json | undefined);
      tr.append(td);
    }
    body.append(tr);
  }
  table.append(head, body);
  wrap.append(table);
  card.append(wrap);
  return card;
}

const CHART_COLORS = ['#3b82f6', '#f59e0b', '#10b981', '#ef4444', '#8b5cf6', '#06b6d4', '#ec4899', '#84cc16'] as const;

function chartColor(index: number): string {
  return CHART_COLORS[index % CHART_COLORS.length] ?? CHART_COLORS[0];
}

interface ParsedSeries {
  label: string;
  values: number[];
}

function niceStep(raw: number): number {
  if (!Number.isFinite(raw) || raw <= 0) return 1;
  const power = Math.pow(10, Math.floor(Math.log10(raw)));
  const fraction = raw / power;
  const nice = fraction <= 1 ? 1 : fraction <= 2 ? 2 : fraction <= 5 ? 5 : 10;
  return nice * power;
}

function chartCard(content: Record<string, unknown>, title?: string): HTMLElement {
  const chartType = content.chartType;
  if (chartType !== 'bar' && chartType !== 'line') {
    throw new Error('chart.chartType must be "bar" or "line".');
  }
  const labels = requireArray(content, 'labels', 'chart').map((raw, index) => {
    if (typeof raw !== 'string') throw new Error(`chart.labels[${index}] must be a string.`);
    return raw;
  });
  const series: ParsedSeries[] = requireArray(content, 'series', 'chart').map((raw, index) => {
    const parsed = requireRecord(raw, `chart.series[${index}]`);
    const label = requireString(parsed, 'label', `chart.series[${index}]`);
    const values = requireArray(parsed, 'values', `chart.series[${index}]`).map((value, valueIndex) => {
      if (typeof value !== 'number' || !Number.isFinite(value)) {
        throw new Error(`chart.series[${index}].values[${valueIndex}] must be a finite number.`);
      }
      return value;
    });
    return { label, values };
  });
  const card = createCard(title);
  if (labels.length === 0 || series.length === 0) {
    card.append(emptyNote('No chart data.'));
    return card;
  }
  const chart = element('div', 'ov-chart');
  chart.append(buildChartSvg(chartType, labels, series));
  chart.append(buildLegend(series));
  card.append(chart);
  return card;
}

function buildChartSvg(chartType: 'bar' | 'line', labels: string[], series: ParsedSeries[]): SVGSVGElement {
  const svgNs = 'http://www.w3.org/2000/svg';
  const width = 640;
  const height = 300;
  const left = 52;
  const right = 16;
  const top = 16;
  const bottom = 54;
  const plotWidth = width - left - right;
  const plotHeight = height - top - bottom;

  let rawMax = 0;
  let rawMin = 0;
  for (const entry of series) {
    for (const value of entry.values) {
      if (value > rawMax) rawMax = value;
      if (value < rawMin) rawMin = value;
    }
  }
  const span = rawMax - rawMin || 1;
  let step = niceStep(span / 4);
  if (!Number.isFinite(step) || step <= 0) step = 1;
  const min = Math.floor(rawMin / step) * step;
  let max = Math.ceil(rawMax / step) * step;
  if (max === min) max = min + step;
  const range = max - min || 1;
  const y = (value: number): number => top + ((max - value) / range) * plotHeight;
  const band = plotWidth / labels.length;
  const centre = (index: number): number => left + band * (index + 0.5);
  const baseline = y(0);

  const svg = document.createElementNS(svgNs, 'svg');
  svg.setAttribute('viewBox', `0 0 ${width} ${height}`);
  svg.setAttribute('preserveAspectRatio', 'xMidYMid meet');
  svg.setAttribute('role', 'img');
  svg.setAttribute('aria-label', `${chartType} chart with ${series.length} series over ${labels.length} labels`);

  for (let value = min; value <= max + step / 1000; value += step) {
    const line = document.createElementNS(svgNs, 'line');
    line.setAttribute('class', 'ov-chart__grid');
    line.setAttribute('x1', String(left));
    line.setAttribute('x2', String(width - right));
    line.setAttribute('y1', String(y(value)));
    line.setAttribute('y2', String(y(value)));
    svg.append(line);

    const tick = document.createElementNS(svgNs, 'text');
    tick.setAttribute('x', String(left - 8));
    tick.setAttribute('y', String(y(value) + 3));
    tick.setAttribute('text-anchor', 'end');
    tick.textContent = humanNumber(value);
    svg.append(tick);
    if (value + step === value) break;
  }

  const axis = document.createElementNS(svgNs, 'line');
  axis.setAttribute('class', 'ov-chart__axis');
  axis.setAttribute('x1', String(left));
  axis.setAttribute('x2', String(width - right));
  axis.setAttribute('y1', String(baseline));
  axis.setAttribute('y2', String(baseline));
  svg.append(axis);

  const rotateLabels = labels.length > 6;
  labels.forEach((label, index) => {
    const tick = document.createElementNS(svgNs, 'text');
    tick.setAttribute('x', String(centre(index)));
    tick.setAttribute('y', String(height - bottom + 18));
    tick.setAttribute('text-anchor', rotateLabels ? 'end' : 'middle');
    if (rotateLabels) {
      tick.setAttribute('transform', `rotate(-35 ${centre(index)} ${height - bottom + 18})`);
    }
    const shown = label.length > 16 ? `${label.slice(0, 15)}…` : label;
    tick.textContent = shown;
    const tooltip = document.createElementNS(svgNs, 'title');
    tooltip.textContent = label;
    tick.append(tooltip);
    svg.append(tick);
  });

  const seriesCount = series.length;
  const barWidth = Math.max(1, (band * 0.72) / seriesCount);
  const showValues = seriesCount === 1 && labels.length <= 12;

  series.forEach((entry, seriesIndex) => {
    const color = chartColor(seriesIndex);
    if (chartType === 'bar') {
      labels.forEach((label, index) => {
        const value = entry.values[index] ?? 0;
        const x = left + band * index + (band - barWidth * seriesCount) / 2 + seriesIndex * barWidth;
        const valueY = y(value);
        const bar = document.createElementNS(svgNs, 'rect');
        bar.setAttribute('x', String(x));
        bar.setAttribute('y', String(Math.min(valueY, baseline)));
        bar.setAttribute('width', String(Math.max(1, barWidth - 1)));
        bar.setAttribute('height', String(Math.max(1, Math.abs(baseline - valueY))));
        bar.setAttribute('rx', '2');
        bar.setAttribute('fill', color);
        const tooltip = document.createElementNS(svgNs, 'title');
        tooltip.textContent = `${label} · ${entry.label}: ${humanNumber(value)}`;
        bar.append(tooltip);
        svg.append(bar);

        if (showValues && value !== 0) {
          const valueText = document.createElementNS(svgNs, 'text');
          valueText.setAttribute('class', 'ov-chart__value');
          valueText.setAttribute('x', String(x + (barWidth - 1) / 2));
          valueText.setAttribute('y', String(value >= 0 ? valueY - 4 : valueY + 12));
          valueText.setAttribute('text-anchor', 'middle');
          valueText.textContent = humanNumber(value);
          svg.append(valueText);
        }
      });
      return;
    }

    const points = labels.map((_, index) => `${centre(index)},${y(entry.values[index] ?? 0)}`).join(' ');
    const polyline = document.createElementNS(svgNs, 'polyline');
    polyline.setAttribute('points', points);
    polyline.setAttribute('fill', 'none');
    polyline.setAttribute('stroke', color);
    polyline.setAttribute('stroke-width', '2');
    polyline.setAttribute('stroke-linejoin', 'round');
    polyline.setAttribute('stroke-linecap', 'round');
    svg.append(polyline);

    labels.forEach((label, index) => {
      const value = entry.values[index] ?? 0;
      const dot = document.createElementNS(svgNs, 'circle');
      dot.setAttribute('cx', String(centre(index)));
      dot.setAttribute('cy', String(y(value)));
      dot.setAttribute('r', '3');
      dot.setAttribute('fill', color);
      const tooltip = document.createElementNS(svgNs, 'title');
      tooltip.textContent = `${label} · ${entry.label}: ${humanNumber(value)}`;
      dot.append(tooltip);
      svg.append(dot);
    });
  });

  return svg;
}

function buildLegend(series: ParsedSeries[]): HTMLElement {
  const legend = element('div', 'ov-legend');
  series.forEach((entry, index) => {
    const item = element('span', 'ov-legend__item');
    const swatch = element('span', 'ov-legend__swatch');
    swatch.style.background = chartColor(index);
    const label = document.createElement('span');
    label.textContent = entry.label;
    item.append(swatch, label);
    legend.append(item);
  });
  return legend;
}

function progressCard(content: Record<string, unknown>, title?: string): HTMLElement {
  const value = requireNumber(content, 'value', 'progress card');
  const rawMax = optionalNumber(content, 'max', 'progress card');
  const max = rawMax !== undefined && rawMax > 0 ? rawMax : 100;
  const label = optionalString(content, 'label', 'progress card');
  const percent = Math.max(0, Math.min(100, (value / max) * 100));
  const card = createCard(title);

  const head = element('div', 'ov-progress__head');
  const labelNode = element('span', 'ov-progress__label');
  if (label) labelNode.textContent = label;
  const valueNode = element('span', 'ov-progress__value');
  valueNode.textContent = max === 100
    ? `${humanNumber(value)}%`
    : `${humanNumber(value)} / ${humanNumber(max)}`;
  head.append(labelNode, valueNode);

  const track = element('div', 'ov-progress__track');
  track.setAttribute('role', 'progressbar');
  track.setAttribute('aria-valuemin', '0');
  track.setAttribute('aria-valuemax', String(max));
  track.setAttribute('aria-valuenow', String(Math.max(0, Math.min(max, value))));
  if (label) track.setAttribute('aria-label', label);
  const fill = element('div', 'ov-progress__fill');
  fill.style.width = `${percent}%`;
  track.append(fill);

  card.append(head, track);
  return card;
}

interface ParsedChoice {
  label: string;
  value: Json;
  description?: string;
}

function choicesCard(content: Record<string, unknown>, title: string | undefined, context: WidgetRenderContext): HTMLElement {
  const multi = optionalBoolean(content, 'multi', 'choices card') === true;
  const options: ParsedChoice[] = requireArray(content, 'options', 'choices card').map((raw, index) => {
    const option = requireRecord(raw, `choices.options[${index}]`);
    return {
      label: requireString(option, 'label', `choices.options[${index}]`),
      value: requireJson(option.value, `choices.options[${index}].value`),
      description: optionalString(option, 'description', `choices.options[${index}]`),
    };
  });
  const settler = createSettler(context);
  const card = createCard(title);
  if (options.length === 0) {
    card.append(emptyNote('No choices.'));
    return card;
  }

  const groupName = `ov-choice-${context.view.id}-${++viewSerial}`;
  const list = element('div', 'ov-choices');
  const inputs: HTMLInputElement[] = [];
  options.forEach((option, index) => {
    const wrapper = document.createElement('label');
    wrapper.className = 'ov-choice';
    const input = document.createElement('input');
    input.type = multi ? 'checkbox' : 'radio';
    input.name = groupName;
    input.value = String(index);
    input.disabled = !settler.interactive;
    const text = element('span', 'ov-choice__text');
    const label = element('span', 'ov-choice__label');
    label.textContent = option.label;
    text.append(label);
    if (option.description) {
      const description = element('span', 'ov-choice__description');
      description.textContent = option.description;
      text.append(document.createElement('br'), description);
    }
    wrapper.append(input, text);
    list.append(wrapper);
    inputs.push(input);
  });

  const hint = element('div', 'ov-hint');
  const actions = element('div', 'ov-actions');
  const submit = button('Submit', true);
  submit.disabled = !settler.interactive;
  actions.append(submit);
  card.append(list, actions, hint);

  submit.addEventListener('click', () => {
    if (settler.settled || !settler.interactive) return;
    hint.textContent = '';
    const selected = options.filter((_, index) => inputs[index]?.checked === true);
    if (multi) {
      const answer = selected.map(option => option.value);
      if (settler.answer(answer)) {
        for (const input of inputs) input.disabled = true;
        submit.disabled = true;
      }
      return;
    }
    if (selected.length !== 1) {
      hint.textContent = 'Select an option first.';
      return;
    }
    const answer = selected[0]!.value;
    if (settler.answer(answer)) {
      for (const input of inputs) input.disabled = true;
      submit.disabled = true;
    }
  });

  return card;
}

function confirmCard(message: string, title: string | undefined, context: WidgetRenderContext): HTMLElement {
  const settler = createSettler(context);
  const card = createCard(title);
  const body = element('div', 'ov-text');
  body.textContent = message;
  const actions = element('div', 'ov-actions');
  const accept = button('Confirm', true);
  const decline = button('Cancel');
  accept.disabled = !settler.interactive;
  decline.disabled = !settler.interactive;
  actions.append(accept, decline);
  card.append(body, actions);

  accept.addEventListener('click', () => {
    if (settler.answer(true)) {
      accept.disabled = true;
      decline.disabled = true;
    }
  });
  decline.addEventListener('click', () => {
    if (settler.answer(false)) {
      accept.disabled = true;
      decline.disabled = true;
    }
  });

  return card;
}

interface ParsedField {
  name: string;
  label: string;
  type: FormFieldType;
  required: boolean;
  options: string[];
  value: string | number | boolean | undefined;
}

const FORM_FIELD_TYPES: FormFieldType[] = ['text', 'number', 'checkbox', 'select', 'textarea'];

function parseFormField(raw: unknown, index: number): ParsedField {
  const where = `form.fields[${index}]`;
  const field = requireRecord(raw, where);
  const name = requireString(field, 'name', where);
  if (!name) throw new Error(`${where}.name must not be empty.`);
  const label = requireString(field, 'label', where);
  const type = field.type;
  if (typeof type !== 'string' || !FORM_FIELD_TYPES.includes(type as FormFieldType)) {
    throw new Error(`${where}.type must be one of ${FORM_FIELD_TYPES.join(', ')}.`);
  }
  const required = optionalBoolean(field, 'required', where) === true;
  const options = field.options === undefined
    ? []
    : requireArray(field, 'options', where).map((option, optionIndex) => {
      if (typeof option !== 'string') throw new Error(`${where}.options[${optionIndex}] must be a string.`);
      return option;
    });
  const value = field.value;
  if (value !== undefined && typeof value !== 'string' && typeof value !== 'number' && typeof value !== 'boolean') {
    throw new Error(`${where}.value must be a string, number or boolean when present.`);
  }
  return { name, label, type: type as FormFieldType, required, options, value };
}

function formCard(content: Record<string, unknown>, title: string | undefined, context: WidgetRenderContext): HTMLElement {
  const fields = requireArray(content, 'fields', 'form card').map(parseFormField);
  const names = new Set<string>();
  for (const field of fields) {
    if (names.has(field.name)) throw new Error(`form.fields contains duplicate name "${field.name}".`);
    names.add(field.name);
  }
  const submitLabel = optionalString(content, 'submitLabel', 'form card') ?? 'Submit';
  const settler = createSettler(context);
  const card = createCard(title);
  if (fields.length === 0) {
    card.append(emptyNote('This form has no fields.'));
    return card;
  }

  const form = document.createElement('form');
  form.className = 'ov-form';
  form.noValidate = true;
  const controls = new Map<string, HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>();
  const errors = new Map<string, HTMLElement>();

  fields.forEach((field, index) => {
    const id = `ov-field-${context.view.id}-${index}-${++viewSerial}`;
    const wrapper = element('div', 'ov-field');
    let control: HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement;

    if (field.type === 'checkbox') {
      wrapper.classList.add('ov-field--check');
      const label = document.createElement('label');
      control = document.createElement('input');
      control.type = 'checkbox';
      control.checked = field.value === true;
      label.append(control);
      const text = document.createElement('span');
      text.textContent = field.label;
      label.append(text);
      if (field.required) label.append(requiredMark());
      wrapper.append(label);
    } else {
      const label = document.createElement('label');
      label.className = 'ov-field__label';
      label.htmlFor = id;
      label.textContent = field.label;
      if (field.required) label.append(requiredMark());
      wrapper.append(label);

      if (field.type === 'textarea') {
        control = document.createElement('textarea');
        control.value = typeof field.value === 'string' ? field.value : '';
      } else if (field.type === 'select') {
        const select = document.createElement('select');
        const placeholder = document.createElement('option');
        placeholder.value = '';
        placeholder.textContent = 'Select…';
        select.append(placeholder);
        for (const option of field.options) {
          const node = document.createElement('option');
          node.value = option;
          node.textContent = option;
          select.append(node);
        }
        if (field.value !== undefined) select.value = String(field.value);
        control = select;
      } else {
        const input = document.createElement('input');
        input.type = field.type === 'number' ? 'number' : 'text';
        if (field.value !== undefined) input.value = String(field.value);
        control = input;
      }
      control.id = id;
      wrapper.append(control);
    }

    control.disabled = !settler.interactive;
    control.name = field.name;
    const error = element('div', 'ov-field__error');
    error.setAttribute('aria-live', 'polite');
    wrapper.append(error);
    errors.set(field.name, error);
    controls.set(field.name, control);
    form.append(wrapper);
  });

  const actions = element('div', 'ov-actions');
  const submit = button(submitLabel, true);
  submit.type = 'submit';
  submit.disabled = !settler.interactive;
  actions.append(submit);
  form.append(actions);
  card.append(form);

  const clearError = (field: ParsedField): void => {
    const error = errors.get(field.name);
    if (error) error.textContent = '';
    const control = controls.get(field.name);
    if (control) control.removeAttribute('aria-invalid');
  };

  form.addEventListener('submit', event => {
    event.preventDefault();
    if (settler.settled || !settler.interactive) return;
    const answer: FormAnswers = {};
    let firstInvalid: HTMLElement | undefined;

    for (const field of fields) {
      clearError(field);
      const control = controls.get(field.name);
      if (!control) continue;
      const error = errors.get(field.name);
      const fail = (message: string): void => {
        if (error) error.textContent = message;
        control.setAttribute('aria-invalid', 'true');
        firstInvalid ??= control;
      };

      if (field.type === 'checkbox') {
        const checked = (control as HTMLInputElement).checked;
        answer[field.name] = checked;
        if (field.required && !checked) fail('This box must be checked.');
        continue;
      }

      if (field.type === 'number') {
        const raw = control.value.trim();
        if (raw === '') {
          answer[field.name] = null;
          if (field.required) fail('A number is required.');
          continue;
        }
        const parsed = Number(raw);
        if (!Number.isFinite(parsed)) {
          answer[field.name] = null;
          fail('Enter a valid number.');
          continue;
        }
        answer[field.name] = parsed;
        continue;
      }

      if (field.type === 'select') {
        const selected = control.value;
        answer[field.name] = selected;
        if (field.required && selected === '') fail('Select an option.');
        continue;
      }

      const text = control.value;
      answer[field.name] = text;
      if (field.required && text.trim() === '') fail('This field is required.');
    }

    if (firstInvalid) {
      firstInvalid.focus();
      return;
    }

    if (settler.answer(answer)) {
      for (const control of controls.values()) control.disabled = true;
      submit.disabled = true;
    }
  });

  return card;
}

function requiredMark(): HTMLElement {
  const mark = element('span', 'ov-field__required');
  mark.textContent = '*';
  mark.setAttribute('aria-hidden', 'true');
  return mark;
}

const HTML_CSP = [
  "default-src 'none'",
  "img-src data: blob:",
  "media-src data: blob:",
  "style-src 'unsafe-inline'",
  "font-src data:",
  "script-src 'unsafe-inline'",
  "connect-src 'none'",
  "form-action 'none'",
  "base-uri 'none'",
  "object-src 'none'",
].join('; ');

const HTML_BRIDGE = [
  `<meta http-equiv="Content-Security-Policy" content="${HTML_CSP}">`,
  '<script>',
  '(function () {',
  '  var send = function (message) { parent.postMessage(message, "*"); };',
  '  window.onlyneWidget = Object.freeze({',
  '    submit: function (value) { send({ type: "onlyne:widget:submit", value: value }); },',
  '    cancel: function () { send({ type: "onlyne:widget:cancel" }); }',
  '  });',
  '  document.addEventListener("click", function (event) {',
  '    var path = event.composedPath ? event.composedPath() : [];',
  '    for (var index = 0; index < path.length; index += 1) {',
  '      if (path[index] && path[index].tagName === "A") { event.preventDefault(); return; }',
  '    }',
  '  }, true);',
  '  document.addEventListener("submit", function (event) { event.preventDefault(); }, true);',
  '  document.addEventListener("keydown", function (event) {',
  '    if (event.key !== "Escape") return;',
  '    event.preventDefault();',
  '    send({ type: "onlyne:widget:cancel" });',
  '  }, true);',
  '})();',
  '</script>',
].join('\n');

function sandboxDocument(html: string): string {
  const head = /<head(?:\s[^>]*)?>/i.exec(html);
  if (head) {
    const at = head.index + head[0].length;
    return `${html.slice(0, at)}${HTML_BRIDGE}${html.slice(at)}`;
  }
  const root = /<html(?:\s[^>]*)?>/i.exec(html);
  if (root) {
    const at = root.index + root[0].length;
    return `${html.slice(0, at)}<head>${HTML_BRIDGE}</head>${html.slice(at)}`;
  }
  return `<!doctype html><html><head>${HTML_BRIDGE}</head><body>${html}</body></html>`;
}

function htmlCard(html: string, title: string | undefined, context: WidgetRenderContext): HTMLElement {
  ensureStyles();
  const settler = createSettler(context);
  const card = createCard(title);
  const frame = document.createElement('iframe');
  frame.className = 'ov-html-frame';
  frame.setAttribute('sandbox', 'allow-scripts');
  frame.setAttribute('referrerpolicy', 'no-referrer');
  const frameLabel = title ?? context.view.title;
  frame.title = frameLabel ? `${frameLabel} (sandboxed HTML)` : 'Sandboxed HTML widget';
  frame.srcdoc = sandboxDocument(html);
  card.append(frame);

  const actions = element('div', 'ov-actions');
  const cancel = button('Cancel');
  cancel.disabled = !settler.interactive;
  actions.append(cancel);
  card.append(actions);

  const onMessage = (event: MessageEvent): void => {
    if (event.source !== frame.contentWindow) return;
    // The sandboxed srcdoc document has an opaque origin; a real navigation would not.
    if (event.origin !== 'null') return;
    if (!settler.interactive || settler.settled) return;
    if (!isRecord(event.data)) return;
    if (event.data.type === 'onlyne:widget:submit') {
      const value = event.data.value;
      if (!isJsonValue(value)) return;
      if (settler.answer(value)) cancel.disabled = true;
      return;
    }
    if (event.data.type === 'onlyne:widget:cancel') {
      if (settler.cancel()) cancel.disabled = true;
    }
  };
  window.addEventListener('message', onMessage, { signal: settler.signal });

  cancel.addEventListener('click', () => {
    if (settler.cancel()) cancel.disabled = true;
  });

  let attached = false;
  const observer = new MutationObserver(() => {
    if (card.isConnected) {
      attached = true;
      return;
    }
    if (!attached) return;
    observer.disconnect();
    settler.dispose();
  });
  const body = document.body;
  if (body) observer.observe(body, { childList: true, subtree: true });
  settler.onSettle(() => {
    observer.disconnect();
    settler.dispose();
  });

  return card;
}

function contentString(content: Json, key: 'text' | 'html'): string {
  if (typeof content === 'string') return content;
  if (isRecord(content) && typeof content[key] === 'string') return content[key];
  return JSON.stringify(content, null, 2);
}

function markdownCard(markdown: string, title?: string): HTMLElement {
  ensureStyles();
  const card = createCard(title);
  card.append(renderMarkdown(markdown));
  return card;
}

interface MarkdownBlockMatch {
  ordered: boolean;
  text: string;
}

function markdownListMatch(line: string): MarkdownBlockMatch | undefined {
  const match = /^(\s*)([-*+]|\d+\.)\s+(.*)$/.exec(line);
  if (!match) return undefined;
  const marker = match[2] ?? '';
  return {
    ordered: /\d+\./.test(marker),
    text: match[3] ?? '',
  };
}

function isBlockStart(line: string): boolean {
  return (
    /^\s*(```|~~~)/.test(line) ||
    /^(#{1,6})\s+/.test(line) ||
    /^\s*>\s?/.test(line) ||
    /^\s*(?:---+|\*\*\*+|___+)\s*$/.test(line) ||
    markdownListMatch(line) !== undefined
  );
}

function renderMarkdown(markdown: string): HTMLElement {
  const root = element('div', 'ov-markdown');
  const lines = markdown.replace(/\r\n?/g, '\n').split('\n');
  let index = 0;

  while (index < lines.length) {
    const line = lines[index];
    if (line === undefined) break;
    if (/^\s*$/.test(line)) {
      index += 1;
      continue;
    }

    const fence = /^\s*(```|~~~)\s*([\w+-]*)\s*$/.exec(line);
    if (fence) {
      const marker = fence[1] ?? '';
      const codeLines: string[] = [];
      index += 1;
      while (index < lines.length) {
        const candidate = lines[index];
        if (candidate === undefined || new RegExp(`^\\s*${marker}\\s*$`).test(candidate)) break;
        codeLines.push(candidate);
        index += 1;
      }
      if (index < lines.length) index += 1;
      const pre = document.createElement('pre');
      const code = document.createElement('code');
      if (fence[2]) code.className = `language-${fence[2]}`;
      code.textContent = codeLines.join('\n');
      pre.append(code);
      root.append(pre);
      continue;
    }

    const heading = /^(#{1,6})\s+(.*)$/.exec(line);
    if (heading) {
      const level = Math.min(6, (heading[1] ?? '').length);
      const node = document.createElement(`h${level}`);
      appendInline(node, heading[2] ?? '');
      root.append(node);
      index += 1;
      continue;
    }

    if (/^\s*(?:---+|\*\*\*+|___+)\s*$/.test(line)) {
      root.append(document.createElement('hr'));
      index += 1;
      continue;
    }

    if (/^\s*>\s?/.test(line)) {
      const quote = document.createElement('blockquote');
      const quoted: string[] = [];
      while (index < lines.length) {
        const candidate = lines[index];
        if (candidate === undefined) break;
        const match = /^\s*>\s?(.*)$/.exec(candidate);
        if (!match) break;
        quoted.push(match[1] ?? '');
        index += 1;
      }
      const paragraph = document.createElement('p');
      appendInline(paragraph, quoted.join('\n'));
      quote.append(paragraph);
      root.append(quote);
      continue;
    }

    const list = markdownListMatch(line);
    if (list) {
      const listNode = document.createElement(list.ordered ? 'ol' : 'ul');
      const expectOrdered = list.ordered;
      while (index < lines.length) {
        const candidate = lines[index];
        if (candidate === undefined) break;
        const item = markdownListMatch(candidate);
        if (!item || item.ordered !== expectOrdered) break;
        let text = item.text;
        index += 1;
        while (index < lines.length) {
          const continuation = lines[index];
          if (continuation === undefined || !/^\s{2,}\S/.test(continuation) || markdownListMatch(continuation)) break;
          text += ` ${continuation.trim()}`;
          index += 1;
        }
        const li = document.createElement('li');
        appendInline(li, text);
        listNode.append(li);
      }
      root.append(listNode);
      continue;
    }

    const paragraphLines: string[] = [line.trim()];
    index += 1;
    while (index < lines.length) {
      const candidate = lines[index];
      if (candidate === undefined || /^\s*$/.test(candidate) || isBlockStart(candidate)) break;
      paragraphLines.push(candidate.trim());
      index += 1;
    }
    const paragraph = document.createElement('p');
    appendInline(paragraph, paragraphLines.join('\n'));
    root.append(paragraph);
  }

  return root;
}

function findClosingBracket(text: string, open: number): number {
  let depth = 0;
  for (let index = open; index < text.length; index += 1) {
    const char = text[index];
    if (char === '\\') {
      index += 1;
      continue;
    }
    if (char === '[') depth += 1;
    else if (char === ']') {
      depth -= 1;
      if (depth === 0) return index;
    }
  }
  return -1;
}

function safeAnchor(label: string, href: string): HTMLAnchorElement | undefined {
  let url: URL;
  try {
    url = new URL(href);
  } catch {
    return undefined;
  }
  if (url.protocol !== 'http:' && url.protocol !== 'https:' && url.protocol !== 'mailto:') return undefined;
  const anchor = document.createElement('a');
  anchor.href = url.href;
  anchor.target = '_blank';
  anchor.rel = 'noopener noreferrer';
  anchor.textContent = label;
  return anchor;
}

function appendInline(target: HTMLElement, text: string): void {
  let index = 0;
  let plain = '';
  const flush = (): void => {
    if (!plain) return;
    target.append(document.createTextNode(plain));
    plain = '';
  };

  while (index < text.length) {
    const char = text[index];

    if (char === '\\' && index + 1 < text.length) {
      plain += text[index + 1];
      index += 2;
      continue;
    }

    if (char === '`') {
      const end = text.indexOf('`', index + 1);
      if (end > index + 1) {
        flush();
        const code = document.createElement('code');
        code.textContent = text.slice(index + 1, end);
        target.append(code);
        index = end + 1;
        continue;
      }
    }

    if (char === '*' || char === '_') {
      const marker = text[index + 1] === char ? char + char : char;
      const end = text.indexOf(marker, index + marker.length);
      if (end > index + marker.length) {
        flush();
        const node = document.createElement(marker.length === 2 ? 'strong' : 'em');
        appendInline(node, text.slice(index + marker.length, end));
        target.append(node);
        index = end + marker.length;
        continue;
      }
    }

    if (char === '~' && text[index + 1] === '~') {
      const end = text.indexOf('~~', index + 2);
      if (end > index + 2) {
        flush();
        const node = document.createElement('del');
        appendInline(node, text.slice(index + 2, end));
        target.append(node);
        index = end + 2;
        continue;
      }
    }

    if (char === '[' || (char === '!' && text[index + 1] === '[')) {
      const open = char === '!' ? index + 1 : index;
      const close = findClosingBracket(text, open);
      if (close !== -1 && text[close + 1] === '(') {
        const urlEnd = text.indexOf(')', close + 2);
        if (urlEnd !== -1) {
          const label = text.slice(open + 1, close);
          const rawUrl = (text.slice(close + 2, urlEnd).trim().split(/\s+/)[0] ?? '').replace(/^<|>$/g, '');
          flush();
          const anchor = rawUrl ? safeAnchor(label || rawUrl, rawUrl) : undefined;
          if (anchor) target.append(anchor);
          else appendInline(target, label || rawUrl);
          index = urlEnd + 1;
          continue;
        }
      }
    }

    if (char === '\n') {
      flush();
      target.append(document.createElement('br'));
      index += 1;
      continue;
    }

    plain += char;
    index += 1;
  }

  flush();
}

/**
 * Registers the built-in renderers on a widget registry.
 *
 * `onlyne.view/v1` carries a `ViewCard` or `ViewCard[]`-shaped `stack`; the
 * text adapters accept either a raw string or `{ text }` / `{ html }`.
 */
export function registerBuiltinViews(registry: WidgetRegistry): void {
  registry.register('onlyne.view/v1', (content, context) => renderViewContent(content, context));
  registry.register('text/plain', (content) => textCard(contentString(content, 'text')));
  registry.register('text/markdown', (content) => markdownCard(contentString(content, 'text')));
  registry.register('text/html', (content, context) => htmlCard(contentString(content, 'html'), undefined, context));
}
