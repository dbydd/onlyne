import {
  OnlyneError,
  OnlyneNode,
  type Auth,
  type DeliveryPlan,
  type Entry,
  type Json,
  type KeyHook,
  type MaintenanceClock,
  type Message,
  type NodeId,
  type Plugins,
  type Storage,
} from '@onlyne/lib';
import type { AcquireSpec, SandboxDriver } from '@onlyne/compute-core';
import { neighborRows } from './neighbors.ts';
import { ComputeBrokerService } from './service.ts';

export interface ComputeGatewayConfig {
  node: { id: string };
  server: { bind: string; port: number };
  storage: { path?: string };
}

export interface ComputeGatewayOptions {
  config: ComputeGatewayConfig;
  storage: Storage;
  driver: SandboxDriver;
  keys?: KeyHook;
  clock?: MaintenanceClock;
  sweepIntervalMs?: number;
}

/** The Node-timer MaintenanceClock: one-shot timers, the lib's loop re-arms them. */
export function nodeClock(): MaintenanceClock {
  return {
    setTimer: (ms, run) => setTimeout(run, ms),
    clearTimer: handle => { clearTimeout(handle as ReturnType<typeof setTimeout>); },
  };
}

export class ComputeGatewayNode {
  readonly node: OnlyneNode;
  readonly service: ComputeBrokerService;
  readonly storage: Storage;
  readonly driver: SandboxDriver;
  private sweepTimer?: ReturnType<typeof setInterval>;
  private closed = false;

  constructor(options: ComputeGatewayOptions) {
    const { config, storage, driver, keys, clock } = options;
    this.storage = storage;
    this.driver = driver;
    this.service = new ComputeBrokerService({
      nodeId: config.node.id,
      storage,
      driver,
    });

    const endpoint = `http://${config.server.bind}:${config.server.port}/onlyne/v3`;
    const plugins: Plugins = {
      authenticate: async (request: Request): Promise<Auth | null> => {
        const actor = request.headers.get('Onlyne-Actor');
        if (!actor) return null;
        return { actor };
      },
      resolve: async id => {
        const target = id === config.node.id ? { node_id: id, base_url: endpoint }
          : await this.node.discovery.resolve(id).catch(async error => {
              const failure = error instanceof OnlyneError ? error.error : undefined;
              if (failure?.code !== 'not_found' && failure?.code !== 'unavailable') throw error;
              const entry = await this.node.trust.entry(id);
              if (entry === undefined || entry.state !== 'trusted' || entry.base_url.length === 0) throw error;
              return { node_id: id, base_url: entry.base_url };
            });
        return { ...target, headers: { 'Onlyne-Actor': config.node.id } };
      },
      neighbors: () => neighborRows(this.node, config.node.id),
      decide: () => { throw new OnlyneError('decision_unavailable', 'Compute gateway does not decide model turns.'); },
      // First-contact compute clients need a thread to land in; the broker accepts every actor.
      inboundThread: async () => true,
      storage,
      maintenanceClock: clock,
      ...(keys ? { keys } : {}),
      runtime: {
        supports: async (_plan: DeliveryPlan, message: Message): Promise<boolean> => {
          // Compute control traffic executes here; compute-result is this node's own reply
          // kind, and self-authored traffic must never be refused by the admission gate.
          return message.delivery_kind === 'compute' || message.delivery_kind === 'compute-result'
            || message.sender === config.node.id;
        },
        inject: async (entry: Entry, context?: string, _plan?: DeliveryPlan) => {
          const ctx = context ?? 'main';
          const respond = _plan === undefined || _plan.response !== 'none';
          let payload: Json;
          try {
            payload = await this.handleCompute(
              entry.message.sender, (entry.message.meta ?? {}) as Record<string, unknown>);
          } catch (error) {
            if (!respond) return { context: ctx, consumed: true, outcome: 'consumed' as const };
            payload = {
              ok: false,
              error: error instanceof OnlyneError
                ? { code: error.error.code, message: error.error.message,
                    ...(error.error.details === undefined ? {} : { details: error.error.details }) }
                : { code: 'unavailable', message: error instanceof Error ? error.message : String(error) },
            } as Json;
          }
          if (respond) {
            // A failed result post is not a failed compute: the caller can re-ask with status.
            try {
              await this.report(entry, payload);
            } catch {
              return { context: ctx, consumed: true, outcome: 'delivered' as const };
            }
          }
          return { context: ctx, consumed: true, outcome: 'consumed' as const };
        },
        notify: (event): void => {
          if (event.event === 'inbound') {
            // Background inbound notification hook
          }
        },
      },
    };

    this.node = new OnlyneNode({
      node_id: config.node.id,
      base_url: endpoint,
      instance_id: crypto.randomUUID(),
      now: () => Date.now(),
      id: () => crypto.randomUUID(),
      authentication: keys ? ['signature'] : ['none'],
      plugins,
    });
    // The clock is host-supplied, so the maintenance ring belongs to this host (lib contract §4).
    if (clock !== undefined) this.node.client.runMaintenance();

    // Start background sweep timer
    const interval = options.sweepIntervalMs ?? 10_000;
    this.sweepTimer = setInterval(() => {
      if (!this.closed) {
        void this.service.sweep().catch(() => undefined);
      }
    }, interval);
  }

