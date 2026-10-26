export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type NodeId = string;
export type DomainId = string;
export interface DomainMembership {
  domain_id: DomainId; node_id: NodeId; state: 'active' | 'left'; revision: string;
}
export interface DomainMutation extends Mutation { expected_revision?: string }
export interface DomainDiscovery {
  domain_id: DomainId; members: DomainMembership[]; unavailable: NodeId[];
}
export interface DiscoveryCandidate {
  node_id: NodeId; instance_id: string; base_url: string; last_seen: string;
}
export interface DiscoveryRecord extends DiscoveryCandidate {
  source: string; received_at: string; expires_at: string;
  verified_at?: string; membership?: DomainMembership | null; error?: Failure;
}
export interface ServiceDescription {
  protocol: 'onlyne/3'; node_id: NodeId; instance_id: string; base_url: string;
  features: string[]; authentication: string[];
  domain: DomainId | null; membership: DomainMembership | null;
}
export interface ThreadRef { owner: NodeId; id: string }
export interface MessageRef { sender: NodeId; id: string }
export interface ResourceRef { owner: NodeId; id: string }
export type ErrorCode = 'invalid_request' | 'unauthenticated' | 'forbidden' | 'not_a_member' | 'not_found' | 'id_conflict' | 'revision_conflict' | 'request_in_progress' | 'conflict' | 'thread_closed' | 'context_lost' | 'retry_window_expired' | 'cursor_expired' | 'payload_too_large' | 'unsupported_media_type' | 'unsupported_delivery' | 'ambiguous_target' | 'ambiguous_resource' | 'capacity_exceeded' | 'internal' | 'unavailable' | 'decision_unavailable';
export interface Failure { code: ErrorCode; message: string; retryable: boolean; details?: Json }
export interface Auth { actor: NodeId; sender?: NodeId; reply_authority?: NodeId }
export interface Transaction {
  get<T>(key: string): Promise<T | undefined>;
  put<T>(key: string, value: T): Promise<void>;
  delete(key: string): Promise<void>;
  scan<T>(prefix: string): Promise<Array<{ key: string; value: T }>>;
}
export interface Storage {
  readonly guarantee: 'durable' | 'binding-defined';
  /** Serializable, atomic commit. Rejecting the callback rolls back every write. */
  transaction<T>(work: (tx: Transaction) => Promise<T>): Promise<T>;
}
export interface Page<T> { items: T[]; next_cursor: string | null }
export interface Mutation { idempotency_key: string; retry_until: string }
export interface DeliveryPlan { response: 'none' | 'async' | 'await'; execution: 'existing' | 'new'; context?: string }
export interface Message {
  id: string;
  sender: NodeId;
  delivery_kind: string;
  msg: string;
  meta?: {
    thread?: ThreadRef;
    in_reply_to?: MessageRef;
    relation?: 'reply' | 'followup';
    mentions?: string[];
    parts?: Json[];
    data?: { [key: string]: Json };
  };
}
export interface SendRequest extends Mutation { message: Message; target_endpoint: NodeId; plan: DeliveryPlan }
export interface Receipt {
  message_ref: MessageRef; receipt_id: string; guarantee: Storage['guarantee'];
  thread_ref: ThreadRef; seq: string; plan: DeliveryPlan;
  thread_created?: boolean; previous_seq?: string;
  /**
   * The message this one answers, in the identity the owner stored: the sender's
   * `{thread,seq}` spelling is resolved before delivery, so this is the pair every
   * later read agrees on. Absent when the message answers nothing.
   */
  in_reply_to?: MessageRef;
}
export interface Entry {
  thread_ref: ThreadRef; seq: string; message: Message; authenticated_author: NodeId;
  reply_authority: NodeId; at: string;
}
export interface ReadScope { peers?: NodeId[]; since_seq?: string; related_to?: MessageRef }
export interface Member { peer: NodeId; state: 'requested' | 'active' | 'revoked' | 'denied'; access: Array<'read' | 'post'>; read_scope: ReadScope; revision: string }
export interface Thread {
  thread_ref: ThreadRef; title: string; state: 'open' | 'closed'; next_seq: string;
  event_seq: string; created_at: string; closed_at: string | null;
  messages_until?: string;
}
export interface Attention {
  obligation_id: string; recipient: NodeId; thread_ref: ThreadRef; seq: string;
  message_ref: MessageRef; authenticated_author: NodeId; acked: boolean;
  disposition?: 'expired';
  plan?: DeliveryPlan;
  entry?: Entry;
}
export interface Grant {
  id: string; recipient: NodeId; thread_ref: ThreadRef; revision: string; acked: boolean;
}
export interface Event {
  id: string; type: 'message' | 'membership' | 'thread_closed' | 'resource_event' | 'operation';
  thread_ref?: ThreadRef; resource_ref?: ResourceRef; message_ref?: MessageRef; data: Json;
}
export interface PageOptions { cursor?: string; page_size?: number }
export interface MessageQuery extends PageOptions { attention?: 'inbox' | 'thread'; since_seq?: string; peer?: NodeId; related_to?: MessageRef }
export interface ReplyPermit {
  message_ref: MessageRef; grantee: NodeId; landing: ThreadRef; expires_at: string;
  remaining: number; revoked: boolean; followup: boolean;
}
export interface Resource {
  resource_ref: ResourceRef; kind: 'im_conversation' | 'compute_job' | 'dataset' | 'mcp';
  operations: string[]; assignee?: NodeId; revision: string;
  readers: NodeId[]; controllers: NodeId[]; event_recipients: NodeId[]; result_recipients: NodeId[];
  event_seq: string; retention_ms: number; data?: Json;
}
export interface ResourceEvent {
  resource_ref: ResourceRef; event_id: string; source_id: string; recipients: NodeId[];
  revision: string; data: Json; at: string; expires_at: string;
  acknowledged: NodeId[]; expired: boolean;
}
export interface OperationRequest extends Mutation {
  operation_id: string; operation: string; expected_revision?: string; data?: Json;
}
export interface OperationResult {
  operation_id: string; state: 'committed' | 'pending' | 'aborted' | 'unknown';
  data?: Json; error?: Failure;
}
export interface OperationRecord {
  actor: NodeId; request: OperationRequest; digest: string; result: OperationResult;
  resource_ref: ResourceRef; accepted_at: string; authorized_revision: string;
}
export interface Handover {
  operation_id: string; resource_ref: ResourceRef; initiator: NodeId; target: NodeId;
  expected_revision: string; decision_deadline: string; accepted: boolean;
  state: 'pending' | 'committed' | 'aborted'; cutoff_event_id: string | null;
  scope?: Array<'events' | 'results' | 'control'>;
}
export interface Outbound {
  request: SendRequest; expected_author: NodeId; state: 'pending' | 'accepted' | 'refused' | 'unknown';
  receipt?: Receipt; error?: Failure; created_at?: string;
}
export interface HistoryMessage {
  message_ref: MessageRef; message: Message; direction: 'in' | 'out';
  thread_ref?: ThreadRef; seq?: string; at: string | null;
  delivery: Outbound['state'] | 'received'; consumption?: 'pending' | 'consumed';
  guarantee?: Storage['guarantee']; error?: Failure;
}
export interface Conversation {
  id: MessageRef; title: string; thread_refs: ThreadRef[]; messages: HistoryMessage[];
  last_at: string | null;
}
export interface HistoryQuery extends PageOptions { thread?: ThreadRef; message?: MessageRef }
/**
 * One queued obligation and what became of it. `context` is where the delivery was sent;
 * `injected` is **observed**, not submitted: it flips only in `confirmConsumption`, once the
 * host has seen the entry in a provider request. A message submitted into a session that never
 * reads it therefore stays `injected: false` and keeps its place in unread and alerts
 * (caerulea report 3, problem 1). The name predates that rule and is kept because renaming it
 * would read every stored row as unobserved and re-inject what was already read.
 */
