import type {
  Auth, Json, OperationRequest, OperationResult, Resource, ResourceDriver, ResourceRef,
} from '@onlyne/lib';
import { OnlyneError, type ResourceService } from '@onlyne/lib';
import type { Connector, ImEvent, SendOutcome } from './index.js';

const SEND_TEXT_MAX = 100_000;

/** Resource-event data is wire JSON: interfaces land as plain objects, never class instances. */
const wire = (value: object): Json => JSON.parse(JSON.stringify(value)) as Json;

/** What the bridge records so a later `reply_to_event` resolves to a platform handle. */
interface HandleRow {
  resource_ref: ResourceRef;
  event_id: string;
  conversation: string;
  message_id: string;
  ref_handle: string;
}

export interface DriverBridgeOptions {
  node: string;
  /**
   * The endpoint's resource service. The library captures the driver when the service is
   * constructed (resources.ts:159), so this arrives as an accessor: the host builds the
   * bridge first, passes it into `OnlyneNode`'s plugins, and the accessor resolves to the
   * node's own service once construction finished.
   */
  resources: () => ResourceService;
  storage: { transaction<T>(work: (tx: {
    get<T>(key: string): Promise<T | undefined>;
    put<T>(key: string, value: T): Promise<void>;
  }) => Promise<T>): Promise<T> };
  connectors: Array<{ ref: ResourceRef; connector: Connector }>;
  /**
   * Stores inline attachment bytes and answers the path the connector can read. A host that
   * cannot write files omits it, and an inline attachment is then refused instead of dropped.
   */
  writeAttachment?: (ref: ResourceRef, name: string, bytes: Uint8Array) => Promise<string>;
}

const handleKey = (ref: ResourceRef, eventId: string) => `im/handle/${encodeURIComponent(ref.owner)}/${encodeURIComponent(ref.id)}/${encodeURIComponent(eventId)}`;

function requireObject(value: Json | undefined, operation: string): Record<string, Json> {
  if (value === undefined || value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new OnlyneError('invalid_request', `The ${operation} data is an object.`);
  }
  return value as Record<string, Json>;
}

function requireString(value: Json | undefined, field: string, operation: string): string {
  if (typeof value !== 'string' || value.length === 0) {
    throw new OnlyneError('invalid_request', `The ${operation} data names ${field}.`);
  }
  return value;
}

/** Inline content travels as base64; a malformed value is the caller's error, not a platform one. */
function bytesOf(content: string): Uint8Array {
  try {
    return Uint8Array.from(atob(content), character => character.charCodeAt(0));
  } catch {
    throw new OnlyneError('invalid_request', 'The attachment content is not valid base64.');
  }
}

/**
 * The platform's observation becomes the operation's result: an explicit rejection is
 * `aborted` with the platform's own words in `details.source`, an unclear outcome stays
 * `unknown`, and a confirmation is `committed` with the platform handles.
 */
function settleOutcome(outcome: SendOutcome): Omit<OperationResult, 'operation_id'> {
  if (outcome.error !== undefined && outcome.state === 'committed') {
    return { state: 'aborted', error: { code: 'capacity_exceeded', message: 'The platform rejected the message.', retryable: false, details: { source: outcome.error } } };
  }
  if (outcome.error !== undefined) {
    return { state: 'unknown', error: { code: 'unavailable', message: 'The platform result is unclear.', retryable: true, details: { source: outcome.error } } };
  }
  if (outcome.state === 'unknown') {
    return { state: 'unknown', error: { code: 'unavailable', message: 'The platform did not confirm the send.', retryable: true } };
  }
  return {
    state: 'committed',
    data: {
      ...(outcome.message_id === undefined ? {} : { message_id: outcome.message_id }),
      ...(outcome.ref_handle === undefined ? {} : { ref_handle: outcome.ref_handle }),
    },
  };
}

/**
 * The bridge between the library's resource operations and platform connectors. Deduplication,
 * event identity, and binding live in `ResourceService`; the bridge translates operation data to
 * platform handles and maps connector outcomes onto the operation's three-way result.
 */
