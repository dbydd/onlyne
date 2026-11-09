import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createDesktopApp } from '../src/app.ts';
import { createDemoOnlyne, demoFingerprint, ensureDemoTeam, resetDemoWorld } from '../src/demo.ts';
import { fingerprintsMatch, formatFingerprint, moveInRail, normalizeLayout } from '../src/ui/layout.ts';
import { assignDeliveryErrors, commandKey, contrastRatio, deskTracks, drawerWanted, filterByQuery, fingerprintHint, fingerprintMarks, formatDeskHash, formatWhen, friendlyRuntimeError, middleEllipsis, navigationFromKey, parseDeskHash, parsePeerUrl, peerPhaseLabel, peerSnapshot, presentIdentifier, revokeConfirmModel, revokePress, samePeerRecord, shouldToastDelivery, tracksFit, typeInField } from '../src/desk-format.ts';
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
ok('peer url rejects a fragment and accepts a service url', !parsePeerUrl('htt').ok && !parsePeerUrl('peer').ok && parsePeerUrl('http://127.0.0.1:3992/onlyne/v3').ok);
ok('fingerprint groups report a mismatch', fingerprintMarks('3603 b38b 722f', '3603b38b722f2052').includes('miss') === false && fingerprintHint(fingerprintMarks('3603 b38b ffff', '3603b38b722f2052848a6909fdaf51f0')).includes('does not match'));
ok('times are relative when recent and absolute later', formatWhen(Date.now() - 30_000, Date.now()).label === 'Just now' && formatWhen('2026-11-01T00:00:00.000Z', Date.parse('2026-11-04T00:00:00.000Z')).label.includes('2026'));
ok('identifiers keep both ends', middleEllipsis('urn:onlyne:agent:desktop:mira', 8, 4).startsWith('urn:only') && middleEllipsis('urn:onlyne:agent:desktop:mira', 8, 4).endsWith('mira'));
ok('command key follows the platform', commandKey('MacIntel', '') === '⌘' && commandKey('Linux', '') === 'Ctrl');
ok('peer phases are human labels', peerPhaseLabel('no_exchange') === 'Not paired' && peerPhaseLabel('exchanged') === 'Trusted');
ok('hash round trip keeps screen and dock apart', (() => {
  const encoded = formatDeskHash({ screen: 'trust', selection: 'c-desk', dock: 'health' });
  const parsed = parseDeskHash(encoded);
  return encoded === '#/trust/c-desk?dock=health' && parsed.screen === 'trust' && parsed.selection === 'c-desk' && parsed.dock === 'health' && parsed.screen !== parsed.dock;
})());
ok('a filter keeps a newly pinned peer', filterByQuery(
  [{ id: 'north' }, { id: 'mira' }],
  'zzz',
  row => row.id,
  row => row.id === 'north',
).some(row => row.id === 'north'));
ok('old timestamps are not raw ISO', !formatWhen('2026-11-03T16:40:00.000Z', Date.parse('2026-11-04T12:00:00.000Z')).label.includes('T16:40'));
ok('a missing runtime is explained without the raw spawn text', (() => {
  const text = friendlyRuntimeError('Executable not found in $PATH: "omp"');
  return text !== undefined && !text.includes('$PATH') && text.includes('OMP');
})());
ok('typing mcgro in a field keeps every letter and does not navigate', (() => {
  const typed = typeInField('mcgro');
  return typed.text === 'mcgro' && typed.screens.length === 0 && typed.text.includes('g') && typed.text.includes('c');
})());
ok('g then c still opens conversations when nothing is being typed', (() => {
  const armed = navigationFromKey({ armed: 0 }, { key: 'g', target: 'other', meta: false, ctrl: false, alt: false, now: 1_000 });
  const next = navigationFromKey(armed.clock, { key: 'c', target: 'other', meta: false, ctrl: false, alt: false, now: 1_200 });
  return armed.screen === null && next.screen === 'chat';
})());
ok('the drawer is only for a narrow viewport', !drawerWanted(1280) && !drawerWanted(1920) && drawerWanted(800));
ok('revoke asks once and commits on the second press', revokePress('idle') === 'armed' && revokePress('armed') === 'commit');
ok('rail, side, stage and dock fit at wide widths', [1280, 1600, 1920, 2560].every(width =>
  tracksFit(width, deskTracks({ dock: true, members: true }))
  && tracksFit(width, deskTracks({ dock: true, members: false }))
  && tracksFit(width, deskTracks({ dock: false, members: false }))));
ok('a second failure keeps the first retry until that send succeeds', (() => {
  const rows = [
    { id: 'u1', role: 'user', conversationId: 'c', createdAt: 10 },
    { id: 'u2', role: 'user', conversationId: 'c', createdAt: 20 },
  ];
  const first = assignDeliveryErrors(rows, 'c', new Map([['u1', 'missing runtime']]), new Set(), [{ text: 'missing runtime', at: 20 }]);
  const kept = first.failures.get('u1') === 'missing runtime' && first.failures.get('u2') === 'missing runtime';
  const solved = assignDeliveryErrors(
    [...rows, { id: 'a2', role: 'assistant', conversationId: 'c', createdAt: 30 }],
    'c', first.failures, new Set(), [],
  );
  const dismissed = assignDeliveryErrors(rows, 'c', first.failures, new Set(['u1']), []);
  return kept && !solved.failures.has('u2') && solved.failures.has('u1') && !dismissed.failures.has('u1') && dismissed.failures.has('u2');
})());
const deskCss = await readFile(new URL('../src/ui/style.css', import.meta.url), 'utf8');
ok('the stage track can shrink and the desktop nav fills its column', deskCss.includes('minmax(0, 1fr)') && /@media \(min-width: 901px\)[\s\S]*width:\s*auto/.test(deskCss));
ok('the navigation button is hidden while the side column is on the grid', deskCss.includes('.btn.only-mobile') && deskCss.includes('display: none'));
ok('an inline failure is not also a toast', shouldToastDelivery(true) === false && shouldToastDelivery(false) === true);
ok('revoke confirm stays inside the action row', revokeConfirmModel(false).grows === false && revokeConfirmModel(true).overlay === true && revokeConfirmModel(true).label === 'Revoke trust' && revokeConfirmModel(false).note === '');
ok('identifiers keep the useful tail', presentIdentifier('http://127.0.0.1:3992/onlyne/v3') === '127.0.0.1:3992/onlyne/v3' && presentIdentifier('urn:onlyne:agent:desktop:mira') === 'agent:desktop:mira');
ok('light text colors clear AA', contrastRatio('#3a342c', '#f6f3ec') >= 4.5 && contrastRatio('#1c1915', '#e7e0d4') >= 4.5);

