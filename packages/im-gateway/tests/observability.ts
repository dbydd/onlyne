import { createHash } from 'node:crypto';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { MemoryStorage, OnlyneError, OnlyneNode, type Transport } from '@onlyne/lib';
import type { Connector, ImEvent } from '@onlyne/im-core';
import { createFakeConnector } from '@onlyne/im-fake';
import { createLogger, type LogSink } from '@onlyne/node-host';
import { createImGateway, nodeClock, traceLine, type GatewayConfig, type ImGateway } from '../src/index.js';

/**
 * The logging contract, driven through one scripted run over the im-fake connector: each
 * required event appears exactly once per occurrence, a body never reaches an info line, and a
 * maintenance burst produces at most one summary.
 */

let failures = 0;
const ok = (label: string, condition: boolean): void => {
  if (condition) console.log(`ok - ${label}`);
  else { failures++; console.error(`not ok - ${label}`); }
};
const same = (label: string, actual: unknown, expected: unknown): void => {
  const left = JSON.stringify(actual);
  const right = JSON.stringify(expected);
  if (left === right) console.log(`ok - ${label}`);
  else { failures++; console.error(`not ok - ${label}\n  actual:   ${left}\n  expected: ${right}`); }
};
/** A parsed JSON body is a boundary value; reads go through this guard instead of inline casts. */
const rec = (value: unknown): Record<string, unknown> | undefined =>
  value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : undefined;

interface CapturedLine { level: 'debug' | 'info' | 'warn' | 'error'; message: string; fields: Record<string, unknown> }

/** A captured sink: the host's `info` line plus the three other levels the convention adds. */
function capture(): { sink: LogSink; lines: CapturedLine[]; of(message: string): CapturedLine[] } {
  const lines: CapturedLine[] = [];
  const take = (level: CapturedLine['level']) => (message: string, fields?: Record<string, unknown>): void => {
    lines.push({ level, message, fields: fields ?? {} });
  };
  const sink: LogSink = Object.assign(take('info'), { debug: take('debug'), warn: take('warn'), error: take('error') });
  return { sink, lines, of: message => lines.filter(line => line.message === message) };
}

const sha8 = (text: string): string => createHash('sha256').update(text, 'utf8').digest('hex').slice(0, 8);

const AGENT = 'urn:onlyne:agent:obs-agent';
const NEXT = 'urn:onlyne:agent:obs-next';
const GHOST = 'urn:onlyne:agent:obs-ghost';
const OWNER = 'urn:onlyne:gateway:im-fake-obs';
const GHOST_OWNER = 'urn:onlyne:gateway:im-fake-obs-ghost';
const INBOUND_BODY = '你好 OBS-SECRET-IN';
const OUTBOUND_BODY = '答复 OBS-SECRET-OUT';

const platformEvent = (source: string, text: string): ImEvent => ({
  platform: 'fake', source_id: source, conversation: 'oc_obs', kind: 'message',
  from: 'ou_sender', text, attachments: [], message_handle: `fk_${source}`, at: new Date().toISOString(),
});

/** A reachable peer: an in-process Onlyne node the gateway delivers to and probes. */
const peer = (node_id: string, base_url: string): OnlyneNode => new OnlyneNode({
  node_id,
  base_url,
  plugins: {
    storage: new MemoryStorage(),
    authenticate: async request => {
      const actor = request.headers.get('Onlyne-Actor');
      return actor === null || actor.length === 0 ? null : { actor };
    },
    resolve: async () => { throw new OnlyneError('not_found', 'The test peer resolves nobody.'); },
    decide: () => { throw new OnlyneError('decision_unavailable', 'The test peer decides nothing.'); },
    maintenanceClock: nodeClock(),
    runtime: { notify: () => undefined, supports: async () => false, inject: async () => { throw new OnlyneError('unsupported_delivery', 'no context'); } },
  },
  now: () => Date.now(),
  id: () => crypto.randomUUID(),
});

const peers = new Map<string, { node: OnlyneNode; base_url: string }>();
const addPeer = (node_id: string): void => {
  const base_url = `http://${node_id.slice(node_id.lastIndexOf(':') + 1)}.test/onlyne/v3`;
  peers.set(node_id, { node: peer(node_id, base_url), base_url });
};
addPeer(AGENT);
addPeer(NEXT);

