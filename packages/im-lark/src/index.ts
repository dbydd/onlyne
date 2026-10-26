import { readFile as nodeReadFile } from 'node:fs/promises';
import type { Json } from '@onlyne/lib';
import { OnlyneError } from '@onlyne/lib';
import { resolveCredential, type Connector, type ImEvent, type SendOutcome } from '@onlyne/im-core';
import {
  createLarkPort, type LarkConnectionState, type LarkHandlers, type LarkPort,
} from './port.js';
import {
  botMemberEvent, cardActionEvent, messageEvent, reactionEvent, recallEvent,
} from './normalize.js';
import { sendMessage } from './send.js';
import { materializeAttachments } from './attachments.js';

export type { LarkPort, LarkConnectionState, LarkHandlers } from './port.js';
export { createLarkPort } from './port.js';

export const LARK_CREDENTIAL_VARIABLES = ['FEISHU_APP_ID', 'FEISHU_APP_SECRET'] as const;
/** Platform operations the connector implements beyond `send`; a resource lists them. */
export const LARK_OPERATIONS = ['update_card', 'recall', 'react'] as const;

/** Lark's own rejections: the platform refused, so the operation is settled rather than unclear. */
const REJECTIONS = new Set(['format_error', 'target_revoked', 'permission_denied', 'rate_limited', 'ssrf_blocked']);

export interface LarkConnectorOptions {
  /** Explicit `app_id:app_secret`; wins over file and environment. */
  credential?: string;
  /** Path to a credential file; `$VAR` references resolve from the environment. */
  credentialFile?: string;
  env?: Record<string, string | undefined>;
  readFile?: (path: string) => Promise<string>;
  /** Where inbound attachments are written; unset keeps platform handles only. */
  mediaDir?: string;
  /** The SDK boundary; production leaves it unset and one is built from the credential. */
  port?: LarkPort;
  writeFile?: (path: string, data: Buffer) => Promise<void>;
  /** WS lifecycle transitions, for the host's log. */
  onState?: (state: LarkConnectionState, error?: Error) => void;
  /** A handler fault; it never stops the event stream. */
  onError?: (error: unknown) => void;
}

export interface LarkConnector extends Connector {
  readonly operations: readonly string[];
  validate(operation: string, data: Json): void;
  operate(operation: string, data: Json): Promise<SendOutcome>;
}

