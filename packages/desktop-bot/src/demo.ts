/**
 * A local demo cluster. It fills the desk with bots, conversations and a node
 * view so every screen can be exercised without OMP, keys or external accounts.
 * Trust approval still checks the fingerprint the caller types.
 */

import { createHash } from 'node:crypto';
import { mkdir } from 'node:fs/promises';
import path from 'node:path';
import type {
  Entry, JoinedThread, Json, LocalAttention, Member, Resource, Thread,
} from '@onlyne/lib';
import type {
  Bot, Capability, CharacterCard, ChatMessage, Conversation, DesktopState, DesktopStore, Lorebook,
} from './contracts.ts';
import type { OnlyneBridge, OnlyneBridgeOptions } from './contracts.ts';
import type { DesktopOnlyneStatus, PeerRow, PendingExchangeRow } from './onlyne.ts';
import { parsePeerUrl, peerSnapshot, type PeerSnapshot } from './desk-format.ts';

const MIRA = 'urn:onlyne:agent:desktop:mira';
const JUN = 'urn:onlyne:agent:desktop:jun';
const LARK = 'urn:onlyne:agent:desktop:lark';
const MCP = 'urn:onlyne:gateway:mcp';
const GROK = 'urn:onlyne:gateway:grok-bot';
const NORTH = 'urn:onlyne:agent:north';
const OBSERVED = 'urn:onlyne:agent:observed';

const AT = '2026-11-03T16:40:00.000Z';
const EARLIER = '2026-11-02T11:05:00.000Z';

export function demoFingerprint(nodeId: string): string {
  return createHash('sha256').update(`onlyne-demo:${nodeId}`).digest('hex').slice(0, 32);
}

interface DemoThread {
  thread: Thread;
  members: Member[];
  entries: Entry[];
  joined: boolean;
}

interface DemoWorld {
  peers: PeerRow[];
  pending: PendingExchangeRow[];
  threads: DemoThread[];
  resources: Resource[];
  inbox: LocalAttention[];
}

let world: DemoWorld | undefined;

export function resetDemoWorld(): void {
  world = undefined;
}

function endpoint(port: number): string {
  return `http://127.0.0.1:${port}/onlyne/v3`;
}

function member(peer: string, state: Member['state'] = 'active'): Member {
  return { peer, state, access: state === 'active' ? ['read', 'post'] : ['read'], read_scope: {}, revision: '1' };
}

function threadRecord(owner: string, id: string, title: string, next: string): Thread {
  return {
    thread_ref: { owner, id }, title, state: 'open', next_seq: next, event_seq: next,
    created_at: EARLIER, closed_at: null,
  };
}

function entry(thread: Thread, seq: string, sender: string, body: string, at: string): Entry {
  return {
    thread_ref: thread.thread_ref,
    seq,
    message: { id: `${thread.thread_ref.id}:${seq}`, sender, delivery_kind: 'note', msg: body, meta: { thread: thread.thread_ref } },
    authenticated_author: sender,
    reply_authority: sender,
    at,
  };
}

function peer(nodeId: string, partial: Partial<PeerRow> & Pick<PeerRow, 'phase' | 'baseUrl'>): PeerRow {
  return {
    nodeId,
    fingerprint: partial.fingerprint === undefined ? demoFingerprint(nodeId) : partial.fingerprint,
    state: partial.state ?? null,
    localApproved: partial.localApproved ?? false,
    remoteApproved: partial.remoteApproved ?? false,
    verified: partial.verified ?? false,
    lastSeen: partial.lastSeen ?? AT,
    source: partial.source ?? 'trust',
    instanceId: partial.instanceId ?? `demo-${nodeId.split(':').pop()}`,
    firstSeen: partial.firstSeen ?? EARLIER,
    lastChecked: partial.lastChecked ?? AT,
    phase: partial.phase,
    baseUrl: partial.baseUrl,
  };
}

function resource(owner: string, id: string, kind: string, operations: string[], handler: string, data: Json): Resource {
  return {
    resource_ref: { owner, id },
    kind,
    operations,
    binding: { handler, cursor: '0' },
    log_readers: [MIRA],
    revision: '1',
    event_seq: '1',
    retention_ms: 86_400_000,
    data,
  };
}

