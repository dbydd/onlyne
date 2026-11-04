/** Shared desk formatting. No DOM, so the host and the renderer can both use it. */

export function commandKey(platform = '', userAgent = ''): '⌘' | 'Ctrl' {
  return /Mac|iPhone|iPad/.test(platform) || /Mac OS/.test(userAgent) ? '⌘' : 'Ctrl';
}

export function chord(mod: '⌘' | 'Ctrl', keys: string): string {
  if (mod === '⌘') return `⌘${keys}`;
  return `Ctrl+${keys.replace(/\s+/g, '')}`;
}

export function parsePeerUrl(value: string): { ok: true; url: string } | { ok: false; error: string } {
  const text = value.trim();
  if (text.length === 0) return { ok: false, error: 'Enter the peer URL.' };
  if (!/^https?:\/\//i.test(text)) return { ok: false, error: 'Start with http:// or https://. A fragment such as “htt” is not a URL.' };
  let url: URL;
  try { url = new URL(text); }
  catch { return { ok: false, error: 'That is not a full URL. Use http://host:port/onlyne/v3.' }; }
  if (url.protocol !== 'http:' && url.protocol !== 'https:') return { ok: false, error: 'Only http and https peer URLs are accepted.' };
  if (url.hostname.length === 0 || url.hostname === 'peer') return { ok: false, error: 'The URL needs a real host name.' };
  if (url.pathname === '/' || url.pathname.length === 0) return { ok: false, error: 'Include the service path, usually /onlyne/v3.' };
  return { ok: true, url: url.toString() };
}

export function middleEllipsis(value: string, head = 14, tail = 10): string {
  if (value.length <= head + tail + 1) return value;
  return `${value.slice(0, head)}…${value.slice(-tail)}`;
}

export interface DeskHash {
  screen: string;
  selection?: string;
  dock?: string;
}

/** `#/trust/c-desk?dock=health` — screen, optional selection, optional dock. */
export function formatDeskHash(route: DeskHash): string {
  const screen = route.screen.length > 0 ? route.screen : 'chat';
  const selection = route.selection ? `/${encodeURIComponent(route.selection)}` : '';
  const dock = route.dock ? `?dock=${encodeURIComponent(route.dock)}` : '';
  return `#/${encodeURIComponent(screen)}${selection}${dock}`;
}

export function parseDeskHash(hash: string): DeskHash {
  const raw = hash.startsWith('#') ? hash.slice(1) : hash;
  const [path = '', query = ''] = raw.split('?');
  const parts = path.split('/').filter(part => part.length > 0).map(part => {
    try { return decodeURIComponent(part); }
    catch { return part; }
  });
  const dock = new URLSearchParams(query).get('dock') ?? undefined;
  return {
    screen: parts[0] && parts[0].length > 0 ? parts[0] : 'chat',
    ...(parts[1] ? { selection: parts[1] } : {}),
    ...(dock && dock.length > 0 ? { dock } : {}),
  };
}

export function friendlyRuntimeError(message: string): string | undefined {
  if (/executable not found|ENOENT|\bomp\b/i.test(message)) {
    return 'No model runtime is available. Install OMP and put the omp command on PATH. These controls stay off until then. The conversation keeps its transcript.';
  }
  return undefined;
}

export function filterByQuery<T>(rows: T[], needle: string, text: (row: T) => string, pinned?: (row: T) => boolean): T[] {
  const query = needle.trim().toLowerCase();
  if (query.length === 0) return rows;
  return rows.filter(row => (pinned !== undefined && pinned(row)) || text(row).toLowerCase().includes(query));
}

export function peerPhaseLabel(phase: string): string {
  switch (phase) {
    case 'exchanged': return 'Trusted';
    case 'invited': return 'Invited';
    case 'requested': return 'Awaiting reply';
    case 'revoked': return 'Revoked';
    case 'no_exchange': return 'Not paired';
    case 'pending': return 'Pending';
    default: return phase.length === 0 ? 'Unknown' : phase.replaceAll('_', ' ');
  }
}

export interface WhenText { label: string; title: string }

export function formatWhen(value: number | string, now = Date.now(), locale?: string): WhenText {
  const time = typeof value === 'number' ? value : Date.parse(value);
  if (!Number.isFinite(time)) return { label: 'Unknown time', title: String(value) };
  const date = new Date(time);
  const title = new Intl.DateTimeFormat(locale, { dateStyle: 'medium', timeStyle: 'short', hourCycle: 'h23' }).format(date);
  const diff = now - time;
  if (diff >= 0 && diff < 45_000) return { label: 'Just now', title };
  if (diff >= 0 && diff < 3_600_000) {
    const minutes = Math.max(1, Math.floor(diff / 60_000));
    return { label: `${minutes} min ago`, title };
  }
  if (diff >= 0 && diff < 86_400_000) {
    const hours = Math.floor(diff / 3_600_000);
    return { label: `${hours} hr ago`, title };
  }
  return { label: title, title };
}

export function fingerprintGroups(value: string): string[] {
  const compact = value.replace(/\s+/g, '').toLowerCase();
  const groups: string[] = [];
  for (let index = 0; index < compact.length; index += 4) groups.push(compact.slice(index, index + 4));
  return groups;
}

export type FingerprintMark = 'empty' | 'partial' | 'match' | 'miss';

/** Compare typed text with the displayed fingerprint, one group at a time. */
export function fingerprintMarks(typed: string, displayed: string): FingerprintMark[] {
  const got = typed.replace(/\s+/g, '').toLowerCase();
  const expect = displayed.replace(/\s+/g, '').toLowerCase();
  const groups = fingerprintGroups(expect);
  return groups.map((group, index) => {
    const slice = got.slice(index * 4, index * 4 + 4);
    if (slice.length === 0) return 'empty';
    if (group.startsWith(slice) && slice.length < group.length) return 'partial';
    return slice === group ? 'match' : 'miss';
  });
}

export function fingerprintHint(marks: FingerprintMark[]): string {
  if (marks.every(mark => mark === 'empty')) return 'Type or paste the fingerprint. It is checked one group at a time.';
  if (marks.every(mark => mark === 'match')) return 'Every group matches. You can approve.';
  const miss = marks.findIndex(mark => mark === 'miss');
  if (miss >= 0) return `Group ${miss + 1} does not match. Approval stays off until every group matches.`;
  const next = marks.findIndex(mark => mark !== 'match');
  return next >= 0 ? `Group ${next + 1} is still incomplete.` : 'Keep going.';
}

/** True for a focused field. Shortcuts must not see those keystrokes. */
export function isTextControl(tag: string, editable = false): boolean {
  const name = tag.toLowerCase();
  return editable || name === 'input' || name === 'textarea' || name === 'select';
}

const GO_SCREENS: Record<string, string> = {
  c: 'chat',
  i: 'inbox',
  p: 'neighbors',
  t: 'trust',
  d: 'threads',
  r: 'resources',
  h: 'health',
  x: 'integrations',
  m: 'team',
  s: 'settings',
};

export interface ChordClock { armed: number }

export interface DeskKey {
  key: string;
  /** `text` while an input, textarea, select, or contenteditable is focused. */
  target: 'text' | 'other';
  meta: boolean;
  ctrl: boolean;
  alt: boolean;
  now: number;
}

/**
 * G-chords navigate only when the key is not aimed at a text control.
 * A text control returns the typed character and clears any armed chord.
 */
export function navigationFromKey(clock: ChordClock, event: DeskKey, screens: Record<string, string> = GO_SCREENS): { clock: ChordClock; screen: string | null; typed: string | null } {
  if (event.target === 'text') {
    const printable = event.key.length === 1 && !event.meta && !event.ctrl && !event.alt;
    return { clock: { armed: 0 }, screen: null, typed: printable ? event.key : null };
  }
  if (event.meta || event.ctrl || event.alt) return { clock, screen: null, typed: null };
  if (event.key === 'g') return { clock: { armed: event.now }, screen: null, typed: null };
  if (clock.armed !== 0 && event.now - clock.armed < 900) {
    return { clock: { armed: 0 }, screen: screens[event.key.toLowerCase()] ?? null, typed: null };
  }
  return { clock: { armed: 0 }, screen: null, typed: null };
}

/** Type into a text control. G and C stay in the field; they do not change screens. */
export function typeInField(keys: string, now = 0): { text: string; screens: string[] } {
  let clock: ChordClock = { armed: 0 };
  let text = '';
  const screens: string[] = [];
  for (const key of keys) {
    const step = navigationFromKey(clock, { key, target: 'text', meta: false, ctrl: false, alt: false, now });
    clock = step.clock;
    now += 30;
    if (step.typed !== null) text += step.typed;
    if (step.screen !== null) screens.push(step.screen);
  }
  return { text, screens };
}

/** The side column is a drawer only below the desktop breakpoint. */
export function drawerWanted(viewportWidth: number): boolean {
  return viewportWidth <= 900;
}

export type RevokePhase = 'idle' | 'armed';

/** First press asks. Second press commits. */
export function revokePress(phase: RevokePhase): 'armed' | 'commit' {
  return phase === 'idle' ? 'armed' : 'commit';
}

export interface DeskTrack { name: string; min: number; fr: boolean }

/** Grid tracks for rail + side + stage + optional members + optional dock. Stage is the only flexible column and its minimum is 0. */
export function deskTracks(options: { dock: boolean; members: boolean }): DeskTrack[] {
  const rail: DeskTrack = { name: 'rail', min: 56, fr: false };
  const stage: DeskTrack = { name: 'stage', min: 0, fr: true };
  if (options.dock && options.members) {
    return [rail, { name: 'nav', min: 180, fr: false }, stage, { name: 'members', min: 200, fr: false }, { name: 'dock', min: 220, fr: false }];
  }
  if (options.dock) return [rail, { name: 'nav', min: 160, fr: false }, stage, { name: 'dock', min: 220, fr: false }];
  if (options.members) return [rail, { name: 'nav', min: 200, fr: false }, stage, { name: 'members', min: 220, fr: false }];
  return [rail, { name: 'nav', min: 200, fr: false }, stage];
}

/** True when the fixed columns fit and the stage can shrink instead of being covered. */
export function tracksFit(width: number, tracks: DeskTrack[]): boolean {
  const stage = tracks.find(track => track.name === 'stage');
  if (stage === undefined || !stage.fr || stage.min !== 0) return false;
  const fixed = tracks.reduce((sum, track) => sum + (track.fr ? 0 : track.min), 0);
  return fixed <= width && tracks.every(track => track.min >= 0);
}

export interface DeliveryRow { id: string; role: string; conversationId: string; createdAt: number }
export interface DeliveryNote { text: string; at: number }

/**
 * Keep a failed send’s Retry until that message is followed by an assistant reply,
 * or the user dismisses it. A later failure binds to a newer user message.
 */
export function assignDeliveryErrors(
  messages: readonly DeliveryRow[],
  conversationId: string,
  failures: ReadonlyMap<string, string>,
  dismissed: ReadonlySet<string>,
  queued: readonly DeliveryNote[],
): { failures: Map<string, string>; queued: DeliveryNote[] } {
  const list = messages.filter(item => item.conversationId === conversationId);
  const next = new Map<string, string>();
  for (const [id, text] of failures) {
    const index = list.findIndex(item => item.id === id);
    if (index < 0 || dismissed.has(id)) continue;
    if (list[index + 1]?.role === 'assistant') continue;
    next.set(id, text);
  }
  const rest: DeliveryNote[] = [];
  for (const note of queued) {
    const open = list.filter((item, index) => {
      if (item.role !== 'user' || dismissed.has(item.id) || next.has(item.id)) return false;
      if (list[index + 1]?.role === 'assistant') return false;
      return item.createdAt >= note.at - 1_500;
    });
    const target = open.at(-1);
    if (target !== undefined) {
      next.set(target.id, note.text);
      continue;
    }
    const newerUser = list.some(item => item.role === 'user' && item.createdAt >= note.at - 200);
    const lastFailed = [...list].reverse().find(item => item.role === 'user' && next.has(item.id));
    if (!newerUser && lastFailed !== undefined) {
      next.set(lastFailed.id, note.text);
      continue;
    }
    rest.push(note);
  }
  return { failures: next, queued: rest };
}
