import { OnlyneError } from './core.js';
import type { ErrorCode, Failure, NodeId, Target, Transport } from './types.js';

// Loopback hosts may speak plain HTTP. Everything else reached by a resolver
// could be a remote peer, so it must be HTTPS: a cleartext binding would put
// bearer credentials on the wire.
const LOOPBACK: Record<string, true> = { '127.0.0.1': true, localhost: true, '[::1]': true };

function isFailure(value: unknown): value is Failure {
  return typeof value === 'object' && value !== null && 'code' in value && 'message' in value && typeof value.code === 'string' && typeof value.message === 'string';
}

// Some bindings answer errors without the envelope when the request never
// reached service code; a bare failure record is still preserved.
function failureRecord(parsed: unknown): unknown {
  return typeof parsed === 'object' && parsed !== null && 'error' in parsed ? parsed.error : parsed;
}

export class FetchTransport implements Transport {
  private readonly resolve: (node: NodeId) => Promise<Target>;
  private readonly fetcher: typeof fetch;

  constructor(resolve: (node: NodeId) => Promise<Target>, fetcher: typeof fetch = fetch) {
    this.resolve = resolve;
    this.fetcher = fetcher;
  }

  async request<T>(target: NodeId, method: string, path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
    const endpoint = await this.resolve(target);
    if (endpoint.node_id !== target) {
      throw new OnlyneError('unavailable', 'The resolver returned a target for another identity.');
    }
    const base = new URL(endpoint.base_url);
    if (base.protocol !== 'https:' && !(base.protocol === 'http:' && LOOPBACK[base.hostname] === true)) {
      throw new OnlyneError('unavailable', 'Plain HTTP is limited to loopback; cross-host peers must use HTTPS.');
    }
    // The route is relative to base_url, which already carries the service
    // prefix; a leading slash would silently drop that prefix.
    const url = new URL('./' + path.replace(/^\/+/, ''), base.pathname.endsWith('/') ? base : base.href + '/');
    if (url.origin !== base.origin) {
      throw new OnlyneError('invalid_request', 'The request path leaves the endpoint origin.');
    }

    const headers = new Headers(endpoint.headers);
    if (body !== undefined) {
      headers.set('Content-Type', 'application/json');
      // A server rejects the whole request when this header contradicts the
      // body, so the key travels as a header only from the top-level field the
      // body already carries; the body keeps its own copy.
      if (typeof body === 'object' && body !== null && 'idempotency_key' in body && typeof body.idempotency_key === 'string' && body.idempotency_key !== '') {
        headers.set('Idempotency-Key', body.idempotency_key);
      }
    }

    let response: Response;
    try {
      response = await this.fetcher(url, {
        method,
        headers,
        body: body === undefined ? undefined : JSON.stringify(body),
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
      return parsed as T;
    }

    const record = failureRecord(parsed);
    if (isFailure(record)) {
      const rejection = new OnlyneError(record.code as ErrorCode, record.message);
      if (typeof record.retryable === 'boolean') rejection.error.retryable = record.retryable;
      if (record.details !== undefined) rejection.error.details = record.details;
      throw rejection;
    }
    throw new OnlyneError(response.status >= 500 ? 'unavailable' : 'internal', `The target answered HTTP ${response.status} without a failure record.`);
  }
}
