import { WechatPortError, createWechatConnector, createWechatPort } from '../src/index.js';
import type {
  WechatConfigResponse, WechatMessage, WechatPort, WechatUpdatesResponse,
} from '../src/port.js';

let failures = 0;
const ok = (label: string, condition: boolean): void => {
  if (condition) console.log(`ok   ${label}`);
  else { failures += 1; console.error(`FAIL ${label}`); }
};

async function waitFor(predicate: () => boolean, label: string): Promise<void> {
  for (let attempt = 0; attempt < 400; attempt += 1) {
    if (predicate()) return;
    await new Promise<void>(resolve => setTimeout(resolve, 5));
  }
  failures += 1;
  console.error(`FAIL timeout: ${label}`);
}

const message = (id: string): WechatMessage => ({
  message_id: id,
  from_user_id: 'wxid_alice',
  message_type: 1,
  create_time_ms: 1730000000000,
  item_list: [{ type: 1, text_item: { text: id } }],
});

const batchOf = (ids: string[], cursor: string): WechatUpdatesResponse => ({
  ret: 0,
  msgs: ids.map(message),
  get_updates_buf: cursor,
});

/** A port whose answers are scripted; with no step left it parks like the real long poll. */
class ScriptedPort implements WechatPort {
  readonly cursors: string[] = [];
  readonly steps: Array<() => Promise<WechatUpdatesResponse>> = [];
  aborted = false;

  async getUpdates(cursor: string, signal?: AbortSignal): Promise<WechatUpdatesResponse> {
    this.cursors.push(cursor);
    const step = this.steps.shift();
    if (step !== undefined) return step();
    return new Promise<WechatUpdatesResponse>(resolve => {
      const done = (): void => { this.aborted = true; resolve({ ret: 0, msgs: [] }); };
      if (signal?.aborted === true) { done(); return; }
      signal?.addEventListener('abort', done, { once: true });
    });
  }

  async sendMessage(): Promise<{ ret: number }> { return { ret: 0 }; }
  async getConfig(): Promise<WechatConfigResponse> { return { ret: 0, typing_ticket: 'ticket' }; }
  async sendTyping(): Promise<void> {}
  async notifyStart(): Promise<void> {}
  async notifyStop(): Promise<void> {}
  rateLimits(): number { return 0; }
}

// --- a rejected event leaves the cursor and is retried ---
const port = new ScriptedPort();
port.steps.push(async () => batchOf(['m-1'], 'buf-1'));
port.steps.push(async () => batchOf(['m-1'], 'buf-1'));
port.steps.push(async () => batchOf(['m-2', 'm-3'], 'buf-2'));

const delivered: string[] = [];
const delays: number[] = [];
let rejectNext = true;
const connector = createWechatConnector({ port, sleep: async ms => { delays.push(ms); } });
connector.onEvent(event => {
  delivered.push(event.source_id);
  if (rejectNext) {
    rejectNext = false;
    throw new Error('listener fault');
  }
});
await connector.start();
await waitFor(() => port.cursors.length >= 4, 'the loop parks after the third batch');

ok('the cursor advances only after a batch is accepted', port.cursors.join(',') === ',,buf-1,buf-2');
ok('a rejected event is redelivered, not dropped', delivered.join(',') === 'm-1,m-1,m-2,m-3');
ok('a batch is emitted in platform order', delivered.slice(2).join(',') === 'm-2,m-3');
ok('a listener failure counts as a retry', connector.status().reconnects === 1);
ok('a listener failure backs off', delays.join(',') === '1000');
ok('the connector is polling after a delivered batch', connector.status().state === 'polling' && connector.status().connected === true);
ok('the last delivered event sets last_event_at', connector.status().last_event_at === new Date(1730000000000).toISOString());

await connector.stop();
ok('stop aborts the long poll', port.aborted);
ok('stop reports stopped', connector.status().state === 'stopped' && connector.status().connected === false);