const directory = await mkdtemp(join(tmpdir(), 'onlyne-desk-'));
try {
  resetDemoWorld();
  const store = await openDesktopStore(directory);
  ok('demo seed writes the team once', (await ensureDemoTeam(store, directory)) === 'seeded');
  ok('demo seed is kept on the next open', (await ensureDemoTeam(store, directory)) === 'kept');
  await store.update(state => {
    state.messages = state.messages.filter(item => item.id !== 'msg-desk-4');
    const first = state.messages[0];
    if (first !== undefined) first.text = 'Continue the scene as the character described above.';
  });
  ok('an old demo transcript is reseeded', (await ensureDemoTeam(store, directory)) === 'seeded');
  const seeded = store.read();
  ok('demo team has three bots and a transcript', seeded.bots.length === 3 && seeded.messages.length >= 8 && seeded.teamAdminId === 'mira');
  ok('seeded messages are natural prose', seeded.messages.every(item => !item.text.includes('Continue the scene') && !/\S{40,}/.test(item.text)));
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
    const before = (await app.command('onlyne.status', { botId: 'mira' }) as { neighbors?: Array<Record<string, unknown>> }).neighbors?.find(item => item.nodeId === 'urn:onlyne:agent:desktop:jun');
    const shot = before === undefined ? undefined : peerSnapshot({ ...before, name: 'Jun', notes: 'filesystem server' });
    const forgotten = shot === undefined ? undefined : await app.command('onlyne.action', { botId: 'mira', action: 'peer.forget', args: { nodeId: shot.nodeId } }) as { peer?: Record<string, unknown> };
    const gone = (await app.command('onlyne.status', { botId: 'mira' }) as { neighbors?: Array<{ nodeId: string }> }).neighbors?.some(item => item.nodeId === 'urn:onlyne:agent:desktop:jun') !== true;
    if (shot !== undefined && forgotten?.peer !== undefined) await app.command('onlyne.action', { botId: 'mira', action: 'peer.restore', args: { peer: { ...forgotten.peer, name: shot.name, notes: shot.notes } } });
    const restored = (await app.command('onlyne.status', { botId: 'mira' }) as { neighbors?: Array<Record<string, unknown>> }).neighbors?.find(item => item.nodeId === 'urn:onlyne:agent:desktop:jun');
    ok('undo restores the trusted peer exactly', shot !== undefined && gone && restored !== undefined && samePeerRecord(peerSnapshot({ ...restored, name: 'Jun', notes: 'filesystem server' }), shot) && restored.phase === 'exchanged' && restored.fingerprint === shot.fingerprint);
    const resources = await app.command('onlyne.action', { botId: 'mira', action: 'resource.list', args: {} }) as { inventory?: { owners?: unknown[] } };
    ok('resource list includes gateway owners', (resources.inventory?.owners?.length ?? 0) >= 2);
    const inbox = await app.command('desktop.read', { botId: 'mira', uri: 'onlyne://inbox' }) as { content?: string };
    const rows = JSON.parse(inbox.content ?? '[]') as unknown[];
    ok('inbox read returns mira rows', Array.isArray(rows) && rows.length >= 2);
    const heard: string[] = [];
    app.subscribe(event => { if (event.type === 'error' && typeof event.message === 'string') heard.push(event.message); });
    await app.command('message.send', { conversationId: 'c-desk', text: '/fail' });
    await new Promise(resolve => setTimeout(resolve, 50));
    ok('sending /fail reports a missing runtime and does not invent a reply', heard.some(item => item.includes('omp')) && !app.snapshot().messages.some(item => item.text.startsWith('Demo reply') && item.text.includes('/fail')));
    await app.command('message.send', { conversationId: 'c-desk', text: 'Leave the north fingerprint on the trust screen.' });
    await new Promise(resolve => setTimeout(resolve, 700));
    const reply = app.snapshot().messages.find(item => item.role === 'assistant' && item.text.includes('north fingerprint'));
    ok('a demo reply quotes the user, not the scene prompt', reply !== undefined && !reply.text.includes('Continue the scene') && !reply.text.includes('I will not pair a peer from a sentence'));
    await app.command('message.send', { conversationId: 'c-notes', text: 'Where does the filesystem server live?' });
    await new Promise(resolve => setTimeout(resolve, 700));
    const jun = app.snapshot().messages.find(item => item.role === 'assistant' && item.botId === 'jun' && item.text.includes('filesystem server'));
    const lark = app.snapshot().messages.find(item => item.botId === 'lark');
    ok('each persona answers in its own short voice', jun !== undefined && jun.text.includes('source') && lark !== undefined && lark.text.includes('gateway') && jun.text !== lark.text);
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
