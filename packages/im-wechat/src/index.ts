import { readFile as nodeReadFile } from 'node:fs/promises';
import { setTimeout as sleepFor } from 'node:timers/promises';
import type { Json } from '@onlyne/lib';
import { OnlyneError } from '@onlyne/lib';
import { resolveCredential, type Connector, type ImEvent, type SendOutcome, type SendRequest } from '@onlyne/im-core';
import {
  WechatPortError, createWechatPort, detailOf,
  type WechatPort, type WechatUpdatesResponse,
} from './port.js';
import { normalizeMessage } from './normalize.js';
import { conversationTarget, outcomeOf, sendMessage } from './send.js';

export type {
  WechatConfigResponse, WechatMessage, WechatMessageItem, WechatMediaRef, WechatOutboundMessage, WechatPort,
  WechatPortOptions, WechatSendResponse, WechatUpdatesResponse,
} from './port.js';
export { WECHAT_DEFAULT_BASE_URL, WechatPortError, createWechatPort } from './port.js';

export const WECHAT_CREDENTIAL_VARIABLES = ['WECHAT_ILINK_BOT_TOKEN'] as const;
/** Platform operations the connector implements beyond `send`; a resource lists them. */
export const WECHAT_OPERATIONS = ['typing'] as const;

const PLATFORM = 'wechat';
const BASE_BACKOFF_MS = 1_000;
const MAX_BACKOFF_MS = 30_000;
/** A dead session needs a QR scan, so retrying it fast only burns the operator's rate budget. */
const SESSION_BACKOFF_MS = 60_000;
const SESSION_ONBOARDING = 'WECHAT_ILINK_BOT_TOKEN (re-scan the ClawBot QR to refresh the session)';

/** The host-side status view; `Connector` deliberately does not carry it, the gateway duck-types it. */
export interface WechatStatus {
  state: string;
  connected: boolean;
  reconnects: number;
  rate_limits: number;
  last_event_at?: string;
  onboarding?: string[];
  error?: string;
}

export interface WechatConnectorOptions {
  /** The bot token from the ClawBot QR login; wins over file and environment. */
  credential?: string;
  /** Path to a file holding the token; `$VAR` references resolve from the environment. */
  credentialFile?: string;
  env?: Record<string, string | undefined>;
  readFile?: (path: string) => Promise<string>;
  baseUrl?: string;
  /** The HTTP boundary; production leaves it unset and one is built from the credential. */
  port?: WechatPort;
  log?: (message: string, fields?: Record<string, unknown>) => void;
  now?: () => number;
  sleep?: (ms: number) => Promise<void>;
}

export interface WechatConnector extends Connector {
  readonly operations: readonly string[];
  validate(operation: string, data: Json): void;
  operate(operation: string, data: Json): Promise<SendOutcome>;
  status(): WechatStatus;
}

/**
 * The iLink connector. Inbound is a long poll of `getupdates`: the cursor is the platform's
 * `get_updates_buf`, held in memory only, and it moves forward at exactly one place — after every
 * message of a batch was accepted by every listener. A rejected event leaves the cursor alone, so
 * the next poll asks for the same batch again and the message is not lost.
 */
