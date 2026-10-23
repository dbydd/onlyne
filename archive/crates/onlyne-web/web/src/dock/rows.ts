// The panels' filters, as pure reads over plain arrays. A tab's body is a
// derived value over the payload and one of these, never over a fold of its
// own, so the same helper can serve both the rows and the count in the tab row
// above them, and the two cannot disagree.
//
// Total and side-effect free throughout: arrays in, arrays out. A filter that
// cannot dead-end is then a matter of which empty state a tab pairs with an
// empty result, not of anything a helper has to remember.

import type { DeliveryView, Event, FaultEvent, SessionView } from '../gen/View';
import { clip, short } from '../lib/format';
import { OPERATOR_ROLE, describeEvent, principalName, principalRole, type Tone } from '../lib/model';
import type { LedgerScope } from '../lib/state/ui.svelte';

// ---- the ledger's rows

/// How much of a delivery's title one ledger line carries. The text filter
/// matches on what the row actually draws, so a word past this point is not a
/// word an operator can find, and pretending otherwise would be worse.
const TITLE_CLIP = 120;

/// One page of ledger rows. A snapshot already carries at most 200 of them, so
/// this is the number the held-back count is measured against rather than one
/// that has to be discovered by watching a tab crawl.
export const LEDGER_CAP = 200;

/// One delivery's line of text, cut to what the row draws.
export function rowTitle(delivery: DeliveryView): string {
  const head = (delivery.out_head ?? '').trim();
  if (head) return clip(head, TITLE_CLIP);
  // No body head yet — an in-flight row has none, and a row this tab first saw
  // on the stream carries none until the ledger is read again. The kind is
  // already a chip on this row, so what is worth saying here is *which* one it
  // is, and that is the task it belongs to.
  return delivery.task_id ? `task ${short(delivery.task_id)}` : short(delivery.msg_id);
}

/// What the ledger's scope switch holds: what is still owed, everything this
/// link has, or the operator's own receipts. Also what the tab row's count is
/// read from, so the number on the tab moves with the switch.
export function scopeLedger(deliveries: DeliveryView[], scope: LedgerScope): DeliveryView[] {
  if (scope === 'all') return deliveries;
  if (scope === 'inbox') return deliveries.filter((delivery) => principalRole(delivery.to) === OPERATOR_ROLE);
  return deliveries.filter((delivery) => delivery.state === 'queued' || delivery.state === 'in_flight');
}

/// The text filter, matched over the whole folded line: the title as drawn,
/// both ends of the route, and the two ids an operator pastes out of the
/// inspector.
export function matchesLedgerQuery(delivery: DeliveryView, query: string): boolean {
  const needle = query.trim().toLowerCase();
  if (needle === '') return true;
  return [
    rowTitle(delivery),
    principalName(delivery.from),
    principalName(delivery.to),
    delivery.msg_id,
    delivery.task_id ?? '',
  ]
    .join(' ')
    .toLowerCase()
    .includes(needle);
}

// ---- the sessions

/// The sessions still open. One read for the `live only` switch and for the
/// tab row's count, so the number on the tab is the number of rows the default
/// view holds.
export function liveSessions(sessions: SessionView[]): SessionView[] {
  return sessions.filter((session) => session.lifecycle !== 'exited');
}

/// The words a session row is searched by: its id, its role and the task it is
/// serving.
export function matchesSessionQuery(session: SessionView, query: string): boolean {
  const needle = query.trim().toLowerCase();
  if (needle === '') return true;
  return [session.session_id, session.role ?? '', session.task_id ?? ''].join(' ').toLowerCase().includes(needle);
}

// ---- the tail

/// The classes actually present in the tail, busiest first. Read off the lines
/// rather than off a fixed list, because the tail is what the build emits and a
/// filter over a class nothing carries can only ever empty a panel.
export function tailKinds(events: Event[]): { kind: string; count: number }[] {
  const counts = new Map<string, number>();
  for (const event of events) {
    const kind = describeEvent(event).kinds;
    counts.set(kind, (counts.get(kind) ?? 0) + 1);
  }
  return [...counts]
    .map(([kind, count]) => ({ kind, count }))
    .sort((a, b) => b.count - a.count || a.kind.localeCompare(b.kind));
}

// ---- the faults

/// One fault the ledger gave a key to. `id` is what both the selection and the
/// acknowledge op are addressed by, so a row without one is not a row this
/// panel can act on.
export type FaultRow = FaultEvent & { id: number };

/// The fault rows a switch holds: the open ones alone, or every row this link
/// has recorded. `open` is the reducer's own reading rather than a second
/// opinion about the state word.
export function faultRows(faults: FaultEvent[], open: ReadonlySet<string>, openOnly: boolean): FaultRow[] {
  return faults.filter(
    (fault): fault is FaultRow => typeof fault.id === 'number' && (!openOnly || open.has(String(fault.id))),
  );
}

/// One fault's reading: the hue and the word. `open` is the only state that
/// means work is owed, so it is the only one that wears a hue, and `failed`
/// keeps it because it names the work's own failure rather than the fault's.
export function faultReading(state: string | null | undefined): { tone: Tone; label: string } {
  if (!state || state === 'acked') return { tone: 'off', label: state ? 'acknowledged' : 'handled' };
  return { tone: state === 'failed' ? 'fail' : 'off', label: state.replace(/_/g, ' ') };
}

// ---- the shared cap

export interface Capped<T> {
  shown: T[];
  /// What the cap is holding back, said plainly: a list that silently stops
  /// reads as a list that has ended.
  held: number;
}

/// Cut a row list to the cap and count what is left. A stream that runs for
/// days would otherwise grow a tab without bound, and the dock is where that
/// is felt first.
export function capRows<T>(rows: T[], cap: number): Capped<T> {
  return rows.length > cap ? { shown: rows.slice(0, cap), held: rows.length - cap } : { shown: rows, held: 0 };
}