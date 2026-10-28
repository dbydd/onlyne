/**
 * The QQ official Bot OpenAPI v2 connector. Inbound events arrive either on the platform's
 * WebSocket gateway (the default, owned by `gateway.ts`) or on the signed HTTP callback the host
 * hands to `handleWebhook`; every outbound platform call goes through the injected port either way.
 */

import { readFile as nodeReadFile } from 'node:fs/promises';
import type { Json } from '@onlyne/lib';
import { OnlyneError } from '@onlyne/lib';
import { resolveCredential, type Connector, type ImEvent, type SendOutcome, type SendRequest } from '@onlyne/im-core';
import { createQqPort, messagePath, type QqMessageBody, type QqPort, type QqSendTarget } from './port.js';
import { normalizeEvent } from './normalize.js';
import { createSendState, outcomeOf, parseConversation, sendMessage } from './send.js';
import { challenge, verifyHex } from './webhook.js';
import { createQqGateway, type QqGateway, type QqGatewayOptions, type QqGatewayState } from './gateway.js';

export {
  createQqPort, messagePath, QqPortError,
  type QqFetch, type QqHttpRequest, type QqHttpResponse, type QqMessageBody, type QqPort,
  type QqPortOptions, type QqPortStatus, type QqSendResult, type QqSendTarget,
} from './port.js';
export { normalizeEvent } from './normalize.js';
export { challenge, deriveEd25519, signHex, verifyHex, type QqEd25519Keys } from './webhook.js';
export { createSendState, parseConversation, sendMessage, type QqMessagePort, type SendState } from './send.js';
export {
  createQqGateway, QQ_INTENTS,
  type QqGateway, type QqGatewayHandlers, type QqGatewayOptions, type QqGatewayState, type QqGatewayStatus,
  type QqSessionStartLimit,
} from './gateway.js';

export const QQ_CREDENTIAL_VARIABLES = ['QQ_BOT_APP_ID', 'QQ_BOT_APP_SECRET'] as const;
/** Platform operations the connector implements beyond `send`; a resource lists them. */
export const QQ_OPERATIONS = ['recall', 'typing'] as const;

/** Where inbound events come from: the platform's gateway socket, or its signed HTTP callback. */
export type QqTransport = 'websocket' | 'webhook';

/** QQ signs every dispatch over its timestamp; a callback older than this is a replay. */
const SIGNATURE_WINDOW_MS = 5 * 60 * 1000;
/** What QQ's `input_notify` shows the user; the platform caps the hint at 60 seconds. */
const TYPING_SECONDS = 60;

export type QqConnectionState =
  | 'onboarding'
  /** The webhook transport is authenticated and ready; the platform calls in. */
  | 'connected'
  | QqGatewayState
  | 'error';

/** The optional platform-status surface the gateway duck-types; not part of `Connector`. */
export interface QqStatus {
  state: string;
  connected: boolean;
  reconnects: number;
  rate_limits: number;
  last_event_at?: string;
  onboarding?: string[];
  error?: string;
  /** The inbound transport in force. */
  transport: QqTransport;
  /** WebSocket gateway counters; present once the gateway transport has started. */
  heartbeat_timeouts?: number;
  resumes?: number;
  resume_failures?: number;
  shards?: number;
}

/** The gateway transport's knobs; the connector supplies the port, the log and the clock. */
export type QqGatewayTuning = Omit<QqGatewayOptions, 'port' | 'log' | 'now'>;

export interface QqConnectorOptions {
  /** Explicit `appId:appSecret`; wins over the file and the environment pair. */
  credential?: string;
  /** Path to a credential file; `$VAR` references resolve from the environment. */
  credentialFile?: string;
  env?: Record<string, string | undefined>;
  readFile?: (path: string) => Promise<string>;
  /** The HTTP boundary; production leaves it unset and one is built from the credential. */
  port?: QqPort;
  log?: (message: string, fields?: Record<string, unknown>) => void;
  /**
   * Inbound transport. The gateway socket is QQ's own receive path and needs no public endpoint;
   * `webhook` serves the signed HTTP callback instead. Default `websocket`.
   */
  transport?: QqTransport;
  /** Gateway transport tuning: the reconnect backoff, and the intents bitmask. */
  gateway?: QqGatewayTuning;
  /** The inbound path the operator registers with QQ; reported once at start. */
  webhookPath?: string;
  now?: () => number;
}

export interface QqConnector extends Connector {
  readonly operations: readonly string[];
  validate(operation: string, data: Json): void;
  operate(operation: string, data: Json): Promise<SendOutcome>;
  status(): QqStatus;
  /** The platform's callback: op 13 validation and signed op 0 dispatches. */
  handleWebhook(request: Request): Promise<Response>;
}

