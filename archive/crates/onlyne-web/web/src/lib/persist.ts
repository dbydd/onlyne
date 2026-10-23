// Tab-scoped memory. Everything the operator arranges here (where a board
// sits, how tall the dock is) is a property of this tab, never of the
// cluster, so it lives in `sessionStorage`: per tab, and gone with it. This
// surface's origin carries a port that changes on every launch, so a fresh
// `onlyne-web` is a fresh slate without anything having to clear it.
//
// Every access is guarded, because a browser with storage disabled must still
// work. Losing the memory costs the next reload and nothing else.

export function load<T>(key: string, fallback: T): T {
  try {
    const raw = sessionStorage.getItem(key);
    if (raw === null) return fallback;
    const parsed: unknown = JSON.parse(raw);
    return parsed === null || parsed === undefined ? fallback : (parsed as T);
  } catch {
    return fallback;
  }
}

export function save(key: string, value: unknown): void {
  try {
    sessionStorage.setItem(key, JSON.stringify(value));
  } catch {
    // A full or disabled store costs the next reload its memory and nothing else.
  }
}