function attention(recipient: string, sender: string, thread: Thread, seq: string, body: string, injected: boolean): LocalAttention {
  const item = entry(thread, seq, sender, body, AT);
  return {
    injected,
    attention: {
      obligation_id: `ob-${sender.split(':').pop()}-${seq}`,
      recipient,
      thread_ref: thread.thread_ref,
      seq,
      message_ref: { sender, id: item.message.id },
      authenticated_author: sender,
      acked: false,
      entry: item,
    },
  };
}

function buildWorld(): DemoWorld {
  const desk = threadRecord(MIRA, 'desktop:context:c-desk', 'Desk', '3');
  const studio = threadRecord(MIRA, 'desktop:context:c-studio', 'Studio', '4');
  const grokDm = threadRecord(GROK, `grokbot:dm:${MIRA}`, 'Grok Bot · desk account', '3');
  const grokGroup = threadRecord(GROK, 'grokbot:group:studio', 'Grok Bot · studio', '2');
  const northThread = threadRecord(NORTH, 'north-intro', 'North introduction', '2');
  return {
    peers: [
      peer(MCP, { phase: 'exchanged', baseUrl: endpoint(3992), state: 'trusted', localApproved: true, remoteApproved: true, verified: true, source: 'trust' }),
      peer(GROK, { phase: 'exchanged', baseUrl: endpoint(3995), state: 'trusted', localApproved: true, remoteApproved: true, verified: true, source: 'trust' }),
      peer(NORTH, { phase: 'invited', baseUrl: endpoint(3997), state: 'pending', localApproved: false, remoteApproved: true, verified: false, source: 'trust', lastSeen: AT }),
      peer(OBSERVED, { phase: 'no_exchange', baseUrl: endpoint(3998), fingerprint: null, state: null, verified: false, source: 'discovery', lastSeen: null, lastChecked: null }),
      peer(JUN, { phase: 'exchanged', baseUrl: endpoint(3993), state: 'trusted', localApproved: true, remoteApproved: true, verified: true, source: 'trust' }),
    ],
    pending: [{
      nodeId: NORTH,
      fingerprint: demoFingerprint(NORTH),
      baseUrl: endpoint(3997),
      at: AT,
    }],
    threads: [
      {
        thread: desk, joined: false,
        members: [member(MIRA), member(JUN)],
        entries: [
          entry(desk, '1', MIRA, 'The desk is open. I will keep pairing decisions on the trust screen.', EARLIER),
          entry(desk, '2', JUN, 'North is still pending. I left the fingerprint on their card.', AT),
        ],
      },
      {
        thread: studio, joined: false,
        members: [member(MIRA), member(JUN), member(LARK)],
        entries: [
          entry(studio, '1', MIRA, 'The filesystem server stays on Jun. Mira is the controller.', EARLIER),
          entry(studio, '2', JUN, 'Stdio only, and the mount list is the heartbeat command we already allow.', AT),
          entry(studio, '3', LARK, 'I will not advertise the Lark gateway until both approvals are exchanged.', AT),
        ],
      },
      {
        thread: grokDm, joined: true,
        members: [member(GROK), member(MIRA)],
        entries: [
          entry(grokDm, '1', MIRA, 'Desk account, one node. This DM is the stable grok-bot thread.', EARLIER),
          entry(grokDm, '2', GROK, 'Recorded. Replies come back through this node.', AT),
        ],
      },
      {
        thread: grokGroup, joined: true,
        members: [member(GROK), member(MIRA), member(JUN)],
        entries: [
          entry(grokGroup, '1', JUN, 'Studio group is separate from the desk DM.', AT),
        ],
      },
      {
        thread: northThread, joined: true,
        members: [member(NORTH), member(MIRA, 'requested')],
        entries: [
          entry(northThread, '1', NORTH, 'North node, asking to exchange keys with the desk.', AT),
        ],
      },
    ],
    resources: [
      resource(MIRA, 'desk', 'desktop.context', ['status'], MIRA, { conversations: ['c-desk', 'c-studio'] }),
      resource(MCP, 'server:hindsight', 'mcp.server', ['recall', 'retain'], MCP, {
        transport: 'http', url: 'http://127.0.0.1:8888/mcp/desk/', controllers: [MIRA],
      }),
      resource(MCP, 'server:files', 'mcp.server', ['read_file', 'list_directory'], MCP, {
        transport: 'stdio', command: 'npx', controllers: [MIRA, JUN],
      }),
      resource(MCP, `mounts:${MIRA}`, 'mcp.mounts', ['list', 'create', 'delete'], MIRA, {
        tasks: [{ id: 'heartbeat', command: '/bin/sh', args: ['-c', "printf 'heartbeat\\n'"] }],
      }),
      resource(GROK, 'webhook', 'grok.webhook', ['deliver'], GROK, {
        configured: true, header: 'Authorization', scheme: 'Bearer', secret: 'environment',
      }),
    ],
    inbox: [
      attention(MIRA, NORTH, northThread, '1', 'North node, asking to exchange keys with the desk.', false),
      attention(MIRA, GROK, grokDm, '2', 'Recorded. Replies come back through this node.', false),
      attention(MIRA, JUN, desk, '2', 'North is still pending. I left the fingerprint on their card.', true),
      attention(JUN, MIRA, studio, '1', 'The filesystem server stays on Jun. Mira is the controller.', false),
    ],
  };
}

