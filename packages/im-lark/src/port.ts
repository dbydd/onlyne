import {
  EventDispatcher, LarkChannel, WSClient,
  type SendInput, type SendOptions, type SendResult,
} from '@larksuiteoapi/node-sdk';

/** The WS lifecycle states the SDK reports; a host maps them onto its own log. */
export type LarkConnectionState = 'ready' | 'reconnecting' | 'reconnected' | 'error';

/** What the connector hands the port; every handler is fire-and-forget by design. */
export interface LarkHandlers {
  message(raw: unknown): void | Promise<void>;
  /** Returns the card callback's answer: Feishu expects a toast within three seconds. */
  cardAction(raw: unknown): unknown | Promise<unknown>;
  reaction(raw: unknown, action: 'added' | 'removed'): void | Promise<void>;
  botMember(raw: unknown, type: 'bot.added' | 'bot.removed'): void | Promise<void>;
  recall(raw: unknown): void | Promise<void>;
}

export interface LarkBotIdentity {
  openId: string;
  name: string;
}

/**
 * The SDK boundary: every platform call the connector makes goes through this interface, so the
 * connector's translation logic runs against a fake in tests and the real SDK in production.
 */
export interface LarkPort {
  start(handlers: LarkHandlers): Promise<void>;
  stop(): Promise<void>;
  status(): { state: string };
  botIdentity(): LarkBotIdentity;
  /** `im.v1.message.get` items: the merge-forward descendants, and the parent itself. */
  fetchSubMessages(messageId: string): Promise<unknown[]>;
  /** The chat a message belongs to; reaction events carry no chat id of their own. */
  messageChat(messageId: string): Promise<string | undefined>;
  send(to: string, input: SendInput, opts?: SendOptions): Promise<SendResult>;
  updateCard(messageId: string, card: object): Promise<void>;
  recallMessage(messageId: string): Promise<void>;
  addReaction(messageId: string, emojiType: string): Promise<string>;
  downloadResource(fileKey: string, type: 'image' | 'file'): Promise<Buffer>;
}

export interface LarkPortOptions {
  appId: string;
  appSecret: string;
  onState?: (state: LarkConnectionState, error?: Error) => void;
}

interface RawMessageItem {
  chat_id?: string;
}

/**
 * One WebSocket connection, owned here. Feishu allows several connections per app and delivers
 * each event to exactly one of them at random, so a second connection would silently split the
 * event stream; the channel's own inbound pipeline is unused for the same reason (it merges and
 * drops events before a caller sees them).
 */
export function createLarkPort(options: LarkPortOptions): LarkPort {
  const channel = new LarkChannel({ appId: options.appId, appSecret: options.appSecret });
  let socket: WSClient | undefined;
  let identity: LarkBotIdentity | undefined;

  const items = async (messageId: string): Promise<RawMessageItem[]> => {
    const response = await channel.rawClient.im.v1.message.get({ path: { message_id: messageId } });
    return (response.data?.items ?? []) as RawMessageItem[];
  };

  return {
    async start(handlers: LarkHandlers): Promise<void> {
      // `bot/v3/info` proves the credential before the socket opens, and its open_id is what
      // normalization needs to recognise an @-mention of this bot.
      const info = await channel.rawClient.request({ url: '/open-apis/bot/v3/info', method: 'GET' }) as {
        bot?: { open_id?: string; app_name?: string };
      };
      if (info.bot?.open_id === undefined) {
        throw new Error('bot/v3/info returned no open_id; the credential cannot serve this app.');
      }
      identity = { openId: info.bot.open_id, name: info.bot.app_name ?? 'bot' };

      const dispatcher = new EventDispatcher({});
      dispatcher.register({
        'im.message.receive_v1': (raw: unknown) => handlers.message(raw),
        'card.action.trigger': (raw: unknown) => handlers.cardAction(raw),
        'im.message.reaction.created_v1': (raw: unknown) => handlers.reaction(raw, 'added'),
        'im.message.reaction.deleted_v1': (raw: unknown) => handlers.reaction(raw, 'removed'),
        'im.chat.member.bot.added_v1': (raw: unknown) => handlers.botMember(raw, 'bot.added'),
        'im.chat.member.bot.deleted_v1': (raw: unknown) => handlers.botMember(raw, 'bot.removed'),
        'im.message.recalled_v1': (raw: unknown) => handlers.recall(raw),
      });

      socket = new WSClient({
        appId: options.appId,
        appSecret: options.appSecret,
        onReady: () => options.onState?.('ready'),
        onReconnecting: () => options.onState?.('reconnecting'),
        onReconnected: () => options.onState?.('reconnected'),
        onError: error => options.onState?.('error', error),
      });
      await socket.start({ eventDispatcher: dispatcher });
    },

    async stop(): Promise<void> {
      socket?.close({ force: true });
      socket = undefined;
    },

    status(): { state: string } {
      return { state: socket?.getConnectionStatus().state ?? 'idle' };
    },

    botIdentity(): LarkBotIdentity {
      if (identity === undefined) throw new Error('The port has not started; bot identity is unknown.');
      return identity;
    },

    async fetchSubMessages(messageId: string): Promise<unknown[]> {
      try {
        return await items(messageId);
      } catch {
        // A merge-forward body we cannot expand still carries its own message id and sender;
        // the converter reports what it has instead of failing the whole event.
        return [];
      }
    },

    async messageChat(messageId: string): Promise<string | undefined> {
      const found = await items(messageId);
      return found[0]?.chat_id;
    },

    send(to: string, input: SendInput, opts?: SendOptions): Promise<SendResult> {
      return channel.send(to, input, opts);
    },

    async updateCard(messageId: string, card: object): Promise<void> {
      await channel.updateCard(messageId, card);
    },

    async recallMessage(messageId: string): Promise<void> {
      await channel.recallMessage(messageId);
    },

    addReaction(messageId: string, emojiType: string): Promise<string> {
      return channel.addReaction(messageId, emojiType);
    },

    downloadResource(fileKey: string, type: 'image' | 'file'): Promise<Buffer> {
      return channel.downloadResource(fileKey, type);
    },
  };
}
