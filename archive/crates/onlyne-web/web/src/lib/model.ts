// The render-side model: one place for every reading the surface makes of the
// view. The fold belongs to Rust (`onlyne_proto::view`) and the columns, the
// open faults and the session words arrive already derived in the payload, so
// nothing here re-derives a state. What is decided here is presentation:
// which of the five status hues a row wears, what a row is called, and what
// shape a task family takes when it is drawn across the boards.
//
// Pure on purpose. Anything here can be called with a plain object, which is
// what lets the trace and the tone tables be reasoned about without a browser.

import type { Board, BoardColumn } from '../gen/Board';
import type {
  AgentPhase,
  DeliveryView,
  Event,
  FaultEvent,
  LedgerState,
  Lifecycle,
  MsgKind,
  Outcome,
  Presence,
  Principal,
  ResourcePhase,
} from '../gen/View';
import { plural, short } from './format';

/// The reserved role the operator speaks as (`spec.rs:39`).
export const OPERATOR_ROLE = '_supervisor';

/// The five status hues, plus `plain` for an ordinary reading.
export type Tone = 'run' | 'wait' | 'done' | 'fail' | 'queue' | 'off' | 'plain';

// ---- principals

/// The role name behind a principal, or null when it is a gateway or a cluster.
export function principalRole(principal: Principal): string | null {
  return 'role' in principal ? principal.role.role : null;
}

/// How a principal is spelled on this surface: one name, whatever it is.
export function principalName(principal: Principal): string {
  if ('role' in principal) return principal.role.role;
  if ('gateway' in principal) return principal.gateway.gateway;
  return principal.cluster.cluster;
}

// ---- status readings

export interface WorkReading {
  tone: Tone;
  label: string;
}

/// One delivery's reading: the ledger's word, the task's verdict and the board
/// column are three axes over one row, and this is the one place they are
/// weighed against each other.
export function workOf(work: {
  state: LedgerState;
  outcome?: Outcome | null;
  column?: BoardColumn | null;
}): WorkReading {
  if (work.outcome === 'failed') return { tone: 'fail', label: 'failed' };
  if (work.outcome === 'cancelled') return { tone: 'fail', label: 'cancelled' };
  if (work.outcome === 'blocked') return { tone: 'wait', label: 'blocked' };
  if (work.state === 'rejected') return { tone: 'fail', label: 'rejected' };
  if (work.state === 'expired') return { tone: 'fail', label: 'expired' };
  if (work.column === 'running') return { tone: 'run', label: 'running' };
  if (work.column === 'waiting') return { tone: 'wait', label: 'waiting' };
  if (work.column === 'queued') return { tone: 'queue', label: 'queued' };
  if (work.state === 'queued') return { tone: 'queue', label: 'queued' };
  if (work.state === 'in_flight') return { tone: 'run', label: 'in flight' };
  return { tone: 'done', label: work.outcome ?? 'done' };
}

export interface SessionReading {
  tone: Tone;
  label: string;
  /// A hollow dot: the session holds no process of its own.
  hollow: boolean;
}

const SESSION_READINGS: Record<string, SessionReading> = {
  opening: { tone: 'queue', label: 'opening', hollow: false },
  busy: { tone: 'run', label: 'busy', hollow: false },
  idle: { tone: 'plain', label: 'idle', hollow: true },
  suspended: { tone: 'queue', label: 'suspended', hollow: true },
  closed: { tone: 'off', label: 'closed', hollow: true },
};

/// One session's state word, derived from the four dimensions the row carries.
///
/// This mirrors `onlyne_proto::view::SessionView::state`, and it mirrors it on
/// purpose: the reducer refuses to store the word (`view.rs` calls a stored
/// reading "a second copy" of the four dimensions), so every renderer derives
/// it — the TUI in Rust, this one here. If that function changes, this line
/// changes with it.
export function sessionWordOf(row: {
  lifecycle: Lifecycle;
  agent: AgentPhase;
  resource: ResourcePhase;
}): string {
  if (row.lifecycle === 'exited' || row.agent === 'gone') return 'closed';
  if (row.lifecycle === 'working') return 'busy';
  if (row.resource === 'closed') return 'suspended';
  if (row.lifecycle === 'created') return 'opening';
  return 'idle';
}

/// One session's reading, from the word the reducer derived in Rust. An
/// unknown word reads as idle rather than blank, so a build that learns one
/// more state still draws a session.
export function sessionOf(word: string): SessionReading {
  return SESSION_READINGS[word] ?? SESSION_READINGS.idle;
}

/// Whether a fault is one no repair verb has moved, mirroring
/// `onlyne_proto::view::fault_is_open`: the `open` word, and an absent word
/// (a row written before the column carried one) read as open because nothing
/// can have moved it.
export function faultIsOpen(fault: FaultEvent): boolean {
  return fault.state == null || fault.state === 'open';
}

const PRESENCE_TONES: Record<Presence, Tone> = {
  online: 'done',
  draining: 'wait',
  offline: 'off',
};

export function presenceTone(presence: Presence): Tone {
  return PRESENCE_TONES[presence] ?? 'off';
}