function liveWorld(): DemoWorld {
  world ??= buildWorld();
  return world;
}

function requireText(value: unknown, name: string): string {
  if (typeof value !== 'string' || value.trim().length === 0) throw new Error(`${name} must be nonempty text`);
  return value.trim();
}

function ownedThreads(nodeId: string): DemoThread[] {
  return liveWorld().threads.filter(item => item.thread.thread_ref.owner === nodeId);
}

function joinedThreads(nodeId: string): DemoThread[] {
  return liveWorld().threads.filter(item => item.joined && item.thread.thread_ref.owner !== nodeId);
}

function statusFor(nodeId: string, closed: boolean): DesktopOnlyneStatus {
  const current = liveWorld();
  const owned = ownedThreads(nodeId).map(item => item.thread);
  const joined: JoinedThread[] = joinedThreads(nodeId).map(item => ({
    thread_ref: item.thread.thread_ref,
    last_seq: item.entries.at(-1)?.seq ?? '0',
    message_count: item.entries.length,
    read_only: true,
    title: item.thread.title,
    state: item.thread.state,
    members: item.members,
  }));
  const mine = current.resources.filter(item => item.resource_ref.owner === nodeId);
  const pendingInbox = current.inbox.filter(item => item.attention.recipient === nodeId && !item.injected).length;
  return {
    node: {
      id: nodeId,
      endpoint: endpoint(nodeId === JUN ? 3993 : nodeId === LARK ? 3994 : 3991),
      fingerprint: demoFingerprint(nodeId),
      instanceId: `demo-${nodeId.split(':').pop()}`,
      authentication: ['signature'],
      state: closed ? 'closed' : 'ready',
    },
    trust: { localFingerprint: demoFingerprint(nodeId), peers: current.peers, pending: current.pending },
    neighbors: current.peers,
    threads: owned,
    joined,
    resources: { owners: [{ owner: nodeId, count: mine.length, items: mine, truncated: false }] },
    contexts: [],
    inbox: { pending: pendingInbox },
  };
}

function json(value: unknown): { content: string; contentType: string } {
  return { content: JSON.stringify(value), contentType: 'application/json' };
}

