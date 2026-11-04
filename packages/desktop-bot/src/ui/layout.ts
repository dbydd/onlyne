/**
 * Desk layout. The rail order, hidden slots and dock are data, so a renderer
 * plugin can rearrange the desk without editing the shell.
 */

export type WorkspaceGroup = 'primary' | 'network' | 'studio' | 'system';

export interface WorkspaceSpec {
  id: string;
  title: string;
  hint: string;
  /** Single key used after G. Empty when the place is palette-only. */
  shortcut: string;
  group: WorkspaceGroup;
  /** Panel id. Null keeps the conversation stage. */
  panel: string | null;
}

export const BUILTIN_WORKSPACES: readonly WorkspaceSpec[] = [
  { id: 'chat', title: 'Conversations', hint: 'Messages with the bots on this desk', shortcut: 'c', group: 'primary', panel: null },
  { id: 'inbox', title: 'Inbox', hint: 'Inbound messages waiting on a node', shortcut: 'i', group: 'network', panel: 'inbox' },
  { id: 'neighbors', title: 'Neighbors', hint: 'Peers this node has observed', shortcut: 'p', group: 'network', panel: 'neighbors' },
  { id: 'trust', title: 'Trust', hint: 'Pairing that requires the fingerprint to be read back', shortcut: 't', group: 'network', panel: 'trust' },
  { id: 'threads', title: 'Threads', hint: 'Onlyne threads this node owns or has joined', shortcut: 'd', group: 'network', panel: 'threads' },
  { id: 'resources', title: 'Resources', hint: 'Capabilities published as resources', shortcut: 'r', group: 'network', panel: 'resources' },
  { id: 'health', title: 'Health', hint: 'Desk, runtime and node status', shortcut: 'h', group: 'system', panel: 'health' },
  { id: 'integrations', title: 'Integrations', hint: 'Grok Bot threads and MCP servers', shortcut: 'x', group: 'system', panel: 'integrations' },
  { id: 'team', title: 'Team', hint: 'Bots, the administrator and capabilities', shortcut: 'm', group: 'system', panel: 'team' },
  { id: 'settings', title: 'Settings', hint: 'Appearance, layout and plugins', shortcut: 's', group: 'system', panel: 'settings' },
];

export interface DeskLayout {
  version: 1;
  rail: string[];
  hidden: string[];
  dock: string | null;
}

export const LAYOUT_STORAGE_KEY = 'onlyne.desk.v1';

export function defaultLayout(catalog: readonly string[]): DeskLayout {
  return { version: 1, rail: [...catalog], hidden: [], dock: null };
}

export function normalizeLayout(saved: unknown, catalog: readonly string[]): DeskLayout {
  const fallback = defaultLayout(catalog);
  if (saved === null || typeof saved !== 'object' || Array.isArray(saved)) return fallback;
  const record = saved as Record<string, unknown>;
  if (record.version !== 1) return fallback;
  const known = new Set(catalog);
  const rail: string[] = [];
  if (Array.isArray(record.rail)) {
    for (const id of record.rail) {
      if (typeof id === 'string' && known.has(id) && !rail.includes(id)) rail.push(id);
    }
  }
  for (const id of catalog) if (!rail.includes(id)) rail.push(id);
  const hidden: string[] = [];
  if (Array.isArray(record.hidden)) {
    for (const id of record.hidden) {
      if (typeof id === 'string' && known.has(id) && id !== 'chat' && !hidden.includes(id)) hidden.push(id);
    }
  }
  const dock = typeof record.dock === 'string' && known.has(record.dock) && record.dock !== 'chat' ? record.dock : null;
  return { version: 1, rail, hidden, dock };
}

export function moveInRail(layout: DeskLayout, id: string, toIndex: number): DeskLayout {
  if (!layout.rail.includes(id)) return layout;
  const rail = layout.rail.filter(item => item !== id);
  const index = Math.max(0, Math.min(toIndex, rail.length));
  rail.splice(index, 0, id);
  return { ...layout, rail };
}

/** Groups a fingerprint into fours so it can be read aloud. */
export function formatFingerprint(value: string): string {
  const compact = value.replace(/\s+/g, '').toLowerCase();
  const parts: string[] = [];
  for (let index = 0; index < compact.length; index += 4) parts.push(compact.slice(index, index + 4));
  return parts.join(' ');
}

export function fingerprintsMatch(typed: string, displayed: string): boolean {
  const normalize = (value: string) => value.replace(/\s+/g, '').toLowerCase();
  return normalize(typed).length > 0 && normalize(typed) === normalize(displayed);
}
