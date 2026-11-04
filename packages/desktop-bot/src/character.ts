import { inflateSync } from "node:zlib";
import type { CharacterCard, Json, Lorebook, LorebookEntry } from './contracts.ts';

const PNG_SIGNATURE = [137, 80, 78, 71, 13, 10, 26, 10];

const text = (value: unknown): string | undefined => typeof value === 'string' ? value : undefined;
const strings = (value: unknown): string[] | undefined =>
  Array.isArray(value) && value.every(item => typeof item === 'string') ? value as string[] : undefined;
const record = (value: unknown): Record<string, unknown> | undefined =>
  value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : undefined;

/** SillyTavern V1, V2 and V3 cards carry the same fields under `data` (V2+) or at the root (V1). */
export function parseCharacterJson(value: unknown): { character: CharacterCard; lorebook?: Lorebook } {
  const root = record(value);
  if (root === undefined) throw new Error('The character JSON is not an object');
  const data = record(root.data) ?? root;
  const character: CharacterCard = {
    ...(text(data.name) === undefined ? {} : { name: text(data.name) }),
    description: text(data.description) ?? '',
    ...(text(data.personality) === undefined ? {} : { personality: text(data.personality) }),
    ...(text(data.scenario) === undefined ? {} : { scenario: text(data.scenario) }),
    ...(text(data.system_prompt) === undefined ? {} : { systemPrompt: text(data.system_prompt) }),
    ...(text(data.post_history_instructions) === undefined ? {} : { postHistoryInstructions: text(data.post_history_instructions) }),
    ...(text(data.mes_example) === undefined ? {} : { exampleDialogue: text(data.mes_example) }),
    ...(text(data.first_mes) === undefined ? {} : { firstMessage: text(data.first_mes) }),
    ...(strings(data.alternate_greetings) === undefined ? {} : { alternateGreetings: strings(data.alternate_greetings) }),
    ...(text(data.creator_notes) === undefined ? {} : { creatorNotes: text(data.creator_notes) }),
    ...(strings(data.tags) === undefined ? {} : { tags: strings(data.tags) }),
    source: 'sillytavern',
  };
  const book = record(data.character_book);
  const lorebook = book === undefined ? undefined : parseLorebook(book);
  return { character, ...(lorebook === undefined ? {} : { lorebook }) };
}

export function parseLorebook(value: unknown): Lorebook {
  const root = record(value);
  if (root === undefined) throw new Error('The lorebook is not an object');
  const raw = root.entries;
  const entries: LorebookEntry[] = [];
  if (Array.isArray(raw)) {
    for (const item of raw) {
      const entry = lorebookEntry(item);
      if (entry !== undefined) entries.push(entry);
    }
  } else if (record(raw) !== undefined) {
    for (const item of Object.values(raw as Record<string, unknown>)) {
      const entry = lorebookEntry(item);
      if (entry !== undefined) entries.push(entry);
    }
  }
  return { ...(text(root.name) === undefined ? {} : { name: text(root.name) }), entries };
}

/** A card saved from the editor: same field names as the app contract, not SillyTavern's. */
export function normalizeCharacter(value: unknown): CharacterCard {
  const item = record(value);
  if (item === undefined) throw new Error('A character must be an object');
  const description = text(item.description) ?? '';
  const optional = (field: string): string | undefined => {
    const result = text(item[field]);
    return result === undefined || result.length === 0 ? undefined : result;
  };
  const greetings = strings(item.alternateGreetings);
  return {
    ...(optional('name') === undefined ? {} : { name: optional('name') }),
    description,
    ...(optional('personality') === undefined ? {} : { personality: optional('personality') }),
    ...(optional('scenario') === undefined ? {} : { scenario: optional('scenario') }),
    ...(optional('systemPrompt') === undefined ? {} : { systemPrompt: optional('systemPrompt') }),
    ...(optional('postHistoryInstructions') === undefined ? {} : { postHistoryInstructions: optional('postHistoryInstructions') }),
    ...(optional('exampleDialogue') === undefined ? {} : { exampleDialogue: optional('exampleDialogue') }),
    ...(optional('firstMessage') === undefined ? {} : { firstMessage: optional('firstMessage') }),
    ...(greetings === undefined || greetings.length === 0 ? {} : { alternateGreetings: greetings }),
    ...(optional('creatorNotes') === undefined ? {} : { creatorNotes: optional('creatorNotes') }),
    ...(strings(item.tags) === undefined ? {} : { tags: strings(item.tags) }),
    ...(optional('avatar') === undefined ? {} : { avatar: optional('avatar') }),
    source: item.source === 'sillytavern' ? 'sillytavern' : 'manual',
  };
}

