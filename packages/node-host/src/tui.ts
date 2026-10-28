import { trustPhase, type KeyExchange, type OnlyneNode, type TrustPhase } from '@onlyne/lib';
import type { LogRing } from './log.ts';
import { t, type TuiTextKey } from './tui-text.ts';

/** One peer as the terminal shows it: a discovery address plus whatever the trust table knows. */
interface Row {
  node: string;
  endpoint?: string;
  phase?: TrustPhase;
  fingerprint?: string;
  /** Resources this gateway serves whose binding points at this node: where events go now. */
  delivery?: string[];
}

const PHASE_KEY: Record<TrustPhase, TuiTextKey> = {
  exchanged: 'phase.exchanged',
  requested: 'phase.requested',
  invited: 'phase.invited',
  revoked: 'phase.revoked',
};

const NO_PHASE = '—';
const SHORT = (node: string): string => node.split(':').pop() ?? node;

function failureText(error: unknown): string {
  if (error instanceof Error && 'error' in error) {
    const inner = (error as { error?: { message?: string } }).error;
    if (typeof inner?.message === 'string') return inner.message;
  }
  return String(error);
}

type Mode = { kind: 'list' } | { kind: 'confirm'; exchange: KeyExchange } | { kind: 'input' };

/**
 * The gateway's foreground surface: one screen for the peers it discovered, the trust stage
 * of each, and the four actions that change one. Every confirmation shows the fingerprint
 * first, because that string — not the node id — is what the two operators compare.
 */
export class GatewayTui {
  #rows: Row[] = [];
  #index = 0;
  #mode: Mode = { kind: 'list' };
  #input = '';
  #message = '';
  #busy = false;
  #closed = false;
  #refreshTimer?: ReturnType<typeof setInterval>;
  #statusLines: string[] = [];

  constructor(
    private readonly node: OnlyneNode,
    private readonly ring: LogRing,
    private readonly endpoint: string,
    private readonly onChange: () => void = () => undefined,
    private readonly status?: () => Promise<string[]>,
  ) {}

