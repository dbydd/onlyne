import { createHash, createPrivateKey, createPublicKey, generateKeyPairSync, sign, verify } from 'node:crypto';
import { canonical, fail, key, OnlyneError } from './core.js';
import type { Context } from './core.js';
import type { Auth, KeyIdentity, KeyMaterial, NodeId, TrustEntry, TrustStatement } from './types.js';

export const SIGNATURE_HEADER = 'Onlyne-Sig';
export const CLOCK_SKEW_MS = 300_000;

function hash(input: string | Uint8Array): string { return createHash('sha256').update(input).digest('hex'); }
function privateKey(material: KeyMaterial) {
  return createPrivateKey({ key: Buffer.from(material.private_key), format: 'der', type: 'pkcs8' });
}
function publicKey(encoded: string) {
  try {
    const raw = Buffer.from(encoded, 'base64');
    if (raw.length !== 32 || raw.toString('base64') !== encoded) fail('unauthenticated', 'Invalid Ed25519 public key.');
    return createPublicKey({ key: { kty: 'OKP', crv: 'Ed25519', x: raw.toString('base64url') }, format: 'jwk' });
  } catch (error) {
    if (error instanceof OnlyneError) throw error;
    return fail('unauthenticated', 'Invalid Ed25519 public key.');
  }
}
function signatureValid(encoded: string, signature: string, payload: string): boolean {
  try { return verify(null, Buffer.from(payload), publicKey(encoded), Buffer.from(signature, 'base64')); }
  catch { return false; }
}
export async function keyFingerprint(raw: Uint8Array): Promise<string> { return hash(raw).slice(0, 16); }

export function generateKeyMaterial(): KeyMaterial {
  const pair = generateKeyPairSync('ed25519');
  return {
    private_key: new Uint8Array(pair.privateKey.export({ format: 'der', type: 'pkcs8' })),
    public_key: new Uint8Array(Buffer.from(pair.publicKey.export({ format: 'jwk' }).x!, 'base64url')),
  };
}

export class KeyService {
  #material?: Promise<KeyMaterial>;
  constructor(private readonly ctx: Context) {}
  get enabled(): boolean { return this.ctx.options.plugins.keys !== undefined; }
  keyId(): NodeId { return this.ctx.node; }
  freshTimestamp(): { timestamp: string; nonce: string } {
    return { timestamp: String(this.ctx.now()), nonce: this.ctx.id() };
  }
  async material(): Promise<KeyMaterial> {
    const hook = this.ctx.options.plugins.keys;
    if (hook === undefined) fail('unsupported_delivery', 'The host supplied no identity key.');
    this.#material ??= hook().then(material => {
      const derived = Buffer.from(createPublicKey(privateKey(material)).export({ format: 'jwk' }).x!, 'base64url');
      if (!derived.equals(Buffer.from(material.public_key))) fail('unauthenticated', 'The private and public keys do not match.');
      return material;
    });
    return this.#material;
  }
  async describe(): Promise<KeyIdentity | undefined> {
    if (!this.enabled) return undefined;
    const material = await this.material();
    return { node_id: this.ctx.node, public_key: Buffer.from(material.public_key).toString('base64'), fingerprint: await keyFingerprint(material.public_key) };
  }
  async sign(payload: string): Promise<string> {
    return sign(null, Buffer.from(payload), privateKey(await this.material())).toString('base64');
  }
  async generate(auth: Auth): Promise<KeyIdentity> {
    this.ctx.owner(auth.actor);
    const hook = this.ctx.options.plugins.keys;
    if (hook?.replace === undefined) fail('unsupported_delivery', 'The host does not support key replacement.');
    if ((await this.ctx.storage.transaction(tx => tx.scan<TrustEntry>('trust/'))).length > 0) fail('conflict', 'Use trust.rotate to replace an established identity.');
    await hook.replace(generateKeyMaterial());
    this.#material = undefined;
    return (await this.describe())!;
  }
  async replace(material: KeyMaterial): Promise<void> {
    const hook = this.ctx.options.plugins.keys;
    if (hook?.replace === undefined) fail('unsupported_delivery', 'The host does not support key replacement.');
    await hook.replace(material);
    this.#material = undefined;
  }
}

function statementPayload(statement: TrustStatement): string {
  const { signature: _signature, previous_signature: _previous, ...body } = statement;
  return `onlyne-trust-v1\n${canonical(body)}`;
}

