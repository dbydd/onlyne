/**
 * The HTTP boundary: every platform call the connector makes goes through this port, so the
 * translation logic runs against a fake in tests and the real QQ OpenAPI in production. The only
 * injected seams are `fetch` and `now`.
 */

export const QQ_API_BASE = 'https://api.bot.qq.com';

/** The token is refreshed this long before the platform's own expiry. */
const TOKEN_REFRESH_WINDOW_MS = 60_000;
/** What the platform answers when it omits `expires_in`; QQ documents 7200 seconds. */
const DEFAULT_TOKEN_LIFETIME_SECONDS = 7200;

export interface QqHttpRequest {
  method: string;
  headers: Record<string, string>;
  body?: string;
}

export interface QqHttpResponse {
  readonly status: number;
  text(): Promise<string>;
}

export type QqFetch = (url: string, init: QqHttpRequest) => Promise<QqHttpResponse>;

export interface QqPortErrorInit {
  /** The HTTP status; `0` when the request never reached the platform. */
  status: number;
  /** The platform's own error code, when its body carried one. */
  code?: string | number;
  /** The platform's own error text, verbatim. */
  detail?: string;
}

/** A platform refusal or a transport failure, with what the platform said kept apart from the summary. */
export class QqPortError extends Error {
  readonly status: number;
  readonly code?: string | number;
  readonly detail?: string;

  constructor(message: string, init: QqPortErrorInit) {
    super(message);
    this.name = 'QqPortError';
    this.status = init.status;
    if (init.code !== undefined) this.code = init.code;
    if (init.detail !== undefined) this.detail = init.detail;
  }
}

/** A QQ conversation coordinate as the platform addresses it. */
export interface QqSendTarget {
  kind: 'c2c' | 'group';
  openid: string;
}

/** The message body QQ v2 accepts; the connector fills only what it sends. */
export interface QqMessageBody {
  msg_type: number;
  content?: string;
  markdown?: { content: string };
  /** The passive reply id from the inbound event. */
  msg_id?: string;
  msg_seq?: number;
  message_reference?: { message_id: string };
  input_notify?: { input_type: 1; input_second: number };
}

export interface QqSendResult {
  id?: string;
  timestamp?: string;
  ext_info?: { ref_idx?: string };
}

export interface QqPortStatus {
  state: string;
  connected: boolean;
  rate_limits: number;
}

export interface QqPort {
  start(): Promise<void>;
  stop(): Promise<void>;
  /** The access token, fetched on demand and cached until shortly before it expires. */
  token(): Promise<string>;
  request<T>(method: string, path: string, body?: unknown): Promise<T>;
  send(target: QqSendTarget, body: QqMessageBody): Promise<QqSendResult>;
  status(): QqPortStatus;
}

export interface QqPortOptions {
  appId: string;
  appSecret: string;
  baseUrl?: string;
  fetch?: QqFetch;
  now?: () => number;
}

/** The message endpoint for one conversation; `messageId` narrows it to that message (retract). */
export function messagePath(target: QqSendTarget, messageId?: string): string {
  const scope = target.kind === 'group' ? 'groups' : 'users';
  const base = `/v2/${scope}/${encodeURIComponent(target.openid)}/messages`;
  return messageId === undefined ? base : `${base}/${encodeURIComponent(messageId)}`;
}