export interface LocalAttention { attention: Attention; context?: string; injected: boolean }
export interface Target { node_id: NodeId; base_url: string; headers?: Record<string, string> }
export interface Transport {
  request<T>(target: NodeId, method: string, path: string, body?: unknown, signal?: AbortSignal): Promise<T>;
}
export interface MsgInput { delivery_kind: string; msg: string; meta?: Record<string, Json> }
export interface PreparedSend { to: NodeId; message: Message; plan: DeliveryPlan }
export type Action =
  | { kind: 'domain'; op: 'create' | 'join' | 'leave' | 'list' | 'describe' | 'discover'; domain_id?: DomainId; expected_revision?: string }
  | { kind: 'send'; to: NodeId; thread?: ThreadRef; context?: string; in_reply_to?: MessageRef; relation?: 'reply' | 'followup'; mentions?: NodeId[]; response: DeliveryPlan['response']; execution: DeliveryPlan['execution'] }
  | { kind: 'read'; scope: 'directory' | 'neighbors' | 'threads' | 'inbox' | 'messages' | 'message' | 'evidence'; thread?: ThreadRef; message?: MessageRef; seq?: string; cursor?: string; page_size?: number; attention?: 'inbox' | 'thread' }
  | { kind: 'thread'; op: 'create' | 'close' | 'list' | 'describe' | 'member_add' | 'member_remove'; thread?: ThreadRef; to?: NodeId; title?: string }
  | { kind: 'resource'; resource: ResourceRef; operation: string; data?: Json; expected_revision?: string };
