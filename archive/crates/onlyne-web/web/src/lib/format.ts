// Small pure formatters. Nothing here reads a store.

/// The first `n` characters of an id. One display convention for every place
/// an id is shown, so a card and its ledger row are cut the same way.
export function short(id: string | null | undefined, n = 8): string {
  return id ? id.slice(0, n) : '';
}

/// A string cut to `max` characters, whitespace flattened, with an ellipsis
/// when it did not fit. A one-line row is the only user of this, and a row
/// that wraps is a row that pushed its neighbours out of alignment.
export function clip(text: string, max: number): string {
  const flat = text.replace(/\s+/g, ' ').trim();
  return flat.length > max ? `${flat.slice(0, max - 1)}…` : flat;
}

/// `1 session`, `2 sessions`.
export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}

/// An ISO timestamp as epoch milliseconds, or null when it is absent or unreadable.
export function parseTime(iso: string | null | undefined): number | null {
  if (!iso) return null;
  const at = Date.parse(iso);
  return Number.isNaN(at) ? null : at;
}

/// A span in coarse units: `now`, `12s`, `4m`, `3h`, `2d`. The five-second
/// floor matches the clock's tick, so a figure never promises finer than it moves.
export function span(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 5) return 'now';
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 48) return `${h}h`;
  return `${Math.floor(h / 24)}d`;
}

/// How long the cluster has been up, from the server's `uptime_s`.
export function uptime(seconds: number | null | undefined): string {
  if (seconds == null) return '';
  const s = Math.floor(seconds);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 48) return `${h}h ${m % 60}m`;
  return `${Math.floor(h / 24)}d ${h % 24}h`;
}

/// A wall clock time in the viewer's own zone, 24-hour.
export function HHMMSS(at: number): string {
  return new Date(at).toLocaleTimeString([], { hour12: false });
}
