import { OnlyneError } from './core.ts';
import type { ErrorCode, Failure, NodeId, Target, Transport, TrustEntry } from './types.ts';
import type { TrustService } from './keys.ts';

/** The trust reads and signing steps a transport performs; a host that builds its own node wraps the real service. */
export type SigningTrust = Pick<TrustService, 'entry' | 'signRequest' | 'verifyResponse'>;

/**
 * RFC 0017 §3: the pairing act a missing-trust failure names, before any host surface is
 * known. `responsible_node` is the holder who still owes the act — this node when it never
 * asked or must approve, the peer when it owes the answer — and `required_action` is what
 * that holder does. A transport that cannot name its own node leaves `self` and the local
 * cases' `responsible_node` null rather than guessing an identity.
 */
export interface TrustFailure {
  self: NodeId | null;
  peer: NodeId;
  reason: 'no_record' | 'pending' | 'revoked';
  responsible_node: NodeId | null;
  required_action: 'request_exchange' | 'confirm_exchange' | 'restore_trust';
}

/** RFC 0017 request signing: transcripts, response verification, trust-state gating. */
export interface SigningTransportOptions {
  trust: SigningTrust;
  /**
   * Where this host's holder reaches the act a missing-trust failure names. The library knows
   * which node owes the act and what it is; only the host knows its own menu, so the failure
   * carries the host's path instead of one this file would have to invent. The path describes
   * the responsible node's surface: the peer's own menu when that holder owes the answer.
   */
  guidance?(details: TrustFailure): { entry: string[] } | undefined;
}

// Loopback hosts may speak plain HTTP. Everything else reached by a resolver
// could be a remote peer, so it must be HTTPS: a cleartext binding would put
// bearer credentials on the wire.
const LOOPBACK: Record<string, true> = { '127.0.0.1': true, localhost: true, '[::1]': true };

/**
 * The base URL a service request may use: HTTPS anywhere, plain HTTP on loopback only.
 * Public reads (a candidate's service description) use the same rule, so one host cannot
 * downgrade the other.
 */
export function serviceBase(value: string): URL {
  let base: URL;
  try { base = new URL(value); } catch { throw new OnlyneError('unavailable', 'The endpoint is not an absolute URL.'); }
  if (base.protocol !== 'https:' && !(base.protocol === 'http:' && LOOPBACK[base.hostname] === true)) {
    throw new OnlyneError('unavailable', 'Plain HTTP is limited to loopback; cross-host peers must use HTTPS.');
  }
  return base;
}

function isFailure(value: unknown): value is Failure {
  return typeof value === 'object' && value !== null && 'code' in value && 'message' in value && typeof value.code === 'string' && typeof value.message === 'string';
}

// Some bindings answer errors without the envelope when the request never
// reached service code; a bare failure record is still preserved.
function failureRecord(parsed: unknown): unknown {
  return typeof parsed === 'object' && parsed !== null && 'error' in parsed ? parsed.error : parsed;
}

/**
 * RFC 0017 §3: what a signed request hits when this node holds no trusted key for the peer.
 * Both node ids travel in the text and in `details`, because a missing key here says nothing
 * about whether the peer runs. The runtime re-sends the record that stays open, so the code is
 * `unavailable` and the message keeps that retry apart from an agent repeating the send.
 *
 * A pending exchange is not a task for the reader: the invitation is already recorded on the
 * side that owes the approval, and the acquisition policy there submits the approval on the
 * next attempt, or that node's holder confirms it (RFC 0018 §4). The text says that instead of
 * prescribing a menu step, because with policy `all` or `segment` no one has to act. The
 * `no_record` and `revoked` cases keep the host's `guidance` menu, since nothing records them
 * yet and only a holder can start or restore the exchange.
 */
function missingTrust(
  self: NodeId | null, target: NodeId, peer: TrustEntry | undefined,
  guidance: SigningTransportOptions['guidance'],
): OnlyneError {
  const here = self ?? 'this node';
  const retry = 'The runtime re-sends the open record once the pairing exists; do not re-send it yourself.';
  const reason: TrustFailure['reason'] = peer === undefined ? 'no_record' : peer.state === 'revoked' ? 'revoked' : 'pending';
  const required_action: TrustFailure['required_action'] = reason === 'pending' ? 'confirm_exchange' : reason === 'revoked' ? 'restore_trust' : 'request_exchange';
  // A pending row says which holder still owes the act: ours when the peer asked, theirs when we did.
  const responsible_node = reason === 'pending' && peer?.local_approved === true ? target : self;
  const details = { self, peer: target, reason, responsible_node, required_action };
  const guide = guidance?.(details);
  const surface = guide === undefined ? '' : `: ${guide.entry.join(' → ')}`;
  const failure = guide === undefined ? details : { ...details, entry: guide.entry };
  if (reason === 'no_record') {
    return new OnlyneError('unavailable',
      `${here} holds no trust record for ${target}, so this request cannot be signed. This agent cannot grant trust; the holder of ${here} completes the pairing through this node's Onlyne surface${surface}. ${retry}`,
      failure);
  }
  if (reason === 'revoked') {
    return new OnlyneError('unavailable',
      `${here} revoked ${target}, so this request cannot be signed. This agent cannot restore trust; the holder of ${here} pairs the two nodes again through this node's Onlyne surface${surface}. ${retry}`,
      failure);
  }
  const message = peer!.local_approved
    ? `${target} has not answered the exchange ${here} requested, so this request cannot be signed yet. The invitation is recorded at ${target} as pending; the acquisition policy on that node submits the approval on the next attempt, or its holder confirms it${surface}. ${retry}`
    : `${target} requested an exchange with ${here} and this node has not approved it, so this request cannot be signed yet. The invitation is recorded here as pending; this node's acquisition policy submits the approval on the next attempt, or its holder confirms it${surface}. ${retry}`;
  return new OnlyneError('unavailable', message, failure);
}

