import { randomBytes } from 'node:crypto';

const ALPHABET = '0123456789abcdefghijklmnopqrstuvwxyz';

/**
 * Short identifiers for values a model or a person may read back: node ids, bot ids,
 * conversations, media. Eight base36 characters are enough to avoid collisions in one app
 * instance and short enough to repeat correctly.
 */
export function shortId(prefix = ''): string {
  const bytes = randomBytes(8);
  let value = '';
  for (const byte of bytes) value += ALPHABET[byte % 36];
  return prefix.length === 0 ? value : `${prefix}-${value}`;
}

export function slug(text: string, fallback = 'bot'): string {
  const value = text.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 24);
  return value.length === 0 ? fallback : value;
}

/** A readable, stable URN for a desktop bot: short enough to copy without transcription errors. */
export function botNodeId(botId: string): string {
  return `urn:onlyne:agent:desktop:${slug(botId)}-${shortId()}`;
}
