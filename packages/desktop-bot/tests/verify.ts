import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createDesktopApp } from '../src/app.ts';
import { createDemoOnlyne, demoFingerprint, ensureDemoTeam, resetDemoWorld } from '../src/demo.ts';
import { fingerprintsMatch, formatFingerprint, moveInRail, normalizeLayout } from '../src/ui/layout.ts';
import { openDesktopStore } from '../src/store.ts';
import { startDesktopServer } from '../src/server.ts';
import type { OnlyneBridgeOptions } from '../src/contracts.ts';

let failures = 0;
const ok = (label: string, condition: boolean): void => {
  if (condition) console.log(`ok   ${label}`);
  else { failures += 1; console.error(`FAIL ${label}`); }
};

const NORTH = 'urn:onlyne:agent:north';
const catalog = ['chat', 'inbox', 'trust', 'settings'];

ok('layout drops an unknown rail id and appends a new one', (() => {
  const layout = normalizeLayout({ version: 1, rail: ['trust', 'missing', 'chat'], hidden: ['chat', 'nope'], dock: 'chat' }, catalog);
  return layout.rail[0] === 'trust' && layout.rail.includes('inbox') && layout.rail.includes('settings')
    && !layout.rail.includes('missing') && layout.hidden.length === 0 && layout.dock === null;
})());

ok('layout rejects a different version', normalizeLayout({ version: 2, rail: ['trust'] }, catalog).rail[0] === 'chat');

ok('moveInRail inserts at the requested index', (() => {
  const next = moveInRail({ version: 1, rail: ['chat', 'inbox', 'trust'], hidden: [], dock: null }, 'trust', 0);
  return next.rail.join(',') === 'trust,chat,inbox';
})());

ok('fingerprint grouping and match ignore spaces', formatFingerprint('abcd1234') === 'abcd 1234' && fingerprintsMatch('ABCD 1234', 'abcd1234') && !fingerprintsMatch('abcd', 'abcd1234'));

const directory = await mkdtemp(join(tmpdir(), 'onlyne-desk-'));
try {
  resetDemoWorld();
  const store = await openDesktopStore(directory);
  ok('demo seed writes the team once', (await ensureDemoTeam(store, directory)) === 'seeded');
  ok('demo seed is kept on the next open', (await ensureDemoTeam(store, directory)) === 'kept');
  const seeded = store.read();
  ok('demo team has three bots and a transcript', seeded.bots.length === 3 && seeded.messages.length >= 8 && seeded.teamAdminId === 'mira');
  ok('demo character and capabilities are present', seeded.bots[0]?.character?.name === 'Mira' && seeded.capabilities.some(item => item.kind === 'mcp'));
  await store.close();

  resetDemoWorld();
  const app = await createDesktopApp({ dataDir: directory, onlyne: createDemoOnlyne, demo: true });
  try {
    const health = await app.command('desktop.health', {}) as { demo?: boolean; bots?: number };
    ok('health reports demo mode', health.demo === true && health.bots === 3);
    const status = await app.command('onlyne.status', { botId: 'mira' }) as {
      node?: { id?: string; state?: string };
      trust?: { pending?: Array<{ nodeId: string; fingerprint: string }> };
      neighbors?: unknown[];
      inbox?: { pending?: number };
    };
    ok('mira node is ready with neighbors and a waiting inbox', status.node?.id === 'urn:onlyne:agent:desktop:mira' && status.node?.state === 'ready' && (status.neighbors?.length ?? 0) >= 4 && (status.inbox?.pending ?? 0) >= 1);
    const pending = status.trust?.pending?.find(item => item.nodeId === NORTH);
    ok('north is waiting with a stable fingerprint', pending?.fingerprint === demoFingerprint(NORTH));
    let rejected = false;
    try { await app.command('onlyne.action', { botId: 'mira', action: 'trust.approve', args: { nodeId: NORTH, fingerprint: 'ffff' } }); }
    catch { rejected = true; }
    ok('approval rejects a fingerprint that was not displayed', rejected);
    const approved = await app.command('onlyne.action', { botId: 'mira', action: 'trust.approve', args: { nodeId: NORTH, fingerprint: demoFingerprint(NORTH) } }) as { phase?: string };
    ok('approval accepts the displayed fingerprint', approved.phase === 'exchanged');
    const resources = await app.command('onlyne.action', { botId: 'mira', action: 'resource.list', args: {} }) as { inventory?: { owners?: unknown[] } };
    ok('resource list includes gateway owners', (resources.inventory?.owners?.length ?? 0) >= 2);
    const inbox = await app.command('desktop.read', { botId: 'mira', uri: 'onlyne://inbox' }) as { content?: string };
    const rows = JSON.parse(inbox.content ?? '[]') as unknown[];
    ok('inbox read returns mira rows', Array.isArray(rows) && rows.length >= 2);
  } finally {
    await app.close();
  }

  resetDemoWorld();
  const serverApp = await createDesktopApp({ dataDir: directory, onlyne: createDemoOnlyne, demo: true });
  const server = await startDesktopServer({ controller: serverApp, port: 0 });
  try {
    const launch = await fetch(server.url, { redirect: 'manual' });
    ok('launch url redirects once', launch.status === 302);
    const cookie = launch.headers.get('set-cookie') ?? '';
    const style = await fetch(`http://127.0.0.1:${server.port}/style.css`);
    const css = await style.text();
    ok('stylesheet publishes the type and color tokens', css.includes('--display') && css.includes('#14120e') && css.includes('#f6f3ec'));
    const bundle = await fetch(`http://127.0.0.1:${server.port}/app.js`);
    const source = await bundle.text();
    ok('browser bundle includes the desk surfaces', source.includes('Inbox') && source.includes('Trust') && source.includes('Integrations'));
    const page = await fetch(`http://127.0.0.1:${server.port}/`, { headers: { cookie: cookie.split(';')[0] ?? '' } });
    ok('session cookie loads the shell', page.ok && (await page.text()).includes('Onlyne'));
    const font = await fetch(`http://127.0.0.1:${server.port}/fonts/source-sans-3-400.woff2`);
    ok('ui font is served', font.ok && font.headers.get('content-type') === 'font/woff2');
  } finally {
    await server.close();
    await serverApp.close();
  }
} finally {
  await rm(directory, { recursive: true, force: true });
}

const options: OnlyneBridgeOptions = {
  dataDir: directory,
  nodeId: 'urn:onlyne:agent:desktop:mira',
  peers: [],
  contexts: () => [],
  inbound: async () => undefined,
  decide: async () => ({}),
  emit: () => undefined,
};
resetDemoWorld();
const bridge = await createDemoOnlyne(options);
const read = await bridge.read('onlyne://threads');
ok('direct bridge read is json', read.contentType === 'application/json' && read.content.includes('grokbot:dm:'));

if (failures > 0) {
  console.error(`${failures} failed`);
  process.exit(1);
}
console.log('desktop desk checks passed');