/// The order a board arranges its cards in: work first, settled last.
export const COLUMN_ORDER: BoardColumn[] = ['running', 'waiting', 'queued', 'done', 'failed_or_blocked'];

// ---- names

/// One delivery's line of text: what it is, in the space a row has. The head
/// the ledger kept is the best answer; kind and sender are the fallback for a
/// row that has not settled into one yet.
export function workTitle(text: {
  out_head?: string | null;
  kind?: MsgKind;
  from?: string | null;
}): string {
  const head = (text.out_head ?? '').trim();
  if (head) return head;
  if (text.kind && text.from) return `${text.kind} from ${text.from}`;
  return text.kind ?? 'delivery';
}

/// What identifies a family: the root task id every hop of a run shares
/// (`Causality::root`). A row this browser first saw on the stream carries no
/// family until the next snapshot fills it in, and its own task id is the
/// closest thing it has — so a freshly sent task still traces as one chain.
export function familyKey(row: { family?: string | null; task_id?: string | null; msg_id: string }): string {
  return row.family ?? row.task_id ?? row.msg_id;
}

// ---- the trace

export interface TraceHop {
  msgId: string;
  /// Position in the drawn order, 0-based.
  index: number;
  /// The hop the envelope's causality carries, when this row was read from
  /// the ledger rather than first seen on the stream.
  causalityHop: number | null;
  /// Null for a gateway or a cluster, which no board renders.
  fromRole: string | null;
  toRole: string | null;
  fromName: string;
  toName: string;
  kind: MsgKind;
  state: LedgerState;
  outcome: Outcome | null;
  title: string;
  at: number;
  /// Both ends are boards on the canvas, so this hop can be drawn.
  drawn: boolean;
}

export interface TraceEdge {
  id: string;
  source: string;
  target: string;
  /// Every drawn step that lands on this pair, in order.
  steps: number[];
  tone: Tone;
}

export interface Trace {
  family: string;
  hops: TraceHop[];
  edges: TraceEdge[];
  /// Every role the family touches, in order of first appearance.
  roles: string[];
  /// The hop the inspector opened on.
  selected: string;
}

export interface TraceInput {
  /// The family's deliveries.
  members: DeliveryView[];
  /// The roles the canvas draws; a hop to anything else is a fact for the
  /// inspector and not a line for the graph.
  canvas: Set<string>;
  /// The joint column per delivery, as the boards arranged them.
  columns: Map<string, BoardColumn>;
  /// When a row was first seen, for a row that carries no clock of its own.
  timeOf: (delivery: DeliveryView) => number;
  selected: string;
}

/// The whole shape of one task family: its hops in order and the lines they
/// draw between boards. A chain is drawn even when the flow runs against a
/// declared route — a completion reports home, and hiding that would hide the
/// end of the story.
export function traceOf(input: TraceInput): Trace {
  const members = [...input.members].sort(
    (a, b) => input.timeOf(a) - input.timeOf(b) || (a.hop ?? 0) - (b.hop ?? 0),
  );
  const roles: string[] = [];
  const hops: TraceHop[] = members.map((delivery, index) => {
    const fromRole = principalRole(delivery.from);
    const toRole = principalRole(delivery.to);
    for (const role of [fromRole, toRole]) {
      if (role && !roles.includes(role)) roles.push(role);
    }
    const drawn =
      fromRole !== null && toRole !== null && fromRole !== toRole && input.canvas.has(fromRole) && input.canvas.has(toRole);
    return {
      msgId: delivery.msg_id,
      index,
      causalityHop: delivery.hop ?? null,
      fromRole,
      toRole,
      fromName: principalName(delivery.from),
      toName: principalName(delivery.to),
      kind: delivery.kind,
      state: delivery.state,
      outcome: delivery.outcome ?? null,
      title: workTitle({ out_head: delivery.out_head, kind: delivery.kind, from: fromRole }),
      at: input.timeOf(delivery),
      drawn,
    };
  });

  const byPair = new Map<string, TraceEdge>();
  for (const hop of hops) {
    if (!hop.drawn || !hop.fromRole || !hop.toRole) continue;
    const id = `${hop.fromRole}->${hop.toRole}`;
    const edge = byPair.get(id) ?? { id, source: hop.fromRole, target: hop.toRole, steps: [], tone: 'plain' as Tone };
    edge.steps.push(hop.index);
    edge.tone = workOf({ state: hop.state, outcome: hop.outcome, column: input.columns.get(hop.msgId) }).tone;
    byPair.set(id, edge);
  }

  return {
    family: familyKey(input.members[0] ?? { msg_id: input.selected }),
    hops,
    edges: [...byPair.values()],
    roles,
    selected: input.selected,
  };
}

// ---- the stream's tail, as one line

export interface EventLine {
  tone: Tone;
  kinds: string;
  text: string;
}

/// One string pulled out of an event payload the wire type leaves open, cut
/// for a row.
function field(data: Record<string, unknown>, key: string): string {
  const value = data[key];
  return typeof value === 'string' ? short(value, 12) : '';
}