/** Routes one gateway's outbound calls into the in-process peers, by node id. */
const transport = (actor: string): Transport => ({
  async request<T>(target: string, method: string, path: string, body?: unknown): Promise<T> {
    const routed = peers.get(target);
    if (routed === undefined) throw new OnlyneError('unavailable', `The test routes no peer ${target}.`);
    const response = await routed.node.fetch(new Request(`${routed.base_url}${path}`, {
      method,
      headers: { 'content-type': 'application/json', 'Onlyne-Actor': actor },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    }));
    const text = await response.text();
    if (!response.ok) {
      const failure = rec(text === '' ? {} : JSON.parse(text));
      const error = rec(failure?.error);
      throw new OnlyneError(
        (typeof error?.code === 'string' ? error.code : 'unavailable') as 'unavailable',
        typeof error?.message === 'string' ? error.message : 'The peer refused.',
      );
    }
    return (text === '' ? undefined : JSON.parse(text)) as T;
  },
});

const dir = await mkdtemp(join(tmpdir(), 'im-observability-'));
const config = (node: string, port: number, assignee: string, storage: string): GatewayConfig => ({
  node: { id: node },
  server: { bind: '127.0.0.1', port },
  storage_path: join(dir, storage),
  channel: { platform: 'fake', conversation: 'oc_obs', assignee },
});
/** The scripted connector: im-fake for events and answers, plus a status the test moves. */
const scripted = () => {
  const script = { events: [] as ImEvent[], answers: [{ state: 'committed' as const, message_id: 'om_obs_1' }] };
  const fake = createFakeConnector(script);
  const state = { value: 'connecting', error: undefined as string | undefined };
  const connector = {
    ...fake,
    status: () => ({
      state: state.value, connected: state.value === 'online', reconnects: 0, rate_limits: 0,
      ...(state.error === undefined ? {} : { error: state.error }),
    }),
  };
  return { connector: connector as Connector, state };
};
const connectorOf = (built: ReturnType<typeof scripted>) => async (): Promise<Connector> => built.connector;

const endpointOf = (config: GatewayConfig): string => `http://127.0.0.1:${config.server.port}/onlyne/v3`;
const call = async (gateway: ImGateway, method: string, path: string, body?: unknown, actor: string = AGENT) => {
  const response = await gateway.node.fetch(new Request(endpointOf(gateway.config) + path, {
    method,
    headers: { 'content-type': 'application/json', 'Onlyne-Actor': actor },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  }));
  const text = await response.text();
  return { status: response.status, body: (text === '' ? {} : JSON.parse(text)) as Record<string, unknown> };
};
const mutation = () => ({ idempotency_key: crypto.randomUUID(), retry_until: new Date(Date.now() + 60_000).toISOString() });

// ---------------------------------------------------------------------------
// The level convention: `log` stays info, and ONLYNE_LOG_LEVEL selects the rest
// ---------------------------------------------------------------------------
const previousLevel = process.env.ONLYNE_LOG_LEVEL;
process.env.ONLYNE_LOG_LEVEL = 'warn';
const warnLogger = createLogger(false, 10);
warnLogger.debug('debug.line');
warnLogger('bare.call');
warnLogger.log('info.line');
warnLogger.warn('warn.line');
const kept = warnLogger.ring.tail(10).map(line => (JSON.parse(line) as { message: string }).message);
same('ONLYNE_LOG_LEVEL=warn keeps warn and drops debug/info', kept, ['warn.line']);
ok('the logger reports the selected level', warnLogger.level === 'warn');
if (previousLevel === undefined) delete process.env.ONLYNE_LOG_LEVEL;
else process.env.ONLYNE_LOG_LEVEL = previousLevel;
const ring = createLogger(false, 10, undefined, 'debug');
ring.debug('debug.line');
ring.warn('warn.line');
same('an explicit level keeps every line at or above it',
  ring.ring.tail(10).map(line => (JSON.parse(line) as { message: string }).message), ['debug.line', 'warn.line']);

// ---------------------------------------------------------------------------
// One scripted run: startup, one inbound, its replay, one send, a handover
// ---------------------------------------------------------------------------
// 0. The library's traces the host renders: a failed pull warns once per window, an ack is detail.
same('a failed reconcile pull is one warning with its reason and count', traceLine({
  event: 'reconcile.failed', owner: AGENT, thread: 'im-fake-oc_obs', code: 'unavailable',
  message: 'no recorded address answered', attempts: 3,
}), {
  level: 'warn', message: 'reconcile.failed',
  fields: { owner: AGENT, thread: 'im-fake-oc_obs', code: 'unavailable', message: 'no recorded address answered', attempts: 3 },
});
same('an ack that closes an obligation is per-pass detail', traceLine({
  event: 'inbound.acked', obligation: 'ob-1', thread: { owner: AGENT, id: 'im-fake-oc_obs' }, seq: '2',
}), {
  level: 'debug', message: 'inbound.acked',
  fields: { owner: AGENT, thread: 'im-fake-oc_obs', seq: '2', obligation: 'ob-1' },
});
same('a trace the host does not render maps to no line', traceLine({
  event: 'outbound.admitted', sender: OWNER, message: 'm-1', target: AGENT, state: 'accepted',
}), undefined);

