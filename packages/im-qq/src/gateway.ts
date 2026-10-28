/**
 * The QQ Bot gateway: QQ's own receive path, one WebSocket per shard. Events arrive as `op 0`
 * dispatches; the access token, `/gateway/bot` and every outbound call stay on the HTTP port.
 *
 * The sequence number advances only after the listener accepted an event, so a delivery that
 * failed is replayed by the platform on the next resume instead of being skipped.
 */

import type { QqPort } from './port.js';

/** `idle` before `start`, `connecting` while a socket is being established. */
export type QqGatewayState = 'idle' | 'connecting' | 'online' | 'resuming' | 'paused' | 'stopped';

/** Group and c2c messages (bit 25) plus interaction callbacks (bit 26): what this connector translates. */
export const QQ_INTENTS = (1 << 25) | (1 << 26);

/** QQ's `session_start_limit`: how many sessions may be opened, and how fast. */
export interface QqSessionStartLimit {
  /** Sessions that may be started at the same time. */
  max_concurrency: number;
  /** Sessions left in the current window. */
  remaining: number;
  /** Milliseconds until `remaining` resets. */
  reset_after: number;
  /** Sessions a full window allows. */
  total: number;
}

export interface QqGatewayStatus {
  state: QqGatewayState;
  /** Connections opened after the first one. */
  reconnects: number;
  /** Heartbeats that went unanswered; each one closed its socket. */
  heartbeat_timeouts: number;
  /** Sessions continued by `op 6` after a drop. */
  resumes: number;
  /** Resume attempts the platform rejected, each followed by a fresh Identify. */
  resume_failures: number;
  shards: number;
}

export interface QqGatewayHandlers {
  /**
   * One `op 0` dispatch. The sequence number advances only after this resolves, so a rejection
   * is redelivered after the next resume.
   */
  dispatch(payload: Record<string, unknown>): void | Promise<void>;
}

export interface QqGatewayOptions {
  /** The HTTP boundary: the access token and `/gateway/bot`. */
  port: QqPort;
  /** The intents bitmask; defaults to `QQ_INTENTS`. */
  intents?: number;
  log?: (message: string, fields?: Record<string, unknown>) => void;
  now?: () => number;
  /** First reconnect waits this long before the exponential starts; 0 reconnects at once. */
  reconnectBaseMs?: number;
  /** The backoff ceiling. */
  reconnectCapMs?: number;
}

export interface QqGateway {
  start(handlers: QqGatewayHandlers): Promise<void>;
  stop(): Promise<void>;
  status(): QqGatewayStatus;
}

const DEFAULT_RECONNECT_BASE_MS = 1_000;
const DEFAULT_RECONNECT_CAP_MS = 30_000;
/** A socket that has not opened by then is dead; QQ's own clients give up at a similar point. */
const CONNECT_TIMEOUT_MS = 15_000;
/** How often a paused session start re-checks its budget. */
const PAUSED_POLL_MS = 250;
/** The Hello interval when the platform omits it; QQ documents 41 250 ms. */
const DEFAULT_HEARTBEAT_MS = 41_250;
/** `session_start_limit.reset_after` of 0 would pause forever, so a missing window is a second. */
const DEFAULT_LIMIT_WINDOW_MS = 1_000;

interface GatewayInfo {
  url: string;
  shards: number;
  limit: QqSessionStartLimit;
}

/** One shard's session, kept across reconnects so a resume can name it. */
interface Shard {
  index: number;
  sessionId?: string;
  /** The last sequence number the listener accepted. */
  seq: number;
}

