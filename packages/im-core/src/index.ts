import type { Json } from '@onlyne/lib';
export { createDriverBridge, type DriverBridgeOptions } from './bridge.js';

/** One platform attachment: either materialized to a local path or kept as a platform handle. */
export interface ImAttachment {
  kind: 'image' | 'file' | 'audio' | 'video' | 'sticker';
  /** Present when the attachment was materialized into the workspace (RFC 0012 §9). */
  path?: string;
  /** The platform's own handle (file_id, image_key, url), kept for re-download and diagnostics. */
  platform_handle: string;
  alt?: string;
}

/** How the platform spells a reply: the three-part coordinate carried over from v2 payload.rs. */
export interface ImReply {
  /** Who this message answers, in the platform's message id. */
  parent?: string;
  /** The discussion anchor the platform reports, when distinct from parent. */
  root?: string;
  /** A platform-independent thread handle, when the platform has one. */
  thread?: string;
  /** The platform inlined the quoted body into the event (Telegram does). */
  quoted_text?: string;
}

/** One inbound platform event, stripped of the platform envelope. */
export type ImEventKind = 'message' | 'action' | 'event';

export interface ImEvent {
  platform: string;
  /** The platform event id: the deduplication key `receive` is called with. */
  source_id: string;
  /** The platform conversation coordinate (lark `oc_`/`ou_`, qq `group:`/`c2c:`). */
  conversation: string;
  kind: ImEventKind;
  /** The platform sender id, when the event carries one. */
  from?: string;
  text: string;
  attachments: ImAttachment[];
  reply?: ImReply;
  /** The platform action id, for kind === 'action' events. */
  action?: { id: string; value?: Json };
  /**
   * The platform lifecycle event, for kind === 'event': a recall, a reaction, the bot joining
   * a chat. These carry no message body; `text` stays empty.
   */
  event?: { type: string; data?: Json };
  /** This event's own platform message handle: what a later reply must quote. */
  message_handle?: string;
  /** The handle a later reply must quote when the platform keeps two ids for one message (QQ). */
  ref_handle?: string;
  at: string;
  /** The platform's own JSON, for diagnostics only. */
  raw?: Json;
}

/** An outbound request, already resolved to platform handles by the driver bridge. */
export interface SendRequest {
  conversation: string;
  text: string;
  format: 'plain' | 'markdown';
  attachments: Array<{ kind: ImAttachment['kind']; path: string; alt?: string }>;
  /** The platform message handle to answer, already translated from `reply_to_event`. */
  reply_to?: string;
  /** Ask the platform to answer inside a thread rather than beside the parent. */
  reply_in_thread?: boolean;
  /** An interactive card, when the platform has one; `text` may then be empty. */
  card?: Json;
  /** The operation id: the platform idempotency key, where the platform takes one. */
  request_id?: string;
}

/**
 * The platform outcome of one send. No `aborted` here: the bridge decides `aborted` from an
 * explicit platform rejection (`error` set); the connector only reports what it observed.
 */
export interface SendOutcome {
  state: 'committed' | 'unknown';
  message_id?: string;
  ref_handle?: string;
  /** The platform's own rejection text, verbatim. */
  error?: string;
  /** Segments that did not land while the head did, and attachments the platform cannot upload. */
  notes?: string[];
}

/**
 * One platform connector: translation only. Deduplication, cursor advance, and binding live in
 * the endpoint's `ResourceService`; a connector that kept any of those would be a second state
 * machine (RFC 0012 §4).
 */
export interface Connector {
  readonly platform: string;
  /** Starts the platform connection. Missing credentials are a state, not an error. */
  start(): Promise<void>;
  /**
   * The inbound event stream. The connector reconnects itself; duplicates are safe. A listener
   * may return a promise: the connector serializes events per conversation through it, so the
   * endpoint's `receive` calls land in arrival order.
   */
  onEvent(listener: (event: ImEvent) => void | Promise<void>): void;
  /** Sends one message; the resolved outcome carries the platform's three-way answer. */
  send(request: SendRequest): Promise<SendOutcome>;
  /** Platform operations beyond `send` that this connector implements (RFC 0012 §3). */
  readonly operations?: readonly string[];
  /** Validates one operation's data; the bridge calls it before claiming the operation id. */
  validate?(operation: string, data: Json): void;
  /** Runs one operation from `operations`; `send` never arrives here. */
  operate?(operation: string, data: Json): Promise<SendOutcome>;
  stop(): Promise<void>;
}

/**
 * Credential resolution, carried over from the v2 chain (archive/crates/onlyne-im-backend/src/credential.rs:67-191):
 * explicit value, then the credential file the operator points at (`$VAR` references resolved from
 * the environment), then the platform's conventional environment variable pair joined with `:`.
 * A missing credential is a state: the connector starts unauthenticated and reports onboarding.
 */
export interface CredentialSource {
  /** An explicit value, from configuration. */
  readonly value?: string;
  /** A path to a credential file; contents may reference `$NAME` environment variables. */
  readonly file?: string;
  /** Environment variable names to consult, in order; pairs join with `:`. */
  readonly variables?: string[];
}

export interface ResolvedCredential {
  value: string | undefined;
  /** Present when no credential was found: what the operator should provide. */
  problem?: string;
}

export async function resolveCredential(
  source: CredentialSource,
  readFile: (path: string) => Promise<string>,
  env: Record<string, string | undefined> = {},
): Promise<ResolvedCredential> {
  if (source.value !== undefined) return { value: source.value };
  if (source.file !== undefined) {
    let text: string;
    try {
      text = await readFile(source.file);
    } catch {
      return { value: undefined, problem: `The credential file is unreadable: ${source.file}` };
    }
    return { value: expandReferences(text.trim(), env) };
  }
  if (source.variables !== undefined && source.variables.length > 0) {
    const pair = source.variables.map(name => env[name]);
    if (pair.every(part => part !== undefined && part.length > 0)) return { value: pair.join(':') };
    if (pair.length === 1 && pair[0] !== undefined && pair[0].length > 0) return { value: pair[0]! };
  }
  return { value: undefined, problem: `Provide a credential via ${source.variables?.join(' + ') ?? 'configuration'}.` };
}

/** `$NAME` resolves from the environment; `$$` is a literal dollar (v2 credential.rs:152-166). */
export function expandReferences(text: string, env: Record<string, string | undefined>): string {
  return text.replace(/\$(\$|[A-Za-z_][A-Za-z0-9_]*)/g, (match, name: string) => {
    if (name === '$') return '$';
    const resolved = env[name];
    return resolved === undefined ? match : resolved;
  });
}