const main = capture();
const mainConfig = config(OWNER, 4991, AGENT, 'store-obs.json');
const built = scripted();
const gateway = await createImGateway(mainConfig, {
  storage: 'memory',
  transport: transport(OWNER),
  factories: { fake: connectorOf(built) },
  log: main.sink,
});

// 1. Startup is static: node, endpoint, platform, transport, storage — never a state snapshot.
const listening = main.of('im.gateway.listening');
ok('startup logs exactly one listening line', listening.length === 1);
same('the listening line carries only the static fields',
  Object.keys(listening[0]?.fields ?? {}).sort(),
  ['endpoint', 'node', 'platform', 'storage', 'transport']);
ok('the listening endpoint is the advertised protocol base', listening[0]?.fields.endpoint === endpointOf(mainConfig));
ok('a startup line carries no status snapshot', listening[0]?.fields.status === undefined && listening[0]?.fields.webhook === undefined);
const binds = main.of('resource.binding').filter(line => line.fields.action === 'bind');
ok('the pre-bound conversation logs one bind', binds.length === 1
  && binds[0]?.fields.resource === `${OWNER}/fake-oc_obs`
  && binds[0]?.fields.old_handler === null && binds[0]?.fields.new_handler === AGENT
  && binds[0]?.fields.cursor === '0' && binds[0]?.fields.revision === 'rev-1');

// 2. One accepted platform event: one inbound.event with dedupe new, and one push attempt.
await gateway.receive(platformEvent('p-1', INBOUND_BODY));
const firstEvent = main.of('inbound.event');
ok('one inbound event logs one line, dedupe new', firstEvent.length === 1
  && firstEvent[0]?.fields.conversation === 'oc_obs' && firstEvent[0]?.fields.kind === 'message'
  && firstEvent[0]?.fields.source_id === 'p-1' && firstEvent[0]?.fields.event_seq === '1'
  && firstEvent[0]?.fields.dedupe === 'new');
same('the inbound line names the body by length and digest',
  { len: firstEvent[0]?.fields.len, sha8: firstEvent[0]?.fields.sha8 },
  { len: INBOUND_BODY.length, sha8: sha8(INBOUND_BODY) });
const pushes = main.of('resource.push');
ok('the accepted push logs one attempt', pushes.length === 1
  && pushes[0]?.fields.target === AGENT && pushes[0]?.fields.seq === '1'
  && pushes[0]?.fields.event_id === '1' && pushes[0]?.fields.outcome === 'accepted'
  && pushes[0]?.fields.attempt === 1);

// 3. The replay of the same platform id is a duplicate, and only one line says new.
await gateway.receive(platformEvent('p-1', INBOUND_BODY));
const afterReplay = main.of('inbound.event');
ok('the replay logs a duplicate, not a second new', afterReplay.length === 2
  && afterReplay[1]?.fields.source_id === 'p-1' && afterReplay[1]?.fields.dedupe === 'duplicate'
  && afterReplay.filter(line => line.fields.dedupe === 'new').length === 1);

// 4. One outbound platform send, through the bridge and the connector.
const sent = await call(gateway, 'POST', `/resources/${OWNER}/fake-oc_obs/operations`, {
  ...mutation(), operation_id: 'op-send-1', operation: 'send', data: { text: OUTBOUND_BODY },
});
const sends = main.of('platform.send');
ok('the committed send carries the platform message id', sent.status === 201 && sends.length === 1
  && sends[0]?.fields.conversation === 'oc_obs' && sends[0]?.fields.outcome === 'committed'
  && sends[0]?.fields.message_id === 'om_obs_1' && sends[0]?.fields.code === undefined);
same('the send line names the body by length and digest',
  { len: sends[0]?.fields.len, sha8: sends[0]?.fields.sha8 },
  { len: OUTBOUND_BODY.length, sha8: sha8(OUTBOUND_BODY) });

// 5. A handover: the ack moves the cursor, the commit moves the binding, the pass logs the move.
const acked = await call(gateway, 'POST', `/resources/${OWNER}/fake-oc_obs/events/1/ack`, mutation());
const handover = await call(gateway, 'POST', `/resources/${OWNER}/fake-oc_obs/operations`, {
  ...mutation(), operation_id: 'op-handover-1', operation: 'handover',
  data: { target: NEXT, handover_deadline: new Date(Date.now() + 300_000).toISOString() },
});
ok('the handover commits before the pass observes it', acked.status === 200 && acked.body.cursor === '1'
  && handover.status === 201 && handover.body.state === 'committed');