function recordOf(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

function codeOf(body: unknown): string | number | undefined {
  const code = recordOf(body)?.code;
  if (typeof code === 'number') return code;
  if (typeof code === 'string' && /^\d+$/.test(code)) return Number(code);
  return undefined;
}

function messageOf(body: unknown): string | undefined {
  const record = recordOf(body);
  const message = record?.message ?? record?.errmsg;
  return typeof message === 'string' && message.length > 0 ? message : undefined;
}

function detailOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function createQqPort(options: QqPortOptions): QqPort {
  const baseUrl = (options.baseUrl ?? QQ_API_BASE).replace(/\/+$/, '');
  const fetchImpl: QqFetch = options.fetch ?? ((url, init) => globalThis.fetch(url, init));
  const now = options.now ?? Date.now;

  let cached: { token: string; expiresAt: number } | undefined;
  let inflight: Promise<string> | undefined;
  let state = 'idle';
  let rateLimits = 0;

  /**
   * A 2xx body can still be a refusal: the QQ error envelope is `{code, message}` and some
   * endpoints answer it with HTTP 200, so a non-zero `code` is read as a rejection.
   */
  function rejection(method: string, path: string, status: number, body: unknown, raw: string): QqPortError {
    const code = codeOf(body);
    const detail = messageOf(body) ?? (raw.trim().length > 0 ? raw.trim() : 'no body');
    const label = code === undefined ? detail : `${code} ${detail}`;
    const summary = status >= 500
      ? `The QQ platform failed ${method} ${path} with HTTP ${status}: ${label}`
      : `The platform rejected ${method} ${path}: ${label}`;
    return new QqPortError(summary, { status, ...(code === undefined ? {} : { code }), detail });
  }

  async function call(method: string, path: string, body: unknown, authorized: boolean): Promise<unknown> {
    const headers: Record<string, string> = { 'content-type': 'application/json' };
    if (authorized) headers.authorization = `QQBot ${await token()}`;
    const init: QqHttpRequest = { method, headers, ...(body === undefined ? {} : { body: JSON.stringify(body) }) };
    let response: QqHttpResponse;
    try {
      response = await fetchImpl(`${baseUrl}${path}`, init);
    } catch (error) {
      throw new QqPortError(`The QQ request ${method} ${path} did not complete: ${detailOf(error)}`, { status: 0 });
    }
    const raw = await response.text();
    if (response.status === 429) rateLimits += 1;
    let parsed: unknown;
    try {
      parsed = raw.length === 0 ? undefined : JSON.parse(raw);
    } catch {
      parsed = undefined;
    }
    if (response.status < 200 || response.status >= 300) {
      throw rejection(method, path, response.status, parsed, raw);
    }
    const code = codeOf(parsed);
    if (code !== undefined && code !== 0) throw rejection(method, path, response.status, parsed, raw);
    return parsed ?? {};
  }

  async function fetchToken(): Promise<string> {
    try {
      const body = await call(
        'POST',
        '/app/getAppAccessToken',
        { appId: options.appId, clientSecret: options.appSecret },
        false,
      );
      const record = recordOf(body);
      const token = record?.access_token;
      if (typeof token !== 'string' || token.length === 0) {
        throw new QqPortError('The QQ token endpoint answered without an access_token.', { status: 200 });
      }
      const lifetime = Number(record?.expires_in);
      const seconds = Number.isFinite(lifetime) && lifetime > 0 ? lifetime : DEFAULT_TOKEN_LIFETIME_SECONDS;
      cached = { token, expiresAt: now() + seconds * 1000 };
      state = 'connected';
      return token;
    } catch (error) {
      state = 'error';
      throw error;
    }
  }

  async function token(): Promise<string> {
    if (cached !== undefined && now() < cached.expiresAt - TOKEN_REFRESH_WINDOW_MS) return cached.token;
    if (inflight !== undefined) return inflight;
    const pending = fetchToken();
    inflight = pending;
    try {
      return await pending;
    } finally {
      if (inflight === pending) inflight = undefined;
    }
  }

  return {
    async start(): Promise<void> {
      // No long connection to open: QQ delivers events to the webhook, and the token is fetched
      // on the first authorized call.
      state = cached === undefined ? 'idle' : 'connected';
    },

    async stop(): Promise<void> {
      cached = undefined;
      inflight = undefined;
      state = 'stopped';
    },

    token,

    async request<T>(method: string, path: string, body?: unknown): Promise<T> {
      return await call(method, path, body, true) as T;
    },

    async send(target: QqSendTarget, body: QqMessageBody): Promise<QqSendResult> {
      return await call('POST', messagePath(target), body, true) as QqSendResult;
    },

    status(): QqPortStatus {
      return { state, connected: state === 'connected', rate_limits: rateLimits };
    },
  };
}