export function requestPayload(request: Request, target: NodeId, digest: string, timestamp: string, nonce: string): string {
  const url = new URL(request.url);
  // Reverse proxies preserve the service path and query. The stable recipient replaces the external authority.
  return canonical({ version: 'onlyne-request-v1', actor: request.headers.get('Onlyne-Actor'), target,
    method: request.method, path: url.pathname + url.search, digest, timestamp, nonce,
    content_type: request.headers.get('content-type'), idempotency_key: request.headers.get('idempotency-key'),
    retry_until: request.headers.get('x-onlyne-retry-until'), last_event_id: request.headers.get('last-event-id') });
}
function responsePayload(target: NodeId, nonce: string, status: number, digest: string): string {
  return canonical({ version: 'onlyne-response-v1', target, nonce, status, digest });
}
export function bodyDigest(body: Uint8Array | string): string { return hash(body); }

export class TrustService {
  constructor(private readonly ctx: Context, private readonly keys: KeyService) {}
  private owner(auth: Auth): void { this.ctx.owner(auth.actor); }
  async list(auth: Auth): Promise<TrustEntry[]> {
    this.owner(auth);
    return (await this.ctx.storage.transaction(tx => tx.scan<TrustEntry>('trust/'))).map(row => row.value).sort((a, b) => a.node_id.localeCompare(b.node_id));
  }
  async entry(node: NodeId): Promise<TrustEntry | undefined> { return this.ctx.storage.transaction(tx => tx.get<TrustEntry>(key('trust', node))); }
  private async identity(): Promise<KeyIdentity> {
    const identity = await this.keys.describe();
    if (identity === undefined) fail('unsupported_delivery', 'Key exchange requires a local identity key.');
    return identity;
  }
  private address(value: string): URL {
    let url: URL;
    try { url = new URL(value); } catch { return fail('invalid_request', 'Expected an absolute node service URL.'); }
    if (url.username || url.password || url.search || url.hash || (url.protocol !== 'https:' && !(url.protocol === 'http:' && ['127.0.0.1', 'localhost', '[::1]'].includes(url.hostname)))) {
      fail('invalid_request', 'Node addresses require HTTPS; HTTP is limited to loopback.');
    }
    return url;
  }
  async inspect(node: NodeId): Promise<KeyIdentity> {
    const target = await this.ctx.options.plugins.resolve(node);
    if (target.node_id !== node) fail('unauthenticated', 'Resolver identity mismatch.');
    const base = this.address(target.base_url);
    const response = await fetch(new URL('./keys', base.href.replace(/\/?$/, '/')), { redirect: 'error', signal: AbortSignal.timeout(this.ctx.options.wait_ms ?? 30_000) });
    if (!response.ok) fail('unavailable', `Key lookup answered HTTP ${response.status}.`);
    const identity = await response.json() as KeyIdentity;
    if (typeof identity.public_key !== 'string') fail('unauthenticated', 'The peer supplied no public key.');
    publicKey(identity.public_key);
    if (identity.node_id !== node || identity.fingerprint !== await keyFingerprint(Buffer.from(identity.public_key, 'base64'))) fail('unauthenticated', 'The peer identity or fingerprint is inconsistent.');
    return identity;
  }
  private async statement(peer: KeyIdentity, approved: boolean, old?: KeyMaterial): Promise<TrustStatement> {
    const own = await this.identity();
    const value: TrustStatement = { node_id: this.ctx.node, target: peer.node_id, public_key: own.public_key,
      target_fingerprint: peer.fingerprint, base_url: this.ctx.baseUrl, approved,
      nonce: this.ctx.id(), timestamp: this.ctx.now(), signature: '' };
    value.signature = await this.keys.sign(statementPayload(value));
    if (old !== undefined) value.previous_signature = sign(null, Buffer.from(statementPayload(value)), privateKey(old)).toString('base64');
    return value;
  }
  private async send(peer: KeyIdentity, approved: boolean, old?: KeyMaterial): Promise<TrustEntry> {
    const target = await this.ctx.options.plugins.resolve(peer.node_id);
    if (target.node_id !== peer.node_id) fail('unauthenticated', 'Resolver identity mismatch.');
    const base = this.address(target.base_url);
    const response = await fetch(new URL('./introductions', base.href.replace(/\/?$/, '/')), {
      method: 'POST', headers: { 'content-type': 'application/json' }, redirect: 'error',
      body: JSON.stringify(await this.statement(peer, approved, old)), signal: AbortSignal.timeout(this.ctx.options.wait_ms ?? 30_000),
    });
    const result = await response.json() as TrustStatement & { error?: { code: string; message: string } };
    if (!response.ok) fail('unauthenticated', result.error?.message ?? 'The peer rejected the exchange.');
    if (result.node_id !== peer.node_id || result.public_key !== peer.public_key) fail('unauthenticated', 'The exchange answered with a different identity.');
    return this.receive(result);
  }
  async request(auth: Auth, node: NodeId, fingerprint: string): Promise<TrustEntry> {
    this.owner(auth);
    const peer = await this.inspect(node);
    if (peer.fingerprint !== fingerprint) fail('unauthenticated', 'The peer key changed after confirmation.', { fingerprint: peer.fingerprint });
    await this.ctx.storage.transaction(async tx => {
      const slot = key('trust', node);
      const previous = await tx.get<TrustEntry>(slot);
      if (previous !== undefined && previous.public_key !== peer.public_key && previous.state !== 'revoked') fail('id_conflict', 'A different peer key is already pinned.');
      await tx.put(slot, { ...peer, state: 'pending', base_url: '', local_approved: true, remote_approved: previous?.remote_approved ?? false,
        first_seen: previous?.first_seen ?? new Date(this.ctx.now()).toISOString(), last_checked: new Date(this.ctx.now()).toISOString() } satisfies TrustEntry);
    });
    return this.send(peer, true);
  }
  async accept(auth: Auth, node: NodeId, fingerprint: string): Promise<TrustEntry> { return this.request(auth, node, fingerprint); }

