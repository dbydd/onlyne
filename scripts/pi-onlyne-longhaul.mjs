#!/usr/bin/env node
import { createConnection } from 'node:net';
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, openSync, writeSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';

const args = parseArgs(process.argv.slice(2));
const root = findWorkspace(args.workspace ? resolve(String(args.workspace)) : process.cwd());
if (!root) die('no .onlyne workspace found; run from the project or pass --workspace <dir>');

const socketPath = join(root, '.onlyne', 'run', 'onlyne.sock');
const logPath = String(args.log ?? join(root, '.onlyne', 'logs', 'pi-onlyne-longhaul.jsonl'));
mkdirSync(dirname(logPath), { recursive: true });
const logFd = openSync(logPath, 'a');

let daemon;
let sub;
let stopping = false;
let reconnectMs = 1000;
let sentCount = 0;
let recvCount = 0;
let missedCount = 0;
let lastInboundAt = null;
const seenInbound = new Set();
const startedAtMs = Date.now();
const armAfterMs = args.armAfter ? parseDuration(args.armAfter) : 0;

process.on('SIGINT', stop);
process.on('SIGTERM', stop);

main().catch((err) => {
  event('fatal', { error: String(err?.stack || err) });
  process.exitCode = 1;
  stop();
});

async function main() {
  event('start', { root, socketPath, logPath, args });
  if (args.start) await ensureDaemon();
  await waitForSocket(socketPath, Number(args.socketTimeoutMs ?? 15000));
  subscribeLoop();
  heartbeatLoop();
  if (armAfterMs > 0) event('armed_timer', { armAfterMs, armedAt: new Date(startedAtMs + armAfterMs).toISOString() });
  await reconcileHistory('startup');
  if (args.sendEvery) sendLoop();
  if (args.duration) setTimeout(stop, parseDuration(args.duration));
}

async function ensureDaemon() {
  try {
    await request(socketPath, { id: 'ping-before-start', op: 'ping' });
    event('daemon_external', {});
    return;
  } catch {}

  const bin = String(args.onlyneBin ?? 'onlyne');
  daemon = spawn(bin, ['--workspace', root, 'run'], {
    cwd: root,
    stdio: ['ignore', 'ignore', 'ignore'],
    detached: false,
  });
  daemon.on('exit', (code, signal) => event('daemon_exit', { code, signal }));
  event('daemon_started', { pid: daemon.pid, bin });
}

function subscribeLoop() {
  if (stopping) return;
  sub = createConnection(socketPath);
  let buf = '';
  sub.setEncoding('utf8');
  sub.on('connect', () => {
    reconnectMs = 1000;
    event('subscribe_connect', {});
    sub.write('{"id":"sub","op":"subscribe_events"}\n');
    reconcileHistory('subscribe_connect').catch((error) => event('history_reconcile_error', { reason: 'subscribe_connect', error: String(error?.message || error) }));
  });
  sub.on('data', (chunk) => {
    buf += chunk;
    for (;;) {
      const idx = buf.indexOf('\n');
      if (idx < 0) break;
      const raw = buf.slice(0, idx);
      buf = buf.slice(idx + 1);
      if (!raw.trim()) continue;
      let line;
      try { line = JSON.parse(raw); } catch (error) { event('bad_json', { raw, error: String(error) }); continue; }
      handleLine(line).catch((error) => event('handle_error', { error: String(error?.stack || error), line }));
    }
  });
  sub.on('error', (error) => event('subscribe_error', { error: String(error.message || error) }));
  sub.on('close', () => {
    event('subscribe_close', {});
    if (!stopping) {
      const wait = reconnectMs;
      reconnectMs = Math.min(reconnectMs * 2, 30000);
      setTimeout(subscribeLoop, wait);
    }
  });
}

async function handleLine(line) {
  if (!line?.event) return event('subscribe_response', line);
  if (line.type !== 'inbound_message') return event('event', { type: line.type });

  const inbound = inboundText(line);
  if (!inbound) return event('inbound_unparsed', line);
  await observeInbound(inbound, 'inbound', false);
}

function inboundText(line) {
  const msg = line?.data?.data ?? line?.data ?? line;
  const channelId = msg.channel_id ?? msg.channelId;
  const conversationId = msg.conversation_id ?? msg.conversationId;
  const messageId = msg.message_id ?? msg.messageId;
  const text = msg.text ?? msg.content ?? msg.body;
  const timestamp = msg.timestamp;
  return channelId && conversationId && typeof text === 'string'
    ? { channelId, conversationId, messageId, text, timestamp }
    : null;
}