// --- a batch of skipped messages is still acknowledged ---
const echoPort = new ScriptedPort();
echoPort.steps.push(async () => ({
  ret: 0,
  msgs: [{ message_id: 'echo-1', from_user_id: 'bot', message_type: 2 }],
  get_updates_buf: 'buf-echo',
}));
const echoConnector = createWechatConnector({ port: echoPort, sleep: async () => undefined });
await echoConnector.start();
await waitFor(() => echoPort.cursors.length >= 2, 'the echo batch is acknowledged');
ok('a batch with nothing to emit still advances the cursor', echoPort.cursors[1] === 'buf-echo');
await echoConnector.stop();

// --- an expired session pauses instead of throwing ---
const expiredPort = new ScriptedPort();
expiredPort.steps.push(async () => {
  throw new WechatPortError('The platform rejected the poll call: session expired', { ret: -14 });
});
const expiredDelays: number[] = [];
const expiredConnector = createWechatConnector({ port: expiredPort, sleep: async ms => { expiredDelays.push(ms); } });
let startThrew = false;
try {
  await expiredConnector.start();
} catch {
  startThrew = true;
}
await waitFor(() => expiredPort.cursors.length >= 2, 'the expired loop retries');
const expiredStatus = expiredConnector.status();
ok('an expired session does not throw out of start', startThrew === false);
ok('an expired session pauses the connector', expiredStatus.state === 'paused' && expiredStatus.connected === false);
ok('an expired session surfaces the platform text', expiredStatus.error?.includes('session expired') === true);
ok('an expired session asks for a fresh QR scan', expiredStatus.onboarding?.[0]?.includes('re-scan the ClawBot QR') === true);
ok('an expired session backs off for a minute', expiredDelays.join(',') === '60000');
ok('an expired session keeps polling', expiredPort.cursors.length === 2);
await expiredConnector.stop();

// --- a transport failure reports an error and doubles the backoff ---
const brokenPort = new ScriptedPort();
brokenPort.steps.push(async () => { throw new WechatPortError('The platform call failed: 500 upstream', { status: 500 }); });
brokenPort.steps.push(async () => { throw new WechatPortError('The platform call failed: 500 upstream', { status: 500 }); });
const brokenDelays: number[] = [];
const brokenConnector = createWechatConnector({ port: brokenPort, sleep: async ms => { brokenDelays.push(ms); } });
await brokenConnector.start();
await waitFor(() => brokenPort.cursors.length >= 3, 'the broken loop parks after two retries');
ok('a transport failure reports an error state', brokenConnector.status().state === 'error');
ok('a transport failure counts every retry', brokenConnector.status().reconnects === 2);
ok('the backoff doubles between attempts', brokenDelays.join(',') === '1000,2000');
await brokenConnector.stop();

// --- the real port treats an abort as the end of a poll ---
const hangingFetch = (_input: string, init: RequestInit): Promise<Response> => new Promise((_resolve, reject) => {
  init.signal?.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')), { once: true });
});
const realPort = createWechatPort({ token: 'tok-1', fetch: hangingFetch, timeoutMs: 0 });
const controller = new AbortController();
const pending = realPort.getUpdates('', controller.signal);
controller.abort();
const answer = await pending;
ok('an aborted poll ends as an empty batch', answer.ret === 0 && answer.msgs?.length === 0);

// --- a call that never answers becomes a retryable error instead of a wedge ---
const stalledPort = createWechatPort({ token: 'tok-1', fetch: hangingFetch, timeoutMs: 20 });
let stalled: unknown;
try {
  await stalledPort.notifyStart();
} catch (error) {
  stalled = error;
}
ok('a stalled call times out', stalled instanceof WechatPortError && stalled.message.includes('made no answer within 20 ms'));
ok('a stalled call is retryable', stalled instanceof WechatPortError && stalled.retryable);

if (failures > 0) {
  console.error(`${failures} failure(s)`);
  process.exit(1);
}
console.log('im-wechat poll: all checks passed');
