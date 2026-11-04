import { OnlyneError, type Auth, type Message, type NodeId, type OnlyneNode, type SendRequest, type ThreadRef } from '@onlyne/lib';
import { t } from './node-text.ts';

/** DM thread id: `grokbot:dm:<agent_id>`. The grok-bot node owns every such thread. */
export const GROK_BOT_DM_PREFIX = 'grokbot:dm:';
/** Group thread id: `grokbot:group:<group_id>`. The grok-bot node owns every such thread. */
export const GROK_BOT_GROUP_PREFIX = 'grokbot:group:';

export type GrokBotChatKind = 'dm' | 'group';

export interface GrokBotChat {
  kind: GrokBotChatKind;
  chat_id: string;
}

const rec = (value: unknown): Record<string, unknown> | undefined =>
  value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : undefined;

const text = (value: unknown): string | undefined =>
  typeof value === 'string' && value.length > 0 ? value : undefined;

/** Stable thread id under the grok-bot node for one assistant DM or one group chat. */
export function grokBotThreadId(chat: GrokBotChat): string {
  if (chat.kind === 'group') return `${GROK_BOT_GROUP_PREFIX}${chat.chat_id}`;
  return `${GROK_BOT_DM_PREFIX}${chat.chat_id}`;
}

/** Parses a grok-bot thread id. Other ids (legacy random UUIDs) return undefined. */
export function parseGrokBotThreadId(id: string): GrokBotChat | undefined {
  if (id.startsWith(GROK_BOT_GROUP_PREFIX)) {
    const chat_id = id.slice(GROK_BOT_GROUP_PREFIX.length);
    return chat_id.length === 0 ? undefined : { kind: 'group', chat_id };
  }
  if (id.startsWith(GROK_BOT_DM_PREFIX)) {
    const chat_id = id.slice(GROK_BOT_DM_PREFIX.length);
    return chat_id.length === 0 ? undefined : { kind: 'dm', chat_id };
  }
  return undefined;
}

/**
 * Chat routing for a landed entry: the thread id is authoritative when it matches the
 * scheme; otherwise the webhook still emits a DM keyed by the sender so a router has a
 * stable pair.
 */
export function grokBotChatFromThread(ref: ThreadRef, fallbackSender: NodeId): GrokBotChat {
  return parseGrokBotThreadId(ref.id) ?? { kind: 'dm', chat_id: fallbackSender };
}

function grokBotObject(container: Record<string, unknown> | undefined): Record<string, unknown> | undefined {
  if (container === undefined) return undefined;
  return rec(container.grokbot);
}

function readGrokBotField(meta: Record<string, unknown> | undefined, name: string): string | undefined {
  if (meta === undefined) return undefined;
  const dotted = text(meta[`grokbot.${name}`]);
  if (dotted !== undefined) return dotted;
  const nested = grokBotObject(meta);
  const fromNested = text(nested?.[name]);
  if (fromNested !== undefined) return fromNested;
  const data = rec(meta.data);
  if (data === undefined) return undefined;
  const dataDotted = text(data[`grokbot.${name}`]);
  if (dataDotted !== undefined) return dataDotted;
  const dataNested = grokBotObject(data);
  return text(dataNested?.[name]);
}

/**
 * Resolves the Grok Bot conversation an inbound Onlyne message belongs to.
 *
 * `meta.data.grokbot.chat_kind` / `chat_id` (HTTP-safe) and the same fields on `meta.grokbot`
 * (programmatic) select the chat. `agent_id` is a DM alias; `group_id` is a group alias.
 * With no grokbot fields, the sender is a DM `agent_id`.
 */
export function grokBotChatFromMessage(message: Message, fallbackSender: NodeId): GrokBotChat {
  const existing = message.meta?.thread;
  if (existing !== undefined) {
    const parsed = parseGrokBotThreadId(existing.id);
    if (parsed !== undefined) return parsed;
  }
  const meta = rec(message.meta);
  const kindRaw = readGrokBotField(meta, 'chat_kind');
  const chatId = readGrokBotField(meta, 'chat_id')
    ?? readGrokBotField(meta, 'agent_id')
    ?? readGrokBotField(meta, 'group_id');
  if (kindRaw === 'group') {
    if (chatId === undefined) throw new OnlyneError('invalid_request', t('error.groupChatId'));
    return { kind: 'group', chat_id: chatId };
  }
  if (kindRaw !== undefined && kindRaw !== 'dm') {
    throw new OnlyneError('invalid_request', t('error.chatKind', { kind: kindRaw }));
  }
  return { kind: 'dm', chat_id: chatId ?? fallbackSender };
}