function lorebookEntry(value: unknown): LorebookEntry | undefined {
  const item = record(value);
  if (item === undefined) return undefined;
  const keys = strings(item.keys) ?? strings(item.key) ?? (text(item.key) === undefined ? [] : [text(item.key)!]);
  const content = text(item.content);
  if (content === undefined || content.length === 0) return undefined;
  const role = item.role === 1 || item.role === 'user' ? 'user' : item.role === 2 || item.role === 'assistant' ? 'assistant' : 'system';
  const order = typeof item.insertion_order === 'number' ? item.insertion_order : typeof item.order === 'number' ? item.order : 100;
  const depth = typeof item.depth === 'number' ? item.depth : 4;
  return {
    id: text(item.id) ?? crypto.randomUUID(), keys, content,
    ...(strings(item.secondary_keys) === undefined ? {} : { secondaryKeys: strings(item.secondary_keys) }),
    ...(typeof item.enabled === 'boolean' ? { enabled: item.enabled } : {}),
    ...(typeof item.constant === 'boolean' ? { constant: item.constant } : {}),
    order, depth, role,
  };
}

/** PNG text chunks: `chara` carries the card, `ccv3` the V3 form. iTXt/zTXt may be compressed. */
export function parsePngCharacter(bytes: Uint8Array): { character: CharacterCard; lorebook?: Lorebook } {
  if (bytes.length < 8 || PNG_SIGNATURE.some((byte, index) => bytes[index] !== byte)) {
    throw new Error('The file is not a PNG image');
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const decoder = new TextDecoder();
  let offset = 8;
  const texts = new Map<string, string>();
  while (offset + 12 <= bytes.length) {
    const length = view.getUint32(offset);
    const type = decoder.decode(bytes.subarray(offset + 4, offset + 8));
    const start = offset + 8;
    if (start + length + 4 > bytes.length) throw new Error('The PNG chunk runs past the file end');
    const body = bytes.subarray(start, start + length);
    if (type === 'tEXt') {
      const zero = body.indexOf(0);
      if (zero > 0) texts.set(decoder.decode(body.subarray(0, zero)), decoder.decode(body.subarray(zero + 1)));
    } else if (type === 'zTXt') {
      const zero = body.indexOf(0);
      if (zero > 0 && body[zero + 1] === 0) texts.set(decoder.decode(body.subarray(0, zero)), inflateSync(body.subarray(zero + 2)).toString('utf8'));
    } else if (type === 'iTXt') {
      const zero = body.indexOf(0);
      if (zero > 0) {
        const compressed = body[zero + 1] === 1;
        let cursor = zero + 3;
        const languageEnd = body.indexOf(0, cursor);
        cursor = languageEnd + 1;
        const translatedEnd = body.indexOf(0, cursor);
        cursor = translatedEnd + 1;
        const value = compressed ? inflateSync(body.subarray(cursor)).toString('utf8') : decoder.decode(body.subarray(cursor));
        texts.set(decoder.decode(body.subarray(0, zero)), value);
      }
    }
    if (type === 'IEND') break;
    offset = start + length + 4;
  }
  const payload = texts.get('ccv3') ?? texts.get('chara');
  if (payload === undefined) throw new Error('The PNG has no embedded character card');
  const json = Buffer.from(payload, 'base64').toString('utf8');
  return parseCharacterJson(JSON.parse(json));
}

/** V2 export: the same field names SillyTavern reads, without its private extensions. */
export function characterToJson(botName: string, card: CharacterCard, lorebook?: Lorebook): Json {
  const data: Record<string, Json> = {
    name: card.name ?? botName,
    description: card.description,
    personality: card.personality ?? '',
    scenario: card.scenario ?? '',
    first_mes: card.firstMessage ?? '',
    mes_example: card.exampleDialogue ?? '',
    system_prompt: card.systemPrompt ?? '',
    post_history_instructions: card.postHistoryInstructions ?? '',
    alternate_greetings: card.alternateGreetings ?? [],
    creator_notes: card.creatorNotes ?? '',
    tags: card.tags ?? [],
    creator: 'Onlyne Desktop Bot',
    character_version: '1.0',
    extensions: {},
  };
  if (lorebook !== undefined) data.character_book = lorebookToJson(lorebook);
  return { spec: 'chara_card_v2', spec_version: '2.0', data } as Json;
}

export function lorebookToJson(lorebook: Lorebook): Json {
  return {
    ...(lorebook.name === undefined ? {} : { name: lorebook.name }),
    entries: lorebook.entries.map(entry => ({
      id: entry.id, keys: entry.keys, secondary_keys: entry.secondaryKeys ?? [], content: entry.content,
      enabled: entry.enabled !== false, constant: entry.constant === true, insertion_order: entry.order ?? 100,
      depth: entry.depth ?? 4, role: entry.role === 'user' ? 1 : entry.role === 'assistant' ? 2 : 0,
    })),
  } as Json;
}

/** Constant entries always apply; keyword entries apply on a case-insensitive substring match. */
export function matchLorebook(lorebook: Lorebook | undefined, text: string): LorebookEntry[] {
  if (lorebook === undefined) return [];
  const haystack = text.toLowerCase();
  return lorebook.entries
    .filter(entry => entry.enabled !== false)
    .filter(entry => entry.constant === true || entry.keys.some(key => key.length > 0 && haystack.includes(key.toLowerCase())))
    .filter(entry => entry.secondaryKeys === undefined || entry.secondaryKeys.length === 0
      || entry.secondaryKeys.some(key => key.length > 0 && haystack.includes(key.toLowerCase())))
    .sort((left, right) => (left.order ?? 100) - (right.order ?? 100));
}
