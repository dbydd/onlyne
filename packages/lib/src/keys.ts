import { createHash, createPrivateKey, createPublicKey, generateKeyPairSync, sign, verify } from 'node:crypto';
import { canonical, fail, key, OnlyneError } from './core.ts';
import type { Context } from './core.ts';
import type { Auth, ErrorCode, KeyIdentity, KeyMaterial, KeyExchange, NodeId, NotifyEvent, TrustEntry, TrustStatement } from './types.ts';
import { trustPhase } from './types.ts';

export const SIGNATURE_HEADER = 'Onlyne-Sig';
export const CLOCK_SKEW_MS = 300_000;
/** How long a failed exchange send waits before the maintenance loop tries it again. */
export const EXCHANGE_RETRY_MS = 60_000;
/** How long a refused statement waits before the loop tries again (unverified refusal). */
export const REFUSED_RETRY_MS = 600_000;

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
  readonly #ctx: Context;

  constructor(ctx: Context) {
    this.#ctx = ctx;
  }
  get enabled(): boolean { return this.#ctx.options.plugins.keys !== undefined; }
  keyId(): NodeId { return this.#ctx.node; }
  freshTimestamp(): { timestamp: string; nonce: string } {
    return { timestamp: String(this.#ctx.now()), nonce: this.#ctx.id() };
  }
  async material(): Promise<KeyMaterial> {
    const hook = this.#ctx.options.plugins.keys;
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
    return { node_id: this.#ctx.node, public_key: Buffer.from(material.public_key).toString('base64'), fingerprint: await keyFingerprint(material.public_key) };
  }
  async sign(payload: string): Promise<string> {
    return sign(null, Buffer.from(payload), privateKey(await this.material())).toString('base64');
  }
  async generate(auth: Auth): Promise<KeyIdentity> {
    this.#ctx.owner(auth.actor);
    const hook = this.#ctx.options.plugins.keys;
    if (hook?.replace === undefined) fail('unsupported_delivery', 'The host does not support key replacement.');
    if ((await this.#ctx.storage.transaction(tx => tx.scan<TrustEntry>('trust/'))).length > 0) fail('conflict', 'Use trust.rotate to replace an established identity.');
    await hook.replace(generateKeyMaterial());
    this.#material = undefined;
    return (await this.describe())!;
  }
  async replace(material: KeyMaterial): Promise<void> {
    const hook = this.#ctx.options.plugins.keys;
    if (hook?.replace === undefined) fail('unsupported_delivery', 'The host does not support key replacement.');
    try {
      await hook.replace(material);
    } finally {
      // The host may have swapped the stored key and then failed. Re-read it: a rotation
      // window is only abandoned when the host still reports the old key.
      this.#material = undefined;
    }
  }
}

function statementPayload(statement: TrustStatement): string {
  const { signature: _signature, previous_signature: _previous, ...body } = statement;
  return `onlyne-trust-v1\n${canonical(body)}`;
}

/**
 * One owed delivery of a signed trust statement (RFC 0017 §3.5). The obligation is
 * independent of the row's stage: an approval that reached the peer as `exchanged`
 * still owes nothing, while an approval whose reply was lost owes a re-send.
 */
interface TrustOutbound {
  node_id: NodeId;
  /** `approval` carries the current approval; `rotation` carries the old-key proof. */
  kind: 'approval' | 'rotation';
  /** The nonce this attempt carries; the peer's reply must echo it to settle the obligation. */
  nonce: string;
  /** This node's public-key fingerprint when the obligation was created; a rotation supersedes it. */
  fingerprint: string;
  /** Last send attempt, epoch milliseconds. The retry window gates the next one. */
  at: number;
  /** Rotation only: the previous private key that proves the rotation to a peer that missed it. */
  previous?: number[];
  /**
   * When an unverified refusal arrived: the epoch millisecond at which the next attempt is
   * allowed. An unauthenticated error is a backoff, never a stop — a forged `forbidden` must
   * not be able to end the retry, and the peer may have nothing to send that would revive it.
   */
  blocked?: number;
}

/**
 * The rotation window (RFC 0017 §2). The key swap spans two stores — the obligation rows and
 * the host's key hook — so one record marks that a swap is in progress. `rotate` writes it
 * with the obligations in one transaction, and the maintenance loop reads it to tell
 * "not swapped yet" from "swapped, statement not delivered".
 */
interface RotationWindow {
  new_fingerprint: string;
  at: number;
}

/**
 * A refusal that ends the delivery obligation: the peer revoked this node, or its key
 * changed without proof, so the same statement can never succeed. Every other failure
 * keeps the obligation — a lost response and a failed verification are both retried by
 * the next window (RFC 0017 §3.5).
 */
function terminalRefusal(error: unknown): boolean {
  return error instanceof OnlyneError && (error.error.code === 'forbidden' || error.error.code === 'id_conflict');
}

/** Serializes key rotations inside one process; cross-process callers fail on the stored window. */
const RotationMutex = new class {
  #tail: Promise<unknown> = Promise.resolve();
  locked<T>(work: () => Promise<T>): Promise<T> {
    const run = () => work();
    const result = this.#tail.then(run, run);
    this.#tail = result.then(() => undefined, () => undefined);
    return result;
  }
}();

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
  readonly #ctx: Context;
  readonly #keys: KeyService;

  constructor(ctx: Context, keys: KeyService) {
    this.#ctx = ctx;
    this.#keys = keys;
  }
  /** The client's loop, so a committed trust change re-arms the cadence; set by the node. */
  #waker: { wakeLoop(): void } | undefined;
  /** @internal Called by OnlyneNode so trust commits reach the maintenance loop. */
  bindLoop(waker: { wakeLoop(): void }): void { this.#waker = waker; }
  private owner(auth: Auth): void { this.#ctx.owner(auth.actor); }
  async list(auth: Auth): Promise<TrustEntry[]> {
    this.owner(auth);
    return (await this.#ctx.storage.transaction(tx => tx.scan<TrustEntry>('trust/'))).map(row => row.value).sort((a, b) => a.node_id.localeCompare(b.node_id));
  }
  async entry(node: NodeId): Promise<TrustEntry | undefined> { return this.#ctx.storage.transaction(tx => tx.get<TrustEntry>(key('trust', node))); }
  /**
   * RFC 0017 §3: stage one of one exchange. Reads the peer's live `/keys`, so the
   * fingerprint the host shows is one the peer proved. Committing is a separate
   * explicit step on the returned session; nothing is recorded or sent here.
   */
  async begin(auth: Auth, node: NodeId): Promise<KeyExchange> {
    this.owner(auth);
    const peer = await this.inspect(node);
    const observed = peer.fingerprint;
    return {
      node_id: peer.node_id, fingerprint: peer.fingerprint, public_key: peer.public_key,
      base_url: await this.peerBase(node),
      // The confirmation is sealed to the fingerprint this session displayed: a host that
      // confirms a value it never showed, or a key that moved since, never reaches the wire.
      confirm: async (fingerprint: string): Promise<TrustEntry> => {
        if (fingerprint !== observed) fail('unauthenticated', 'The confirmed fingerprint is not the one begin displayed.', { fingerprint: observed });
        return this.request(auth, node, fingerprint);
      },
    };
  }
  private async identity(): Promise<KeyIdentity> {
    const identity = await this.#keys.describe();
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
    const base = this.address(await this.peerBase(node));
    const response = await fetch(new URL('./keys', base.href.replace(/\/?$/, '/')), { redirect: 'error', signal: AbortSignal.timeout(this.#ctx.options.wait_ms ?? 30_000) });
    if (!response.ok) fail('unavailable', `Key lookup answered HTTP ${response.status}.`);
    const identity = await response.json() as KeyIdentity;
    if (typeof identity.public_key !== 'string') fail('unauthenticated', 'The peer supplied no public key.');
    publicKey(identity.public_key);
    if (identity.node_id !== node || identity.fingerprint !== await keyFingerprint(Buffer.from(identity.public_key, 'base64'))) fail('unauthenticated', 'The peer identity or fingerprint is inconsistent.');
    return identity;
  }
  /**
   * The peer's address. Discovery answers for every peer this node has observed; a peer that
   * introduced itself before discovery ever saw it has no record at all, and the address its own
   * signed statement carried is then the only one there is. Without that fallback an inbound
   * invitation can never be accepted: the confirmation resolves the peer before it may answer,
   * so the row would sit at `invited` forever while both holders had approved.
   */
  private async peerBase(node: NodeId): Promise<string> {
    try {
      const target = await this.#ctx.options.plugins.resolve(node);
      if (target.node_id !== node) fail('unauthenticated', 'Resolver identity mismatch.');
      return target.base_url;
    } catch (error) {
      const recorded = (await this.entry(node))?.base_url;
      if (!(error instanceof OnlyneError) || error.error.code !== 'not_found' || recorded === undefined || recorded === '') throw error;
      return recorded;
    }
  }
  private async statement(peer: KeyIdentity, approved: boolean, nonce: string, replyTo?: string, old?: Uint8Array): Promise<TrustStatement> {
    const own = await this.identity();
    const value: TrustStatement = { node_id: this.#ctx.node, target: peer.node_id, public_key: own.public_key,
      target_fingerprint: peer.fingerprint, base_url: this.#ctx.baseUrl, approved,
      nonce, timestamp: this.#ctx.now(), signature: '' };
    if (replyTo !== undefined) value.reply_nonce = replyTo;
    value.signature = await this.#keys.sign(statementPayload(value));
    if (old !== undefined) {
      const key = createPrivateKey({ key: Buffer.from(old), format: 'der', type: 'pkcs8' });
      value.previous_signature = sign(null, Buffer.from(statementPayload(value)), key).toString('base64');
    }
    return value;
  }
  /**
   * Sends one statement and settles the matching obligation only on a reply that proves
   * it answered this attempt (RFC 0017 §3.5). A missing, unsigned or mismatched reply
   * leaves the obligation owed, so the maintenance loop re-sends with a fresh nonce.
   */
  private async send(peer: KeyIdentity, approved: boolean, attempt: { nonce: string; old?: Uint8Array }): Promise<TrustEntry> {
    const base = this.address(await this.peerBase(peer.node_id));
    const statement = await this.statement(peer, approved, attempt.nonce, undefined, attempt.old);
    const response = await fetch(new URL('./introductions', base.href.replace(/\/?$/, '/')), {
      method: 'POST', headers: { 'content-type': 'application/json' }, redirect: 'error',
      body: JSON.stringify(statement), signal: AbortSignal.timeout(this.#ctx.options.wait_ms ?? 30_000),
    });
    const result = await response.json() as TrustStatement & { error?: { code: ErrorCode; message: string; retryable?: boolean } };
    if (!response.ok) {
      // Keep the peer's own code: a revoked peer answers `forbidden` and a failed
      // verification answers `unauthenticated`, and only the first ends the obligation.
      const rejection = new OnlyneError(result.error?.code ?? 'unauthenticated', result.error?.message ?? 'The peer rejected the exchange.');
      if (typeof result.error?.retryable === 'boolean') rejection.error.retryable = result.error.retryable;
      throw rejection;
    }
    if (result.node_id !== peer.node_id || result.public_key !== peer.public_key) fail('unauthenticated', 'The exchange answered with a different identity.');
    if (result.reply_nonce !== statement.nonce) fail('unauthenticated', 'The exchange reply does not answer this introduction.');
    const entry = await this.receive(result);
    await this.settle(peer.node_id, statement.nonce);
    return entry;
  }
  /** Clears the obligation an answered attempt created; a newer attempt keeps its own. */
  private settle(node: NodeId, nonce: string): Promise<void> {
    return this.#ctx.storage.transaction(async tx => {
      const slot = key('trust-outbound', node);
      const owed = await tx.get<TrustOutbound>(slot);
      if (owed === undefined || owed.nonce !== nonce) return;
      await tx.delete(slot);
      if (owed.kind === 'rotation') await this.closeWindow(tx);
    });
  }
  /** The rotation window closes when its last statement is gone. */
  private async closeWindow(tx: import('./types.ts').Transaction): Promise<void> {
    const remaining = await tx.scan<TrustOutbound>(key('trust-outbound') + '/');
    if (remaining.every(row => row.value.kind !== 'rotation')) await tx.delete(key('security', 'rotation'));
  }
  /**
   * Rewrites or deletes one obligation only if the row is still the attempt this pass read.
   * `rotate` can replace an approval obligation with a rotation one mid-pass; a whole-row
   * write-back from the stale snapshot would drop the old-key proof it just stored.
   */
  private async compareObligation(node: NodeId, expected: TrustOutbound, next: TrustOutbound | null): Promise<boolean> {
    return this.#ctx.storage.transaction(async tx => {
      const slot = key('trust-outbound', node);
      const current = await tx.get<TrustOutbound>(slot);
      if (current === undefined || current.nonce !== expected.nonce || current.kind !== expected.kind) return false;
      if (next === null) await tx.delete(slot);
      else await tx.put(slot, next);
      return true;
    });
  }
  private rotationWindow(): Promise<RotationWindow | undefined> {
    return this.#ctx.storage.transaction(tx => tx.get<RotationWindow>(key('security', 'rotation')));
  }
  /** Backs off an obligation after a refusal this node cannot authenticate. */
  private park(node: NodeId, nonce: string): Promise<void> {
    return this.#ctx.storage.transaction(async tx => {
      const slot = key('trust-outbound', node);
      const owed = await tx.get<TrustOutbound>(slot);
      if (owed !== undefined && owed.nonce === nonce) await tx.put(slot, { ...owed, blocked: this.#ctx.now() + REFUSED_RETRY_MS });
    });
  }
  /** The swap never completed: drop the window and every statement prepared under it. */
  private abandonRotation(): Promise<void> {
    return this.#ctx.storage.transaction(async tx => {
      await tx.delete(key('security', 'rotation'));
      for (const row of await tx.scan<TrustOutbound>(key('trust-outbound') + '/')) {
        if (row.value.kind === 'rotation') await tx.delete(row.key);
      }
    });
  }
  async request(auth: Auth, node: NodeId, fingerprint: string): Promise<TrustEntry> {
    this.owner(auth);
    const peer = await this.inspect(node);
    if (peer.fingerprint !== fingerprint) fail('unauthenticated', 'The peer key changed after confirmation.', { fingerprint: peer.fingerprint });
    const own = await this.identity();
    const nonce = this.#ctx.id();
    const committed = await this.#ctx.storage.transaction(async tx => {
      const slot = key('trust', node);
      const before = await tx.get<TrustEntry>(slot);
      if (before !== undefined && before.public_key !== peer.public_key && before.state !== 'revoked') fail('id_conflict', 'A different peer key is already pinned.');
      // Mutual approval is already known locally — our approval now, theirs from the stored
      // statement — so the row is trusted at once. The send only confirms the transcript;
      // a failed re-send must not demote a working signature relation either. Revocation
      // cleared remote approval, so only a row that still carries it counts as mutual.
      const mutual = before?.public_key === peer.public_key && (before.state === 'trusted' || (before.state === 'pending' && before.remote_approved));
      const state: TrustEntry['state'] = mutual ? 'trusted' : 'pending';
      const entry: TrustEntry = { ...peer, state, base_url: before?.base_url ?? '', local_approved: true, remote_approved: before?.remote_approved ?? false,
        first_seen: before?.first_seen ?? new Date(this.#ctx.now()).toISOString(), last_checked: new Date(this.#ctx.now()).toISOString() };
      await tx.put(slot, entry);
      // A repeat confirmation of an established row is a refresh: timestamps move, nothing is
      // owed and nothing is sent, so a host that renders twice cannot spam the peer. While the
      // relation is still one-sided, the same call means "try again" and does owe a statement
      // (RFC 0017 §3.3).
      const fresh = before === undefined || before.state !== 'trusted' || !before.local_approved || before.public_key !== peer.public_key;
      // An unacknowledged rotation owns this peer's delivery: its statement carries the old-key
      // proof, and overwriting it with a plain approval would drop that proof for good. The
      // confirmation still refreshes the row; the rotation statement also carries the approval.
      const existing = await tx.get<TrustOutbound>(key('trust-outbound', node));
      const rotating = existing?.kind === 'rotation';
      if (fresh && !rotating) {
        // Committed with the row, so a crash between commit and send still owes the delivery;
        // only the peer's answering reply settles it.
        await tx.put(key('trust-outbound', node), { node_id: node, kind: 'approval', nonce, fingerprint: own.fingerprint, at: this.#ctx.now() } satisfies TrustOutbound);
      }
      return { before, entry, fresh, rotating };
    });
    // Accepting an invitation and initiating an exchange are different host events.
    const cause = committed.before?.remote_approved === true && committed.before.local_approved === false ? 'local-accept' : 'local-request';
    this.notify(cause, committed.entry, committed.before);
    if (!committed.fresh || committed.rotating) return committed.entry;
    return this.send(peer, true, { nonce }).catch(async (error: unknown) => {
      if (terminalRefusal(error)) await this.park(node, nonce);
      throw error;
    });
  }
  private notify(cause: NotifyEvent extends infer T ? T extends { cause: infer C } ? C : never : never, entry: TrustEntry, previous?: TrustEntry): void {
    try { this.#waker?.wakeLoop(); } catch { /* a loop fault never blocks the commit */ }
    this.#ctx.options.plugins.runtime?.notify?.({ event: 'trust', cause, phase: trustPhase(entry), entry, ...(previous === undefined ? {} : { previous }) });
  }
  async accept(auth: Auth, node: NodeId, fingerprint: string): Promise<TrustEntry> { return this.request(auth, node, fingerprint); }

  async receive(value: TrustStatement): Promise<TrustEntry> {
    const own = await this.identity();
    if (value === null || typeof value !== 'object' || typeof value.node_id !== 'string' || value.node_id === this.#ctx.node
      || value.target !== this.#ctx.node || value.target_fingerprint !== own.fingerprint
      || typeof value.nonce !== 'string' || value.nonce.length < 1 || value.nonce.length > 128
      || typeof value.signature !== 'string' || typeof value.public_key !== 'string' || typeof value.approved !== 'boolean'
      || !Number.isSafeInteger(value.timestamp) || Math.abs(this.#ctx.now() - value.timestamp) > CLOCK_SKEW_MS) fail('unauthenticated', 'Invalid or expired key exchange transcript.');
    this.address(value.base_url);
    if (!signatureValid(value.public_key, value.signature, statementPayload(value))) fail('unauthenticated', 'Invalid key exchange signature.');
    const fingerprint = await keyFingerprint(Buffer.from(value.public_key, 'base64'));
    const seed = this.#ctx.options.plugins.trust_seeds?.find(row => row.node_id === value.node_id);
    if (seed !== undefined && seed.fingerprint !== fingerprint) fail('unauthenticated', 'The peer differs from its pinned fingerprint.', { fingerprint });
    const settled = await this.#ctx.storage.transaction(async tx => {
      await this.consumeNonce(tx, value.node_id, `exchange:${value.nonce}`, value.timestamp);
      const slot = key('trust', value.node_id);
      const previous = await tx.get<TrustEntry>(slot);
      if (previous?.state === 'revoked') fail('forbidden', 'This peer was revoked. Local approval is required.');
      // RFC 0017 §3.3: with no recorded relationship there is nothing this statement could
      // answer, so a first-contact refusal is not a fact about either side.
      if (previous === undefined && !value.approved) fail('invalid_request', 'A first statement must carry the peer approval it claims.');
      const changed = previous !== undefined && previous.public_key !== value.public_key;
      if (changed && (value.previous_signature === undefined || !signatureValid(previous.public_key, value.previous_signature, statementPayload(value)))) {
        // No local repair exists: this node cannot learn which key the peer still pins, and the
        // old key is gone once its rotation window closed. Only revoke + re-confirm recovers
        // (RFC 0017 §2).
        fail('id_conflict', 'The peer key changed without proof from the previous key; revoke this peer and confirm again.', { fingerprint });
      }
      // A proven rotation keeps this node's approval: the old-key proof says the identity is
      // continuous, so the user does not re-approve (RFC 0017 §3.3). An unproven key change
      // never reaches this line — it fails as `id_conflict` above.
      const local = seed !== undefined || previous?.local_approved === true;
      const entry: TrustEntry = { node_id: value.node_id, public_key: value.public_key, fingerprint,
        state: local && value.approved ? 'trusted' : 'pending', local_approved: local, remote_approved: value.approved,
        base_url: value.base_url, first_seen: previous?.first_seen ?? new Date(this.#ctx.now()).toISOString(), last_checked: new Date(this.#ctx.now()).toISOString(),
        ...(changed ? { history: [...(previous.history ?? []), previous.fingerprint] } : previous?.history === undefined ? {} : { history: previous.history }) };
      await tx.put(slot, entry);
      // The peer spoke: whatever parked this node's own obligation may no longer hold.
      const owedSlot = key('trust-outbound', value.node_id);
      const owed = await tx.get<TrustOutbound>(owedSlot);
      if (owed?.blocked !== undefined) {
        const { blocked: _blocked, ...rest } = owed;
        await tx.put(owedSlot, rest);
      }
      return { entry, previous };
    });
    this.notify('peer-statement', settled.entry, settled.previous);
    return settled.entry;
  }
  async introduce(value: TrustStatement): Promise<TrustStatement> {
    const entry = await this.receive(value);
    // The reply echoes the request nonce: that is what lets the sender settle its obligation.
    // While this node's own rotation is unacknowledged, every statement carries the old-key
    // proof, so a peer still pinning the previous key can accept this reply (RFC 0017 §2).
    const owed = await this.#ctx.storage.transaction(tx => tx.get<TrustOutbound>(key('trust-outbound', value.node_id)));
    const old = owed?.previous === undefined ? undefined : new Uint8Array(owed.previous);
    return this.statement(entry, entry.local_approved, this.#ctx.id(), value.nonce, old);
  }
  async revoke(auth: Auth, node: NodeId): Promise<void> {
    this.owner(auth);
    const previous = await this.#ctx.storage.transaction(async tx => {
      const slot = key('trust', node);
      const previous = await tx.get<TrustEntry>(slot);
      if (previous !== undefined) await tx.put(slot, { ...previous, state: 'revoked', public_key: '', local_approved: false, remote_approved: false });
      // A revoked peer owes nothing: drop the statement, and close a rotation window it was
      // the last member of, or every later rotation would be refused.
      const owedSlot = key('trust-outbound', node);
      const owed = await tx.get<TrustOutbound>(owedSlot);
      if (owed !== undefined) {
        await tx.delete(owedSlot);
        if (owed.kind === 'rotation') await this.closeWindow(tx);
      }
      return previous;
    });
    if (previous !== undefined) this.notify('local-revoke', { ...previous, state: 'revoked', public_key: '', local_approved: false, remote_approved: false }, previous);
  }

  /**
   * RFC 0017 §3.5: re-drives every owed statement whose reply has not arrived. The stage
   * of the row is irrelevant — an `exchanged` row still owes the delivery when its reply
   * was lost. Each attempt uses a fresh nonce, so the peer's nonce cache cannot mistake
   * a re-send for a replay. The maintenance loop calls this; the host never does.
   */
  async redrive(signal?: AbortSignal): Promise<void> {
    if (!this.#keys.enabled) return;
    const owed = await this.#ctx.storage.transaction(tx => tx.scan<TrustOutbound>(key('trust-outbound') + '/'));
    for (const row of owed) {
      if (signal?.aborted) return;
      const obligation = row.value;
      const entry = await this.entry(obligation.node_id);
      // A revoked peer or a deleted row ends the obligation; a local revoke is the only
      // thing that may drop it besides the peer's own answer.
      if (entry === undefined || entry.state === 'revoked') {
        if (await this.compareObligation(obligation.node_id, obligation, null) && obligation.kind === 'rotation') {
          await this.#ctx.storage.transaction(tx => this.closeWindow(tx));
        }
        continue;
      }
      const own = await this.identity();
      if (obligation.kind === 'rotation') {
        const window = await this.rotationWindow();
        if (window === undefined) {
          // A rotation statement without its window is an orphan: nothing will ever prove it.
          await this.compareObligation(obligation.node_id, obligation, null);
          continue;
        }
        if (own.fingerprint !== window.new_fingerprint) {
          // The host still reports the old key. If this process is the one swapping, the host
          // hook is simply slow: wait for it, however long it takes. Otherwise the process
          // restarted mid-swap and the swap never completed, so the preparation is dead weight
          // and the peers still hold a key this node still uses.
          if (this.#rotating) continue;
          await this.abandonRotation();
          continue;
        }
      } else if (obligation.fingerprint !== own.fingerprint) {
        // An approval statement signed by a key this node no longer uses; the rotation
        // statement that replaced it carries the same approval.
        await this.compareObligation(obligation.node_id, obligation, null);
        continue;
      }
      // An unverified refusal delays the next attempt; it never ends the obligation.
      let current = obligation;
      if (current.blocked !== undefined) {
        if (this.#ctx.now() < current.blocked) continue;
        const { blocked: _blocked, ...rest } = current;
        if (!await this.compareObligation(current.node_id, current, rest)) continue;
        current = rest;
      }
      if (this.#ctx.now() - current.at < EXCHANGE_RETRY_MS) continue;
      const nonce = this.#ctx.id();
      const old = current.previous === undefined ? undefined : new Uint8Array(current.previous);
      // Address the key the peer publishes now. A statement's `target_fingerprint` is the key
      // it is written for; reusing the stored one would make two nodes that rotated at the
      // same time reject each other forever (RFC 0017 §3.5).
      let live: KeyIdentity;
      try {
        live = await this.inspect(entry.node_id);
      } catch {
        continue; // unreachable this pass; the obligation stays
      }
      if (!await this.compareObligation(current.node_id, current, { ...current, nonce, at: this.#ctx.now() })) continue;
      try {
        await this.send(live, entry.local_approved, { nonce, ...(old === undefined ? {} : { old }) });
      } catch (error) {
        // An unverified error never destroys the obligation: it is parked, so a refusal the
        // peer did not actually send cannot lose the approval. A transport failure or a failed
        // verification simply keeps it for the next window.
        if (terminalRefusal(error)) await this.park(entry.node_id, nonce);
      }
    }
  }
  async rotate(auth: Auth): Promise<KeyIdentity> {
    this.owner(auth);
    // The whole operation runs under one process-wide lock. The guard in the transaction
    // alone cannot prevent a second rotation that starts while the first is still awaiting
    // `keys.replace`: it would pass a window-free guard and swap again with a proof from a
    // key generation the peers have already moved past.
    return RotationMutex.locked(() => this.#rotate(auth));
  }
  /** True while this process is inside `rotate`; tells a slow hook from a crash leftover. */
  #rotating = false;
  async #rotate(auth: Auth): Promise<KeyIdentity> {
    const window = await this.rotationWindow();
    if (window !== undefined) fail('conflict', 'A rotation is still unacknowledged; a second swap would skip the proof a peer needs.');
    const previous = await this.#keys.material();
    // Every non-revoked relation where the peer may already hold this node's old key owes the
    // rotation statement: our own approval (requested) or the peer's recorded statement
    // (invited) both mean the old key is pinned somewhere.
    const peers = (await this.list(auth)).filter(row => row.state !== 'revoked' && (row.local_approved || row.remote_approved));
    const next = generateKeyMaterial();
    const fingerprint = await keyFingerprint(next.public_key);
    // The window and every obligation are written in one transaction, so the maintenance loop
    // can tell a swap in progress from a completed one. Each obligation carries the old private
    // key, so an unreachable peer is re-driven later and a restart keeps the proof.
    await this.#ctx.storage.transaction(async tx => {
      if (await tx.get(key('security', 'rotation')) !== undefined) {
        fail('conflict', 'A rotation is still unacknowledged; a second swap would skip the proof a peer needs.');
      }
      // No peer to tell: swap the key without opening a window that nothing could close.
      if (peers.length === 0) return;
      for (const peer of peers) {
        await tx.put(key('trust-outbound', peer.node_id), {
          node_id: peer.node_id, kind: 'rotation', nonce: this.#ctx.id(), fingerprint, at: this.#ctx.now(), previous: Array.from(previous.private_key),
        } satisfies TrustOutbound);
      }
      await tx.put(key('security', 'rotation'), { new_fingerprint: fingerprint, at: this.#ctx.now() } satisfies RotationWindow);
    });
    this.#rotating = true;
    try {
      await this.#keys.replace(next);
    } finally {
      this.#rotating = false;
    }
    const old = previous.private_key;
    for (const peer of peers) {
      const obligation = await this.#ctx.storage.transaction(tx => tx.get<TrustOutbound>(key('trust-outbound', peer.node_id)));
      if (obligation === undefined) continue;
      let live: KeyIdentity;
      try {
        live = await this.inspect(peer.node_id);
      } catch {
        continue; // the obligation carries the old key and is re-driven later
      }
      try {
        // The statement asserts what is true: a peer this node has not approved yet receives
        // `approved=false` with the rotation proof (RFC 0017 §3.3).
        await this.send(live, peer.local_approved, { nonce: obligation.nonce, old });
      } catch (error) {
        if (terminalRefusal(error)) await this.park(peer.node_id, obligation.nonce);
      }
    }
    const identity = await this.identity();
    for (const peer of peers) this.notify('rotation', (await this.entry(peer.node_id)) ?? peer, peer);
    return identity;
  }
  private async consumeNonce(tx: import('./types.ts').Transaction, actor: string, nonce: string, timestamp: number): Promise<void> {
    const slot = key('nonce', actor, nonce);
    if (await tx.get(slot) !== undefined) fail('unauthenticated', 'This nonce was already used.');
    for (const row of await tx.scan<number>('nonce/')) if (row.value < this.#ctx.now() - CLOCK_SKEW_MS) await tx.delete(row.key);
    await tx.put(slot, timestamp);
  }
  async signRequest(request: Request, target: NodeId, raw: Uint8Array | string): Promise<void> {
    const { timestamp, nonce } = this.#keys.freshTimestamp();
    request.headers.set('Onlyne-Actor', this.#ctx.node);
    const signature = await this.#keys.sign(requestPayload(request, target, hash(raw), timestamp, nonce));
    request.headers.set(SIGNATURE_HEADER, `v1 ${encodeURIComponent(this.#ctx.node)} ${timestamp} ${nonce} ${signature}`);
  }
  async authenticate(request: Request, digest: string): Promise<Auth> {
    const identity = await this.identity();
    const details = { ...identity };
    const parts = request.headers.get(SIGNATURE_HEADER)?.split(' ');
    if (parts?.length !== 5 || parts[0] !== 'v1') fail('unauthenticated', 'A node signature is required.', details);
    let actor: string;
    try { actor = decodeURIComponent(parts[1]!); } catch { return fail('unauthenticated', 'Invalid signing identity.', details); }
    const timestamp = parts[2]!, nonce = parts[3]!, signature = parts[4]!;
    if (!Number.isSafeInteger(Number(timestamp)) || Math.abs(this.#ctx.now() - Number(timestamp)) > CLOCK_SKEW_MS || !/^[a-zA-Z0-9-]{1,128}$/.test(nonce)) fail('unauthenticated', 'Invalid signature timestamp or nonce.', details);
    const entry = await this.entry(actor);
    if (entry?.state !== 'trusted' || actor === this.#ctx.node || request.headers.get('Onlyne-Actor') !== actor) fail('unauthenticated', 'The caller is not a trusted peer.', details);
    if (!signatureValid(entry.public_key, signature, requestPayload(request, this.#ctx.node, digest, timestamp, nonce))) fail('unauthenticated', 'Invalid request signature.', details);
    await this.#ctx.storage.transaction(async tx => {
      if ((await tx.get<TrustEntry>(key('trust', actor)))?.state !== 'trusted') fail('unauthenticated', 'The caller was revoked.', details);
      await this.consumeNonce(tx, actor, nonce, Number(timestamp));
    });
    return { actor };
  }
  async signResponse(request: Request, response: Response, body: string): Promise<void> {
    const nonce = request.headers.get(SIGNATURE_HEADER)?.split(' ')[3] ?? '';
    response.headers.set('Onlyne-Response-Sig', await this.#keys.sign(responsePayload(this.#ctx.node, nonce, response.status, hash(body))));
  }
  async verifyResponse(target: NodeId, request: Request, response: Response, body: string): Promise<void> {
    const peer = await this.entry(target);
    const nonce = request.headers.get(SIGNATURE_HEADER)?.split(' ')[3] ?? '';
    if (peer?.state !== 'trusted' || !signatureValid(peer.public_key, response.headers.get('Onlyne-Response-Sig') ?? '', responsePayload(target, nonce, response.status, hash(body)))) fail('unauthenticated', 'The response does not prove the expected node identity.');
  }
}