  async receive(value: TrustStatement): Promise<TrustEntry> {
    const own = await this.identity();
    if (value === null || typeof value !== 'object' || typeof value.node_id !== 'string' || value.node_id === this.ctx.node
      || value.target !== this.ctx.node || value.target_fingerprint !== own.fingerprint
      || typeof value.nonce !== 'string' || value.nonce.length < 1 || value.nonce.length > 128
      || typeof value.signature !== 'string' || typeof value.public_key !== 'string' || typeof value.approved !== 'boolean'
      || !Number.isSafeInteger(value.timestamp) || Math.abs(this.ctx.now() - value.timestamp) > CLOCK_SKEW_MS) fail('unauthenticated', 'Invalid or expired key exchange transcript.');
    this.address(value.base_url);
    if (!signatureValid(value.public_key, value.signature, statementPayload(value))) fail('unauthenticated', 'Invalid key exchange signature.');
    const fingerprint = await keyFingerprint(Buffer.from(value.public_key, 'base64'));
    const seed = this.ctx.options.plugins.trust_seeds?.find(row => row.node_id === value.node_id);
    if (seed !== undefined && seed.fingerprint !== fingerprint) fail('unauthenticated', 'The peer differs from its pinned fingerprint.', { fingerprint });
    return this.ctx.storage.transaction(async tx => {
      await this.consumeNonce(tx, value.node_id, `exchange:${value.nonce}`, value.timestamp);
      const slot = key('trust', value.node_id);
      const previous = await tx.get<TrustEntry>(slot);
      if (previous?.state === 'revoked') fail('forbidden', 'This peer was revoked. Local approval is required.');
      const changed = previous !== undefined && previous.public_key !== value.public_key;
      if (changed && (value.previous_signature === undefined || !signatureValid(previous.public_key, value.previous_signature, statementPayload(value)))) fail('id_conflict', 'The peer key changed without proof from the previous key.');
      const local = seed !== undefined || (changed ? false : previous?.local_approved === true);
      const entry: TrustEntry = { node_id: value.node_id, public_key: value.public_key, fingerprint,
        state: local && value.approved ? 'trusted' : 'pending', local_approved: local, remote_approved: value.approved,
        base_url: value.base_url, first_seen: previous?.first_seen ?? new Date(this.ctx.now()).toISOString(), last_checked: new Date(this.ctx.now()).toISOString(),
        ...(changed ? { history: [...(previous.history ?? []), previous.fingerprint] } : previous?.history === undefined ? {} : { history: previous.history }) };
      await tx.put(slot, entry);
      return entry;
    });
  }
  async introduce(value: TrustStatement): Promise<TrustStatement> {
    const entry = await this.receive(value);
    return this.statement(entry, entry.local_approved);
  }
  async revoke(auth: Auth, node: NodeId): Promise<void> {
    this.owner(auth);
    await this.ctx.storage.transaction(async tx => {
      const slot = key('trust', node);
      const previous = await tx.get<TrustEntry>(slot);
      if (previous !== undefined) await tx.put(slot, { ...previous, state: 'revoked', public_key: '', local_approved: false, remote_approved: false });
    });
  }
  async rotate(auth: Auth): Promise<KeyIdentity> {
    this.owner(auth);
    const previous = await this.keys.material();
    const peers = (await this.list(auth)).filter(row => row.state === 'trusted');
    // The journal keeps the previous signing key until every peer has received its rotation proof.
    const slot = 'security/rotation';
    let journal = await this.ctx.storage.transaction(tx => tx.get<{ previous: number[]; peers: TrustEntry[] }>(slot));
    if (journal === undefined) {
      journal = { previous: Array.from(previous.private_key), peers };
      await this.ctx.storage.transaction(tx => tx.put(slot, journal));
      await this.keys.replace(generateKeyMaterial());
    }
    const old: KeyMaterial = { private_key: new Uint8Array(journal.previous), public_key: previous.public_key };
    for (const peer of journal.peers.slice()) {
      await this.send(peer, true, old);
      journal.peers = journal.peers.filter(row => row.node_id !== peer.node_id);
      await this.ctx.storage.transaction(tx => tx.put(slot, journal));
    }
    await this.ctx.storage.transaction(tx => tx.delete(slot));
    return this.identity();
  }
  private async consumeNonce(tx: import('./types.js').Transaction, actor: string, nonce: string, timestamp: number): Promise<void> {
    const slot = key('nonce', actor, nonce);
    if (await tx.get(slot) !== undefined) fail('unauthenticated', 'This nonce was already used.');
    for (const row of await tx.scan<number>('nonce/')) if (row.value < this.ctx.now() - CLOCK_SKEW_MS) await tx.delete(row.key);
    await tx.put(slot, timestamp);
  }
  async signRequest(request: Request, target: NodeId, raw: Uint8Array | string): Promise<void> {
    const { timestamp, nonce } = this.keys.freshTimestamp();
    request.headers.set('Onlyne-Actor', this.ctx.node);
    const signature = await this.keys.sign(requestPayload(request, target, hash(raw), timestamp, nonce));
    request.headers.set(SIGNATURE_HEADER, `v1 ${encodeURIComponent(this.ctx.node)} ${timestamp} ${nonce} ${signature}`);
  }
  async authenticate(request: Request, digest: string): Promise<Auth> {
    const identity = await this.identity();
    const details = { ...identity };
    const parts = request.headers.get(SIGNATURE_HEADER)?.split(' ');
    if (parts?.length !== 5 || parts[0] !== 'v1') fail('unauthenticated', 'A node signature is required.', details);
    let actor: string;
    try { actor = decodeURIComponent(parts[1]!); } catch { return fail('unauthenticated', 'Invalid signing identity.', details); }
    const timestamp = parts[2]!, nonce = parts[3]!, signature = parts[4]!;
    if (!Number.isSafeInteger(Number(timestamp)) || Math.abs(this.ctx.now() - Number(timestamp)) > CLOCK_SKEW_MS || !/^[a-zA-Z0-9-]{1,128}$/.test(nonce)) fail('unauthenticated', 'Invalid signature timestamp or nonce.', details);
    const entry = await this.entry(actor);
    if (entry?.state !== 'trusted' || actor === this.ctx.node || request.headers.get('Onlyne-Actor') !== actor) fail('unauthenticated', 'The caller is not a trusted peer.', details);
    if (!signatureValid(entry.public_key, signature, requestPayload(request, this.ctx.node, digest, timestamp, nonce))) fail('unauthenticated', 'Invalid request signature.', details);
    await this.ctx.storage.transaction(async tx => {
      if ((await tx.get<TrustEntry>(key('trust', actor)))?.state !== 'trusted') fail('unauthenticated', 'The caller was revoked.', details);
      await this.consumeNonce(tx, actor, nonce, Number(timestamp));
    });
    return { actor };
  }
  async signResponse(request: Request, response: Response, body: string): Promise<void> {
    const nonce = request.headers.get(SIGNATURE_HEADER)?.split(' ')[3] ?? '';
    response.headers.set('Onlyne-Response-Sig', await this.keys.sign(responsePayload(this.ctx.node, nonce, response.status, hash(body))));
  }
  async verifyResponse(target: NodeId, request: Request, response: Response, body: string): Promise<void> {
    const peer = await this.entry(target);
    const nonce = request.headers.get(SIGNATURE_HEADER)?.split(' ')[3] ?? '';
    if (peer?.state !== 'trusted' || !signatureValid(peer.public_key, response.headers.get('Onlyne-Response-Sig') ?? '', responsePayload(target, nonce, response.status, hash(body)))) fail('unauthenticated', 'The response does not prove the expected node identity.');
  }
}