export function createDemoOnlyne(options: OnlyneBridgeOptions): Promise<OnlyneBridge> {
  let closed = false;
  const nodeId = options.nodeId;
  const findThread = (owner: string, id: string): DemoThread => {
    const found = liveWorld().threads.find(item => item.thread.thread_ref.owner === owner && item.thread.thread_ref.id === id);
    if (found === undefined) throw new Error(`No thread ${owner}/${id}.`);
    return found;
  };

  const bridge: OnlyneBridge = {
    status: async () => statusFor(nodeId, closed),
    async dispatch(input) {
      return {
        demo: true,
        nodeId,
        delivery_kind: input.delivery_kind,
        recorded: true,
        note: 'The demo node recorded this locally. It did not contact a peer.',
      };
    },
    async read(uri: string) {
      let url: URL;
      try { url = new URL(uri); }
      catch { throw new Error(`read expects an onlyne:// URI, got ${uri}.`); }
      if (url.protocol !== 'onlyne:') throw new Error(`read expects an onlyne:// URI, got ${uri}.`);
      const section = url.hostname.toLowerCase();
      const current = liveWorld();
      if (section === 'status') return json(await bridge.status());
      if (section === 'neighbors') return json(current.peers);
      if (section === 'inbox') return json(current.inbox.filter(item => item.attention.recipient === nodeId));
      if (section === 'threads') {
        return json({
          owned: ownedThreads(nodeId).map(item => item.thread),
          joined: joinedThreads(nodeId).map(item => item.thread),
        });
      }
      if (section === 'resources') {
        return json({
          owners: groupResources(current.resources).map(group => ({
            owner: group.owner,
            page: { items: group.items, next_cursor: null },
          })),
        });
      }
      throw new Error(`Unknown onlyne:// URI ${uri}.`);
    },
    async action(name, args) {
      const current = liveWorld();
      if (name === 'connect') {
        const parsed = parsePeerUrl(typeof args.url === 'string' ? args.url : '');
        if (!parsed.ok) throw new Error(parsed.error);
        const url = parsed.url;
        const host = new URL(url).host.replace(/[^\w.-]/g, '-');
        const id = `urn:onlyne:agent:${host}`;
        if (!current.peers.some(item => item.nodeId === id)) {
          current.peers.push(peer(id, { phase: 'no_exchange', baseUrl: url, fingerprint: null, state: null, source: 'discovery', verified: false, lastSeen: new Date().toISOString() }));
        }
        const observed = current.peers.find(item => item.nodeId === id)!;
        options.emit({ type: 'onlyne.observed', nodeId: id });
        return { action: name, peer: observed, trust: null };
      }
      if (name === 'peer.forget') {
        const id = requireText(args.nodeId, 'peer.forget.nodeId');
        const existing = current.peers.find(item => item.nodeId === id);
        if (existing === undefined) throw new Error(`No peer named ${id}.`);
        const pending = current.pending.find(item => item.nodeId === id) ?? null;
        current.peers = current.peers.filter(item => item.nodeId !== id);
        current.pending = current.pending.filter(item => item.nodeId !== id);
        return { action: name, nodeId: id, peer: { ...existing }, pending };
      }
      if (name === 'peer.restore') {
        const raw = args.peer;
        if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) throw new Error('peer.restore needs the forgotten peer record.');
        const snapshot: PeerSnapshot = peerSnapshot(raw as Record<string, unknown>);
        if (snapshot.nodeId.length === 0) throw new Error('peer.restore needs a node id.');
        const restored = {
          nodeId: snapshot.nodeId,
          baseUrl: snapshot.baseUrl,
          fingerprint: snapshot.fingerprint,
          phase: snapshot.phase,
          state: snapshot.state,
          localApproved: snapshot.localApproved,
          remoteApproved: snapshot.remoteApproved,
          verified: snapshot.verified,
          lastSeen: snapshot.lastSeen,
          source: snapshot.source,
          instanceId: snapshot.instanceId,
          firstSeen: snapshot.firstSeen,
          lastChecked: snapshot.lastChecked,
          ...(snapshot.name.length > 0 ? { name: snapshot.name } : {}),
          ...(snapshot.notes.length > 0 ? { notes: snapshot.notes } : {}),
        } as PeerRow;
        const existing = current.peers.find(item => item.nodeId === restored.nodeId);
        if (existing === undefined) current.peers.push(restored);
        else Object.assign(existing, restored);
        const pendingRaw = args.pending;
        if (pendingRaw !== null && typeof pendingRaw === 'object' && !Array.isArray(pendingRaw)) {
          const pending = pendingRaw as PendingExchangeRow;
          if (typeof pending.nodeId === 'string' && pending.nodeId.length > 0) {
            current.pending = current.pending.filter(item => item.nodeId !== pending.nodeId);
            current.pending.push(pending);
          }
        }
        return { action: name, peer: existing ?? restored };
      }
      if (name === 'trust.request') {
        const id = requireText(args.nodeId, 'trust.request.nodeId');
        const known = current.peers.find(item => item.nodeId === id);
        const row: PendingExchangeRow = {
          nodeId: id,
          fingerprint: known?.fingerprint ?? demoFingerprint(id),
          baseUrl: known?.baseUrl ?? endpoint(3999),
          at: new Date().toISOString(),
        };
        current.pending = current.pending.filter(item => item.nodeId !== id);
        current.pending.push(row);
        options.emit({ type: 'onlyne.trust.request', nodeId: id, fingerprint: row.fingerprint, localFingerprint: demoFingerprint(nodeId) });
        return { action: name, nodeId: id, fingerprint: row.fingerprint, baseUrl: row.baseUrl, localFingerprint: demoFingerprint(nodeId), phase: 'awaiting-approval' };
      }
      if (name === 'trust.approve') {
        const id = requireText(args.nodeId, 'trust.approve.nodeId');
        const fingerprint = requireText(args.fingerprint, 'trust.approve.fingerprint');
        const held = current.pending.find(item => item.nodeId === id);
        if (held === undefined) throw new Error(`No displayed fingerprint for ${id}; call trust.request first.`);
        if (fingerprint.replace(/\s+/g, '').toLowerCase() !== held.fingerprint.replace(/\s+/g, '').toLowerCase()) {
          throw new Error('The approved fingerprint is not the one displayed.');
        }
        current.pending = current.pending.filter(item => item.nodeId !== id);
        const existing = current.peers.find(item => item.nodeId === id);
        const approved = peer(id, {
          phase: 'exchanged',
          baseUrl: held.baseUrl,
          fingerprint: held.fingerprint,
          state: 'trusted',
          localApproved: true,
          remoteApproved: true,
          verified: true,
          source: 'trust',
          lastSeen: new Date().toISOString(),
        });
        if (existing === undefined) current.peers.push(approved);
        else Object.assign(existing, approved);
        options.emit({ type: 'onlyne.trust', nodeId: id, phase: 'exchanged', fingerprint: held.fingerprint });
        return { action: name, nodeId: id, entry: existing ?? approved, phase: 'exchanged' };
      }
      if (name === 'trust.revoke') {
        const id = requireText(args.nodeId, 'trust.revoke.nodeId');
        current.pending = current.pending.filter(item => item.nodeId !== id);
        const existing = current.peers.find(item => item.nodeId === id);
        if (existing !== undefined) {
          existing.phase = 'revoked';
          existing.state = 'revoked';
          existing.localApproved = false;
          existing.verified = false;
        }
        options.emit({ type: 'onlyne.trust', nodeId: id, phase: 'revoked' });
        return { action: name, nodeId: id };
      }
      if (name === 'thread.read') {
        const owner = requireText(args.owner, 'thread.read.owner');
        const id = requireText(args.id, 'thread.read.id');
        const found = findThread(owner, id);
        return {
          action: name,
          threadRef: found.thread.thread_ref,
          thread: found.thread,
          members: found.members,
          entries: { items: found.entries, next_cursor: null },
        };
      }
      if (name === 'resource.list') {
        const owners = Array.isArray(args.owners) ? args.owners.filter((item): item is string => typeof item === 'string') : [];
        const items = owners.length === 0 ? current.resources : current.resources.filter(item => owners.includes(item.resource_ref.owner));
        return {
          action: name,
          inventory: {
            owners: groupResources(items).map(group => ({ owner: group.owner, page: { items: group.items, next_cursor: null } })),
          },
        };
      }
      throw new Error(`Unknown Onlyne action ${name}.`);
    },
    async close() {
      closed = true;
    },
  };
  return Promise.resolve(bridge);
}

