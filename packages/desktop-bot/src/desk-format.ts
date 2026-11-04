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