function recordOf(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

function numberOr(value: unknown, fallback: number): number {
  return typeof value === 'number' && Number.isFinite(value) ? value : fallback;
}

function detailOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function createQqGateway(options: QqGatewayOptions): QqGateway {
  const port = options.port;
  const log = options.log ?? (() => {});
  const now = options.now ?? Date.now;
  const intents = options.intents ?? QQ_INTENTS;
  const baseMs = options.reconnectBaseMs ?? DEFAULT_RECONNECT_BASE_MS;
  const capMs = options.reconnectCapMs ?? DEFAULT_RECONNECT_CAP_MS;

  let state: QqGatewayState = 'idle';
  let stopping = false;
  let handlers: QqGatewayHandlers | undefined;
  let shardCount = 1;
  let reconnects = 0;
  let heartbeatTimeouts = 0;
  let resumes = 0;
  let resumeFailures = 0;
  const loops: Promise<void>[] = [];
  const sockets = new Set<WebSocket>();
  const sleepers = new Set<() => void>();

  // `/gateway/bot` is fetched once and reused: QQ rate-limits it, and a reconnect storm must not
  // turn into a discovery storm.
  let discovered: GatewayInfo | undefined;
  let discovering: Promise<GatewayInfo> | undefined;

  let limit: QqSessionStartLimit = { max_concurrency: 1, remaining: 1, reset_after: 0, total: 1 };
  let windowEndsAt = 0;
  let starting = 0;

  function transition(next: QqGatewayState, fields: Record<string, unknown> = {}): void {
    if (state === next) return;
    const from = state;
    state = next;
    log('qq gateway state', { from, to: next, ...fields });
  }

  /** Sleeps, but a stop wakes it: `stop` must not wait out a 30 s backoff. */
  function pause(ms: number): Promise<void> {
    if (stopping || ms <= 0) return Promise.resolve();
    const { promise, resolve } = Promise.withResolvers<void>();
    const wake = (): void => {
      clearTimeout(timer);
      sleepers.delete(wake);
      resolve();
    };
    const timer = setTimeout(wake, ms);
    sleepers.add(wake);
    return promise;
  }

  /** The first reconnect is immediate, then doubling with ±20% jitter so shards do not sync up. */
  function backoff(attempt: number): number {
    if (attempt <= 0) return 0;
    const exponential = Math.min(capMs, baseMs * 2 ** (attempt - 1));
    return Math.round(exponential * (0.8 + Math.random() * 0.4));
  }

  async function fetchInfo(): Promise<GatewayInfo> {
    const body = recordOf(await port.request<unknown>('GET', '/gateway/bot'));
    const url = typeof body?.url === 'string' && body.url.length > 0 ? body.url : undefined;
    if (url === undefined) throw new Error('The QQ /gateway/bot answer carries no url.');
    const raw = recordOf(body?.session_start_limit) ?? {};
    return {
      url,
      shards: Math.max(1, Math.floor(numberOr(body?.shards, 1))),
      limit: {
        max_concurrency: Math.max(1, Math.floor(numberOr(raw.max_concurrency, 1))),
        remaining: Math.max(0, Math.floor(numberOr(raw.remaining, 1))),
        reset_after: Math.max(0, numberOr(raw.reset_after, 0)),
        total: Math.max(1, Math.floor(numberOr(raw.total, 1))),
      },
    };
  }

  async function info(refresh: boolean): Promise<GatewayInfo> {
    if (discovered !== undefined && !refresh) return discovered;
    if (discovering !== undefined) return discovering;
    const pending = fetchInfo();
    discovering = pending;
    try {
      const next = await pending;
      discovered = next;
      observeLimit(next.limit);
      return next;
    } finally {
      if (discovering === pending) discovering = undefined;
    }
  }

  function observeLimit(next: QqSessionStartLimit): void {
    limit = next;
    windowEndsAt = next.remaining > 0 ? 0 : now() + Math.max(DEFAULT_LIMIT_WINDOW_MS, next.reset_after);
  }

  /** One session start, or `undefined` when a stop interrupted the wait. */
  async function acquire(shard: number): Promise<(() => void) | undefined> {
    let paused = false;
    while (!stopping) {
      if (windowEndsAt !== 0 && now() >= windowEndsAt) {
        limit = { ...limit, remaining: limit.total };
        windowEndsAt = 0;
      }
      if (limit.remaining > 0 && starting < limit.max_concurrency) {
        limit = { ...limit, remaining: limit.remaining - 1 };
        // The last session of the window starts the wait, not the next call: without this the
        // pause would have no end to look forward to.
        if (limit.remaining === 0) windowEndsAt = now() + Math.max(DEFAULT_LIMIT_WINDOW_MS, limit.reset_after);
        starting += 1;
        let released = false;
        return () => {
          if (released) return;
          released = true;
          starting = Math.max(0, starting - 1);
        };
      }
      if (!paused) {
        paused = true;
        log('qq gateway session start deferred', {
          shard, remaining: limit.remaining, reset_after: limit.reset_after, concurrency: starting,
        });
        transition('paused', { shard, remaining: limit.remaining });
      }
      await pause(PAUSED_POLL_MS);
    }
    return undefined;
  }

  /**
   * One socket, from Hello to close. Resolves `true` when the session reached READY or RESUMED,
   * so the caller knows whether the next attempt is a resume or a fresh handshake.
   */
  function connect(shard: Shard, url: string, total: number, release: () => void): Promise<boolean> {
    const { promise, resolve } = Promise.withResolvers<boolean>();
    const run = (): void => {
      let socket: WebSocket | undefined;
      let heartbeat: ReturnType<typeof setTimeout> | undefined;
      let ack: ReturnType<typeof setTimeout> | undefined;
      let connectTimer: ReturnType<typeof setTimeout> | undefined;
      let interval = DEFAULT_HEARTBEAT_MS;
      let online = false;
      let started = false;
      let failed = false;
      let settled = false;
      /** Serialized per connection: one event at a time, in sequence order. */
      let chain: Promise<void> = Promise.resolve();

      const send = (payload: Record<string, unknown>): void => {
        if (socket?.readyState !== 1) return;
        try {
          socket.send(JSON.stringify(payload));
        } catch (error) {
          log('qq gateway send failed', { shard: shard.index, op: payload.op, error: detailOf(error) });
        }
      };

      const finish = (): void => {
        if (settled) return;
        settled = true;
        clearTimeout(heartbeat);
        clearTimeout(ack);
        clearTimeout(connectTimer);
        release();
        if (socket !== undefined) {
          sockets.delete(socket);
          try {
            socket.close();
          } catch {
            // Already closing: the close event may have been what brought us here.
          }
        }
        resolve(online);
      };

      const beat = (): void => {
        if (settled) return;
        send({ op: 1, d: shard.seq });
        // A missed ACK is a dead connection: QQ answers every op 1 with op 11.
        ack = setTimeout(() => {
          heartbeatTimeouts += 1;
          log('qq gateway heartbeat unanswered', { shard: shard.index, seq: shard.seq, interval });
          transition('connecting', { shard: shard.index, reason: 'heartbeat_timeout' });
          finish();
        }, interval);
        heartbeat = setTimeout(beat, interval);
      };

      const begin = async (): Promise<void> => {
        if (started) return;
        started = true;
        let token: string;
        try {
          token = await port.token();
        } catch (error) {
          log('qq gateway token unavailable', { shard: shard.index, error: detailOf(error) });
          finish();
          return;
        }
        if (settled) return;
        if (shard.sessionId !== undefined) {
          transition('resuming', { shard: shard.index, session_id: shard.sessionId, seq: shard.seq });
          send({ op: 6, d: { token: `QQBot ${token}`, session_id: shard.sessionId, seq: shard.seq } });
          return;
        }
        send({
          op: 2,
          d: {
            token: `QQBot ${token}`,
            intents,
            shard: [shard.index, total],
            properties: { $os: 'linux', $browser: 'onlyne', $device: 'onlyne' },
          },
        });
      };

      const dispatch = (payload: Record<string, unknown>): void => {
        const type = typeof payload.t === 'string' ? payload.t : undefined;
        const seq = typeof payload.s === 'number' ? payload.s : undefined;
        if (type === 'READY') {
          const sessionId = recordOf(payload.d)?.session_id;
          if (typeof sessionId === 'string' && sessionId.length > 0) shard.sessionId = sessionId;
          if (seq !== undefined && seq > shard.seq) shard.seq = seq;
          online = true;
          release();
          transition('online', { shard: shard.index, session_id: shard.sessionId });
          return;
        }
        if (type === 'RESUMED') {
          resumes += 1;
          if (seq !== undefined && seq > shard.seq) shard.seq = seq;
          online = true;
          release();
          transition('online', { shard: shard.index, seq: shard.seq });
          return;
        }
        if (seq === undefined) return;
        chain = chain.then(async () => {
          if (failed || settled) return;
          const listener = handlers;
          try {
            if (listener !== undefined) await listener.dispatch(payload);
            if (seq > shard.seq) shard.seq = seq;
          } catch (error) {
            // Stop advancing: the resume replays from `shard.seq`, so this event comes back.
            failed = true;
            log('qq gateway dispatch failed', {
              shard: shard.index, seq, type,
              source: recordOf(payload.d)?.id ?? payload.id, error: detailOf(error),
            });
            finish();
          }
        });
      };

      const onMessage = (event: MessageEvent): void => {
        let payload: Record<string, unknown> | undefined;
        try {
          payload = recordOf(JSON.parse(String(event.data)));
        } catch {
          payload = undefined;
        }
        if (payload === undefined) return;
        const op = payload.op;
        if (op === 10) {
          const heartbeatInterval = numberOr(recordOf(payload.d)?.heartbeat_interval, 0);
          if (heartbeatInterval > 0) interval = heartbeatInterval;
          heartbeat = setTimeout(beat, interval);
          void begin();
          return;
        }
        if (op === 11) {
          if (ack !== undefined) {
            clearTimeout(ack);
            ack = undefined;
          }
          return;
        }
        if (op === 1) {
          send({ op: 1, d: shard.seq });
          return;
        }
        if (op === 7) {
          log('qq gateway asked for a reconnect', { shard: shard.index, seq: shard.seq });
          finish();
          return;
        }
        if (op === 9) {
          const resumable = payload.d === true;
          if (shard.sessionId !== undefined) resumeFailures += 1;
          log('qq gateway rejected the session', {
            shard: shard.index, resumable, session_id: shard.sessionId, seq: shard.seq,
          });
          if (!resumable) {
            shard.sessionId = undefined;
            shard.seq = 0;
          }
          transition('connecting', { shard: shard.index, reason: 'invalid_session' });
          finish();
          return;
        }
        if (op === 0) dispatch(payload);
      };

      try {
        socket = new WebSocket(url);
      } catch (error) {
        log('qq gateway socket could not open', { shard: shard.index, url, error: detailOf(error) });
        finish();
        return;
      }
      sockets.add(socket);
      if (stopping) {
        // A stop landed between the session start and the dial: close what was just opened.
        finish();
        return;
      }
      connectTimer = setTimeout(() => {
        log('qq gateway socket did not open in time', { shard: shard.index, url, timeout_ms: CONNECT_TIMEOUT_MS });
        finish();
      }, CONNECT_TIMEOUT_MS);
      socket.addEventListener('open', () => {
        if (connectTimer !== undefined) {
          clearTimeout(connectTimer);
          connectTimer = undefined;
        }
      });
      socket.addEventListener('message', onMessage);
      socket.addEventListener('error', error => {
        log('qq gateway socket error', { shard: shard.index, error: detailOf(error) });
        finish();
      });
      socket.addEventListener('close', event => {
        if (!settled) log('qq gateway socket closed', { shard: shard.index, code: event.code, seq: shard.seq });
        finish();
      });
    };
    run();
    return promise;
  }

  /** One shard's whole life: discover, start a session, and reconnect until a stop. */
  async function session(index: number, total: number): Promise<void> {
    const shard: Shard = { index, seq: 0 };
    let attempt = 0;
    let refresh = false;
    while (!stopping) {
      let target: GatewayInfo;
      try {
        target = await info(refresh);
      } catch (error) {
        log('qq gateway discovery failed', { shard: index, error: detailOf(error), attempt });
        transition('connecting', { shard: index });
        await pause(backoff(attempt));
        attempt += 1;
        continue;
      }
      const release = await acquire(index);
      if (release === undefined) return;
      transition(shard.sessionId === undefined ? 'connecting' : 'resuming', { shard: index, seq: shard.seq });
      const online = await connect(shard, target.url, Math.max(1, target.shards), release);
      if (stopping) return;
      reconnects += 1;
      // A session that never came online means the url or the budget is stale: rediscover.
      refresh = !online;
      attempt = online ? 0 : attempt + 1;
      log('qq gateway connection ended', {
        shard: index, reconnects, online, session_id: shard.sessionId, seq: shard.seq, attempt,
      });
      await pause(backoff(online ? 0 : attempt));
    }
  }

  async function control(): Promise<void> {
    let attempt = 0;
    while (!stopping) {
      try {
        const first = await info(true);
        const count = Math.max(1, first.shards);
        shardCount = count;
        log('qq gateway discovered', {
          url: first.url, shards: count, remaining: first.limit.remaining, reset_after: first.limit.reset_after,
        });
        await Promise.all(Array.from({ length: count }, (_, index) => session(index, count)));
        return;
      } catch (error) {
        log('qq gateway discovery failed', { error: detailOf(error), attempt });
        transition('connecting');
        await pause(backoff(attempt));
        attempt += 1;
      }
    }
  }

  return {
    async start(next: QqGatewayHandlers): Promise<void> {
      if (loops.length > 0) return;
      handlers = next;
      stopping = false;
      transition('connecting');
      // Not awaited: a network outage is a state the operator watches, not a failed start.
      loops.push(control());
    },

    async stop(): Promise<void> {
      stopping = true;
      for (const wake of [...sleepers]) wake();
      for (const socket of [...sockets]) {
        try {
          socket.close();
        } catch {
          // Already closing.
        }
      }
      await Promise.all(loops);
      loops.length = 0;
      discovered = undefined;
      transition('stopped');
    },

    status(): QqGatewayStatus {
      return {
        state,
        reconnects,
        heartbeat_timeouts: heartbeatTimeouts,
        resumes,
        resume_failures: resumeFailures,
        shards: shardCount,
      };
    },
  };
}