function recordOf(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

export function createQqConnector(options: QqConnectorOptions = {}): QqConnector {
  const now = options.now ?? Date.now;
  const log = options.log ?? (() => {});
  const transport: QqTransport = options.transport ?? 'websocket';
  const listeners: Array<(event: ImEvent) => void | Promise<void>> = [];
  /** One chain per conversation: the endpoint's `receive` calls land in arrival order. */
  const queues = new Map<string, Promise<void>>();
  const sendState = createSendState();

  let port: QqPort | undefined;
  let gateway: QqGateway | undefined;
  let lifecycle: QqConnectionState = 'onboarding';
  let onboarding: string[] | undefined;
  let failure: string | undefined;
  let lastEventAt: string | undefined;
  let appId: string | undefined;
  let appSecret: string | undefined;
  /** The credential resolved and the token was proven: outbound calls can go. */
  let ready = false;

  function emit(event: ImEvent): Promise<void> {
    const previous = queues.get(event.conversation) ?? Promise.resolve();
    const run = previous.then(async () => {
      for (const listener of listeners) await listener(event);
    });
    // The chain survives a failing listener so the next event in this conversation still runs;
    // the caller keeps the rejection and answers the platform with a retryable status.
    const settled = run.catch(() => {});
    queues.set(event.conversation, settled);
    void settled.then(() => {
      if (queues.get(event.conversation) === settled) queues.delete(event.conversation);
    });
    return run;
  }

  function refuse(code: string, message: string, status: number): Response {
    return Response.json({ error: { code, message } }, { status });
  }

  function credentialFields(problem: string | undefined): string[] {
    const env = options.env ?? process.env;
    // A configured file that could not be read: the resolve problem is the real fix, not the pair.
    if (options.credentialFile !== undefined) return [problem ?? `Provide ${QQ_CREDENTIAL_VARIABLES.join(' + ')}.`];
    const missing = QQ_CREDENTIAL_VARIABLES.filter(name => (env[name] ?? '').length === 0);
    return missing.length > 0 ? missing : [problem ?? `Provide ${QQ_CREDENTIAL_VARIABLES.join(' + ')}.`];
  }

  return {
    platform: 'qq',
    operations: QQ_OPERATIONS,

    async start(): Promise<void> {
      lifecycle = 'connecting';
      const credential = await resolveCredential(
        { value: options.credential, file: options.credentialFile, variables: [...QQ_CREDENTIAL_VARIABLES] },
        options.readFile ?? (path => nodeReadFile(path, 'utf8')),
        options.env ?? process.env,
      );
      if (credential.value === undefined) {
        // A missing credential is a state, not a failure: the host renders the fields to provide.
        lifecycle = 'onboarding';
        onboarding = credentialFields(credential.problem);
        failure = undefined;
        log('qq connector is onboarding', { onboarding });
        return;
      }
      const separator = credential.value.indexOf(':');
      if (separator <= 0 || separator === credential.value.length - 1) {
        lifecycle = 'error';
        failure = 'The QQ credential is appId:appSecret.';
        onboarding = [...QQ_CREDENTIAL_VARIABLES];
        log('qq connector rejected the credential shape', { error: failure });
        return;
      }
      appId = credential.value.slice(0, separator);
      appSecret = credential.value.slice(separator + 1);
      port = options.port ?? createQqPort({
        appId,
        appSecret,
        ...(options.now === undefined ? {} : { now: options.now }),
      });
      try {
        await port.start();
        // Fetching the token proves the pair: QQ answers a bad secret here and nowhere earlier.
        await port.token();
        ready = true;
        onboarding = undefined;
        failure = undefined;
        if (transport === 'webhook') {
          lifecycle = 'connected';
          log('qq connector is connected', {
            app_id: appId,
            transport,
            webhook_path: options.webhookPath ?? 'unset',
          });
          return;
        }
        gateway = createQqGateway({
          port,
          log,
          now,
          ...(options.gateway ?? {}),
        });
        // Not awaited to a socket: a gateway that cannot reach QQ stays a retrying state, and
        // the host's status panel shows it, instead of a failed start.
        await gateway.start({
          dispatch: async payload => {
            const event = normalizeEvent(payload, now);
            if (event === undefined) return;
            // A rejection leaves the sequence number where it was: the platform replays the event.
            await emit(event);
            lastEventAt = event.at;
          },
        });
        lifecycle = 'connecting';
        log('qq connector is dialing the gateway', { app_id: appId, transport });
      } catch (error) {
        ready = false;
        lifecycle = 'error';
        failure = error instanceof Error ? error.message : String(error);
        log('qq connector could not authenticate', { error: failure });
      }
    },

    onEvent(listener: (event: ImEvent) => void | Promise<void>): void {
      listeners.push(listener);
    },

    async send(request: SendRequest): Promise<SendOutcome> {
      if (port === undefined || !ready) {
        const hint = onboarding === undefined ? '' : `; provide ${onboarding.join(', ')}`;
        return { state: 'unknown', error: `The QQ connector is not connected${hint}.` };
      }
      return sendMessage(port, request, sendState);
    },

    validate(operation: string, data: Json): void {
      if (!(QQ_OPERATIONS as readonly string[]).includes(operation)) {
        throw new OnlyneError('invalid_request', `The QQ connector does not implement ${operation}.`);
      }
      const raw = recordOf(data) ?? {};
      if (operation === 'recall' && (typeof raw.message_id !== 'string' || raw.message_id.length === 0)) {
        throw new OnlyneError('invalid_request', 'recall names message_id.');
      }
      if (typeof raw.conversation !== 'string' || raw.conversation.length === 0) {
        throw new OnlyneError('invalid_request', `${operation} names conversation.`);
      }
    },

    async operate(operation: string, data: Json): Promise<SendOutcome> {
      if (port === undefined || !ready) {
        return { state: 'unknown', error: 'The QQ connector is not connected.' };
      }
      const raw = recordOf(data) ?? {};
      const conversation = typeof raw.conversation === 'string' ? raw.conversation : '';
      const target: QqSendTarget | undefined = parseConversation(conversation);
      if (target === undefined) {
        return {
          state: 'unknown',
          error: `The QQ connector addresses c2c:<openid> or group:<openid>, not ${conversation}.`,
        };
      }
      try {
        if (operation === 'recall') {
          await port.request('DELETE', messagePath(target, String(raw.message_id)));
          return { state: 'committed' };
        }
        if (operation === 'typing') {
          if (target.kind === 'group') return { state: 'unknown', error: 'QQ typing is c2c only.' };
          const body: QqMessageBody = { msg_type: 6, input_notify: { input_type: 1, input_second: TYPING_SECONDS } };
          await port.send(target, body);
          return { state: 'committed' };
        }
        return { state: 'unknown', error: `The QQ connector does not implement ${operation}.` };
      } catch (error) {
        return outcomeOf(error, 'operation');
      }
    },

    async handleWebhook(request: Request): Promise<Response> {
      if (request.method !== 'POST') {
        return refuse('method_not_allowed', 'The QQ webhook accepts POST only.', 405);
      }
      const rawBody = await request.text();
      let payload: unknown;
      try {
        payload = JSON.parse(rawBody);
      } catch {
        return refuse('invalid_request', 'The QQ webhook body is not JSON.', 400);
      }
      const envelope = recordOf(payload);
      if (envelope === undefined) {
        return refuse('invalid_request', 'The QQ webhook body is not an object.', 400);
      }
      if (appSecret === undefined) {
        return refuse('unavailable', 'The QQ connector has no credential, so a callback cannot be validated.', 503);
      }

      if (envelope.op === 13) {
        const claimed = request.headers.get('x-bot-appid');
        if (claimed !== null && appId !== undefined && claimed !== appId) {
          return refuse('unauthenticated', 'X-Bot-Appid does not match the configured app id.', 401);
        }
        const answer = challenge(appSecret, rawBody);
        if (answer === undefined) {
          return refuse('invalid_request', 'The validation callback names plain_token and event_ts.', 400);
        }
        return Response.json(answer);
      }

      if (envelope.op === 0) {
        const timestamp = request.headers.get('x-signature-timestamp');
        const signature = request.headers.get('x-signature-ed25519');
        if (timestamp === null || signature === null) {
          return refuse('unauthenticated', 'The QQ dispatch carries no signature headers.', 401);
        }
        const stamp = Number(timestamp);
        if (!Number.isFinite(stamp) || Math.abs(now() - stamp * 1000) > SIGNATURE_WINDOW_MS) {
          return refuse('unauthenticated', 'The QQ dispatch timestamp is outside the five-minute window.', 401);
        }
        if (!verifyHex(appSecret, `${timestamp}${rawBody}`, signature)) {
          return refuse('unauthenticated', 'The QQ dispatch signature does not verify.', 401);
        }
        const event = normalizeEvent(payload, now);
        if (event !== undefined) {
          try {
            await emit(event);
          } catch (error) {
            const detail = error instanceof Error ? error.message : String(error);
            // Never ack a batch whose durable receive failed: QQ retries a non-2xx answer.
            log('qq inbound listener failed', { source_id: event.source_id, error: detail });
            return refuse('unavailable', `The inbound event was not stored: ${detail}`, 500);
          }
          lastEventAt = event.at;
        }
        return Response.json({ op: 12 });
      }

      // Heartbeats and anything newer than this connector knows: acknowledged, not translated.
      return Response.json({ op: 12 });
    },

    status(): QqStatus {
      const observed = gateway?.status();
      return {
        state: observed?.state ?? lifecycle,
        connected: observed === undefined ? lifecycle === 'connected' : observed.state === 'online',
        reconnects: observed?.reconnects ?? 0,
        rate_limits: port?.status().rate_limits ?? 0,
        transport,
        ...(lastEventAt === undefined ? {} : { last_event_at: lastEventAt }),
        ...(onboarding === undefined || onboarding.length === 0 ? {} : { onboarding }),
        ...(failure === undefined ? {} : { error: failure }),
        ...(observed === undefined ? {} : {
          heartbeat_timeouts: observed.heartbeat_timeouts,
          resumes: observed.resumes,
          resume_failures: observed.resume_failures,
          shards: observed.shards,
        }),
      };
    },

    async stop(): Promise<void> {
      listeners.length = 0;
      queues.clear();
      await gateway?.stop();
      gateway = undefined;
      await port?.stop();
      ready = false;
      lifecycle = 'stopped';
    },
  };
}