async function observeInbound(inbound, eventType, missed) {
  if (!inbound) return;
  if (inbound.messageId && seenInbound.has(inbound.messageId)) return;
  if (inbound.messageId) seenInbound.add(inbound.messageId);
  if (missed) missedCount++;
  else recvCount++;
  const now = Date.now();
  const eventTime = inbound.timestamp ? Date.parse(inbound.timestamp) || now : now;
  const idleMs = eventTime - (lastInboundAt ? Date.parse(lastInboundAt) : startedAtMs);
  lastInboundAt = new Date(eventTime).toISOString();
  const armed = armAfterMs === 0 || eventTime - startedAtMs >= armAfterMs;
  event(armed ? `${eventType}_after_idle` : `${eventType}_before_arm`, { ...inbound, idleMs, armed });

  if (!args.noConsume && inbound.messageId) {
    const res = await request(socketPath, {
      id: `consume-${Date.now()}`,
      op: 'mark_io_consumed',
      message_id: inbound.messageId,
    });
    event('mark_consumed', { ok: !!res.ok, messageId: inbound.messageId, error: res.error });
  }
  if (armed && args.exitOnInbound) setTimeout(stop, 50);
}

async function reconcileHistory(reason) {
  const limit = Number(args.reconcileLimit ?? 50);
  const res = await request(socketPath, {
    id: `hist-${Date.now()}`,
    op: 'fetch_all_history',
    limit,
  });
  if (!res.ok || !Array.isArray(res.data)) {
    event('history_reconcile_failed', { reason, error: res.error ?? res });
    return;
  }
  let found = 0;
  for (const msg of res.data.slice().reverse()) {
    if (msg.direction !== 'inbound') continue;
    if (!msg.message_id || seenInbound.has(msg.message_id)) continue;
    const ts = msg.timestamp ? Date.parse(msg.timestamp) : 0;
    if (ts && ts < startedAtMs - parseDuration(args.reconcileSince ?? '5m')) continue;
    found++;
    await observeInbound(inboundText(msg), 'missed_inbound', true);
  }
  if (found) event('history_reconciled', { reason, found });
}

async function heartbeatLoop() {
  while (!stopping) {
    await sleep(parseDuration(args.pingEvery ?? '60s'));
    if (stopping) break;
    try {
      const ping = await request(socketPath, { id: `ping-${Date.now()}`, op: 'ping' });
      const status = await request(socketPath, { id: `status-${Date.now()}`, op: 'status' });
      await reconcileHistory('heartbeat');
      event('heartbeat', { ok: !!ping.ok && !!status.ok, channels: status.data?.channels, recvCount, missedCount, sentCount, lastInboundAt });
    } catch (error) {
      event('heartbeat_error', { error: String(error?.message || error) });
    }
  }
}

async function sendLoop() {
  const channelId = String(args.channel ?? 'qqbot');
  while (!stopping) {
    await sleep(parseDuration(args.sendEvery));
    if (stopping) break;
    await sendSmoke(channelId);
  }
}

async function sendSmoke(channelId) {
  const text = String(args.text ?? `Onlyne longhaul smoke\n\n- channel: ${channelId}\n- time: ${new Date().toISOString()}\n- sent: ${sentCount + 1}`);
  const attempts = Number(args.attempts ?? 3);
  let last;
  for (let i = 0; i < Math.max(1, attempts); i++) {
    try {
      const res = await request(socketPath, {
        id: `send-${Date.now()}-${i}`,
        op: 'send_message',
        channel_id: channelId,
        text,
        raw_text: args.rawText === true,
      });
      last = res;
      if (res.ok) {
        sentCount++;
        event('send_ok', { channelId, messageId: res.data?.message_id, sentCount });
        return;
      }
    } catch (error) {
      last = { error: String(error?.message || error) };
    }
    await sleep(Math.min(1000 * (i + 1), 5000));
  }
  event('send_failed', { channelId, last });
}

function request(path, req) {
  return new Promise((resolve, reject) => {
    const socket = createConnection(path);
    let data = '';
    socket.setEncoding('utf8');
    socket.on('error', reject);
    socket.on('connect', () => socket.write(`${JSON.stringify(req)}\n`));
    socket.on('data', (chunk) => {
      data += chunk;
      const idx = data.indexOf('\n');
      if (idx < 0) return;
      socket.end();
      try { resolve(JSON.parse(data.slice(0, idx))); } catch (error) { reject(error); }
    });
  });
}

