// The one state the screens render, fed by the SSE stream the Rust side folds.
// The browser holds no fold of its own: `view`, `boards` and `readings` arrive
// already read off `onlyne-proto::view` and `onlyne-web::render`, and every
// change below is a frame the stream pushed.
//
// Three structural decisions hold this together.
//
// **The payload is `$state.raw`.** A frame carries the whole view, so a deep
// proxy over hundreds of rows would be built and thrown away every time the
// stream moved. Only the reference changes, so only the reference is tracked.
//
// **Rows this tab first saw on the stream carry no clock.** `enqueued_at` is
// the `ledger` read's, and a `ledger_state` event carries none of it, so the
// `seen` map holds the moment this tab first saw such a row. Every timestamp
// on this surface is read through `timeOf`, which prefers the row's own clock.
//
// **A frame boundary is observable exactly once**, through `onFrame`, because
// the graph's edge pulses and the ledger's highlight both ride the difference
// between two frames.

import type { Board, BoardCard, SessionCounts } from '../../gen/Board';
import type { DeliveryView, Event, FaultEvent, Lifecycle, RoleInfo, SessionView, View } from '../../gen/View';
import { parseTime } from '../format';
import { crowded, familyKey, faultIsOpen, roleBoards, routesOf, sessionWordOf, traceOf } from '../model';
import { openStream } from '../net/stream';
import type { Payload } from '../net/api';
import { statusOf, type Transport } from './clock.svelte';

/// Work that has not settled: the columns a board shows as owed.
const UNSETTLED: Record<BoardCard['column'], boolean> = {
  queued: true,
  running: true,
  waiting: true,
  done: false,
  failed_or_blocked: false,
};

/// Working sessions first, then the ones that have not started, then the idle
/// ones, then the closed: the order an operator scans the panel in.
const SESSION_RANK: Record<Lifecycle, number> = { working: 0, created: 1, idle: 2, exited: 3 };

const EMPTY_VIEW: View = { cluster: {}, roles: {}, sessions: {}, deliveries: {}, faults: {}, event_tail: [] };

class ClusterStore {
  /// The payload as the wire sent it: never mutated in place, and not proxied
  /// — a frame replaces the whole reference, so the reference is all that has
  /// to be tracked.
  raw = $state.raw<Payload>({
    cursor: 0,
    link: '',
    view: EMPTY_VIEW,
    boards: [],
  });

  token = $state('');
  transport = $state<Transport>('connecting');
  detail = $state('');
  lastFrameAt = $state(0);

  /// When this tab first saw a delivery, by `msg_id`, for the rows that arrive
  /// carrying no clock of their own.
  private seen = new Map<string, number>();
  private listeners = new Set<(previous: Payload, next: Payload) => void>();
  private stop: (() => void) | null = null;
  /// The previous tail's keys and stamps, so a frame only digests its own.
  private eventKeys: string[] = [];
  private eventStamps: number[] = [];

  view = $derived(this.raw.view);
  boards = $derived(this.raw.boards);

  live = $derived(this.transport === 'live');
  stale = $derived(this.raw.view.stale === true);
  /// The header's word about the link.
  status = $derived(statusOf(this.transport, this.stale));
  statusDetail = $derived(
    this.transport === 'refused'
      ? this.detail
      : this.raw.link.startsWith('offline')
        ? this.raw.link
        : 'the admin link is live',
  );

  /// The boards the canvas draws, and the ones it does not: `_supervisor` is a
  /// logical node with a built-in reach to every role and no process behind it.
  canvasBoards = $derived(roleBoards(this.raw.boards));
  boardByRole = $derived(new Map(this.raw.boards.map((board) => [board.role, board])));
  canvasRoles = $derived(new Set(this.canvasBoards.map((board) => board.role)));

  /// The route the canvas draws between two boards, and the roles that reach
  /// every role. A `*` names no single line, so it is reported once per role
  /// rather than drawn as a fan.
  routes = $derived(routesOf(this.canvasBoards).routes);
  reaches = $derived(routesOf(this.canvasBoards).reaches);
  /// Whether this graph is crowded enough to dim its lines. Read once here
  /// rather than once per edge: it walks every board and every route, and an
  /// edge that asked for it on its own would pay that cost per line per frame.
  dimEdges = $derived(crowded(this.boards));

  /// Newest first. `msg_id` is a uuid v4 and carries no order of its own, so
  /// the sort is on the row's clock, or on this tab's first sight of it.
  deliveries = $derived(
    Object.values(this.raw.view.deliveries ?? {}).sort((a, b) => this.timeOf(b) - this.timeOf(a)),
  );
  deliveryById = $derived(new Map(this.deliveries.map((delivery) => [delivery.msg_id, delivery])));
  families = $derived.by(() => {
    const groups = new Map<string, DeliveryView[]>();
    for (const delivery of this.deliveries) {
      const key = familyKey(delivery);
      const group = groups.get(key);
      if (group) group.push(delivery);
      else groups.set(key, [delivery]);
    }
    return groups;
  });

  /// The column each board put a delivery in: the reducer's joint reading of
  /// the two axes, read off the boards rather than derived a second time.
  columnById = $derived.by(() => {
    const columns = new Map<string, BoardCard['column']>();
    for (const board of this.raw.boards) {
      for (const card of board.cards ?? []) columns.set(card.msg_id, card.column);
    }
    return columns;
  });

  /// Unsettled deliveries per drawn route, by `from->to`. A card sits on the
  /// board it is addressed to, so a card `from` planner on builder's board is
  /// load on the planner-to-builder edge, whatever the delivery's state word is.
  routeLoad = $derived.by(() => {
    const load = new Map<string, number>();
    for (const board of this.canvasBoards) {
      for (const card of board.cards ?? []) {
        if (!UNSETTLED[card.column] || !card.from || card.from === board.role) continue;
        const key = `${card.from}->${board.role}`;
        load.set(key, (load.get(key) ?? 0) + 1);
      }
    }
    return load;
  });