export function createLarkConnector(options: LarkConnectorOptions = {}): LarkConnector {
  let port: LarkPort | undefined = options.port;
  const listeners: Array<(event: ImEvent) => void | Promise<void>> = [];
  /** One chain per conversation: the endpoint's `receive` calls land in arrival order. */
  const queues = new Map<string, Promise<void>>();

  const report = (error: unknown): void => { options.onError?.(error); };
  const background = (work: () => Promise<void>): void => { void work().catch(report); };

  function emit(event: ImEvent): Promise<void> {
    const previous = queues.get(event.conversation) ?? Promise.resolve();
    const next = previous
      .then(async () => { for (const listener of listeners) await listener(event); })
      .catch(report);
    queues.set(event.conversation, next);
    void next.then(() => {
      if (queues.get(event.conversation) === next) queues.delete(event.conversation);
    });
    return next;
  }

  function live(): LarkPort {
    if (port === undefined) throw new Error('The Lark connector has not started.');
    return port;
  }

  function handlers(): LarkHandlers {
    return {
      message(raw) {
        background(async () => {
          const p = live();
          const event = await messageEvent(raw, {
            botIdentity: p.botIdentity(),
            fetchSubMessages: id => p.fetchSubMessages(id),
          });
          if (event === undefined) return;
          const materialized = await materializeAttachments(event, {
            ...(options.mediaDir === undefined ? {} : { mediaDir: options.mediaDir }),
            download: (key, type) => p.downloadResource(key, type),
            ...(options.writeFile === undefined ? {} : { write: options.writeFile }),
          });
          await emit(materialized);
        });
      },
      cardAction(raw) {
        background(async () => {
          const event = cardActionEvent(raw);
          if (event !== undefined) await emit(event);
        });
        // Feishu wants the callback answered within three seconds; the work continues above.
        return { toast: { type: 'info', content: '已收到' } };
      },
      reaction(raw, action) {
        background(async () => {
          const p = live();
          const messageId = (raw as { message_id?: string }).message_id;
          if (messageId === undefined) return;
          // Reaction events carry no chat id, so the chat is read from the message itself.
          const chatId = await p.messageChat(messageId);
          if (chatId === undefined) return;
          const event = reactionEvent(raw, action, chatId);
          if (event !== undefined) await emit(event);
        });
      },
      botMember(raw, type) {
        background(async () => {
          const event = botMemberEvent(raw, type);
          if (event !== undefined) await emit(event);
        });
      },
      recall(raw) {
        background(async () => {
          const event = recallEvent(raw);
          if (event !== undefined) await emit(event);
        });
      },
    };
  }

  function outcomeOf(error: unknown): SendOutcome {
    const code = (error as { code?: string } | undefined)?.code;
    const message = error instanceof Error ? error.message : String(error);
    return REJECTIONS.has(code ?? '') ? { state: 'committed', error: message } : { state: 'unknown', error: message };
  }

  return {
    platform: 'lark',
    operations: LARK_OPERATIONS,

    async start(): Promise<void> {
      if (port === undefined) {
        const credential = await resolveCredential(
          {
            value: options.credential,
            file: options.credentialFile,
            variables: [...LARK_CREDENTIAL_VARIABLES],
          },
          options.readFile ?? (path => nodeReadFile(path, 'utf8')),
          options.env ?? process.env,
        );
        if (credential.value === undefined) {
          throw new Error(`Lark credential unavailable: ${credential.problem ?? 'not provided'}`);
        }
        const separator = credential.value.indexOf(':');
        if (separator <= 0) throw new Error('The Lark credential is app_id:app_secret.');
        port = createLarkPort({
          appId: credential.value.slice(0, separator),
          appSecret: credential.value.slice(separator + 1),
          ...(options.onState === undefined ? {} : { onState: options.onState }),
        });
      }
      await port.start(handlers());
    },

    onEvent(listener: (event: ImEvent) => void | Promise<void>): void {
      listeners.push(listener);
    },

    async send(request): Promise<SendOutcome> {
      if (port === undefined) {
        return { state: 'unknown', error: 'The connector has not started; no Lark client is available.' };
      }
      return sendMessage(port, request);
    },

    validate(operation: string, data: Json): void {
      const raw = data as Record<string, Json>;
      if (!(LARK_OPERATIONS as readonly string[]).includes(operation)) {
        throw new OnlyneError('invalid_request', `The Lark connector does not implement ${operation}.`);
      }
      if (typeof raw.message_id !== 'string' || raw.message_id.length === 0) {
        throw new OnlyneError('invalid_request', `${operation} names message_id.`);
      }
      if (operation === 'update_card' && (raw.card === null || typeof raw.card !== 'object' || Array.isArray(raw.card))) {
        throw new OnlyneError('invalid_request', 'update_card carries a card object.');
      }
      if (operation === 'react' && (typeof raw.emoji !== 'string' || raw.emoji.length === 0)) {
        throw new OnlyneError('invalid_request', 'react names an emoji.');
      }
    },

    async operate(operation: string, data: Json): Promise<SendOutcome> {
      if (port === undefined) return { state: 'unknown', error: 'The connector has not started.' };
      const raw = data as Record<string, Json>;
      try {
        if (operation === 'update_card') {
          await port.updateCard(raw.message_id as string, raw.card as object);
          return { state: 'committed' };
        }
        if (operation === 'recall') {
          await port.recallMessage(raw.message_id as string);
          return { state: 'committed' };
        }
        if (operation === 'react') {
          await port.addReaction(raw.message_id as string, raw.emoji as string);
          return { state: 'committed' };
        }
        return { state: 'unknown', error: `The Lark connector does not implement ${operation}.` };
      } catch (error) {
        return outcomeOf(error);
      }
    },

    async stop(): Promise<void> {
      await port?.stop();
    },
  };
}
