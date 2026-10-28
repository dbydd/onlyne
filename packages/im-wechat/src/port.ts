/**
 * The iLink Bot API boundary: every platform call the connector makes goes through this interface,
 * so the connector's translation logic runs against a fake in tests and the real HTTP API in
 * production. Primary source: `developers.weixin.qq.com/doc/aispeech/knowledge/openapi/Clawbotrelated.*`
 * and `Tencent/openclaw-weixin` `docs/protocol.md`.
 */

export const WECHAT_DEFAULT_BASE_URL = 'https://ilinkai.weixin.qq.com';

/** `iLink-App-ClientVersion`: 0x00010000, spelled as the decimal string the API takes. */
const CLIENT_VERSION = 65536;
const UINT32 = 0x1_0000_0000;
const DEFAULT_CHANNEL_VERSION = '3.0.0';
const DEFAULT_BOT_AGENT = 'Onlyne';
/** A long poll holds the request for about 35 s; this guards a call that never answers at all. */
const DEFAULT_TIMEOUT_MS = 60_000;

/** One media reference inside a message item; `full_url` is pre-signed, the query param is not. */
export interface WechatMediaRef {
  full_url?: string;
  encrypt_query_param?: string;
  aes_key?: string;
}

export interface WechatMessageItem {
  /** 1 text, 2 image, 3 voice, 4 file, 5 video. */
  type?: number;
  msg_id?: string;
  ref_msg?: { msg_id?: string; [key: string]: unknown };
  text_item?: { text?: string };
  image_item?: WechatMediaRef;
  voice_item?: WechatMediaRef & { text?: string };
  file_item?: WechatMediaRef & { file_name?: string; len?: number };
  video_item?: WechatMediaRef;
}

export interface WechatMessage {
  seq?: number;
  message_id?: number | string;
  from_user_id?: string;
  to_user_id?: string;
  client_id?: string;
  create_time_ms?: number;
  session_id?: string;
  group_id?: string;
  /** 1 user message, 2 the bot's own outbound. */
  message_type?: number;
  message_state?: number;
  item_list?: WechatMessageItem[];
  context_token?: string;
  run_id?: string;
}

export interface WechatUpdatesResponse {
  ret: number;
  errcode?: number;
  errmsg?: string;
  msgs?: WechatMessage[];
  get_updates_buf?: string;
  longpolling_timeout_ms?: number;
}

export interface WechatSendResponse {
  ret: number;
  errcode?: number;
  errmsg?: string;
}

export interface WechatConfigResponse {
  ret: number;
  errcode?: number;
  errmsg?: string;
  typing_ticket?: string;
}

/** The `msg` envelope of `sendmessage`; the port adds `base_info` around it. */
export interface WechatOutboundMessage {
  from_user_id: string;
  to_user_id: string;
  client_id: string;
  message_type: number;
  message_state: number;
  context_token?: string;
  item_list: Array<{ type: number; text_item: { text: string } }>;
}

/**
 * One iLink answer that did not succeed. `ret`/`errmsg` are the platform's own rejection (it
 * answered, so the operation is settled); without them the call never reached the platform and
 * the caller cannot tell whether it landed.
 */
export class WechatPortError extends Error {
  readonly status: number | undefined;
  readonly ret: number | undefined;
  readonly errcode: number | undefined;
  readonly errmsg: string | undefined;
  readonly retryable: boolean;
  /** The caller's signal ended the call: a normal end of polling, not a fault. */
  readonly aborted: boolean;
  /** `ret` or `errcode` is -14: the bot token's session is gone until the QR is scanned again. */
  readonly sessionExpired: boolean;

  constructor(message: string, options: {
    status?: number;
    ret?: number;
    errcode?: number;
    errmsg?: string;
    retryable?: boolean;
    aborted?: boolean;
    cause?: unknown;
  } = {}) {
    super(message, options.cause === undefined ? undefined : { cause: options.cause });
    this.name = 'WechatPortError';
    this.status = options.status;
    this.ret = options.ret;
    this.errcode = options.errcode;
    this.errmsg = options.errmsg;
    this.retryable = options.retryable ?? false;
    this.aborted = options.aborted ?? false;
    this.sessionExpired = options.ret === -14 || options.errcode === -14;
  }
}

/**
 * The platform calls this connector makes. `getUpdates` takes the caller's abort signal because
 * the long poll is the one call a shutdown interrupts; `rateLimits` is the host's observation of
 * HTTP 429 answers, counted here because this is where they arrive.
 */
export interface WechatPort {
  getUpdates(cursor: string, signal?: AbortSignal): Promise<WechatUpdatesResponse>;
  sendMessage(message: WechatOutboundMessage): Promise<WechatSendResponse>;
  getConfig(userId: string, contextToken?: string): Promise<WechatConfigResponse>;
  sendTyping(userId: string, typingTicket: string, status: 1 | 2): Promise<void>;
  /** Lifecycle: a failure is logged by the caller, never fatal. */
  notifyStart(): Promise<void>;
  notifyStop(): Promise<void>;
  rateLimits(): number;
}

export interface WechatPortOptions {
  /** The bot token from the ClawBot QR login. */
  token: string;
  baseUrl?: string;
  fetch?: (input: string, init: RequestInit) => Promise<Response>;
  channelVersion?: string;
  botAgent?: string;
  /** `X-WECHAT-UIN` is a random uint32 per call; injected so a test can pin it. */
  random?: () => number;
  /** 0 disables the stall guard. */
  timeoutMs?: number;
}