export function createDriverBridge(options: DriverBridgeOptions): ResourceDriver & {
  receive: (event: ImEvent) => Promise<void>;
  platformHandle: (ref: ResourceRef, eventId: string) => Promise<{ message_id: string; ref_handle: string } | undefined>;
} {
  const { node, storage, connectors } = options;
  const resources = () => options.resources();
  const endpoint: Auth = { actor: node };
  const byRef = new Map(connectors.map(entry => [`${entry.ref.owner}/${entry.ref.id}`, entry.connector]));

  async function recordHandle(row: HandleRow): Promise<void> {
    await storage.transaction(async tx => { await tx.put(handleKey(row.resource_ref, row.event_id), row); });
  }

  async function handleOf(ref: ResourceRef, eventId: string): Promise<HandleRow | undefined> {
    return storage.transaction(async tx => tx.get<HandleRow>(handleKey(ref, eventId)));
  }

  async function receive(event: ImEvent): Promise<void> {
    const ref = connectors.find(entry => entry.connector.platform === event.platform)?.ref;
    if (ref === undefined) {
      throw new OnlyneError('invalid_request', `No connector serves platform ${event.platform}.`);
    }
    const data = wire({
      platform: event.platform,
      conversation: event.conversation,
      kind: event.kind,
      ...(event.from === undefined ? {} : { from: event.from }),
      text: event.text,
      attachments: event.attachments,
      ...(event.reply === undefined ? {} : { reply: event.reply }),
      ...(event.action === undefined ? {} : { action: event.action }),
      ...(event.event === undefined ? {} : { event: event.event }),
      at: event.at,
    });
    const stored = await resources().receive(endpoint, ref, { source_id: event.source_id, data });
    const existing = await handleOf(ref, stored.event_id);
    if (existing !== undefined) return; // a redelivery of an event whose handles we already hold
    if (event.kind !== 'event' && event.message_handle === undefined) {
      throw new OnlyneError('invalid_request', 'An inbound message event carries its own platform message handle.');
    }
    // A lifecycle event (recall, reaction, bot added) has no message of its own to quote.
    if (event.message_handle === undefined) return;
    // The event's own handles are the reply target for a later outbound message (v2 ImEvent.id
    // and the QQ second id carried over); without them a reply cannot quote this event.
    await recordHandle({
      resource_ref: ref, event_id: stored.event_id,
      conversation: event.conversation,
      message_id: event.message_handle,
      ref_handle: event.ref_handle ?? event.message_handle,
    });
  }

  return {
    async validate(resource: Resource, operation: string, data: Json | undefined): Promise<void> {
      const connector = byRef.get(`${resource.resource_ref.owner}/${resource.resource_ref.id}`);
      if (operation === 'send') {
        const raw = requireObject(data, 'send');
        const card = raw.card;
        const text = typeof raw.text === 'string' ? raw.text : '';
        if (card === undefined && text.length === 0) {
          throw new OnlyneError('invalid_request', 'The send data carries a text or a card.');
        }
        if (text.length > SEND_TEXT_MAX) {
          throw new OnlyneError('invalid_request', 'The send text exceeds the platform limit.');
        }
        if (card !== undefined && (card === null || typeof card !== 'object' || Array.isArray(card))) {
          throw new OnlyneError('invalid_request', 'The send card is an object.');
        }
        if (raw.format !== undefined && raw.format !== 'plain' && raw.format !== 'markdown') {
          throw new OnlyneError('invalid_request', 'The send format is plain or markdown.');
        }
        if (raw.attachments !== undefined) {
          if (!Array.isArray(raw.attachments)) throw new OnlyneError('invalid_request', 'The send attachments are a list.');
          for (const item of raw.attachments) {
            if (item === null || typeof item !== 'object' || Array.isArray(item)) {
              throw new OnlyneError('invalid_request', 'Each attachment names a kind and a path or inline content.');
            }
            const entry = item as Record<string, Json>;
            if (entry.kind !== 'image' && entry.kind !== 'file' && entry.kind !== 'audio' && entry.kind !== 'video' && entry.kind !== 'sticker') {
              throw new OnlyneError('invalid_request', 'The attachment kind is image, file, audio, video or sticker.');
            }
            const hasPath = typeof entry.path === 'string' && entry.path.length > 0;
            const hasContent = typeof entry.content === 'string' && entry.content.length > 0;
            if (hasPath === hasContent) {
              throw new OnlyneError('invalid_request', 'Each attachment carries either a path or inline content, not both.');
            }
            if (hasContent) requireString(entry.name, 'name', 'send');
          }
        }
        if (raw.reply_to_event !== undefined) {
          requireString(raw.reply_to_event, 'reply_to_event', 'send');
        }
        if (raw.reply_in_thread !== undefined && typeof raw.reply_in_thread !== 'boolean') {
          throw new OnlyneError('invalid_request', 'The send reply_in_thread is a boolean.');
        }
        return;
      }
      // Every other operation is the connector's own; it must have declared it.
      if (connector?.operations?.includes(operation) === true) {
        const raw = requireObject(data, operation);
        connector.validate?.(operation, raw);
        return;
      }
      throw new OnlyneError('invalid_request', `The bridge does not implement operation ${operation}.`);
    },

    async execute(resource: Resource, request: OperationRequest): Promise<Omit<OperationResult, 'operation_id'>> {
      const ref = resource.resource_ref;
      const connector = byRef.get(`${ref.owner}/${ref.id}`);
      if (connector === undefined) {
        return { state: 'unknown', error: { code: 'unavailable', message: 'No connector serves this resource.', retryable: true } };
      }
      if (request.operation !== 'send') {
        if (connector.operations?.includes(request.operation) !== true || connector.operate === undefined) {
          return { state: 'aborted', error: { code: 'unsupported_delivery', message: `The bridge does not implement operation ${request.operation}.`, retryable: false } };
        }
        try {
          return settleOutcome(await connector.operate(request.operation, request.data ?? {}));
        } catch {
          return { state: 'unknown', error: { code: 'unavailable', message: 'The platform call failed without an answer.', retryable: true } };
        }
      }
      const raw = requireObject(request.data, 'send');
      const data = (resource.data ?? {}) as Record<string, Json>;
      const conversation = typeof data.conversation === 'string'
        ? data.conversation
        : undefined;
      if (conversation === undefined) {
        return { state: 'aborted', error: { code: 'invalid_request', message: 'The resource names no conversation coordinate.', retryable: false } };
      }
      let replyTo: string | undefined;
      if (typeof raw.reply_to_event === 'string') {
        const handle = await handleOf(ref, raw.reply_to_event);
        if (handle === undefined) {
          return { state: 'aborted', error: { code: 'not_found', message: 'The reply references an event this node never received.', retryable: false } };
        }
        replyTo = handle.ref_handle ?? handle.message_id;
      }
      let outcome;
      // A path the host can already read passes through; inline bytes become a path here.
      const attachments: Array<{ kind: 'image' | 'file' | 'audio' | 'video' | 'sticker'; path: string; alt?: string }> = [];
      for (const item of Array.isArray(raw.attachments) ? raw.attachments : []) {
        const entry = item as Record<string, Json>;
        const kind = entry.kind as 'image' | 'file' | 'audio' | 'video' | 'sticker';
        const alt = typeof entry.alt === 'string' ? entry.alt : undefined;
        if (typeof entry.path === 'string' && entry.path.length > 0) {
          attachments.push({ kind, path: entry.path, ...(alt === undefined ? {} : { alt }) });
          continue;
        }
        if (options.writeAttachment === undefined) {
          return { state: 'aborted', error: { code: 'unsupported_delivery', message: 'This host cannot store inline attachments.', retryable: false } };
        }
        try {
          const path = await options.writeAttachment(ref, entry.name as string, bytesOf(entry.content as string));
          attachments.push({ kind, path, ...(alt === undefined ? {} : { alt }) });
        } catch (error) {
          if (error instanceof OnlyneError) throw error;
          return { state: 'aborted', error: { code: 'unavailable', message: 'The host could not store the attachment.', retryable: true } };
        }
      }
      try {
        outcome = await connector.send({
          conversation,
          text: typeof raw.text === 'string' ? raw.text : '',
          format: raw.format === 'markdown' ? 'markdown' : 'plain',
          attachments,
          ...(replyTo === undefined ? {} : { reply_to: replyTo }),
          ...(raw.card === undefined ? {} : { card: raw.card }),
          ...(raw.reply_in_thread === true ? { reply_in_thread: true } : {}),
          request_id: request.operation_id,
        });
      } catch {
        return { state: 'unknown', error: { code: 'unavailable', message: 'The platform call failed without an answer.', retryable: true } };
      }
      return settleOutcome(outcome);
    },

    receive,

    /** The platform handles recorded for one inbound event, so a later reply can quote it. */
    async platformHandle(ref: ResourceRef, eventId: string): Promise<{ message_id: string; ref_handle: string } | undefined> {
      const row = await handleOf(ref, eventId);
      return row === undefined ? undefined : { message_id: row.message_id, ref_handle: row.ref_handle };
    },
  };
}