  async start(): Promise<void> {
    process.stdout.write('\x1b[?1049h\x1b[?25l');
    if (process.stdin.isTTY) process.stdin.setRawMode(true);
    process.stdin.resume();
    process.stdin.setEncoding('utf8');
    process.stdin.on('data', this.#onData);
    process.stdout.on('resize', this.#onResize);
    await this.#refresh();
    this.#refreshTimer = setInterval(() => { void this.#refresh(); }, 1000);
  }

  async close(): Promise<void> {
    if (this.#closed) return;
    this.#closed = true;
    clearInterval(this.#refreshTimer);
    process.stdin.off('data', this.#onData);
    process.stdout.off('resize', this.#onResize);
    process.stdin.pause();
    if (process.stdin.isTTY) process.stdin.setRawMode(false);
    process.stdout.write('\x1b[?25h\x1b[?1049l');
  }

  #onResize = (): void => { this.#draw(); };

  #onData = (chunk: string): void => {
    if (this.#closed) return;
    // One `data` event can carry a whole paste or a burst of keystrokes, so it is split into
    // keys rather than treated as one. Each key runs in order; `#busy` still gates actions.
    for (const token of tokens(chunk)) void this.#handle(token);
  };

  async #handle(token: Token): Promise<void> {
    if (this.#busy) return;
    if (this.#mode.kind === 'confirm') {
      if (token === 'y') await this.#settle(true);
      else if (token === 'n' || token === 'esc') { this.#mode = { kind: 'list' }; this.#message = t('notice.cancelled'); this.#draw(); }
      return;
    }
    if (this.#mode.kind === 'input') {
      if (token === 'esc') { this.#mode = { kind: 'list' }; this.#input = ''; this.#draw(); return; }
      if (token === 'enter') { await this.#connect(); return; }
      if (token === 'backspace') { this.#input = this.#input.slice(0, -1); this.#draw(); return; }
      if (typeof token === 'string' && token.length === 1 && token >= ' ') { this.#input += token; this.#draw(); }
      return;
    }
    if (token === 'q') { await this.close(); this.onChange(); return; }
    if (token === 'up') { this.#move(-1); return; }
    if (token === 'down') { this.#move(1); return; }
    if (token === 'c') { this.#mode = { kind: 'input' }; this.#input = ''; this.#message = t('notice.enterAddress'); this.#draw(); return; }
    const row = this.#rows[this.#index];
    if (row === undefined) return;
    if (token === 't') { await this.#begin(row); return; }
    if (token === 'a') { await this.#accept(row); return; }
    if (token === 'r') { await this.#revoke(row); return; }
  }

  #move(delta: number): void {
    if (this.#rows.length === 0) return;
    this.#index = (this.#index + delta + this.#rows.length) % this.#rows.length;
    this.#draw();
  }

  async #refresh(): Promise<void> {
    if (this.#closed) return;
    try {
      const [trust, records, status] = await Promise.all([
        this.node.trust.list(this.node.auth),
        this.node.discovery.listRecords(),
        this.status?.() ?? [],
      ]);
      this.#statusLines = status;
      const handlers = new Map<string, string[]>();
      const resources = await this.node.resources.list(this.node.auth, { page_size: 100 });
      for (const resource of resources.items) {
        const list = handlers.get(resource.binding.handler) ?? [];
        list.push(resource.resource_ref.id);
        handlers.set(resource.binding.handler, list);
      }
      const rows = new Map<string, Row>();
      for (const record of records) {
        if (record.node_id === this.node.auth.actor) continue;
        rows.set(record.node_id, { node: record.node_id, endpoint: record.base_url });
      }
      for (const entry of trust) {
        if (entry.node_id === this.node.auth.actor) continue;
        const row = rows.get(entry.node_id) ?? { node: entry.node_id };
        rows.set(entry.node_id, { ...row, phase: trustPhase(entry), fingerprint: entry.fingerprint, endpoint: row.endpoint ?? entry.base_url });
      }
      for (const [handler, ids] of handlers) {
        if (handler === this.node.auth.actor) continue;
        const row = rows.get(handler) ?? { node: handler };
        rows.set(handler, { ...row, delivery: ids });
      }
      this.#rows = [...rows.values()].sort((a, b) => (a.node < b.node ? -1 : a.node > b.node ? 1 : 0));
      if (this.#index >= this.#rows.length) this.#index = Math.max(0, this.#rows.length - 1);
      this.#draw();
    } catch (error) {
      this.#message = t('notice.refreshFailed', { error: failureText(error) });
      this.#draw();
    }
  }

  async #begin(row: Row): Promise<void> {
    if (row.phase === 'exchanged') { this.#message = t('notice.alreadyExchanged', { node: SHORT(row.node) }); this.#draw(); return; }
    await this.#run(async () => {
      const exchange = await this.node.trust.begin(this.node.auth, row.node);
      this.#mode = { kind: 'confirm', exchange };
      this.#message = '';
    });
  }

  async #accept(row: Row): Promise<void> {
    if (row.phase !== 'invited' || row.fingerprint === undefined) {
      this.#message = t('notice.noRequest', { node: SHORT(row.node) });
      this.#draw();
      return;
    }
    const fingerprint = row.fingerprint;
    await this.#run(async () => {
      await this.node.trust.accept(this.node.auth, row.node, fingerprint);
      this.#message = t('notice.accepted', { node: SHORT(row.node) });
    });
  }

  async #revoke(row: Row): Promise<void> {
    if (row.phase === undefined) { this.#message = t('notice.noTrust', { node: SHORT(row.node) }); this.#draw(); return; }
    await this.#run(async () => {
      await this.node.trust.revoke(this.node.auth, row.node);
      this.#message = t('notice.revoked', { node: SHORT(row.node) });
    });
  }

  async #connect(): Promise<void> {
    const value = this.#input.trim();
    this.#mode = { kind: 'list' };
    this.#input = '';
    if (value === '') { this.#message = t('notice.noAddress'); this.#draw(); return; }
    await this.#run(async () => {
      const url = new URL(value.includes('://') ? value : `http://${value}`);
      url.pathname = url.pathname.replace(/\/+$/, '') || '/onlyne/v3';
      const record = await this.node.discovery.observe(url.href, 'manual');
      this.#message = t('notice.connected', { node: record.node_id });
    });
  }

  async #settle(accept: boolean): Promise<void> {
    const mode = this.#mode;
    if (mode.kind !== 'confirm') return;
    if (!accept) { this.#mode = { kind: 'list' }; this.#message = t('notice.cancelled'); this.#draw(); return; }
    this.#mode = { kind: 'list' };
    await this.#run(async () => {
      const entry = await mode.exchange.confirm(mode.exchange.fingerprint);
      this.#message = t('notice.submitted', { node: SHORT(entry.node_id), phase: t(PHASE_KEY[trustPhase(entry)]) });
    });
  }

  async #run(work: () => Promise<void>): Promise<void> {
    this.#busy = true;
    this.#draw();
    try { await work(); }
    catch (error) { this.#message = t('notice.operationFailed', { error: failureText(error) }); }
    finally {
      this.#busy = false;
      await this.#refresh();
    }
  }

  #draw(): void {
    if (this.#closed) return;
    // One cell short of the terminal width: writing the last cell auto-wraps, which would
    // consume a second row per line and push the frame upward.
    const width = Math.max(48, (process.stdout.columns ?? 80) - 1);
    const height = Math.max(16, process.stdout.rows ?? 24);
    const lines: string[] = [];
    const push = (text: string): void => { lines.push(fit(text, width)); };

    push(t('title', { actor: this.node.auth.actor, endpoint: this.endpoint }));
    push(rule(width));

    if (this.#statusLines.length > 0) {
      const budget = Math.max(3, Math.floor(height / 3));
      for (const line of this.#statusLines.slice(0, budget)) push(line);
      if (this.#statusLines.length > budget) push('…');
      push(rule(width));
    }

    const peerHeader = this.#rows.length === 0 ? t('peers.none') : t('peers.count', { count: this.#rows.length });
    push(peerHeader);
    for (const [index, row] of this.#rows.entries()) {
      const marker = index === this.#index ? '▸' : ' ';
      const phase = row.phase === undefined ? NO_PHASE : t(PHASE_KEY[row.phase]);
      // Delivery is the binding's answer, not the trust stage's: a node can be exchanged and
      // still receive nothing, and the handler can be a node with no key exchange at all.
      const delivery = row.delivery === undefined ? '' : t('column.delivery');
      push(`${marker} ${SHORT(row.node).padEnd(14)} ${phase.padEnd(14)} ${delivery.padEnd(8)} ${row.endpoint ?? ''}`);
    }

    push(rule(width));
    const selected = this.#rows[this.#index];
    if (selected === undefined) {
      push(t('hint.idle'));
    } else {
      push(selected.node);
      push(`${t('label.fingerprint').padEnd(6)}${selected.fingerprint ?? t('value.notExchanged')}`);
      push(`${t('label.endpoint').padEnd(6)}${selected.endpoint ?? t('value.unknown')}`);
      if (selected.delivery !== undefined) push(`${t('label.delivery').padEnd(6)}${selected.delivery.join(t('list.separator'))}`);
    }

    push(rule(width));
    if (this.#mode.kind === 'confirm') {
      const exchange = this.#mode.exchange;
      push(t('confirm.title', { node: SHORT(exchange.node_id) }));
      push(`  ${exchange.fingerprint}`);
      push(t('confirm.keys'));
    } else if (this.#mode.kind === 'input') {
      push(t('input.prompt', { value: this.#input }));
    } else {
      push(t('help.keys'));
    }
    if (this.#message !== '') push(this.#message);
    if (this.#busy) push('…');

    const logLines = Math.max(0, height - lines.length - 1);
    if (logLines > 0) {
      push(rule(width));
      for (const line of this.ring.tail(logLines)) push(line);
    }

    let frame = '\x1b[H';
    for (let index = 0; index < height; index++) {
      frame += `${lines[index] ?? ''}\x1b[K${index === height - 1 ? '' : '\r\n'}`;
    }
    frame += '\x1b[J';
    process.stdout.write(frame);
  }
}

type Token = 'up' | 'down' | 'enter' | 'esc' | 'backspace' | 'y' | 'n' | 'q' | 't' | 'a' | 'r' | 'c' | string;

/** Splits one stdin chunk into keys: CSI arrows, a bare escape, and every printable character. */
function tokens(chunk: string): Token[] {
  const out: Token[] = [];
  let rest = chunk.replace(/\x1b\[20[01]~/g, '');
  while (rest.length > 0) {
    if (rest.startsWith('\x1b[A')) { out.push('up'); rest = rest.slice(3); continue; }
    if (rest.startsWith('\x1b[B')) { out.push('down'); rest = rest.slice(3); continue; }
    if (rest.startsWith('\x1b[')) {
      // Any other control sequence is not a key this surface uses; drop it whole.
      const end = rest.slice(2).search(/[@-~]/);
      rest = end < 0 ? '' : rest.slice(end + 3);
      continue;
    }
    if (rest.startsWith('\x1b')) { out.push('esc'); rest = rest.slice(1); continue; }
    const character = rest[0]!;
    if (character === '\r' || character === '\n') out.push('enter');
    else if (character === '\x7f' || character === '\b') out.push('backspace');
    else if (character === '\x03') out.push('q');
    else if (character >= ' ') out.push(character);
    rest = rest.slice(1);
  }
  return out;
}

function fit(text: string, width: number): string {
  if (text.length <= width) return text;
  return `${text.slice(0, width - 1)}…`;
}

function rule(width: number): string {
  return '─'.repeat(width);
}
