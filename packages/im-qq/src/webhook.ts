/**
 * QQ's callback authentication: Ed25519 over an app-secret-derived seed. The platform derives the
 * same key pair from the AppSecret, which is why the secret alone both signs the `op 13` challenge
 * and verifies every `op 0` dispatch.
 */

import { createPrivateKey, createPublicKey, sign, verify, type KeyObject } from 'node:crypto';

/** PKCS#8 header for an Ed25519 private key: the 32-byte seed follows it. */
const PKCS8_ED25519_PREFIX = Buffer.from('302e020100300506032b657004220420', 'hex');
const SEED_LENGTH = 32;
/** An Ed25519 SPKI ends with the 32-byte public key after this many header bytes. */
const SPKI_HEADER_LENGTH = 12;
const SIGNATURE_LENGTH = 64;
const CACHE_LIMIT = 8;

export interface QqEd25519Keys {
  privateKey: KeyObject;
  publicKey: KeyObject;
  /** The raw 32-byte public key, hex: what QQ's console shows for the app. */
  publicHex: string;
}

/** Key derivation costs a scalar multiplication, so one secret's pair is derived once. */
const cache = new Map<string, QqEd25519Keys>();

export function deriveEd25519(secret: string): QqEd25519Keys {
  const cached = cache.get(secret);
  if (cached !== undefined) return cached;
  if (secret.length === 0) throw new Error('The QQ app secret is empty; no Ed25519 key can be derived.');
  const seed = Buffer.from(secret.repeat(Math.ceil(SEED_LENGTH / secret.length)), 'utf8').subarray(0, SEED_LENGTH);
  const privateKey = createPrivateKey({
    key: Buffer.concat([PKCS8_ED25519_PREFIX, seed]),
    format: 'der',
    type: 'pkcs8',
  });
  const publicKey = createPublicKey(privateKey);
  const publicHex = publicKey.export({ format: 'der', type: 'spki' }).subarray(SPKI_HEADER_LENGTH).toString('hex');
  const keys: QqEd25519Keys = { privateKey, publicKey, publicHex };
  if (cache.size >= CACHE_LIMIT) {
    const oldest = cache.keys().next();
    if (!oldest.done) cache.delete(oldest.value);
  }
  cache.set(secret, keys);
  return keys;
}

export function signHex(secret: string, message: string | Buffer): string {
  // The platform signs the raw body bytes; QQ messages are UTF-8, so the string form must encode
  // the same way. For the ASCII-only challenge the two encodings agree byte for byte.
  const data = typeof message === 'string' ? Buffer.from(message, 'utf8') : message;
  return sign(null, data, deriveEd25519(secret).privateKey).toString('hex');
}

export function verifyHex(secret: string, message: string | Buffer, signatureHex: string): boolean {
  const signature = Buffer.from(signatureHex, 'hex');
  // A malformed hex string decodes short rather than throwing; the length is the real check.
  if (signature.length !== SIGNATURE_LENGTH) return false;
  const data = typeof message === 'string' ? Buffer.from(message, 'utf8') : message;
  try {
    return verify(null, data, deriveEd25519(secret).publicKey, signature);
  } catch {
    return false;
  }
}

/**
 * The `op 13` callback validation: QQ asks the bot to prove it holds the AppSecret by signing
 * `event_ts + plain_token`. The answer echoes the token with the signature.
 */
export function challenge(secret: string, bodyJson: string): { plain_token: string; signature: string } | undefined {
  let payload: unknown;
  try {
    payload = JSON.parse(bodyJson);
  } catch {
    return undefined;
  }
  if (payload === null || typeof payload !== 'object' || Array.isArray(payload)) return undefined;
  const envelope = payload as Record<string, unknown>;
  if (envelope.op !== 13) return undefined;
  const d = envelope.d;
  if (d === null || typeof d !== 'object' || Array.isArray(d)) return undefined;
  const fields = d as Record<string, unknown>;
  const plainToken = fields.plain_token;
  const eventTs = fields.event_ts;
  if (typeof plainToken !== 'string' || plainToken.length === 0) return undefined;
  const timestamp = typeof eventTs === 'string' ? eventTs : typeof eventTs === 'number' ? String(eventTs) : undefined;
  if (timestamp === undefined || timestamp.length === 0) return undefined;
  return { plain_token: plainToken, signature: signHex(secret, `${timestamp}${plainToken}`) };
}