/**
 * One lifecycle transition of the local node, emitted for offline diagnosis. Every field is an
 * identifier, a state name or a count: message bodies and credentials never appear here.
 */
export type TraceEvent =
  | { event: 'inbound.received'; obligation: string; thread: ThreadRef; seq: string; sender: NodeId; relation?: 'reply' | 'followup'; valid: boolean; context?: string; injected: boolean }
  | { event: 'inbound.duplicate'; sender: NodeId; message: string }
  | { event: 'inbound.acked'; obligation: string; thread: ThreadRef; seq: string }
  | { event: 'inbound.inject'; thread: ThreadRef; seq: string; context: string; outcome: InjectOutcome; reason?: string }
  | { event: 'consumption.confirmed'; thread: ThreadRef; seq: string; context: string; via: 'fragment' | 'tool_result' }
  | { event: 'outbound.admitted'; sender: NodeId; message: string; target: string; state: string; guarantee?: string; code?: string }
  | { event: 'outbound.redriven'; sender: NodeId; message: string; target: string }
  | { event: 'outbound.expired'; sender: NodeId; message: string; code: string }
  | { event: 'await.settled'; thread: ThreadRef; seq: string; waiters: number };
/** What the host did with one inbound entry, reported back with the injection result. */
export type InjectOutcome = 'delivered' | 'queued' | 'waiting' | 'skipped' | 'consumed';
export interface Runtime {
  supports(plan: DeliveryPlan, message?: Message): Promise<boolean>;
  /** Submission returns consumed:false; an observed request boundary may confirm it later. */
  inject(entry: Entry, context?: string, plan?: DeliveryPlan): Promise<{ context: string; consumed: boolean; outcome?: InjectOutcome }>;
  todo?(entry: Entry, context: string): Promise<void>;
  /**
   * RFC 0017 §5: the one notification channel. The node calls it for every committed
   * trust transition, every inbound entry, and after each maintenance pass that changed
   * the pending-work answer. A host that renders from these events never polls storage
   * on its own; a host that misses one still converges on the next maintenance pass.
   */
  notify(event: NotifyEvent): void;
  /**
   * Optional structured trace. The node calls it on every lifecycle transition; the host maps
   * it onto its own log, so a delivery can be followed from admission to consumption offline.
   */
  trace?(event: TraceEvent): void;
  /**
   * Called by the host once it observes that the model consumed an `await` tool result
   * (RFC 0008 §5). The library records that as the shared consumption for the entry's
   * `(context, ThreadRef, seq)`, so the automatic fragment does not present it again.
   */
  confirmToolResult?(entry: Entry, context: string): Promise<void>;
}
export interface ResourceDriver {
  validate(resource: Resource, operation: string, data: Json | undefined): Promise<void>;
  execute(resource: Resource, request: OperationRequest, signal?: AbortSignal): Promise<Omit<OperationResult, 'operation_id'>>;
  query?(resource: Resource, operation_id: string): Promise<Omit<OperationResult, 'operation_id'>>;
}
/** RFC 0017 key material the host hands the library: the signing key and the public key it proves. */
export interface KeyMaterial {
  readonly private_key: Uint8Array;
  readonly public_key: Uint8Array;
}

/** Load-or-create the node identity key; storage is the host's choice (RFC 0017 §2). */
export type KeyHook = (() => Promise<KeyMaterial>) & { replace?(material: KeyMaterial): Promise<void> };

export interface KeyIdentity {
  node_id: NodeId;
  public_key: string;
  fingerprint: string;
}

export interface TrustSeed { node_id: NodeId; fingerprint: string }

export interface TrustStatement {
  node_id: NodeId;
  target: NodeId;
  public_key: string;
  target_fingerprint: string;
  base_url: string;
  approved: boolean;
  nonce: string;
  timestamp: number;
  signature: string;
  previous_signature?: string;
  /**
   * Set on a reply: the nonce of the request this statement answers. A sender settles its
   * delivery obligation only on a reply that echoes the nonce it sent (RFC 0017 §3.5).
   */
  reply_nonce?: string;
}

/**
 * The observable stage of one trust relation, derived only from the approval halves
 * (RFC 0017 §3). A host renders and decides from this value; it never re-derives
 * the stage from the raw halves.
 */