function titleFor(chat: GrokBotChat): string {
  return chat.kind === 'group' ? `Grok Bot group ${chat.chat_id}` : `Grok Bot DM ${chat.chat_id}`;
}

function mutation(): { idempotency_key: string; retry_until: string } {
  return {
    idempotency_key: crypto.randomUUID(),
    retry_until: new Date(Date.now() + 60_000).toISOString(),
  };
}

/** Creates the stable thread when missing. A concurrent create of the same id is benign. */
export async function ensureGrokBotThread(node: OnlyneNode, ref: ThreadRef, title: string): Promise<void> {
  try {
    await node.threads.describe(node.auth, ref);
    return;
  } catch (error) {
    if (!(error instanceof OnlyneError) || error.error.code !== 'not_found') throw error;
  }
  try {
    await node.threads.create(node.auth, { ...mutation(), title, id: ref.id });
  } catch (error) {
    if (!(error instanceof OnlyneError) || error.error.code !== 'id_conflict') throw error;
  }
}

/** Grants the sender post+read on a grok-bot-owned thread when they are not already active. */
export async function ensureGrokBotMember(node: OnlyneNode, ref: ThreadRef, peer: NodeId): Promise<void> {
  try {
    const described = await node.threads.describe(node.auth, ref);
    const existing = described.members.find(member => member.peer === peer);
    if (existing?.state === 'active' && existing.access.includes('post')) return;
  } catch (error) {
    if (!(error instanceof OnlyneError) || error.error.code !== 'not_found') throw error;
  }
  await node.threads.member(node.auth, ref, peer, { state: 'active', access: ['post', 'read'] }, mutation());
}

/**
 * Binds an inbound admission onto the stable grok-bot thread before the library sees it.
 *
 * Peer messages without `meta.thread` land on `grokbot:dm:<sender>` or the group named in
 * grokbot metadata — never a fresh random UUID. Self-sends (the `/grok/reply` path) do not
 * invent a thread: they keep an explicit `thread`, or leave it unset so `in_reply_to` can
 * recover the original landing, or fail if both are absent.
 */
export async function bindGrokBotInbound(node: OnlyneNode, owner: NodeId, request: SendRequest): Promise<SendRequest> {
  const message = request.message;
  const sender = message.sender;
  const existing = message.meta?.thread;
  const inbound = sender !== owner;

  let landing: ThreadRef | undefined = existing;
  if (landing === undefined) {
    if (!inbound) return request;
    const chat = grokBotChatFromMessage(message, sender);
    landing = { owner, id: grokBotThreadId(chat) };
  }

  if (landing.owner === owner) {
    const chat = parseGrokBotThreadId(landing.id) ?? (inbound ? grokBotChatFromMessage(message, sender) : undefined);
    const title = chat === undefined ? (message.msg.replace(/\s+/g, ' ').trim().slice(0, 100) || 'Grok Bot thread') : titleFor(chat);
    await ensureGrokBotThread(node, landing, title);
    if (inbound) await ensureGrokBotMember(node, landing, sender);
  }

  return {
    ...request,
    message: {
      ...message,
      meta: { ...message.meta, thread: landing },
    },
  };
}

/** Host-side rewrite of `messages.admit` so HTTP and programmatic admissions share the bind. */
export function installGrokBotAdmit(node: OnlyneNode, owner: NodeId): void {
  const original = node.messages.admit.bind(node.messages);
  node.messages.admit = async (auth: Auth, request: SendRequest) => {
    const receipt = await original(auth, await bindGrokBotInbound(node, owner, request));
    // The stable thread is created before admit, so the library's first-contact flag is
    // false. Seq 1 on a grokbot:dm/group id is still the first message of that conversation.
    if (receipt.thread_created === true) return receipt;
    if (receipt.seq === '1' && parseGrokBotThreadId(receipt.thread_ref.id) !== undefined) {
      return { ...receipt, thread_created: true };
    }
    return receipt;
  };
}
