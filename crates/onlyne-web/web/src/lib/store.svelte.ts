// The one state the screens render, fed by the SSE stream the Rust side
// folds. The browser holds no fold of its own: `view` and `boards` arrive
// already read off `onlyne-proto::view`, and every change below is a frame
// the stream pushed (`docs/v2-PLAN.md` line 357: 浏览器只渲染、只发操作).

import { getView, postOp, Refused } from './api';
import type { View } from '../gen/View';
import type { Board } from '../gen/Board';

export const app = $state({
  token: '',
  connected: false,
  cursor: 0,
  link: 'offline: connecting',
  view: { cluster: {}, roles: {}, sessions: {}, deliveries: {}, faults: {}, event_tail: [], stale: false } as unknown as View,
  boards: [] as Board[],
  selectedFamily: null as string | null,
  notice: '',
});

let source: EventSource | null = null;
let reconnectTimer: number | null = null;

export function setNotice(text: string) {
  app.notice = text;
  window.setTimeout(() => {
    if (app.notice === text) app.notice = '';
  }, 6000);
}

export function selectFamily(family: string | null) {
  app.selectedFamily = app.selectedFamily === family ? null : family;
}

function adopt(payload: { cursor: number; link: string; view: unknown; boards: unknown[] }) {
  app.cursor = payload.cursor;
  app.link = payload.link;
  app.view = payload.view as unknown as View;
  app.boards = payload.boards as unknown as Board[];
}

export function connect(token: string) {
  app.token = token;
  if (source) source.close();
  if (reconnectTimer !== null) window.clearTimeout(reconnectTimer);

  // The first frame comes from the view read; the stream then carries every
  // change, resuming from the cursor the last frame named.
  getView(token)
    .then((payload) => {
      adopt(payload);
      openStream();
    })
    .catch(() => {
      app.link = 'offline: no server answer';
      reconnectTimer = window.setTimeout(() => connect(token), 1500);
    });
}

function openStream() {
  const token = app.token;
  source = new EventSource(`/api/stream?token=${encodeURIComponent(token)}&cursor=${app.cursor}`);
  source.addEventListener('view', (event) => {
    adopt(JSON.parse((event as MessageEvent).data));
    app.connected = true;
  });
  source.onerror = () => {
    app.connected = false;
    source?.close();
    source = null;
    // Resume from the last cursor the stream named: the server sends the
    // current frame only if we are behind it.
    reconnectTimer = window.setTimeout(() => connect(token), 1000);
  };
}

/// The `spec_get` answer, guarded at the two fields the edit needs.
export interface SpecView {
  source_hash: string;
  spec: { client?: Array<{ role?: string; allowed_targets?: string[] }> };
}

function asSpecView(answer: unknown): SpecView {
  if (answer !== null && typeof answer === 'object' && 'source_hash' in answer && 'spec' in answer) {
    const sourceHash: unknown = answer.source_hash;
    const spec: unknown = answer.spec;
    if (typeof sourceHash === 'string' && spec !== null && typeof spec === 'object' && 'client' in spec) {
      const client: unknown = spec.client;
      const entries = Array.isArray(client) ? client : [];
      return {
        source_hash: sourceHash,
        spec: { client: entries.flatMap(roleEntry) },
      };
    }
  }
  throw new Error('the spec answer carried no hash');
}

function roleEntry(entry: unknown): Array<{ role?: string; allowed_targets?: string[] }> {
  if (entry === null || typeof entry !== 'object') return [];
  const role = 'role' in entry && typeof entry.role === 'string' ? entry.role : undefined;
  const targets =
    'allowed_targets' in entry && Array.isArray(entry.allowed_targets)
      ? entry.allowed_targets.filter((target): target is string => typeof target === 'string')
      : undefined;
  return [{ role, allowed_targets: targets }];
}

function refusalText(error: unknown): string {
  if (error instanceof Refused) return error.message;
  if (error instanceof Error) return error.message;
  return String(error);
}

function isConflict(error: unknown): boolean {
  return error instanceof Refused && error.code === 'conflict';
}

/// Add one allowed route by applying the typed edit the server knows, with
/// one retry when the spec moved under the hash we read.
export async function addRoute(role: string, target: string) {
  for (let attempt = 0; attempt < 2; attempt += 1) {
    const view = asSpecView(await postOp(app.token, { op: 'spec_get' }));
    const entry = view.spec.client?.find((client) => client.role === role);
    const targets = entry?.allowed_targets ?? [];
    if (targets.includes(target)) {
      setNotice(`${role} → ${target} is already allowed`);
      return;
    }
    try {
      // The body travels under `args`, as every op that carries one does —
      // `base_hash` and `edits` at the top level are not the shape the server
      // reads, so the edit was refused however well the hash was.
      await postOp(app.token, {
        op: 'spec_apply',
        args: {
          base_hash: view.source_hash,
          edits: [{ edit: 'set_targets', args: { role, targets: [...targets, target] } }],
        },
      });
      setNotice(`allowed ${role} → ${target}; the spec reloaded`);
      return;
    } catch (error) {
      if (isConflict(error) && attempt === 0) continue;
      setNotice(`spec edit refused: ${refusalText(error)}`);
      return;
    }
  }
}

/// Write a task to a board: a `_supervisor` send, so its receipt lands on the
/// operator's board.
export async function sendTask(role: string, text: string) {
  try {
    const answer = await postOp(app.token, { op: 'send', args: { to: role, text } });
    const task = taskOf(answer);
    setNotice(`sent ${task ? `task ${task.slice(0, 8)} ` : ''}to ${role}`);
  } catch (error) {
    setNotice(`send refused: ${refusalText(error)}`);
  }
}

function taskOf(answer: unknown): string | null {
  if (answer !== null && typeof answer === 'object' && 'task' in answer) {
    const task: unknown = answer.task;
    if (typeof task === 'string') return task;
  }
  return null;
}
