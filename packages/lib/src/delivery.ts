import { canonical, decimal, key } from './core.ts';
import type { Attention, Entry, Failure, Message, NodeId, Outbound, SendRequest, ThreadRef } from './types.ts';

/** Result of one delivery: `admission` is what the peer recorded, `wait` is what this caller observed. */
export interface SendOutcome {
  receipt?: Outbound['receipt'];
  admission: Outbound['state'];
  reply?: Entry;
  /** The target as the caller spelled it, present only with `resolved` when the two differ. */
  requested?: string;
  /**
   * The node `requested` resolved to. Present whenever the addressee differs from the
   * endpoint that stores the entry — which is this node whenever it started the
   * conversation — so an initiator's receipt never reads as a message the node sent itself.
   */
  resolved?: NodeId;
  /**
   * Whether the reply body is safe to render. `withheld` means the same body is already
   * confirmed consumed in this context through another path (RFC 0008 §5), so the caller
   * renders the arrival facts and the original message reference only.
   */
  reply_body?: 'included' | 'withheld';
  wait?: 'answered' | 'timed_out' | 'cancelled';
  error?: Failure;
}

export interface SendOptions {
  signal?: AbortSignal;
  retry_until?: string;
  idempotency_key?: string;
  expected_author?: string;
  /**
   * The caller's own spelling of the addressee. Recorded on the outbound record beside the
   * resolved node id when the two differ, so the record shows both what was asked and what
   * was resolved (RFC 0005 §4: the receipt reports the landing, not the request).
   */
  requested?: string;
  /**
   * The sender's own consumption identity for the awaited reply (RFC 0008 §5). It stays
   * local and never stands in for the wire plan's receiver-side context.
   */
  context?: string;
}

export interface MsgOptions {
  signal?: AbortSignal;
  /** The sender's own consumption identity for an awaited reply; `msg` passes it to `send`. */
  context?: string;
}

/** An outbound record plus the digest that fixes its message identity and the sender's own consumption identity. */
export interface StoredOutbound extends Outbound { digest?: string; local_context?: string }

/** Confirmed consumption for one slot, written by whichever path the host confirmed. */
export interface ConsumptionRecord {
  context: string; thread_ref: ThreadRef; seq: string;
  confirmed: boolean; via: 'tool_result' | 'fragment'; at: string;
}

/** A todo the host still owes for one consumed entry; `done` is written after its hook succeeds. */
export interface PendingTodo { entry: Entry; context: string; done: boolean }

/**
 * The bounded retry series for one known owner thread reconciliation could not pull. `dead`
 * is terminal: a thread is only asked again after its owner answers on another path.
 */
export interface ReconcileBackoff {
  owner: NodeId; thread: string; attempt: number; next_attempt: string;
  code: Failure['code']; message: string; dead?: boolean;
}

/** When this node last asked one trusted owner for the memberships it holds here. */
export interface GrantResyncMark { peer: NodeId; at: number }

/** Codes the peer records as a terminal refusal before acceptance (RFC 0003 §5). */
const TERMINAL: Record<string, true> = {
  invalid_request: true, forbidden: true, not_a_member: true, not_found: true,
  id_conflict: true, revision_conflict: true, conflict: true, thread_closed: true,
  retry_window_expired: true, payload_too_large: true, unsupported_media_type: true,
  unsupported_delivery: true, ambiguous_target: true, ambiguous_resource: true,
};
export const MAX_CURSOR_PAGES = 10_000;
/** A binding check must not hold the maintenance pass behind an unreachable resource owner. */
export const RESOURCE_CHECK_MS = 5_000;
/** One failure line per scope per window; a dead owner stays observable without flooding. */
export const FAILURE_WINDOW_MS = 300_000;
/** A trusted owner is asked for the memberships it holds for this node at most this often. */
export const GRANT_RESYNC_MS = 300_000;
/** One retry series for an unreachable peer: doubling from 24s, capped at 15 minutes. */
const RETRY_BASE_MS = 24_000;
const RETRY_MAX_MS = 900_000;
/** Attempts one series makes before its record becomes a dead letter. */
export const MAX_ATTEMPTS = 10;

/** The delay before attempt number `attempt` (1-based) of one series. */
export function backoffMs(attempt: number): number {
  return Math.min(RETRY_BASE_MS * 2 ** Math.max(0, attempt - 1), RETRY_MAX_MS);
}