export function createWechatConnector(options: WechatConnectorOptions = {}): WechatConnector {
  const sleep = options.sleep ?? ((ms: number) => sleepFor(ms));
  const clock = options.now ?? (() => Date.now());
  let port = options.port;
  let controller: AbortController | undefined;
  let state = 'stopped';
  let connected = false;
  let reconnects = 0;
  let lastEventAt: string | undefined;
  let onboarding: string[] = [];
  let failureText: string | undefined;

  const listeners: Array<(event: ImEvent) => void | Promise<void>> = [];
  /** One chain per conversation: the endpoint's `receive` calls land in arrival order. */
  const queues = new Map<string, Promise<void>>();

  function emit(event: ImEvent): Promise<void> {
    const previous = queues.get(event.conversation) ?? Promise.resolve();
    const next = previous.catch(() => undefined).then(async () => {
      for (const listener of listeners) await listener(event);
    });
    queues.set(event.conversation, next);
    const settle = (): void => {
      if (queues.get(event.conversation) === next) queues.delete(event.conversation);
    };
    void next.then(settle, settle);
    return next;
  }

  async function poll(client: WechatPort, signal: AbortSignal): Promise<void> {
    let cursor = '';
    let failures = 0;
    while (!signal.aborted) {
      let batch: WechatUpdatesResponse;
      try {
        batch = await client.getUpdates(cursor, signal);
      } catch (failure) {
        if (signal.aborted) return;
        failures += 1;
        reconnects += 1;
        failureText = detailOf(failure);
        if (failure instanceof WechatPortError && failure.sessionExpired) {
          state = 'paused';
          connected = false;
          onboarding = [SESSION_ONBOARDING];
          options.log?.('wechat: the iLink session expired; polling is paused until the ClawBot QR is scanned again', { error: failureText });
          await sleep(SESSION_BACKOFF_MS);
          continue;
        }
        state = 'error';
        connected = false;
        options.log?.('wechat: getupdates failed; retrying', { error: failureText, attempt: failures });
        await sleep(Math.min(BASE_BACKOFF_MS * 2 ** (failures - 1), MAX_BACKOFF_MS));
        continue;
      }
      if (signal.aborted) return;

      try {
        for (const message of batch.msgs ?? []) {
          const event = normalizeMessage(message, clock);
          if (event === undefined) continue;
          await emit(event);
          lastEventAt = event.at;
        }
      } catch (failure) {
        failures += 1;
        reconnects += 1;
        failureText = detailOf(failure);
        options.log?.('wechat: a listener rejected an event; the batch will be redelivered', { error: failureText, attempt: failures });
        await sleep(Math.min(BASE_BACKOFF_MS * 2 ** (failures - 1), MAX_BACKOFF_MS));
        continue;
      }
      if (signal.aborted) return;

      // The one place the cursor moves: the whole batch was accepted, so the platform may drop it.
      cursor = batch.get_updates_buf ?? cursor;
      failures = 0;
      connected = true;
      state = 'polling';
      failureText = undefined;
      onboarding = [];
    }
  }

  /** The operation's target user id, validated before any platform call. */
  function typingTarget(operation: string, data: Json): string {
    if (!(WECHAT_OPERATIONS as readonly string[]).includes(operation)) {
      throw new OnlyneError('invalid_request', `The WeChat connector does not implement ${operation}.`);
    }
    const raw = data !== null && typeof data === 'object' && !Array.isArray(data) ? data as Record<string, Json> : undefined;
    const conversation = raw?.conversation;
    const target = typeof conversation === 'string' ? conversationTarget(conversation) : undefined;
    if (target === undefined) {
      throw new OnlyneError('invalid_request', 'typing names conversation as user:<id> or group:<id>.');
    }
    return target.id;
  }

  return {
    platform: PLATFORM,
    operations: WECHAT_OPERATIONS,

    async start(): Promise<void> {
      if (controller !== undefined) return;
      if (port === undefined) {
        const env = options.env ?? process.env;
        const resolved = await resolveCredential(
          {
            value: options.credential,
            file: options.credentialFile,
            variables: [...WECHAT_CREDENTIAL_VARIABLES],
          },
          options.readFile ?? (path => nodeReadFile(path, 'utf8')),
          env,
        );
        const token = resolved.value?.trim();
        if (token === undefined || token.length === 0) {
          state = 'onboarding';
          connected = false;
          // An unreadable credential file is reported verbatim; a plain absence names the field.
          onboarding = [
            resolved.problem !== undefined && (options.credential !== undefined || options.credentialFile !== undefined)
              ? resolved.problem
              : WECHAT_CREDENTIAL_VARIABLES[0],
          ];
          options.log?.('wechat: no bot token; the channel is onboarding', { onboarding: onboarding.join(', ') });
          return;
        }
        const baseUrl = options.baseUrl ?? env.WECHAT_ILINK_BASE_URL;
        port = createWechatPort({ token, ...(baseUrl === undefined ? {} : { baseUrl }) });
      }

      const client = port;
      state = 'connecting';
      connected = false;
      failureText = undefined;
      void client.notifyStart().catch(failure => options.log?.('wechat: notifystart failed', { error: detailOf(failure) }));
      const run = new AbortController();
      controller = run;
      void poll(client, run.signal).catch(failure => {
        state = 'error';
        connected = false;
        failureText = detailOf(failure);
        options.log?.('wechat: the poll loop stopped unexpectedly', { error: failureText });
      });
    },

    onEvent(listener: (event: ImEvent) => void | Promise<void>): void {
      listeners.push(listener);
    },

    async send(request: SendRequest): Promise<SendOutcome> {
      const client = port;
      if (client === undefined) {
        return { state: 'unknown', error: 'The WeChat connector is not connected; no iLink session is available.' };
      }
      return sendMessage(client, request);
    },

    validate(operation: string, data: Json): void {
      typingTarget(operation, data);
    },

    async operate(operation: string, data: Json): Promise<SendOutcome> {
      const userId = typingTarget(operation, data);
      const client = port;
      if (client === undefined) {
        return { state: 'unknown', error: 'The WeChat connector is not connected; no iLink session is available.' };
      }
      try {
        const config = await client.getConfig(userId);
        const ticket = config.typing_ticket;
        if (typeof ticket !== 'string' || ticket.length === 0) {
          return { state: 'unknown', error: 'The platform call failed: getconfig answered without a typing_ticket.' };
        }
        await client.sendTyping(userId, ticket, 1);
        return { state: 'committed' };
      } catch (failure) {
        return outcomeOf(failure);
      }
    },

    async stop(): Promise<void> {
      const run = controller;
      controller = undefined;
      run?.abort();
      listeners.length = 0;
      queues.clear();
      state = 'stopped';
      connected = false;
      if (port !== undefined) {
        await port.notifyStop().catch(failure => options.log?.('wechat: notifystop failed', { error: detailOf(failure) }));
      }
    },

    status(): WechatStatus {
      return {
        state,
        connected,
        reconnects,
        rate_limits: port?.rateLimits() ?? 0,
        ...(lastEventAt === undefined ? {} : { last_event_at: lastEventAt }),
        ...(onboarding.length === 0 ? {} : { onboarding: [...onboarding] }),
        ...(failureText === undefined ? {} : { error: failureText }),
      };
    },
  };
}
