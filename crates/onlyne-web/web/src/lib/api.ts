// The HTTP seam: every request carries the startup token the URL gave, and
// nothing else — the browser renders and sends ops, the Rust side folds.

import type { WebOp } from '../gen/WebOp';
import type { View } from '../gen/View';
import type { Board } from '../gen/Board';

/// One `/api/view` answer and one SSE frame, as the Rust payload serialises
/// them.
export interface Payload {
  cursor: number;
  link: string;
  view: View;
  boards: Board[];
}

/// The admin surface's refusal, as the op handler answers it.
export class Refused extends Error {
  readonly code: string;
  constructor(code: string, message: string) {
    super(message);
    this.code = code;
  }
}

function withToken(path: string, token: string): string {
  return `${path}${path.includes('?') ? '&' : '?'}token=${encodeURIComponent(token)}`;
}

export async function getView(token: string): Promise<Payload> {
  const response = await fetch(withToken('/api/view', token));
  if (!response.ok) throw new Error(`view: ${response.status}`);
  return (await response.json()) as Payload;
}

export async function postOp(token: string, op: WebOp): Promise<unknown> {
  const response = await fetch(withToken('/api/op', token), {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(op),
  });
  const body: unknown = await response.json().catch(() => null);
  const refused = refusalOf(body, `http_${response.status}`);
  if (!response.ok) throw refused;
  return body;
}

function refusalOf(body: unknown, fallbackCode: string): Refused {
  if (body !== null && typeof body === 'object' && 'error' in body) {
    const error: unknown = body.error;
    if (error !== null && typeof error === 'object' && 'code' in error && 'message' in error) {
      const code: unknown = error.code;
      const message: unknown = error.message;
      if (typeof code === 'string' && typeof message === 'string') {
        return new Refused(code, message);
      }
    }
  }
  return new Refused(fallbackCode, 'the op failed');
}

export interface NodePos {
  x: number;
  y: number;
}

export interface Layout {
  nodes: Record<string, NodePos>;
}

export async function getLayout(token: string): Promise<Layout> {
  const response = await fetch(withToken('/api/layout', token));
  if (!response.ok) throw new Error(`layout: ${response.status}`);
  const layout = (await response.json()) as Layout;
  return { nodes: layout?.nodes ?? {} };
}

export async function putLayout(token: string, nodes: Record<string, NodePos>): Promise<void> {
  await fetch(withToken('/api/layout', token), {
    method: 'PUT',
    headers: { 'content-type': 'application/json' },
    // The whole body is a `Layout`; `satisfies Layout['nodes']` asked whether
    // the wrapper itself was a node map, which it is not.
    body: JSON.stringify({ nodes } satisfies Layout),
  });
}