function colon(...parts: string[]): string {
  return parts.filter(Boolean).join(' · ');
}

/// What one event says, in a line: the class, then whatever the event itself
/// carries that names what moved. The tail is a stream of these, so the shape
/// of the line is the panel's whole design.
export function describeEvent(event: Event): EventLine {
  switch (event.type) {
    case 'role_presence': {
      const data = event.data;
      const state = data.state === 'offline' ? 'off' : data.state === 'draining' ? 'wait' : 'run';
      return {
        tone: state,
        kinds: 'role',
        text: colon(data.role, data.state, data.sessions ? plural(data.sessions, 'session') : '', data.detail ?? ''),
      };
    }
    case 'session_state': {
      const data = event.data;
      const lifecycle = data.projection?.lifecycle;
      return {
        tone: lifecycle === 'working' ? 'run' : lifecycle === 'exited' ? 'off' : 'plain',
        kinds: 'session',
        text: colon(data.role ?? '', short(data.session_id, 8), lifecycle ?? '', data.projection?.agent ?? ''),
      };
    }
    case 'ledger_state': {
      const data = event.data;
      const reading = workOf({ state: data.state, outcome: data.outcome ?? null });
      return {
        tone: reading.tone,
        kinds: 'ledger',
        text: colon(data.kind, `${principalName(data.from)} to ${principalName(data.to)}`, reading.label),
      };
    }
    case 'fault': {
      const data = event.data;
      return {
        tone: data.state && data.state !== 'open' ? 'off' : 'fail',
        kinds: 'fault',
        text: colon(data.kind ?? 'fault', data.role ?? '', field(data as Record<string, unknown>, 'task_id'), data.reason ?? ''),
      };
    }
    case 'gateway_presence': {
      const data = event.data;
      return {
        tone: data.state === 'online' ? 'run' : data.state === 'reconnecting' ? 'wait' : 'fail',
        kinds: 'gateway',
        text: colon(data.gateway, data.platform, data.state),
      };
    }
    case 'spec_reloaded': {
      const data = event.data;
      return {
        tone: 'plain',
        kinds: 'spec',
        text: colon('spec reloaded', `${data.roles ?? 0} roles`, `${data.routes ?? 0} routes`),
      };
    }
    case 'turn_end_without_complete':
      return { tone: 'wait', kinds: 'settle', text: 'a turn ended without a completion' };
    case 'delivery_blocked':
      return { tone: 'wait', kinds: 'settle', text: 'a delivery is blocked' };
    default: {
      // `handoff` and anything a later build adds: the class word, then
      // whatever of the usual names the payload happens to carry. `data` is
      // left open by the wire type, so it is read, not trusted.
      const data = (event.data ?? {}) as Record<string, unknown>;
      return {
        tone: 'plain',
        kinds: String(event.type),
        text: colon(field(data, 'role'), field(data, 'session_id'), field(data, 'task_id')),
      };
    }
  }
}

// ---- the graph's routes

export interface Route {
  id: string;
  source: string;
  target: string;
}

/// The edges the graph draws: the cross-role routes the boards declare.
/// A `*` names no single line, so such a role travels in `reaches` instead —
/// the canvas says "reaches every role" once, rather than drawing a fan that
/// would bury the routes the operator actually declared.
export function routesOf(boards: Board[]): { routes: Route[]; reaches: string[] } {
  const named = new Set(boards.map((board) => board.role));
  const routes: Route[] = [];
  const reaches: string[] = [];
  for (const board of boards) {
    for (const target of board.edges ?? []) {
      if (target === '*') {
        if (!reaches.includes(board.role)) reaches.push(board.role);
        continue;
      }
      if (target === board.role || !named.has(target)) continue;
      routes.push({ id: `${board.role}->${target}`, source: board.role, target });
    }
  }
  return { routes, reaches };
}

/// The boards the canvas draws. The operator's is not one of them:
/// `_supervisor` is a logical node — the standing name for whoever runs the
/// cluster — with a built-in reach to every role and no process behind it.
export function roleBoards(boards: Board[]): Board[] {
  return boards.filter((board) => !board.operator);
}

/// When a graph is crowded enough that its lines are dimmed. A share alone
/// reads a two-role cluster as dense, so it is only consulted over a graph big
/// enough for "most pairs" to mean anything; the absolute count carries the rest.
///
/// The share is measured **directed against directed**: a route has a
/// direction, so the denominator is every ordered pair, not every unordered
/// one. Comparing directed edges with undirected pairs counts each line twice
/// and calls a six-role cluster with eight routes half full.
export const DENSITY_SHARE = 0.5;
export const DENSITY_ROLES = 4;
export const DENSITY_ABSOLUTE = 64;

export function crowded(boards: Board[]): boolean {
  const drawn = roleBoards(boards);
  const roles = drawn.length;
  const edges = routesOf(drawn).routes.length;
  if (edges >= DENSITY_ABSOLUTE) return true;
  if (roles < DENSITY_ROLES) return false;
  const possible = roles * (roles - 1);
  return possible > 0 && edges / possible >= DENSITY_SHARE;
}
