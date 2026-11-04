import {
  MemoryStorage, OnlyneError, OnlyneNode, type Auth, type Json, type KeyHook, type MaintenanceClock,
  type NodeId, type Plugins, type Storage, type Transport,
} from '@onlyne/lib';
import { FileStorage } from '@onlyne/im-gateway/storage';
import type { LogSink } from '@onlyne/node-host';
import type { McpServerConfig } from './config.ts';
import { t } from './server-text.ts';

export function nodeClock(): MaintenanceClock {
  return {
    setTimer: (ms, run) => setTimeout(run, ms),
    clearTimer: handle => clearTimeout(handle as ReturnType<typeof setTimeout>),
  };
}

export interface OnlyneHost {
  readonly node: OnlyneNode;
  readonly config: McpServerConfig;
  readonly storage: Storage;
  stop(): Promise<void>;
}

export interface OnlyneHostOptions {
  storage?: 'memory' | 'disk';
  transport?: Transport;
  keys?: KeyHook;
  log?: LogSink;
}

/**
 * An Onlyne node the MCP server drives. Tools call `threads` / `messages` / `client` on this
 * object; stdio is only the MCP transport, not a substitute for the node's HTTP peer surface.
 */
export async function createOnlyneHost(config: McpServerConfig, options: OnlyneHostOptions = {}): Promise<OnlyneHost> {
  const storage: Storage = options.storage === 'memory' ? new MemoryStorage() : new FileStorage(config.storage_path);
  const owner: NodeId = config.node.id;
  const base_url = `http://${config.server.bind}:${config.server.port}/onlyne/v3`;
  let node: OnlyneNode;
  const authenticate = async (request: Request): Promise<Auth | null> => {
    const actor = request.headers.get('Onlyne-Actor');
    if (actor === null || actor.length === 0) return null;
    if (actor === owner) throw new OnlyneError('forbidden', t('error.claimIdentity'));
    return { actor };
  };
  const plugins: Omit<Plugins, 'storage'> = {
    authenticate,
    maintenanceClock: nodeClock(),
    resolve: async id => {
      const target = id === owner ? { node_id: id, base_url } : await node.discovery.resolve(id);
      return { ...target, headers: { 'Onlyne-Actor': owner } };
    },
    domainCandidates: domain_id => node.discovery.candidates(domain_id),
    executionContexts: async () => (config.contexts ?? ['main']).map(alias => ({
      alias, summary: t('context.summary'), state: 'idle',
    })) as Json,
    ...(options.keys === undefined ? {} : { keys: options.keys }),
    decide: () => { throw new OnlyneError('decision_unavailable', t('error.decide')); },
    inboundThread: async () => true,
    runtime: {
      notify: event => {
        if (event.event !== 'trust') return;
        options.log?.('onlyne.trust', {
          node: event.entry.node_id, phase: event.phase, fingerprint: event.entry.fingerprint,
        });
      },
      supports: async plan => plan.response !== 'await',
      inject: async () => ({ context: owner, consumed: true, outcome: 'delivered' }),
    },
  };
  node = new OnlyneNode({
    node_id: owner,
    base_url,
    ...(options.keys === undefined ? {} : { authentication: ['signature'] }),
    plugins: {
      ...plugins, storage,
      ...(options.transport === undefined ? {} : { transport: options.transport }),
    },
    now: () => Date.now(),
    id: () => crypto.randomUUID(),
  });
  return {
    node, config, storage,
    async stop(): Promise<void> {
      node.client.stopMaintenance();
    },
  };
}