/** Anything that is not a recorded refusal leaves the admission unknown, never "undelivered". */
export function admissionOf(failure: Failure): Outbound['state'] {
  return failure.retryable === false && TERMINAL[failure.code] === true ? 'refused' : 'unknown';
}

export function failureOf(error: unknown): Failure {
  const reported = reportedFailure(error);
  if (reported !== undefined) return reported;
  return {
    code: 'unavailable',
    message: error instanceof Error ? error.message : 'The transport failed without a response.',
    retryable: true,
  };
}

/** The failure record a thrown `OnlyneError` carries, read only when both fields are really there. */
function reportedFailure(error: unknown): Failure | undefined {
  if (typeof error !== 'object' || error === null || !('error' in error)) return undefined;
  const value = error.error;
  if (typeof value !== 'object' || value === null) return undefined;
  if (!('code' in value) || typeof value.code !== 'string') return undefined;
  if (!('message' in value) || typeof value.message !== 'string') return undefined;
  // The binding builds these from the ErrorCode union before throwing.
  const reported = value as Failure;
  return reported;
}

/**
 * Request identity for one delivery (RFC 0003 §3 step 3). The plan, the local
 * idempotency key and `retry_until` describe delivery, not the message.
 */
export function messageDigest(request: SendRequest): string {
  const meta = request.message.meta;
  return canonical({
    sender: request.message.sender,
    target_endpoint: request.target_endpoint,
    work: meta?.work ?? null,
    thread_ref: meta?.thread ?? null,
    in_reply_to: meta?.in_reply_to ?? null,
    relation: meta?.relation ?? null,
    mentions: meta?.mentions ?? null,
    delivery_kind: request.message.delivery_kind,
    msg: request.message.msg,
    parts: meta?.parts ?? null,
    data: meta?.data ?? null,
  });
}

/** A legitimate reply to `original`: same reference, reply relation, a different authenticated author. */
export function isReply(original: Pick<Message, 'sender' | 'id'>, entry: Entry): boolean {
  const meta = entry.message.meta;
  const related = meta?.in_reply_to;
  if (related === undefined || related.sender !== original.sender || related.id !== original.id) return false;
  if (meta?.relation !== undefined && meta.relation !== 'reply') return false;
  return entry.authenticated_author !== original.sender;
}

/** Replay order is the thread's own: `(owner, id)` then `seq` as a number, never a UUID sort. */
export function replayOrder(a: Entry | undefined, b: Entry | undefined): number {
  if (a === undefined) return b === undefined ? 0 : -1;
  if (b === undefined) return 1;
  if (a.thread_ref.owner !== b.thread_ref.owner) return a.thread_ref.owner < b.thread_ref.owner ? -1 : 1;
  if (a.thread_ref.id !== b.thread_ref.id) return a.thread_ref.id < b.thread_ref.id ? -1 : 1;
  const left = decimal(a.seq);
  const right = decimal(b.seq);
  return left < right ? -1 : left > right ? 1 : 0;
}

export const outboundKey = (sender: string, id: string) => key('outbound', sender, id);
export const inboxKey = (obligation_id: string) => key('inbox', obligation_id);
export const replyKey = (original: Pick<Message, 'sender' | 'id'>) => key('reply', original.sender, original.id);
export const reconcileBackoffKey = (ref: ThreadRef) => key('reconcile-backoff', ref.owner, ref.id);
export const grantResyncKey = (peer: NodeId) => key('grant-resync', peer);
/** One consumption slot per `(context, ThreadRef, seq)`, shared by every delivery path (RFC 0004 §5). */
export const consumptionKey = (context: string, thread_ref: ThreadRef, seq: string) => key('consumed', context, thread_ref.owner, thread_ref.id, seq);
/** The todo one consumed entry owes the host, until its hook succeeds. */
export const todoKey = (context: string, thread_ref: ThreadRef, seq: string) => key('todo', context, thread_ref.owner, thread_ref.id, seq);
/** In-process identity of one thread entry: its `ThreadRef` and sequence. */
export const entryKey = (thread_ref: ThreadRef, seq: string) => key(thread_ref.owner, thread_ref.id, seq);

/** The durable part of an obligation: identity and owner-attested facts, body optional. */
export function attentionOf(entry: Entry, obligation_id: string, recipient: string): Attention {
  return {
    obligation_id, recipient, thread_ref: entry.thread_ref, seq: entry.seq,
    message_ref: { sender: entry.message.sender, id: entry.message.id },
    authenticated_author: entry.authenticated_author, acked: false, entry,
  };
}