function groupResources(items: Resource[]): Array<{ owner: string; items: Resource[] }> {
  const groups = new Map<string, Resource[]>();
  for (const item of items) {
    const owner = item.resource_ref.owner;
    const list = groups.get(owner);
    if (list === undefined) groups.set(owner, [item]);
    else list.push(item);
  }
  return [...groups.entries()].map(([owner, grouped]) => ({ owner, items: grouped }));
}

function bot(partial: Pick<Bot, 'id' | 'name' | 'description' | 'instructions' | 'memory' | 'thinking'> & { nodeId: string; capabilities: string[]; character?: CharacterCard; lorebook?: Lorebook }, dataDir: string, now: number): Bot {
  return {
    id: partial.id,
    name: partial.name,
    description: partial.description,
    instructions: partial.instructions,
    memory: partial.memory,
    workspace: path.join(dataDir, 'workspaces', partial.id),
    thinking: partial.thinking,
    capabilities: partial.capabilities,
    runtime: 'omp-rpc',
    runtimeOptions: {},
    onlyne: { nodeId: partial.nodeId, peers: [endpoint(3992), endpoint(3995)] },
    plugins: { host: [], renderer: [] },
    ...(partial.character === undefined ? {} : { character: partial.character }),
    ...(partial.lorebook === undefined ? {} : { lorebook: partial.lorebook }),
    createdAt: now,
    updatedAt: now,
  };
}