export class FetchTransport implements Transport {
  private readonly resolve: (node: NodeId) => Promise<Target>;
  private readonly fetcher: typeof fetch;
  private readonly signing: SigningTransportOptions | undefined;

  constructor(resolve: (node: NodeId) => Promise<Target>, fetcher: typeof fetch = fetch, signing?: SigningTransportOptions) {
    this.resolve = resolve;
    this.fetcher = fetcher;
    this.signing = signing;
  }

  async request<T>(target: NodeId, method: string, path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
    const endpoint = await this.resolve(target);
    if (endpoint.node_id !== target) {
      throw new OnlyneError('unavailable', 'The resolver returned a target for another identity.');
    }
    const base = serviceBase(endpoint.base_url);
    // The route is relative to base_url, which already carries the service
    // prefix; a leading slash would silently drop that prefix.
    const url = new URL('./' + path.replace(/^\/+/, ''), base.pathname.endsWith('/') ? base : base.href + '/');
    if (url.origin !== base.origin) {
      throw new OnlyneError('invalid_request', 'The request path leaves the endpoint origin.');
    }

    const headers = new Headers(endpoint.headers);
    const serialized = body === undefined ? '' : JSON.stringify(body);
    if (body !== undefined) {
      headers.set('Content-Type', 'application/json');
      // A server rejects the whole request when this header contradicts the
      // body, so the key travels as a header only from the top-level field the
      // body already carries; the body keeps its own copy.
      if (typeof body === 'object' && body !== null && 'idempotency_key' in body && typeof body.idempotency_key === 'string' && body.idempotency_key !== '') {
        headers.set('Idempotency-Key', body.idempotency_key);
      }
    }
    if (this.signing !== undefined) {
      const peer = await this.signing.trust.entry(target);
      // No trusted key for this peer: the request cannot be signed, and sending it unsigned
      // would be refused anyway. Report a retryable outage so the record stays open while the
      // node holder completes the pairing, instead of recording a permanent refusal. The
      // resolver supplies `Onlyne-Actor`, so the outbound request already names this node.
      if (peer?.state !== 'trusted') {
        throw missingTrust(headers.get('Onlyne-Actor'), target, peer, this.signing.guidance);
      }
      {
        const signer = new Request(url.href, { method, headers });
        await this.signing.trust.signRequest(signer, target, serialized);
        for (const [name, value] of signer.headers) headers.set(name, value);
      }
    }

    let response: Response;
    const request = new Request(url.href, { method, headers });
    try {
      response = await this.fetcher(url, {
        method,
        headers,
        body: body === undefined ? undefined : serialized,
        signal,
      });
    } catch (error) {
      // A consumer's cancellation is not a reachability fault; the caller sees
      // its own abort reason instead of a retryable outage.
      if (signal?.aborted) throw signal.reason;
      throw new OnlyneError('unavailable', 'The target could not be reached.');
    }

    let text: string;
    try {
      text = await response.text();
    } catch {
      throw new OnlyneError('unavailable', 'The response body could not be read.');
    }
    let parsed: unknown;
    try {
      parsed = text === '' ? undefined : JSON.parse(text);
    } catch {
      parsed = undefined;
    }

    if (response.ok) {
      if (parsed === undefined) throw new OnlyneError('unavailable', 'The target returned a malformed response.');
      // A signed request is only ever sent to a trusted peer, so its answer must prove that
      // identity; `verifyResponse` fails when the row is missing, untrusted or the signature
      // does not cover this request's nonce.
      if (this.signing !== undefined) await this.signing.trust.verifyResponse(target, request, response, text);
      return parsed as T;
    }

    const record = failureRecord(parsed);
    // A signed request is answered by a signed response even when it fails; an unsigned or
    // forged error must not be read as the peer's refusal.
    if (this.signing !== undefined) await this.signing.trust.verifyResponse(target, request, response, text);
    if (isFailure(record)) {
      const rejection = new OnlyneError(record.code as ErrorCode, record.message);
      if (typeof record.retryable === 'boolean') rejection.error.retryable = record.retryable;
      if (record.details !== undefined) rejection.error.details = record.details;
      throw rejection;
    }
    throw new OnlyneError(response.status >= 500 ? 'unavailable' : 'internal', `The target answered HTTP ${response.status} without a failure record.`);
  }
}