await gateway.maintain();
const handovers = main.of('resource.binding').filter(line => line.fields.action === 'handover');
ok('the observed handover names the old and the new handler with the cursor', handovers.length === 1
  && handovers[0]?.fields.resource === `${OWNER}/fake-oc_obs`
  && handovers[0]?.fields.old_handler === AGENT && handovers[0]?.fields.new_handler === NEXT
  && handovers[0]?.fields.cursor === '1'
  && typeof handovers[0]?.fields.revision === 'string' && handovers[0]?.fields.revision !== 'rev-1');

// 6. A connector transition pair, then the failure state with its error text.
const states = main.of('connector.state');
ok('the first pass observes the state it found', states.length === 1
  && states[0]?.fields.from === 'unknown' && states[0]?.fields.to === 'connecting'
  && states[0]?.fields.platform === 'fake');
built.state.value = 'online';
await gateway.maintain();
const secondState = main.of('connector.state')[1];
ok('a changed state is one line, chained to the one before', secondState?.fields.from === 'connecting'
  && secondState?.fields.to === 'online');
built.state.value = 'error';
built.state.error = 'the platform closed the socket';
await gateway.maintain();
const thirdState = main.of('connector.state')[2];
ok('the failure path carries the error text', thirdState?.fields.to === 'error'
  && thirdState?.fields.error === 'the platform closed the socket');
ok('a repeated state produces nothing', main.of('connector.state').length === 3);
await gateway.maintain();
ok('a pass with the same state logs no further transition', main.of('connector.state').length === 3);

// 7. A maintenance burst: one summary, never one line per pass.
for (let index = 0; index < 20; index += 1) await gateway.maintain();
const summaries = main.of('gateway.maintain.summary');
ok('a burst of 21 passes produces at most one summary', summaries.length === 1);
const passes = summaries[0]?.fields.passes;
ok('the summary counts the passes it covers', typeof passes === 'number' && passes >= 1
  && summaries[0]?.fields.replayed === 0 && summaries[0]?.fields.failures === 0);

// 8. The live readout: bindings, cursors and the owed count, never a body.
const report = await gateway.statusReport();
const reported = rec(report.resources[0]);
ok('the readout names the node, platform and transport', report.node === OWNER
  && report.platform === 'fake' && report.transport === 'platform');
ok('the readout carries the connector state and the counters', report.connector.state === 'error'
  && report.connector.connected === false && report.counters.events === 2 && report.counters.sends === 1);
ok('the readout carries each resource with its binding and cursor', report.resources.length === 1
  && reported?.handler === NEXT && reported.cursor === '1'
  && typeof reported.revision === 'string' && reported.revision !== 'rev-1');
ok('the readout counts the owed pushes', report.owed === 0);
ok('the readout carries no message body', !JSON.stringify(report).includes('OBS-SECRET'));

// 9. No body text reaches any logged line.
const leaked = main.lines.filter(line => JSON.stringify(line.fields).includes('OBS-SECRET'));
ok('no log line carries a message body', leaked.length === 0);

await gateway.stop();

// ---------------------------------------------------------------------------
// A push that cannot be delivered: one attempt line with its error text
// ---------------------------------------------------------------------------
const ghostRun = capture();
const ghost = scripted();
const ghostGateway = await createImGateway(config(GHOST_OWNER, 4992, GHOST, 'store-ghost.json'), {
  storage: 'memory',
  transport: transport(GHOST_OWNER),
  factories: { fake: connectorOf(ghost) },
  log: ghostRun.sink,
});
await ghostGateway.receive(platformEvent('g-1', '幽灵'));
const ghostPushes = ghostRun.of('resource.push');
ok('an unreachable recipient logs the attempt with its outcome and error', ghostPushes.length === 1
  && ghostPushes[0]?.fields.target === GHOST && ghostPushes[0]?.fields.outcome === 'timeout'
  && ghostPushes[0]?.fields.attempt === 1
  && typeof ghostPushes[0]?.fields.error === 'string' && (ghostPushes[0]?.fields.error as string).length > 0);
const ghostReport = await ghostGateway.statusReport();
ok('the failed push stays owed in the readout', ghostReport.owed === 1);
await ghostGateway.stop();

await rm(dir, { recursive: true, force: true });

if (failures > 0) {
  console.error(`im-gateway observability: ${failures} check(s) failed`);
  process.exit(1);
}
console.log('im-gateway observability: all checks passed');