export function demoState(dataDir: string): DesktopState {
  const now = Date.parse(AT);
  const miraCharacter: CharacterCard = {
    name: 'Mira',
    description: 'Keeps the desk calm. Speaks in short sentences and refuses to approve a peer the operator has not read back.',
    personality: 'Precise, unhurried, unwilling to invent a fingerprint.',
    scenario: 'A private desk where three bots share one team and each bot owns one Onlyne node.',
    systemPrompt: 'You are Mira, the desk administrator. Never approve trust on the operator\'s behalf.',
    firstMessage: 'The desk is open. Pairing stays on the trust screen until you type the fingerprint you saw.',
    tags: ['desk', 'admin'],
    source: 'manual',
  };
  const lorebook: Lorebook = {
    name: 'Desk',
    entries: [{
      id: 'lore-fingerprint',
      keys: ['fingerprint', 'pairing', 'trust'],
      content: 'Approval is valid only when the operator types the fingerprint that is already on screen. A click is not a confirmation.',
      enabled: true,
      constant: true,
      order: 1,
    }],
  };
  const mira = bot({
    id: 'mira', name: 'Mira', nodeId: MIRA, capabilities: ['cap-hindsight'], thinking: 'medium', character: miraCharacter, lorebook,
    description: 'Team administrator. Holds the desk node and the pairing rule.',
    instructions: 'Coordinate the team. Keep trust decisions with the operator. Prefer the existing thread over a new one.',
    memory: 'North is waiting on a fingerprint. The filesystem server belongs to Jun.',
  }, dataDir, now);
  const jun = bot({
    id: 'jun', name: 'Jun', nodeId: JUN, capabilities: ['cap-files'], thinking: 'low',
    description: 'Research partner. Runs the local filesystem server.',
    instructions: 'Answer with the source of a claim. Do not widen a tool allow-list.',
    memory: 'Hindsight is Mira\'s bank. Files stay on this machine.',
  }, dataDir, now);
  const lark = bot({
    id: 'lark', name: 'Lark', nodeId: LARK, capabilities: [], thinking: 'inherit',
    description: 'Watches gateways. Does not hold IM credentials.',
    instructions: 'Report gateway state. Never invent a platform token.',
    memory: 'No IM credential is stored on this desk.',
  }, dataDir, now);

  const conversations: Conversation[] = [
    { id: 'c-desk', title: 'Desk', kind: 'direct', botIds: ['mira'], adminBotId: 'mira', memory: 'Pairing notes for the desk.', greeting: miraCharacter.firstMessage, greeted: true, createdAt: now - 86_400_000, updatedAt: now },
    { id: 'c-studio', title: 'Studio', kind: 'group', botIds: ['mira', 'jun', 'lark'], adminBotId: 'mira', memory: 'Controllers: Mira. Filesystem server: Jun.', scene: { group: { mode: 'manual', order: ['mira', 'jun', 'lark'], auto: false, maxAutoTurns: 4 } }, createdAt: now - 3_600_000, updatedAt: now - 60_000 },
    { id: 'c-notes', title: 'Field notes', kind: 'direct', botIds: ['jun'], adminBotId: 'jun', memory: '', createdAt: now - 7_200_000, updatedAt: now - 120_000 },
  ];
  const messages: ChatMessage[] = [
    { id: 'msg-desk-1', conversationId: 'c-desk', role: 'assistant', botId: 'mira', text: miraCharacter.firstMessage ?? '', createdAt: now - 86_000_000 },
    { id: 'msg-desk-2', conversationId: 'c-desk', role: 'user', text: 'North showed a fingerprint on their screen. I have not typed it here yet.', createdAt: now - 80_000_000 },
    { id: 'msg-desk-3', conversationId: 'c-desk', role: 'assistant', botId: 'mira', text: 'Leave it pending. Type that fingerprint on Trust, and only if it matches the one already on screen.', createdAt: now - 79_000_000 },
    { id: 'msg-desk-4', conversationId: 'c-desk', role: 'user', text: 'I will read it on their screen and type it there.', createdAt: now - 78_000_000 },
    { id: 'msg-studio-1', conversationId: 'c-studio', role: 'user', text: 'Jun, keep the filesystem server on your bot. Mira stays the controller.', createdAt: now - 3_000_000 },
    { id: 'msg-studio-2', conversationId: 'c-studio', role: 'assistant', botId: 'jun', text: 'Stdio only. The allow-list is read_file and list_directory. I will not add a write tool from this chat.', createdAt: now - 2_400_000 },
    { id: 'msg-studio-3', conversationId: 'c-studio', role: 'assistant', botId: 'mira', text: 'Noted. The mount resource is heartbeat, and it runs the command already in the gateway configuration.', createdAt: now - 1_800_000 },
    { id: 'msg-studio-4', conversationId: 'c-studio', role: 'assistant', botId: 'lark', text: 'The Grok gateway is a neighbor, not a credential. Its webhook key stays in the gateway environment.', createdAt: now - 60_000 },
    { id: 'msg-notes-1', conversationId: 'c-notes', role: 'user', text: 'Where does a Grok Bot DM live?', createdAt: now - 7_000_000 },
    { id: 'msg-notes-2', conversationId: 'c-notes', role: 'assistant', botId: 'jun', text: 'One grok-bot node for the account. The DM is thread grokbot:dm under that gateway, not a second keypair.', createdAt: now - 6_800_000 },
  ];
  const capabilities: Capability[] = [
    { id: 'cap-hindsight', name: 'Hindsight', kind: 'mcp', config: { transport: 'http', url: 'http://127.0.0.1:8888/mcp/desk/', allowed_tools: ['recall', 'retain'] } },
    { id: 'cap-files', name: 'Workspace files', kind: 'mcp', config: { transport: 'stdio', command: 'npx', args: ['-y', '@modelcontextprotocol/server-filesystem'], allowed_tools: ['read_file', 'list_directory'] } },
  ];
  return {
    version: 1,
    bots: [mira, jun, lark],
    teamAdminId: 'mira',
    conversations,
    messages,
    capabilities,
    media: [],
    settings: {
      theme: 'dark',
      locale: 'en',
      hostPlugins: [],
      rendererPlugins: [],
      customCss: '',
      nodeId: MIRA,
      peers: [endpoint(3992), endpoint(3995)],
    },
  };
}

export async function ensureDemoTeam(store: DesktopStore, dataDir: string): Promise<'seeded' | 'kept'> {
  if (store.read().messages.some(item => item.id === 'msg-desk-4')) return 'kept';
  const next = demoState(dataDir);
  for (const item of next.bots) await mkdir(item.workspace, { recursive: true });
  await store.update(state => {
    state.bots = next.bots;
    state.teamAdminId = next.teamAdminId;
    state.conversations = next.conversations;
    state.messages = next.messages;
    state.capabilities = next.capabilities;
    state.media = next.media;
    state.settings = next.settings;
    return 'seeded' as const;
  });
  return 'seeded';
}