export type TrustPhase = 'invited' | 'requested' | 'exchanged' | 'revoked';

/** RFC 0017 §5: what changed when the node calls `runtime.notify`. */
export type NotifyEvent =
  | { event: 'work-pending'; pending: boolean }
  | { event: 'trust'; cause: 'local-request' | 'local-accept' | 'local-revoke' | 'peer-statement' | 'rotation'; phase: TrustPhase; entry: TrustEntry; previous?: TrustEntry }
  | { event: 'inbound'; thread: ThreadRef; seq: string; sender: NodeId };

/** RFC 0017 §3: one exchange, as one object. `begin` hands this to the host. */
export interface KeyExchange {
  /** The peer the host named; the statement proves this identity, never a guess from discovery. */
  readonly node_id: NodeId;
  /** The fingerprint the peer's live key lookup proved at `begin` time. */
  readonly fingerprint: string;
  readonly public_key: string;
  readonly base_url: string;
  /**
   * Commits local approval and sends the signed introduction. `fingerprint` must repeat
   * the value shown to the user: a changed key between display and commit fails here.
   */
  confirm(fingerprint: string): Promise<TrustEntry>;
}

/** One trust-table row: a peer's verified public key and the exchange state. */
export interface TrustEntry {
  readonly node_id: NodeId;
  readonly public_key: string;
  readonly fingerprint: string;
  readonly state: 'pending' | 'trusted' | 'revoked';
  readonly first_seen: string;
  readonly last_checked: string;
  readonly local_approved: boolean;
  readonly remote_approved: boolean;
  readonly base_url: string;
  readonly history?: string[];
}

/** The stage derived from the approval halves; the single source hosts read (RFC 0017 §3). */
export function trustPhase(entry: Pick<TrustEntry, 'local_approved' | 'remote_approved' | 'state'>): TrustPhase {
  if (entry.state === 'revoked') return 'revoked';
  if (entry.local_approved && entry.remote_approved) return 'exchanged';
  return entry.local_approved ? 'requested' : 'invited';
}

export interface Plugins {
  storage: Storage;
  authenticate(request: Request): Promise<Auth | null>;
  keys?: KeyHook;
  trust_seeds?: TrustSeed[];
  resolve(node: NodeId): Promise<Target>;
  decide(input: MsgInput): Promise<Action>;
  runtime?: Runtime;
  /** Supplied when the host wants the library to own the maintenance loop (RFC 0017 §6). */
  maintenanceClock?: MaintenanceClock;
  resources?: ResourceDriver;
  transport?: Transport;
  mayAdmit?(actor: NodeId, thread: ThreadRef): Promise<boolean>;
  /** Owner policy permitting a first-contact thread and membership for this actor. */
  inboundThread?(actor: NodeId): Promise<boolean>;
  mayHandover?(resource: Resource, initiator: NodeId, target: NodeId): Promise<boolean>;
  directory?(): Promise<Json>;
  executionContexts?(): Promise<Json>;
  domainCandidates?(domain_id: DomainId): Promise<NodeId[]>;
  /** Resolves a thread-local mention alias to the member node it names. */
  mention?(alias: string, thread: ThreadRef): Promise<NodeId>;
  /** Observed nodes with trust state; the same rows onlyne://neighbors renders. */
  neighbors?(): Promise<unknown>;
}
export interface NodeOptions {
  node_id: NodeId; base_url: string; plugins: Plugins;
  instance_id?: string; authentication?: string[];
  request_bytes?: number; retry_max_ms?: number; wait_ms?: number;
  /**
   * The trusted tick source, required at construction (RFC 0017 §6). Every protocol
   * clock read — transcript windows, nonce expiry, retry gates — goes through `now`,
   * and every nonce through `id`. A host that cannot supply an unguessable `id` must
   * refuse to run a keyed node instead of falling back to a default.
   */
  now: () => number;
  id: () => string;
}

/**
 * RFC 0017 §6: the timer primitives the library's maintenance loop needs. Supplying
 * them opts into `client.runMaintenance`, the loop the library owns: cadence, wake
 * handling and the pending-work re-arm all live there, so a host cannot poll at a
 * rate the protocol does not sanction. `setTimer` is one-shot: it calls `run` once and
 * the loop re-arms, so a host that repeats the callback breaks the cadence contract.
 * Every clock read goes through `NodeOptions.now`; the clock carries no second time source.
 */
export interface MaintenanceClock {
  setTimer(ms: number, run: () => void): unknown;
  clearTimer(handle: unknown): void;
}
