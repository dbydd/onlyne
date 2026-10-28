import type { OnlyneNode, ServiceDescription } from '@onlyne/lib';
import { NetworkDiscovery } from './network.ts';
import type { LogRing } from './log.ts';
import { GatewayTui } from './tui.ts';

export { createNodeKey } from './keys.ts';
export { NetworkDiscovery } from './network.ts';
export { createLogger, leveled, resolveLogLevel } from './log.ts';
export type { GatewayLogger, LeveledLog, LogLevel, LogLine, LogRing, LogSink } from './log.ts';
export { GatewayTui } from './tui.ts';

export interface NodeHostOptions {
  node: OnlyneNode;
  endpoint: string;
  bind: string;
  seeds?: string[];
  /** The log ring the TUI renders; the host's logger owns it. */
  ring?: LogRing;
  /** Defaults to the terminal surface when both ends of the process are a TTY. */
  interactive?: boolean;
  /** Gateway-specific state rendered with the trust terminal. */
  status?: () => Promise<string[]>;
  log(message: string, fields?: Record<string, unknown>): void;
}

/**
 * The host half every gateway shares: publish the bound endpoint over DNS-SD, browse for
 * peers, re-observe the catalog, and — on a terminal — hand the operator the trust surface.
 * Discovery only adds candidates; a key exchange still needs both fingerprints confirmed.
 */
export async function startNodeHost(options: NodeHostOptions): Promise<{ close(): Promise<void> }> {
  const { node, log } = options;
  const controller = new AbortController();
  const response = await node.fetch(new Request(`${options.endpoint}/description`));
  const description = await response.json() as ServiceDescription;
  const network = new NetworkDiscovery({
    node_id: node.auth.actor, instance_id: description.instance_id, endpoint: options.endpoint, bind: options.bind,
    onCandidate: async (url, source, expectedNode) => {
      if (expectedNode === node.auth.actor) return;
      await node.discovery.observe(url, source, expectedNode, controller.signal);
    },
    onError: error => log('onlyne.discovery.failed', { error: String(error) }),
  });
  try { await network.start(); }
  catch (error) { log('onlyne.discovery.unavailable', { error: String(error) }); }
  for (const seed of options.seeds ?? []) {
    try { await node.discovery.observe(seed, 'bootstrap', undefined, controller.signal); }
    catch (error) { log('onlyne.discovery.failed', { seed, error: String(error) }); }
  }
  let refreshing = false;
  const timer = setInterval(() => {
    if (refreshing) return;
    refreshing = true;
    void node.discovery.refresh(controller.signal)
      .catch(error => { if (!controller.signal.aborted) log('onlyne.discovery.failed', { error: String(error) }); })
      .finally(() => { refreshing = false; });
  }, 30_000);

  let tui: GatewayTui | undefined;
  // `ONLYNE_NO_TUI=1` keeps the plain JSON log on stdout, for a pipe or a screen a TUI would spoil.
  const interactive = (options.interactive ?? true) && process.env.ONLYNE_NO_TUI !== '1';
  if (interactive && options.ring !== undefined && process.stdin.isTTY && process.stdout.isTTY) {
    const surface = new GatewayTui(node, options.ring, options.endpoint, undefined, options.status);
    try {
      log('onlyne.tui.starting');
      await surface.start();
      tui = surface;
      log('onlyne.tui.ready');
    } catch (error) {
      log('onlyne.tui.unavailable', { error: String(error) });
    }
  }

  return {
    async close(): Promise<void> {
      clearInterval(timer);
      await tui?.close();
      controller.abort();
      await network.close();
    },
  };
}
