/**
 * Desk surfaces for the Onlyne node: inbox, neighbors, trust, threads,
 * resources, health, integrations and the team. Each one is a panel plugin.
 */

import type { Bot, Capability, DesktopState } from '../contracts.ts';
import type { DesktopUiApi } from './api.ts';
import { fingerprintGroups, fingerprintHint, fingerprintMarks, formatWhen, middleEllipsis, parsePeerUrl, peerPhaseLabel } from '../desk-format.ts';
import { formatFingerprint } from './layout.ts';

export interface DeskHooks {
  notify(message: string, level?: string): void;
  open(id: string): void;
  createBot(): void;
}

type Command = DesktopUiApi['command'];
type Child = Node | string | number | null | undefined | false;
type Attrs = Record<string, unknown>;

function h<K extends keyof HTMLElementTagNameMap>(tag: K, attrs: Attrs = {}, ...children: Child[]): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (value === undefined || value === null || value === false) continue;
    if (key.startsWith('on') && typeof value === 'function') node.addEventListener(key.slice(2).toLowerCase(), value as EventListener);
    else if (key === 'class') node.className = String(value);
    else if (key === 'text') node.textContent = String(value);
    else if (key === 'value' && (node instanceof HTMLInputElement || node instanceof HTMLTextAreaElement || node instanceof HTMLSelectElement)) node.value = String(value);
    else if (key === 'disabled' && (node instanceof HTMLButtonElement || node instanceof HTMLInputElement || node instanceof HTMLSelectElement || node instanceof HTMLTextAreaElement)) node.disabled = Boolean(value);
    else if (key === 'hidden') node.hidden = Boolean(value);
    else node.setAttribute(key, value === true ? '' : String(value));
  }
  for (const child of children) {
    if (child === null || child === undefined || child === false) continue;
    node.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return node;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function textOf(value: unknown): string {
  if (typeof value === 'string') return value;
  if (typeof value === 'number' || typeof value === 'boolean') return String(value);
  return '';
}

function pick(record: Record<string, unknown> | null, keys: string[]): string {
  if (record === null) return '';
  for (const key of keys) {
    const value = textOf(record[key]);
    if (value.length > 0) return value;
  }
  return '';
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

let scopeId = '';
let demoKnown: boolean | null = null;

function botsOf(state: DesktopState): Bot[] {
  return state.bots;
}

function scopeOf(state: DesktopState): string {
  if (scopeId.length > 0 && state.bots.some(bot => bot.id === scopeId)) return scopeId;
  const admin = state.teamAdminId;
  scopeId = admin !== undefined && state.bots.some(bot => bot.id === admin) ? admin : state.bots[0]?.id ?? '';
  return scopeId;
}

function scopePayload(state: DesktopState): { botId: string } | Record<string, never> {
  const botId = scopeOf(state);
  return botId.length > 0 ? { botId } : {};
}

async function knowDemo(command: Command): Promise<boolean> {
  if (demoKnown !== null) return demoKnown;
  try {
    const health = await command<{ demo?: boolean }>('desktop.health');
    demoKnown = isRecord(health) && health.demo === true;
  } catch {
    demoKnown = false;
  }
  return demoKnown;
}

function skeleton(lines = 4): HTMLElement {
  return h('div', { class: 'skeleton-stack', role: 'status', 'aria-label': 'Loading' },
    ...Array.from({ length: lines }, (_, index) => h('div', { class: index === lines - 1 ? 'skeleton short' : 'skeleton' })));
}

function emptyBlock(title: string, body: string, action?: HTMLElement): HTMLElement {
  return h('div', { class: 'desk-empty' },
    h('h2', { text: title }),
    h('p', { text: body }),
    action ?? false);
}

function errorBlock(message: string, retry: () => void): HTMLElement {
  return h('div', { class: 'desk-error', role: 'alert' },
    h('h2', { text: 'This view could not be read' }),
    h('p', { text: message }),
    h('button', { class: 'btn', type: 'button', onclick: retry }, 'Try again'));
}

function chip(text: string, tone = ''): HTMLElement {
  return h('span', { class: tone.length > 0 ? `chip ${tone}` : 'chip', text });
}

function botSelect(state: DesktopState, onChange: () => void): HTMLSelectElement {
  const select = h('select', { 'aria-label': 'Bot whose node to read' });
  const current = scopeOf(state);
  for (const bot of botsOf(state)) select.append(h('option', { value: bot.id, text: bot.name }));
  select.value = current;
  select.addEventListener('change', () => {
    scopeId = select.value;
    onChange();
  });
  return select;
}

function page(eyebrow: string, title: string, lede: string, tools: Child[], body: HTMLElement, note?: string): HTMLElement {
  return h('div', { class: 'desk-page' },
    h('header', { class: 'desk-page-head' },
      h('div', { class: 'desk-page-copy' },
        h('p', { class: 'eyebrow', text: eyebrow }),
        h('h1', { text: title }),
        h('p', { class: 'lede', text: lede })),
      h('div', { class: 'desk-page-tools' }, ...tools)),
    note === undefined ? false : h('p', { class: 'mode-note', text: note }),
    body);
}

function kv(rows: Array<[string, Child]>): HTMLElement {
  const list = h('dl', { class: 'kv' });
  for (const [key, value] of rows) {
    if (value === false || value === null || value === undefined || value === '') continue;
    const dd = h('dd');
    if (typeof value === 'string' || typeof value === 'number') dd.textContent = String(value);
    else dd.append(value);
    list.append(h('dt', { text: key }), dd);
  }
  return list;
}

function copyValue(hooks: DeskHooks, value: string, label: string): void {
  void navigator.clipboard?.writeText(value).then(
    () => hooks.notify(label, 'ok'),
    () => hooks.notify('The clipboard is unavailable. Select the value and copy it.', 'warn'),
  );
}

function identifier(value: string, hooks: DeskHooks): HTMLElement {
  if (value.length === 0) return h('span', { text: '—' });
  const button = h('button', { class: 'id-copy', type: 'button', 'aria-label': `Copy ${value}` }, 'Copy');
  button.addEventListener('click', () => copyValue(hooks, value, 'Copied.'));
  return h('span', { class: 'identifier', title: value },
    h('span', { class: 'identifier-text', text: middleEllipsis(value) }),
    button);
}

function whenView(value: string): HTMLElement | string {
  if (value.length === 0) return '';
  const when = formatWhen(value);
  return h('time', { datetime: value, title: when.title, text: when.label });
}

async function loadStatus(command: Command, state: DesktopState): Promise<Record<string, unknown>> {
  const value = await command('onlyne.status', scopePayload(state));
  if (!isRecord(value)) throw new Error('Node status was not an object.');
  return value;
}

async function loadRead(command: Command, state: DesktopState, uri: string): Promise<unknown> {
  const value = await command<{ content?: unknown }>('desktop.read', { uri, ...scopePayload(state) });
  if (!isRecord(value) || typeof value.content !== 'string') throw new Error('The node read returned no document.');
  return JSON.parse(value.content) as unknown;
}

function asArray(value: unknown): Record<string, unknown>[] {
  if (!Array.isArray(value)) return [];
  return value.filter(isRecord);
}

function messageBody(row: Record<string, unknown>): string {
  const attention = isRecord(row.attention) ? row.attention : row;
  const entry = isRecord(attention.entry) ? attention.entry : null;
  const message = entry !== null && isRecord(entry.message) ? entry.message : isRecord(attention.message) ? attention.message : null;
  return message === null ? '' : pick(message, ['msg', 'text']);
}

function messageSender(row: Record<string, unknown>): string {
  const attention = isRecord(row.attention) ? row.attention : row;
  const entry = isRecord(attention.entry) ? attention.entry : null;
  const message = entry !== null && isRecord(entry.message) ? entry.message : null;
  return pick(message, ['sender']) || pick(attention, ['authenticated_author', 'sender']);
}

function shortNode(id: string): string {
  const parts = id.split(':').filter(part => part.length > 0);
  return parts.length === 0 ? id : parts[parts.length - 1]!;
}

function toneForPhase(phase: string): string {
  if (phase === 'exchanged' || phase === 'trusted') return 'ok';
  if (phase === 'revoked') return 'err';
  if (phase === 'invited' || phase === 'requested' || phase === 'pending' || phase === 'awaiting-approval') return 'warn';
  return '';
}

function inboxPanel(api: DesktopUiApi): (state: DesktopState) => HTMLElement {
  return state => {
    const body = h('div', { class: 'desk-split' });
    let filter: 'waiting' | 'all' = 'waiting';
    const tools = [botSelect(state, () => { void load(); })];
    const root = page('Network', 'Inbox', 'Messages admitted to this bot’s node. Waiting rows have not been injected into a conversation.', tools, body);
    const load = async (): Promise<void> => {
      body.replaceChildren(skeleton(5));
      try {
        const demo = await knowDemo(api.command);
        if (demo && root.querySelector('.mode-note') === null) {
          root.insertBefore(h('p', { class: 'mode-note', text: 'Demo node. These rows are stored with the desk, not delivered by a live peer.' }), body);
        }
        const value = await loadRead(api.command, api.state(), 'onlyne://inbox');
        const rows = asArray(value);
        paint(rows);
      } catch (error) {
        body.replaceChildren(errorBlock(errorText(error), () => { void load(); }));
      }
    };
    const paint = (rows: Record<string, unknown>[]): void => {
      const visible = rows.filter(row => filter === 'all' || row.injected !== true);
      const list = h('div', { class: 'desk-list', role: 'list' });
      const detail = h('div', { class: 'desk-detail' });
      const show = (row: Record<string, unknown>): void => {
        const attention = isRecord(row.attention) ? row.attention : row;
        const thread = isRecord(attention.thread_ref) ? attention.thread_ref : null;
        detail.replaceChildren(
          h('div', { class: 'detail-kicker' }, chip(row.injected === true ? 'Injected' : 'Waiting', row.injected === true ? '' : 'warn')),
          h('h2', { text: messageSender(row) || 'Unknown sender' }),
          kv([
            ['Delivery', pick(isRecord(attention.entry) && isRecord(attention.entry.message) ? attention.entry.message : null, ['delivery_kind']) || 'note'],
            ['Sequence', pick(attention, ['seq'])],
            ['Thread', thread === null ? '' : `${pick(thread, ['owner'])} / ${pick(thread, ['id'])}`],
            ['When', pick(isRecord(attention.entry) ? attention.entry : null, ['at'])],
          ]),
          h('blockquote', { class: 'desk-quote', text: messageBody(row) || 'This row has no message body.' }));
      };
      if (visible.length === 0) {
        body.replaceChildren(emptyBlock(
          filter === 'waiting' ? 'Nothing is waiting' : 'The inbox is empty',
          filter === 'waiting' ? 'Every admitted message for this bot has already been injected, or none has arrived.' : 'This node has no inbox rows yet.',
          filter === 'waiting' && rows.length > 0 ? h('button', { class: 'btn', type: 'button', onclick: () => { filter = 'all'; paint(rows); } }, 'Show injected') : undefined,
        ));
        return;
      }
      visible.forEach((row, index) => {
        const button = h('button', {
          class: 'desk-row', type: 'button', role: 'listitem',
          onclick: () => {
            for (const item of list.querySelectorAll('.desk-row')) item.classList.remove('on');
            button.classList.add('on');
            show(row);
          },
        },
        h('span', { class: 'row-title', text: shortNode(messageSender(row)) || 'Message' }),
        h('span', { class: 'row-sub', text: messageBody(row) || 'Empty body' }));
        list.append(button);
        if (index === 0) {
          button.classList.add('on');
          show(row);
        }
      });
      const filters = h('div', { class: 'segmented', role: 'tablist', 'aria-label': 'Inbox filter' },
        ...(['waiting', 'all'] as const).map(value => h('button', {
          class: filter === value ? 'on' : '', type: 'button', role: 'tab', 'aria-selected': filter === value ? 'true' : 'false',
          onclick: () => { filter = value; paint(rows); },
        }, value === 'waiting' ? 'Waiting' : 'All')));
      body.replaceChildren(h('div', { class: 'desk-list-wrap' }, filters, list), detail);
    };
    void load();
    return root;
  };
}

function neighborsPanel(api: DesktopUiApi, hooks: DeskHooks): (state: DesktopState) => HTMLElement {
  return state => {
    const body = h('div', { class: 'desk-stack' });
    const url = h('input', { type: 'url', placeholder: 'Peer URL', 'aria-label': 'Peer URL', autocomplete: 'off', spellcheck: 'false' });
    const errorLine = h('p', { class: 'field-error', hidden: true });
    const connect = h('button', { class: 'btn primary', type: 'button' }, 'Observe');
    const query = h('input', { type: 'search', placeholder: 'Filter peers', 'aria-label': 'Filter peers' });
    let rows: Record<string, unknown>[] = [];
    const tools = [botSelect(state, () => { void load(); })];
    const root = page('Network', 'Neighbors', 'Peers this node has seen. Observing records an address. It does not approve a fingerprint.', tools, body);
    const runConnect = (): void => {
      const parsed = parsePeerUrl(url.value);
      if (!parsed.ok) {
        errorLine.hidden = false;
        errorLine.textContent = parsed.error;
        url.setAttribute('aria-invalid', 'true');
        url.focus();
        return;
      }
      errorLine.hidden = true;
      url.removeAttribute('aria-invalid');
      connect.disabled = true;
      void api.command('onlyne.action', { action: 'connect', args: { url: parsed.url }, ...scopePayload(api.state()) })
        .then(() => {
          url.value = '';
          hooks.notify('Peer observed. Pairing still happens on Trust.', 'ok');
          return load();
        })
        .catch(error => {
          errorLine.hidden = false;
          errorLine.textContent = errorText(error);
        })
        .finally(() => { connect.disabled = false; });
    };
    connect.addEventListener('click', runConnect);
    url.addEventListener('input', () => { errorLine.hidden = true; url.removeAttribute('aria-invalid'); });
    query.addEventListener('input', () => paint(rows));
    const load = async (): Promise<void> => {
      body.replaceChildren(skeleton());
      try {
        const status = await loadStatus(api.command, api.state());
        const demo = await knowDemo(api.command);
        if (demo && root.querySelector('.mode-note') === null) root.insertBefore(h('p', { class: 'mode-note', text: 'Demo node. Endpoints are records in the local trust table.' }), body);
        rows = asArray(status.neighbors);
        paint(rows);
      } catch (error) {
        body.replaceChildren(errorBlock(errorText(error), () => { void load(); }));
      }
    };
    const paint = (all: Record<string, unknown>[]): void => {
      const needle = query.value.trim().toLowerCase();
      const visible = all.filter(row => {
        if (needle.length === 0) return true;
        const blob = [pick(row, ['nodeId', 'node_id']), pick(row, ['baseUrl', 'base_url']), peerPhaseLabel(pick(row, ['phase', 'state']))].join(' ').toLowerCase();
        return blob.includes(needle);
      });
      const grid = h('div', { class: 'card-grid' });
      for (const row of visible) grid.append(neighborCard(row));
      const empty = visible.length === 0
        ? emptyBlock(
          all.length === 0 ? 'No neighbors yet' : 'No peers match',
          all.length === 0 ? 'Observe a full http(s) URL. Approval is a separate step on Trust, after a fingerprint is on screen.' : 'Try a node name, host, or status such as Trusted.',
          all.length === 0 ? h('button', { class: 'btn primary', type: 'button', onclick: () => url.focus() }, 'Observe a peer') : undefined,
        )
        : false;
      body.replaceChildren(connectBar(), query, empty === false ? grid : empty);
    };
    const neighborCard = (row: Record<string, unknown>): HTMLElement => {
      const nodeId = pick(row, ['nodeId', 'node_id']);
      const endpoint = pick(row, ['baseUrl', 'base_url']);
      const phase = pick(row, ['phase', 'state']) || 'no_exchange';
      const label = peerPhaseLabel(phase);
      const trust = h('button', { class: 'btn sm', type: 'button' }, phase === 'exchanged' ? 'Review' : 'Start trust');
      trust.addEventListener('click', () => {
        if (phase === 'exchanged') { hooks.open('trust'); return; }
        void api.command('onlyne.action', { action: 'trust.request', args: { nodeId }, ...scopePayload(api.state()) })
          .then(() => { hooks.notify(`A fingerprint for ${shortNode(nodeId)} is waiting on Trust.`, 'ok'); hooks.open('trust'); })
          .catch(error => hooks.notify(errorText(error), 'error'));
      });
      const revoke = h('button', { class: 'btn sm', type: 'button' }, 'Revoke');
      revoke.addEventListener('click', () => {
        void api.command('onlyne.action', { action: 'trust.revoke', args: { nodeId }, ...scopePayload(api.state()) })
          .then(() => { hooks.notify(`Revoked ${shortNode(nodeId)}.`, 'ok'); return load(); })
          .catch(error => hooks.notify(errorText(error), 'error'));
      });
      const forget = h('button', { class: 'btn sm', type: 'button' }, 'Forget');
      forget.addEventListener('click', () => {
        void api.command('onlyne.action', { action: 'peer.forget', args: { nodeId }, ...scopePayload(api.state()) })
          .then(() => { hooks.notify(`Forgot ${shortNode(nodeId)}.`, 'ok'); return load(); })
          .catch(error => hooks.notify(errorText(error), 'error'));
      });
      const copy = h('button', { class: 'btn sm', type: 'button' }, 'Copy endpoint');
      copy.addEventListener('click', () => copyValue(hooks, endpoint, 'Endpoint copied.'));
      return h('article', { class: 'peer-card' },
        h('div', { class: 'peer-top' }, h('h2', { text: shortNode(nodeId) }), chip(label, toneForPhase(phase))),
        identifier(nodeId, hooks),
        kv([
          ['Endpoint', identifier(endpoint, hooks)],
          ['Fingerprint', formatFingerprint(pick(row, ['fingerprint']))],
          ['Last seen', whenView(pick(row, ['lastSeen', 'last_seen']))],
          ['Verified', row.verified === true ? 'Yes' : 'No'],
        ]),
        h('div', { class: 'row-actions' }, trust, phase === 'exchanged' ? revoke : forget, endpoint.length > 0 ? copy : false));
    };
    const connectBar = (): HTMLElement => h('form', { class: 'inline-form', onsubmit: (event: Event) => { event.preventDefault(); runConnect(); } },
      h('label', { class: 'field' },
        h('span', { text: 'Observe a peer' }),
        url,
        h('span', { class: 'field-hint', text: 'Example: http://127.0.0.1:3992/onlyne/v3. A partial string is rejected and no card is created.' }),
        errorLine),
      connect);
    void load();
    return root;
  };
}

function trustPanel(api: DesktopUiApi, hooks: DeskHooks): (state: DesktopState) => HTMLElement {
  return state => {
    const body = h('div', { class: 'desk-stack' });
    const tools = [botSelect(state, () => { void load(); })];
    const root = page('Network', 'Trust', 'Approval sends the fingerprint you type. It has to match the one already on screen.', tools, body);
    const load = async (): Promise<void> => {
      body.replaceChildren(skeleton());
      try {
        const status = await loadStatus(api.command, api.state());
        const node = isRecord(status.node) ? status.node : null;
        const trust = isRecord(status.trust) ? status.trust : null;
        const local = pick(trust, ['localFingerprint']) || pick(node, ['fingerprint']);
        const pending = trust === null ? [] : asArray(trust.pending);
        const peers = trust === null ? [] : asArray(trust.peers);
        body.replaceChildren(localCard(local, pick(node, ['id'])), pendingList(pending), peerList(peers));
      } catch (error) {
        body.replaceChildren(errorBlock(errorText(error), () => { void load(); }));
      }
    };
    const localCard = (fingerprint: string, nodeId: string): HTMLElement => {
      const copy = h('button', { class: 'btn', type: 'button' }, 'Copy');
      copy.addEventListener('click', () => {
        void navigator.clipboard?.writeText(fingerprint).then(
          () => hooks.notify('Local fingerprint copied.', 'ok'),
          () => hooks.notify('Could not copy. Select the fingerprint and copy it manually.', 'warn'),
        );
      });
      return h('section', { class: 'trust-local' },
        h('div', {}, h('p', { class: 'eyebrow', text: 'This node' }), h('h2', { text: 'Local fingerprint' })),
        h('p', { class: 'fingerprint', text: fingerprint.length > 0 ? formatFingerprint(fingerprint) : 'Not reported' }),
        h('div', { class: 'trust-local-meta' }, h('span', { class: 'mono break', text: nodeId || 'Node id unavailable' }), fingerprint.length > 0 ? copy : false));
    };
    const pendingList = (rows: Record<string, unknown>[]): HTMLElement => {
      if (rows.length === 0) return h('section', { class: 'desk-section' }, emptyBlock('No pairing is waiting', 'Nothing is approved from an empty list. Start from a neighbor when you can read their fingerprint on their own screen.', h('button', { class: 'btn primary', type: 'button', onclick: () => hooks.open('neighbors') }, 'Open neighbors')));
      return h('section', { class: 'desk-section' },
        h('h2', { class: 'section-title', text: 'Waiting for you' }),
        ...rows.map(row => pendingCard(row)));
    };
    const pendingCard = (row: Record<string, unknown>): HTMLElement => {
      const nodeId = pick(row, ['nodeId', 'node_id']);
      const fingerprint = pick(row, ['fingerprint']);
      const input = h('input', { type: 'text', spellcheck: 'false', autocomplete: 'off', 'aria-label': `Type the fingerprint for ${nodeId}`, placeholder: 'Type or paste the fingerprint' });
      const groups = h('div', { class: 'fp-groups', 'aria-hidden': 'true' });
      const approve = h('button', { class: 'btn primary', type: 'button', disabled: true }, 'Approve fingerprint');
      const decline = h('button', { class: 'btn', type: 'button' }, 'Decline');
      const hint = h('p', { class: 'field-hint' });
      const sync = (): void => {
        const marks = fingerprintMarks(input.value, fingerprint);
        groups.replaceChildren(...marks.map((mark, index) => h('span', { class: `fp-group ${mark}`, text: fingerprintGroups(fingerprint)[index] ?? '' })));
        hint.textContent = fingerprintHint(marks);
        approve.disabled = !marks.every(mark => mark === 'match');
      };
      input.addEventListener('input', sync);
      sync();
      approve.addEventListener('click', () => {
        if (!fingerprintMarks(input.value, fingerprint).every(mark => mark === 'match')) return;
        approve.disabled = true;
        void api.command('onlyne.action', { action: 'trust.approve', args: { nodeId, fingerprint: input.value.trim() }, ...scopePayload(api.state()) })
          .then(() => { hooks.notify(`Trusted ${shortNode(nodeId)}.`, 'ok'); return load(); })
          .catch(error => { hooks.notify(errorText(error), 'error'); approve.disabled = false; });
      });
      decline.addEventListener('click', () => {
        decline.disabled = true;
        void api.command('onlyne.action', { action: 'trust.revoke', args: { nodeId }, ...scopePayload(api.state()) })
          .then(() => { hooks.notify(`Declined ${shortNode(nodeId)}.`, 'ok'); return load(); })
          .catch(error => { hooks.notify(errorText(error), 'error'); decline.disabled = false; });
      });
      return h('article', { class: 'trust-card' },
        h('div', { class: 'peer-top' }, h('h3', { text: shortNode(nodeId) }), chip('pending', 'warn')),
        identifier(nodeId, hooks),
        h('p', { class: 'field-label', text: 'Displayed fingerprint' }),
        h('p', { class: 'fingerprint', text: formatFingerprint(fingerprint) }),
        kv([['Endpoint', identifier(pick(row, ['baseUrl', 'base_url']), hooks)], ['Shown at', whenView(pick(row, ['at']))]]),
        h('label', { class: 'field' }, h('span', { text: 'Read it back' }), input, groups),
        hint,
        h('div', { class: 'row-actions' }, approve, decline));
    };
    const peerList = (rows: Record<string, unknown>[]): HTMLElement => {
      const trusted = rows.filter(row => pick(row, ['phase', 'state']) === 'exchanged' || row.localApproved === true);
      if (trusted.length === 0) return h('section', {});
      return h('section', { class: 'desk-section' },
        h('h2', { class: 'section-title', text: 'Exchanged' }),
        h('div', { class: 'card-grid' }, ...trusted.map(row => {
          const nodeId = pick(row, ['nodeId', 'node_id']);
          const revoke = h('button', { class: 'btn', type: 'button' }, 'Revoke');
          revoke.addEventListener('click', () => {
            revoke.disabled = true;
            void api.command('onlyne.action', { action: 'trust.revoke', args: { nodeId }, ...scopePayload(api.state()) })
              .then(() => { hooks.notify(`Revoked ${shortNode(nodeId)}.`, 'ok'); return load(); })
              .catch(error => { hooks.notify(errorText(error), 'error'); revoke.disabled = false; });
          });
          return h('article', { class: 'peer-card' },
            h('div', { class: 'peer-top' }, h('h3', { text: shortNode(nodeId) }), chip(pick(row, ['phase']) || 'exchanged', 'ok')),
            h('p', { class: 'mono break', text: nodeId }),
            kv([['Fingerprint', formatFingerprint(pick(row, ['fingerprint']))]]),
            revoke);
        })));
    };
    void load();
    return root;
  };
}

interface ThreadRow { owner: string; id: string; title: string; state: string; joined: boolean }

function collectThreads(status: Record<string, unknown>): ThreadRow[] {
  const rows: ThreadRow[] = [];
  const push = (value: unknown, joined: boolean): void => {
    for (const item of asArray(value)) {
      const ref = isRecord(item.thread_ref) ? item.thread_ref : item;
      const owner = pick(isRecord(ref) ? ref : null, ['owner']);
      const id = pick(isRecord(ref) ? ref : null, ['id']);
      if (owner.length === 0 || id.length === 0) continue;
      rows.push({
        owner, id,
        title: pick(item, ['title']) || id,
        state: pick(item, ['state']) || (joined ? 'joined' : 'open'),
        joined,
      });
    }
  };
  push(status.threads, false);
  push(status.joined, true);
  return rows;
}

function threadsPanel(api: DesktopUiApi): (state: DesktopState) => HTMLElement {
  return state => {
    const split = h('div', { class: 'desk-split' });
    const query = h('input', { type: 'search', placeholder: 'Filter threads', 'aria-label': 'Filter threads' });
    const body = h('div', { class: 'desk-stack' }, query, split);
    const tools = [botSelect(state, () => { void load(); })];
    const root = page('Network', 'Threads', 'Threads this node owns, and threads it was invited into. A joined thread stays read-only here.', tools, body);
    query.addEventListener('input', () => { void load(); });
    const load = async (): Promise<void> => {
      split.replaceChildren(skeleton());
      try {
        const status = await loadStatus(api.command, api.state());
        const needle = query.value.trim().toLowerCase();
        const rows = collectThreads(status).filter(row => needle.length === 0 || `${row.title} ${row.id} ${row.owner}`.toLowerCase().includes(needle));
        if (rows.length === 0) {
          split.replaceChildren(emptyBlock(needle.length === 0 ? 'No threads' : 'No threads match', needle.length === 0 ? 'Owned and joined threads appear here after the node admits a message or receives an invitation.' : 'Try a title, owner, or thread id.'));
          return;
        }
        const list = h('div', { class: 'desk-list', role: 'list' });
        const detail = h('div', { class: 'desk-detail' }, h('p', { class: 'lede', text: 'Select a thread.' }));
        const show = (row: ThreadRow): void => {
          detail.replaceChildren(skeleton(3));
          void api.command('onlyne.action', { action: 'thread.read', args: { owner: row.owner, id: row.id }, ...scopePayload(api.state()) })
            .then(value => { detail.replaceChildren(threadDetail(row, value)); })
            .catch(error => { detail.replaceChildren(errorBlock(errorText(error), () => show(row))); });
        };
        rows.forEach((row, index) => {
          const button = h('button', { class: 'desk-row', type: 'button', onclick: () => {
            for (const item of list.querySelectorAll('.desk-row')) item.classList.remove('on');
            button.classList.add('on');
            show(row);
          } },
          h('span', { class: 'row-title', text: row.title }),
          h('span', { class: 'row-sub', text: `${row.joined ? 'Joined' : 'Owned'} · ${row.state}` }));
          list.append(button);
          if (index === 0) { button.classList.add('on'); show(row); }
        });
        split.replaceChildren(list, detail);
      } catch (error) {
        split.replaceChildren(errorBlock(errorText(error), () => { void load(); }));
      }
    };
    void load();
    return root;
  };
}

function threadDetail(row: ThreadRow, value: unknown): HTMLElement {
  const record = isRecord(value) ? value : {};
  const thread = isRecord(record.thread) ? record.thread : {};
  const members = asArray(record.members);
  const entriesRecord = isRecord(record.entries) ? record.entries : {};
  const entries = asArray(entriesRecord.items ?? record.entries);
  return h('div', { class: 'desk-stack' },
    h('div', { class: 'detail-kicker' }, chip(row.joined ? 'Joined' : 'Owned', row.joined ? '' : 'ok'), chip(pick(thread, ['state']) || row.state)),
    h('h2', { text: pick(thread, ['title']) || row.title }),
    kv([
      ['Owner', row.owner],
      ['Id', row.id],
      ['Next sequence', pick(thread, ['next_seq'])],
    ]),
    h('h3', { class: 'section-title', text: 'Members' }),
    members.length === 0 ? h('p', { class: 'lede', text: 'No member table was returned.' }) : h('ul', { class: 'plain-list' }, ...members.map(member => h('li', {},
      h('span', { class: 'mono', text: pick(member, ['peer']) }),
      ' ',
      chip(pick(member, ['state']) || 'unknown')))),
    h('h3', { class: 'section-title', text: 'Entries' }),
    entries.length === 0 ? h('p', { class: 'lede', text: 'No entries in this page.' }) : h('ol', { class: 'entry-list' }, ...entries.map(item => {
      const message = isRecord(item.message) ? item.message : null;
      return h('li', {},
        h('div', { class: 'entry-meta' }, h('span', { text: pick(item, ['seq']) }), h('span', { class: 'mono', text: message === null ? '' : pick(message, ['sender']) })),
        h('p', { text: message === null ? '' : pick(message, ['msg']) }));
    })));
}

function resourceOwners(value: unknown): Array<{ owner: string; items: Record<string, unknown>[] }> {
  const record = isRecord(value) ? value : {};
  const inventory = isRecord(record.inventory) ? record.inventory : record;
  const owners = asArray(inventory.owners ?? record.owners);
  const groups: Array<{ owner: string; items: Record<string, unknown>[] }> = [];
  for (const owner of owners) {
    const page = isRecord(owner.page) ? owner.page : null;
    const items = asArray(page?.items ?? owner.items);
    const name = pick(owner, ['owner']);
    if (name.length === 0 && items.length === 0) continue;
    groups.push({ owner: name || 'unknown', items });
  }
  return groups;
}

function resourcesPanel(api: DesktopUiApi): (state: DesktopState) => HTMLElement {
  return state => {
    const split = h('div', { class: 'desk-split' });
    const query = h('input', { type: 'search', placeholder: 'Filter resources', 'aria-label': 'Filter resources' });
    const body = h('div', { class: 'desk-stack' }, query, split);
    const tools = [botSelect(state, () => { void load(); })];
    const root = page('Network', 'Resources', 'Resources this desk can see, grouped by owner. Operations are the calls a controller is allowed to make.', tools, body);
    query.addEventListener('input', () => { void load(); });
    const load = async (): Promise<void> => {
      split.replaceChildren(skeleton());
      try {
        const value = await api.command('onlyne.action', { action: 'resource.list', args: {}, ...scopePayload(api.state()) });
        const groups = resourceOwners(value);
        const needle = query.value.trim().toLowerCase();
        const items = groups.flatMap(group => group.items.map(item => ({ owner: group.owner, item }))).filter(entry => {
          if (needle.length === 0) return true;
          const ref = isRecord(entry.item.resource_ref) ? entry.item.resource_ref : null;
          return `${pick(ref, ['id'])} ${entry.owner} ${pick(entry.item, ['kind'])}`.toLowerCase().includes(needle);
        });
        if (items.length === 0) {
          split.replaceChildren(emptyBlock(needle.length === 0 ? 'No resources' : 'No resources match', needle.length === 0 ? 'MCP servers, mounts and other capabilities show up here once a node publishes them.' : 'Try an owner, id, or kind.'));
          return;
        }
        const list = h('div', { class: 'desk-list', role: 'list' });
        const detail = h('div', { class: 'desk-detail' });
        const show = (owner: string, item: Record<string, unknown>): void => {
          const ref = isRecord(item.resource_ref) ? item.resource_ref : null;
          const operations = Array.isArray(item.operations) ? item.operations.filter((entry): entry is string => typeof entry === 'string') : [];
          const binding = isRecord(item.binding) ? item.binding : null;
          detail.replaceChildren(
            h('div', { class: 'detail-kicker' }, chip(pick(item, ['kind']) || 'resource')),
            h('h2', { text: pick(ref, ['id']) || 'Resource' }),
            kv([
              ['Owner', pick(ref, ['owner']) || owner],
              ['Handler', pick(binding, ['handler'])],
              ['Revision', pick(item, ['revision'])],
            ]),
            h('h3', { class: 'section-title', text: 'Operations' }),
            operations.length === 0 ? h('p', { class: 'lede', text: 'This resource publishes no operations.' }) : h('ul', { class: 'plain-list' }, ...operations.map(name => h('li', { class: 'mono', text: name }))),
            ...(isRecord(item.data) ? [h('pre', { class: 'desk-pre', text: JSON.stringify(item.data, null, 2) })] : []));
        };
        items.forEach((entry, index) => {
          const ref = isRecord(entry.item.resource_ref) ? entry.item.resource_ref : null;
          const button = h('button', { class: 'desk-row', type: 'button', onclick: () => {
            for (const item of list.querySelectorAll('.desk-row')) item.classList.remove('on');
            button.classList.add('on');
            show(entry.owner, entry.item);
          } },
          h('span', { class: 'row-title', text: pick(ref, ['id']) || 'Resource' }),
          h('span', { class: 'row-sub', text: `${shortNode(entry.owner)} · ${pick(entry.item, ['kind'])}` }));
          list.append(button);
          if (index === 0) { button.classList.add('on'); show(entry.owner, entry.item); }
        });
        split.replaceChildren(list, detail);
      } catch (error) {
        split.replaceChildren(errorBlock(errorText(error), () => { void load(); }));
      }
    };
    void load();
    return root;
  };
}

function metric(label: string, value: string, hint = ''): HTMLElement {
  return h('div', { class: 'metric' },
    h('span', { class: 'metric-label', text: label }),
    h('strong', { class: 'metric-value', text: value }),
    hint.length > 0 ? h('span', { class: 'metric-hint', text: hint }) : false);
}

function healthPanel(api: DesktopUiApi, hooks: DeskHooks): (state: DesktopState) => HTMLElement {
  return state => {
    const body = h('div', { class: 'desk-stack' });
    const tools = [botSelect(state, () => { void load(); })];
    const root = page('Desk', 'Health', 'The application, the selected bot’s runtime, and that bot’s Onlyne node.', tools, body);
    const load = async (): Promise<void> => {
      body.replaceChildren(skeleton());
      try {
        const [desk, runtime, status] = await Promise.all([
          api.command<Record<string, unknown>>('desktop.health'),
          api.command<unknown>('runtime.health').catch(error => error),
          loadStatus(api.command, api.state()).catch(error => error),
        ]);
        const node = status instanceof Error || !isRecord(status) ? null : isRecord(status.node) ? status.node : null;
        const inbox = status instanceof Error || !isRecord(status) ? null : isRecord(status.inbox) ? status.inbox : null;
        const runtimes = isRecord(runtime) && Array.isArray(runtime.runtimes) ? runtime.runtimes.filter(isRecord) : [];
        const runtimeError = runtime instanceof Error ? runtime.message : '';
        const nodeError = status instanceof Error ? status.message : '';
        body.replaceChildren(
          h('div', { class: 'metric-grid' },
            metric('Bots', textOf(desk.bots) || '0'),
            metric('Conversations', textOf(desk.conversations) || '0'),
            metric('Messages', textOf(desk.messages) || '0'),
            metric('Node', node === null ? 'unavailable' : pick(node, ['state']) || 'ready', node === null ? '' : shortNode(pick(node, ['id']))),
            metric('Inbox waiting', inbox === null ? '—' : textOf(inbox.pending) || '0'),
            metric('Threads', status instanceof Error || !isRecord(status) ? '—' : String(asArray(status.threads).length + asArray(status.joined).length))),
          node === null ? h('p', { class: 'lede', text: nodeError || 'Node status is unavailable.' }) : kv([
            ['Node', pick(node, ['id'])],
            ['Endpoint', pick(node, ['endpoint'])],
            ['Fingerprint', formatFingerprint(pick(node, ['fingerprint']))],
            ['Authentication', Array.isArray(node.authentication) ? node.authentication.join(', ') : ''],
          ]),
          h('div', { class: 'row-actions' },
            h('button', { class: 'btn', type: 'button', onclick: () => hooks.open('activity') }, 'Activity'),
            h('button', { class: 'btn', type: 'button', onclick: () => hooks.open('neighbors') }, 'Neighbors')),
          h('h2', { class: 'section-title', text: 'Runtimes' }),
          runtimeError.length > 0 ? h('p', { class: 'lede', text: runtimeError }) : runtimes.length === 0
            ? emptyBlock('No runtime is running', 'A runtime starts when a conversation sends a message. Until OMP is on PATH, the demo answers locally and labels the reply.', h('button', { class: 'btn primary', type: 'button', onclick: () => hooks.open('chat') }, 'Back to the conversation'))
            : h('div', { class: 'table-wrap' }, h('table', { class: 'desk-table' },
              h('thead', {}, h('tr', {}, ...['Bot', 'Conversation', 'Settled', 'Pending', 'Last error'].map(label => h('th', { text: label })))),
              h('tbody', {}, ...runtimes.map(row => h('tr', {},
                h('td', { text: pick(row, ['botId']) }),
                h('td', { text: pick(row, ['conversationId']) }),
                h('td', { text: row.settled === false ? 'no' : 'yes' }),
                h('td', { text: textOf(row.pendingCalls) || '0' }),
                h('td', { text: pick(row, ['lastError']) || '—' })))))),
        );
      } catch (error) {
        body.replaceChildren(errorBlock(errorText(error), () => { void load(); }));
      }
    };
    void load();
    return root;
  };
}

function integrationsPanel(api: DesktopUiApi, hooks: DeskHooks): (state: DesktopState) => HTMLElement {
  return state => {
    const body = h('div', { class: 'desk-stack' });
    const tools = [botSelect(state, () => { void load(); })];
    const root = page('Desk', 'Integrations', 'Grok Bot threads and MCP servers visible from this node. Secrets stay in the process environment.', tools, body);
    const load = async (): Promise<void> => {
      body.replaceChildren(skeleton());
      try {
        const [status, listed] = await Promise.all([
          loadStatus(api.command, api.state()),
          api.command('onlyne.action', { action: 'resource.list', args: {}, ...scopePayload(api.state()) }),
        ]);
        const threads = collectThreads(status).filter(row => row.id.startsWith('grokbot:'));
        const resources = resourceOwners(listed).flatMap(group => group.items.map(item => ({ item, owner: group.owner })));
        const mcp = resources.filter(entry => pick(entry.item, ['kind']).startsWith('mcp'));
        const grokResources = resources.filter(entry => pick(entry.item, ['kind']).startsWith('grok'));
        const capabilities = api.state().capabilities.filter((item): item is Extract<Capability, { kind: 'mcp' }> => item.kind === 'mcp');
        body.replaceChildren(
          section('Grok Bot', threads.length === 0 && grokResources.length === 0
            ? emptyBlock('No Grok Bot thread', 'A grok-bot gateway keeps one node per account. Assistant DMs use grokbot:dm and groups use grokbot:group. They show up here after this node has joined them.')
            : h('div', { class: 'desk-stack' },
              ...grokResources.map(({ item }) => {
                const data = isRecord(item.data) ? item.data : null;
                return h('article', { class: 'peer-card' },
                  h('div', { class: 'peer-top' }, h('h3', { text: 'Webhook' }), chip(data?.configured === true ? 'configured' : 'unconfigured', data?.configured === true ? 'ok' : 'warn')),
                  h('p', { class: 'lede', text: 'The signing key is read from the gateway environment. This desk does not store or display it.' }),
                  kv([['Header', pick(data, ['header'])], ['Scheme', pick(data, ['scheme'])]]));
              }),
              threads.length === 0 ? false : h('ul', { class: 'plain-list' }, ...threads.map(row => h('li', {},
                h('span', { text: row.title }), ' ', chip(row.id.startsWith('grokbot:group:') ? 'group' : 'dm'),
                h('div', { class: 'mono break', text: row.id })))))),
          section('MCP capabilities on this team', capabilities.length === 0
            ? emptyBlock('No MCP capability is attached', 'Add one from Team. A capability selects a server for a bot; the gateway resource is what other nodes call.')
            : h('div', { class: 'card-grid' }, ...capabilities.map(item => h('article', { class: 'peer-card' },
              h('div', { class: 'peer-top' }, h('h3', { text: item.name }), chip('mcp')),
              configView(item.config as Record<string, unknown>))))),
          section('MCP resources', mcp.length === 0
            ? emptyBlock('No MCP resource is published', 'An MCP gateway registers each server as a resource whose operations are that server’s tools.')
            : h('div', { class: 'card-grid' }, ...mcp.map(({ item }) => {
              const ref = isRecord(item.resource_ref) ? item.resource_ref : null;
              const operations = Array.isArray(item.operations) ? item.operations.filter((entry): entry is string => typeof entry === 'string') : [];
              return h('article', { class: 'peer-card' },
                h('div', { class: 'peer-top' }, h('h3', { text: pick(ref, ['id']) || 'server' }), chip(pick(item, ['kind']))),
                h('p', { class: 'mono break', text: pick(ref, ['owner']) }),
                operations.length === 0 ? h('p', { class: 'lede', text: 'No operations.' }) : h('p', { class: 'mono', text: operations.join(' · ') }));
            }))),
          h('section', { class: 'desk-section quiet' },
            h('h2', { class: 'section-title', text: 'MCP server process' }),
            h('p', { class: 'lede', text: 'onlyne-mcp-server is the reverse direction: one Onlyne node exposed as stdio tools (inbox, threads, neighbors, resources, send). It is a separate process from this window and uses its own key.' }),
            h('button', { class: 'btn', type: 'button', onclick: () => hooks.open('resources') }, 'Open resources')),
        );
      } catch (error) {
        body.replaceChildren(errorBlock(errorText(error), () => { void load(); }));
      }
    };
    void load();
    return root;
  };
}

function configView(config: Record<string, unknown>): HTMLElement {
  const safe = redact(config);
  const rows = h('div', { class: 'kv' });
  for (const [key, value] of Object.entries(safe)) {
    const shown = Array.isArray(value) ? value.map(item => String(item)).join(', ') : typeof value === 'string' ? value : JSON.stringify(value);
    rows.append(h('div', { class: 'config-row' }, h('span', { class: 'config-key', text: key }), h('span', { text: shown })));
  }
  const raw = h('pre', { class: 'desk-pre', hidden: true, text: JSON.stringify(safe, null, 2) });
  const toggle = h('button', { class: 'btn sm', type: 'button' }, 'Show JSON');
  toggle.addEventListener('click', () => {
    raw.hidden = !raw.hidden;
    toggle.textContent = raw.hidden ? 'Show JSON' : 'Hide JSON';
  });
  return h('div', { class: 'config-view' }, rows, toggle, raw);
}

function redact(config: Record<string, unknown>): Record<string, unknown> {
  const copy: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(config)) {
    copy[key] = /key|token|secret|password|authorization/i.test(key) ? '••••' : value;
  }
  return copy;
}

function section(title: string, content: HTMLElement): HTMLElement {
  return h('section', { class: 'desk-section' }, h('h2', { class: 'section-title', text: title }), content);
}

function teamPanel(api: DesktopUiApi, hooks: DeskHooks): (state: DesktopState) => HTMLElement {
  return () => {
    const root = h('div', { class: 'desk-page' });
    const paint = (): void => {
      const state = api.state();
      const admin = state.teamAdminId;
      root.replaceChildren(
        h('header', { class: 'desk-page-head' },
          h('div', { class: 'desk-page-copy' },
            h('p', { class: 'eyebrow', text: 'Desk' }),
            h('h1', { text: 'Team' }),
            h('p', { class: 'lede', text: 'One administrator manages the roster. Every bot keeps its own node, key and configuration.' })),
          h('div', { class: 'desk-page-tools' }, h('button', { class: 'btn primary', type: 'button', onclick: () => hooks.createBot() }, 'New bot'))),
        h('div', { class: 'card-grid' }, ...state.bots.map(bot => {
          const make = h('button', { class: 'btn', type: 'button', disabled: bot.id === admin }, bot.id === admin ? 'Administrator' : 'Make administrator');
          if (bot.id !== admin) make.addEventListener('click', () => {
            make.disabled = true;
            void api.command('team.admin', { botId: bot.id })
              .then(() => hooks.notify(`${bot.name} is the administrator.`, 'ok'))
              .catch(error => { hooks.notify(errorText(error), 'error'); make.disabled = false; });
          });
          return h('article', { class: 'peer-card' },
            h('div', { class: 'peer-top' }, h('h2', { text: bot.name }), bot.id === admin ? chip('admin', 'ok') : chip('member')),
            h('p', { class: 'lede', text: bot.description || 'No description.' }),
            kv([
              ['Node', identifier(bot.onlyne?.nodeId ?? '', hooks)],
              ['Thinking', bot.thinking],
              ['Model', bot.model ? `${bot.model.provider}/${bot.model.id}` : 'OMP default'],
              ['Capabilities', bot.capabilities.length === 0 ? 'none' : bot.capabilities.join(', ')],
            ]),
            h('div', { class: 'row-actions' }, make, h('button', { class: 'btn', type: 'button', onclick: () => hooks.open('character') }, 'Character')));
        })),
        section('Capabilities', state.capabilities.length === 0
          ? emptyBlock('No capabilities in the library', 'MCP servers, skills and extensions are chosen per bot. The library is shared by the team.')
          : h('div', { class: 'card-grid' }, ...state.capabilities.map(item => h('article', { class: 'peer-card' },
            h('div', { class: 'peer-top' }, h('h3', { text: item.name }), chip(item.kind)),
            item.kind === 'mcp' ? configView(item.config as Record<string, unknown>) : identifier(item.path, hooks))))),
        h('div', { class: 'row-actions' }, h('button', { class: 'btn', type: 'button', onclick: () => hooks.open('capabilities') }, 'Edit the library')));
    };
    paint();
    api.subscribe(() => { if (root.isConnected) paint(); });
    return root;
  };
}

export function registerDeskSurfaces(api: DesktopUiApi, hooks: DeskHooks): () => void {
  const registered = [
    api.registerPanel('inbox', 'Inbox', inboxPanel(api)),
    api.registerPanel('neighbors', 'Neighbors', neighborsPanel(api, hooks)),
    api.registerPanel('trust', 'Trust', trustPanel(api, hooks)),
    api.registerPanel('threads', 'Threads', threadsPanel(api)),
    api.registerPanel('resources', 'Resources', resourcesPanel(api)),
    api.registerPanel('health', 'Health', healthPanel(api, hooks)),
    api.registerPanel('integrations', 'Integrations', integrationsPanel(api, hooks)),
    api.registerPanel('team', 'Team', teamPanel(api, hooks)),
  ];
  return () => { for (const dispose of registered) dispose(); };
}