async function waitForSocket(path, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let last;
  while (Date.now() < deadline) {
    try {
      const res = await request(path, { id: 'wait-ping', op: 'ping' });
      if (res.ok) return;
      last = new Error(JSON.stringify(res));
    } catch (error) { last = error; }
    await sleep(250);
  }
  throw last ?? new Error('socket not ready');
}

function findWorkspace(start) {
  let dir = resolve(start);
  for (;;) {
    if (existsSync(join(dir, '.onlyne', 'config.toml'))) return dir;
    const parent = dirname(dir);
    if (parent === dir) return null;
    dir = parent;
  }
}

function parseArgs(argv) {
  const out = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--start') out.start = true;
    else if (a === '--raw-text') out.rawText = true;
    else if (a === '--exit-on-inbound') out.exitOnInbound = true;
    else if (a === '--no-consume') out.noConsume = true;
    else if (a === '--show-heartbeat') out.showHeartbeat = true;
    else if (a === '--help' || a === '-h') usage();
    else if (a.startsWith('--')) {
      const key = a.slice(2).replace(/-([a-z])/g, (_, c) => c.toUpperCase());
      const next = argv[i + 1];
      if (!next || next.startsWith('--')) die(`missing value for ${a}`);
      out[key] = next;
      i++;
    } else die(`unknown arg: ${a}`);
  }
  return out;
}

function parseDuration(value) {
  if (typeof value === 'number') return value;
  const s = String(value).trim();
  const m = s.match(/^(\d+(?:\.\d+)?)(ms|s|m|h|d)?$/);
  if (!m) die(`bad duration: ${s}`);
  const n = Number(m[1]);
  const unit = m[2] ?? 'ms';
  return n * ({ ms: 1, s: 1000, m: 60000, h: 3600000, d: 86400000 }[unit]);
}

function event(type, data) {
  const row = { ts: new Date().toISOString(), type, ...data };
  const line = `${JSON.stringify(row)}\n`;
  writeSync(logFd, line);
  if (type !== 'heartbeat' || args.showHeartbeat) console.log(line.trim());
}

function sleep(ms) { return new Promise((r) => setTimeout(r, ms)); }

function stop() {
  if (stopping) return;
  stopping = true;
  event('stop', { recvCount, sentCount, lastInboundAt });
  try { sub?.destroy(); } catch {}
  try { daemon?.kill('SIGTERM'); } catch {}
  process.exit();
}

function die(message) {
  console.error(message);
  process.exit(2);
}

function usage() {
  console.log(`Usage:
  scripts/pi-onlyne-longhaul.mjs [options]

Options:
  --workspace <dir>       Workspace root or child directory. Defaults to cwd/upward search.
  --start                 Start 'onlyne --workspace <root> run' if socket is not already live.
  --onlyne-bin <path>     onlyne binary for --start. Defaults to 'onlyne'.
  --log <path>            JSONL log path. Defaults to .onlyne/logs/pi-onlyne-longhaul.jsonl.
  --ping-every <dur>      Ping/status interval. Defaults to 60s. Logged to file only by default.
  --show-heartbeat        Also print heartbeat rows to the pane.
  --duration <dur>        Stop after duration, e.g. 12h or 1d. Defaults to forever.
  --arm-after <dur>       Mark inbound as long-idle only after this duration, e.g. 12h.
  --reconcile-limit <n>   Recent history rows checked for missed inbounds. Defaults to 50.
  --reconcile-since <dur> On startup, ignore history older than this before start. Defaults to 5m.
  --exit-on-inbound       Exit after the first armed inbound is observed.
  --no-consume            Do not call mark_io_consumed. Default mimics pi-onlyne and consumes.
  --send-every <dur>      Optional periodic send interval, e.g. 30m. Omit for idle-inbound tests.
  --channel <id>          Channel for periodic send. Defaults to qqbot.
  --text <text>           Periodic send text. Defaults to timestamp smoke Markdown.
  --raw-text              Send periodic text literally instead of Markdown.
  --attempts <n>          send_message retry attempts. Defaults to 3.

Examples:
  # Best long-idle inbound test: leave it quiet, send QQ manually after 12h/1d.
  node scripts/pi-onlyne-longhaul.mjs --duration 1d --arm-after 12h --exit-on-inbound

  # Optional outbound watchdog, not recommended for reproducing idle-inbound bugs.
  node scripts/pi-onlyne-longhaul.mjs --start --onlyne-bin target/debug/onlyne --duration 1d --send-every 1h --channel qqbot
`);
  process.exit(0);
}