export function detailOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function createWechatPort(options: WechatPortOptions): WechatPort {
  const baseUrl = (options.baseUrl ?? WECHAT_DEFAULT_BASE_URL).replace(/\/+$/, '');
  const fetchImpl = options.fetch ?? ((input: string, init: RequestInit) => fetch(input, init));
  const random = options.random ?? Math.random;
  const timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS;
  let rateLimits = 0;

  const uin = (): string => {
    const draw = Math.floor(Math.abs(random()) * UINT32) % UINT32;
    return Buffer.from(String(draw), 'utf8').toString('base64');
  };

  const headers = (): Record<string, string> => ({
    'Content-Type': 'application/json',
    AuthorizationType: 'ilink_bot_token',
    Authorization: `Bearer ${options.token}`,
    'X-WECHAT-UIN': uin(),
    'iLink-App-Id': 'bot',
    'iLink-App-ClientVersion': String(CLIENT_VERSION),
  });

  const baseInfo = (): { channel_version: string; bot_agent: string } => ({
    channel_version: options.channelVersion ?? DEFAULT_CHANNEL_VERSION,
    bot_agent: options.botAgent ?? DEFAULT_BOT_AGENT,
  });

  /**
   * One authenticated POST. `call` names the call in the error text, because the platform's
   * `errmsg` answers "what did the platform say" and never says which call asked.
   */
  async function post<T>(path: string, call: string, body: unknown, signal?: AbortSignal): Promise<T> {
    const guard = timeoutMs > 0 ? AbortSignal.timeout(timeoutMs) : undefined;
    const combined = signal !== undefined && guard !== undefined
      ? AbortSignal.any([signal, guard])
      : (signal ?? guard);

    let response: Response;
    try {
      response = await fetchImpl(`${baseUrl}${path}`, {
        method: 'POST',
        headers: headers(),
        body: JSON.stringify(body),
        ...(combined === undefined ? {} : { signal: combined }),
      });
    } catch (failure) {
      if (signal?.aborted === true) {
        throw new WechatPortError('The platform call was aborted.', { aborted: true, retryable: true, cause: failure });
      }
      if (guard?.aborted === true) {
        throw new WechatPortError(`The platform call failed: ${call} made no answer within ${timeoutMs} ms.`, { retryable: true, cause: failure });
      }
      throw new WechatPortError(`The platform call failed: ${detailOf(failure)}`, { retryable: true, cause: failure });
    }

    const text = await response.text();
    if (response.status === 429) {
      rateLimits += 1;
      throw new WechatPortError(`The platform rate-limited ${call}: 429 ${text.trim()}`, { status: 429, retryable: true });
    }
    if (!response.ok) {
      throw new WechatPortError(`The platform call failed: ${response.status} ${text.trim()}`, {
        status: response.status,
        retryable: response.status >= 500,
      });
    }

    let parsed: unknown;
    try {
      parsed = JSON.parse(text);
    } catch {
      throw new WechatPortError(`The platform call failed: ${response.status} returned a body that is not JSON.`, { status: response.status });
    }
    if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
      throw new WechatPortError(`The platform call failed: ${response.status} returned no JSON object.`, { status: response.status });
    }

    const record = parsed as Record<string, unknown>;
    const ret = typeof record.ret === 'number' ? record.ret : 0;
    const errcode = typeof record.errcode === 'number' ? record.errcode : undefined;
    const errmsg = typeof record.errmsg === 'string' && record.errmsg.length > 0 ? record.errmsg : undefined;
    if (ret !== 0 || (errcode !== undefined && errcode !== 0)) {
      const reason = errmsg ?? `ret ${ret}${errcode === undefined || errcode === ret ? '' : ` errcode ${errcode}`}`;
      throw new WechatPortError(`The platform rejected ${call}: ${reason}`, {
        status: response.status,
        ret,
        ...(errcode === undefined ? {} : { errcode }),
        ...(errmsg === undefined ? {} : { errmsg }),
      });
    }
    return parsed as T;
  }

  return {
    async getUpdates(cursor, signal) {
      try {
        return await post<WechatUpdatesResponse>('/ilink/bot/getupdates', 'the poll call', { get_updates_buf: cursor, base_info: baseInfo() }, signal);
      } catch (failure) {
        if (failure instanceof WechatPortError && failure.aborted) return { ret: 0, msgs: [] };
        throw failure;
      }
    },

    async sendMessage(message) {
      return post<WechatSendResponse>('/ilink/bot/sendmessage', 'the send', { msg: message, base_info: baseInfo() });
    },

    async getConfig(userId, contextToken) {
      return post<WechatConfigResponse>('/ilink/bot/getconfig', 'the getconfig call', {
        ilink_user_id: userId,
        ...(contextToken === undefined ? {} : { context_token: contextToken }),
        base_info: baseInfo(),
      });
    },

    async sendTyping(userId, typingTicket, status) {
      await post<WechatSendResponse>('/ilink/bot/sendtyping', 'the sendtyping call', {
        ilink_user_id: userId,
        typing_ticket: typingTicket,
        status,
        base_info: baseInfo(),
      });
    },

    async notifyStart() {
      await post<WechatSendResponse>('/ilink/bot/msg/notifystart', 'the notifystart call', { base_info: baseInfo() });
    },

    async notifyStop() {
      await post<WechatSendResponse>('/ilink/bot/msg/notifystop', 'the notifystop call', { base_info: baseInfo() });
    },

    rateLimits: () => rateLimits,
  };
}