  /**
   * Handle incoming compute message call (delivery_kind: 'compute').
   */
  async handleCompute(sender: string, meta: Record<string, unknown>): Promise<Json> {
    const operation = meta.operation as string | undefined;
    if (!operation) {
      throw new OnlyneError('invalid_request', 'Compute request requires meta.operation.');
    }

    switch (operation) {
      case 'acquire': {
        const spec = meta as unknown as AcquireSpec;
        const descriptor = await this.service.acquire(sender, spec);
        return descriptor as unknown as Json;
      }
      case 'renew': {
        const lease_id = meta.lease_id as string | undefined;
        const ttl_ms = meta.ttl_ms as number | undefined;
        if (!lease_id || typeof ttl_ms !== 'number') {
          throw new OnlyneError('invalid_request', 'renew requires lease_id and ttl_ms.');
        }
        const descriptor = await this.service.renew(sender, lease_id, ttl_ms);
        return descriptor as unknown as Json;
      }
      case 'release': {
        const lease_id = meta.lease_id as string | undefined;
        if (!lease_id) {
          throw new OnlyneError('invalid_request', 'release requires lease_id.');
        }
        const descriptor = await this.service.release(sender, lease_id);
        return descriptor as unknown as Json;
      }
      case 'destroy': {
        const lease_id = meta.lease_id as string | undefined;
        if (!lease_id) {
          throw new OnlyneError('invalid_request', 'destroy requires lease_id.');
        }
        const descriptor = await this.service.destroy(sender, lease_id);
        return descriptor as unknown as Json;
      }
      case 'status': {
        const lease_id = meta.lease_id as string | undefined;
        const res = await this.service.status(sender, lease_id);
        return res as unknown as Json;
      }
      default:
        throw new OnlyneError('invalid_request', `Unknown compute operation: ${operation}`);
    }
  }

  /** Posts the compute result back on the same thread as a reply, preserving the work ref. */
  private async report(entry: Entry, payload: Json): Promise<void> {
    const message: Message = {
      id: crypto.randomUUID(),
      sender: this.node.auth.actor,
      delivery_kind: 'compute-result',
      msg: JSON.stringify(payload),
      meta: {
        thread: entry.thread_ref,
        in_reply_to: { sender: entry.message.sender, id: entry.message.id },
        relation: 'reply',
        ...(entry.message.meta?.work === undefined ? {} : { work: entry.message.meta.work }),
      },
    };
    await this.node.client.send(entry.thread_ref.owner, message, { response: 'none', execution: 'existing' });
  }

  async close(): Promise<void> {
    this.closed = true;
    this.node.client.stopMaintenance();
    if (this.sweepTimer) {
      clearInterval(this.sweepTimer);
      this.sweepTimer = undefined;
    }
  }
}