  sessions = $derived(
    Object.values(this.raw.view.sessions ?? {}).sort(
      (a, b) =>
        SESSION_RANK[a.lifecycle] - SESSION_RANK[b.lifecycle] ||
        (a.role ?? '').localeCompare(b.role ?? '') ||
        a.session_id.localeCompare(b.session_id),
    ),
  );
  sessionsByRole = $derived.by(() => {
    const byRole = new Map<string, SessionView[]>();
    for (const session of this.sessions) {
      const role = session.role ?? '';
      const group = byRole.get(role);
      if (group) group.push(session);
      else byRole.set(role, [session]);
    }
    return byRole;
  });
  sessionByTask = $derived.by(() => {
    const byTask = new Map<string, SessionView>();
    for (const session of this.sessions) {
      if (session.task_id) byTask.set(session.task_id, session);
    }
    return byTask;
  });

  faults = $derived(
    Object.values(this.raw.view.faults ?? {})
      .filter((fault): fault is FaultEvent => fault !== null && fault !== undefined)
      .sort((a, b) => (b.id ?? 0) - (a.id ?? 0)),
  );
  /// The faults no repair verb has moved, by the reducer's own reading.
  openFaults = $derived(this.faults.filter(faultIsOpen));

  events = $derived(this.raw.view.event_tail ?? []);

  /// The header's counters.
  stats = $derived.by(() => {
    let sessions = 0;
    let inFlight = 0;
    for (const board of this.canvasBoards) {
      const counts = board.counts ?? {};
      sessions += (counts.busy ?? 0) + (counts.idle ?? 0) + (counts.suspended ?? 0);
      for (const card of board.cards ?? []) if (UNSETTLED[card.column]) inFlight += 1;
    }
    return {
      roles: this.canvasBoards.length,
      online: this.canvasBoards.filter((board) => board.presence !== 'offline').length,
      sessions,
      inFlight,
    };
  });

  /// The moment to read a delivery by: its own clock, or this tab's first
  /// sight of it. A stream-born row carries neither until the next snapshot.
  timeOf = (delivery: DeliveryView): number =>
    parseTime(delivery.enqueued_at) ?? this.seen.get(delivery.msg_id) ?? 0;

  countsOf = (role: string): SessionCounts => this.boardByRole.get(role)?.counts ?? {};

  /// One role's board, cards and all, when the canvas is not the caller.
  boardCards = (role: string): BoardCard[] => this.boardByRole.get(role)?.cards ?? [];

  /// The state word for one session, derived from the row the reducer holds.
  sessionWord = (sessionId: string): string => {
    const session = this.view.sessions?.[sessionId];
    return session ? sessionWordOf(session) : 'idle';
  };

  roleInfo = (role: string): RoleInfo | undefined => this.view.roles?.[role];

  /// The trace one family draws across the boards, read from the message the
  /// inspector opened on.
  traceFor = (msgId: string) => {
    const delivery = this.deliveryById.get(msgId);
    if (!delivery) return null;
    return traceOf({
      members: this.families.get(familyKey(delivery)) ?? [delivery],
      canvas: this.canvasRoles,
      columns: this.columnById,
      timeOf: this.timeOf,
      selected: msgId,
    });
  };

  /// When this tab first saw each tail entry, positionally aligned with
  /// `events`. A frame carries the whole tail and no arrival time, so the
  /// entries above the previous head are the ones that just happened.
  stamps = $state.raw<number[]>([]);

  /// Watch a frame land. The difference between two payloads is what an edge
  /// pulse and a row flash are made of, and this is the only place it exists.
  onFrame = (listener: (previous: Payload, next: Payload) => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  connect = (token: string) => {
    this.token = token;
    this.stop?.();
    this.stop = openStream(token, {
      frame: (payload) => this.adopt(payload),
      transport: (state, detail) => {
        this.transport = state;
        this.detail = detail;
      },
    });
  };

  private adopt(payload: Payload) {
    const previous = this.raw;
    const now = Date.now();
    for (const delivery of Object.values(payload.view.deliveries ?? {})) {
      if (!delivery.enqueued_at && !this.seen.has(delivery.msg_id)) this.seen.set(delivery.msg_id, now);
    }
    this.stampEvents(payload.view.event_tail ?? [], now);
    this.raw = payload;
    this.lastFrameAt = now;
    for (const listener of this.listeners) listener(previous, payload);
  }

  private stampEvents(tail: Event[], now: number) {
    const keys = tail.map((event) => JSON.stringify(event));
    // Where the tail this tab already had starts. A tail that was extended
    // carries its old head further down; a tail that was replaced (a resync)
    // does not carry it at all, and then nothing in it is new.
    const head = this.eventKeys.length > 0 ? keys.indexOf(this.eventKeys[0]) : -1;
    const stamps: number[] = [];
    for (let index = 0; index < keys.length; index += 1) {
      if (head < 0) {
        // A tail with no overlap is a tail this tab cannot date. Left at zero,
        // which the screen reads as "no time", rather than stamped `now` and
        // shown as a burst of things that did not just happen.
        stamps.push(0);
        continue;
      }
      const carried = index - head;
      stamps.push(carried >= 0 ? (this.eventStamps[carried] ?? now) : now);
    }
    this.eventKeys = keys;
    this.eventStamps = stamps;
    this.stamps = stamps;
  }
}

export const cluster = new ClusterStore();
