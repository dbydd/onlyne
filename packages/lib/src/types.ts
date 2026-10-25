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
export type ErrorCode = 'invalid_request' | 'unauthenticated' | 'forbidden' | 'not_a_member' | 'not_found' | 'id_conflict' | 'revision_conflict' | 'request_in_progress' | 'conflict' | 'thread_closed' | 'retry_window_expired' | 'cursor_expired' | 'payload_too_large' | 'unsupported_media_type' | 'unsupported_delivery' | 'ambiguous_target' | 'ambiguous_resource' | 'capacity_exceeded' | 'internal' | 'unavailable' | 'decision_unavailable';
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
  receipt?: Receipt; error?: Failure;
}
export interface LocalAttention { attention: Attention; context?: string; injected: boolean }
export interface Target { node_id: NodeId; base_url: string; headers?: Record<string, string> }
export interface Transport {
  request<T>(target: NodeId, method: string, path: string, body?: unknown, signal?: AbortSignal): Promise<T>;
}
export interface MsgInput { delivery_kind: string; msg: string; meta?: Record<string, Json> }
export type Action =
  | { kind: 'domain'; op: 'create' | 'join' | 'leave' | 'list' | 'describe' | 'discover'; domain_id?: DomainId; expected_revision?: string }
  | { kind: 'send'; to: NodeId; thread?: ThreadRef; context?: string; in_reply_to?: MessageRef; relation?: 'reply' | 'followup'; mentions?: NodeId[]; response: DeliveryPlan['response']; execution: DeliveryPlan['execution'] }
  | { kind: 'read'; scope: 'directory' | 'threads' | 'inbox' | 'messages' | 'message' | 'evidence'; thread?: ThreadRef; message?: MessageRef; seq?: string; cursor?: string; page_size?: number; attention?: 'inbox' | 'thread' }
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
  supports(plan: DeliveryPlan): Promise<boolean>;
  /** Submission returns consumed:false; an observed request boundary may confirm it later. */
  inject(entry: Entry, context?: string, plan?: DeliveryPlan): Promise<{ context: string; consumed: boolean; outcome?: InjectOutcome }>;
  todo?(entry: Entry, context: string): Promise<void>;
  /** The host may drop to an idle cadence; an admission or a settled reply flips it back. */
  wake?(): void;
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

export interface Plugins {
  storage: Storage;
  authenticate(request: Request): Promise<Auth | null>;
  keys?: KeyHook;
  trust_seeds?: TrustSeed[];
  resolve(node: NodeId): Promise<Target>;
  decide(input: MsgInput): Promise<Action>;
  runtime?: Runtime;
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
}
export interface NodeOptions {
  node_id: NodeId; base_url: string; plugins: Plugins;
  instance_id?: string; authentication?: string[];
  request_bytes?: number; retry_max_ms?: number; wait_ms?: number;
  now?: () => number; id?: () => string;
}
