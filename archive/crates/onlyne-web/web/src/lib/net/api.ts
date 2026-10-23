// The HTTP seam. Every request carries the startup token and nothing else.
// The browser renders and sends ops; the Rust side folds.
//
// The guard in `onlyne-web/src/lib.rs` answers a request that carries no
// token, a foreign `Host`, or a foreign `Origin` with 401 or 403 and a plain
// text sentence. Those two are told apart from every other failure here,
// because a refused token is a fact the operator has to act on: a fresh
// `onlyne-web` mints a fresh token, and a retry loop hides that.

import type { Board } from '../../gen/Board';
import type { View } from '../../gen/View';
import type { WebOp } from '../../gen/WebOp';

/// One `/api/view` answer and one SSE frame, as the Rust payload serialises them.
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

/// The guard turned this request away: a wrong token, or a name it does not
/// serve. Retrying cannot help, so nothing does.
export class Unauthorized extends Error {}

function withToken(path: string, token: string): string {
  return `${path}${path.includes('?') ? '&' : '?'}token=${encodeURIComponent(token)}`;
}

export async function getView(token: string): Promise<Payload> {
  const response = await fetch(withToken('/api/view', token));
  if (response.status === 401 || response.status === 403) {
    throw new Unauthorized((await response.text()).trim() || 'the guard refused this request');
  }
  if (!response.ok) throw new Error(`the view read failed: ${response.status}`);
  return (await response.json()) as Payload;
}

export async function postOp(token: string, op: WebOp): Promise<unknown> {
  const response = await fetch(withToken('/api/op', token), {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(op),
  });
  if (response.status === 401 || response.status === 403) {
    throw new Unauthorized((await response.text()).trim() || 'the guard refused this request');
  }
  const body: unknown = await response.json().catch(() => null);
  if (!response.ok) throw refusalOf(body, `http_${response.status}`);
  return body;
}

/// The `{ error: { code, message } }` body every refusal shares, or a plain
/// refusal built from the status when the body is not one.
function refusalOf(body: unknown, fallbackCode: string): Refused {
  if (body !== null && typeof body === 'object' && 'error' in body) {
    const error: unknown = body.error;
    if (error !== null && typeof error === 'object' && 'code' in error && 'message' in error) {
      const code: unknown = error.code;
      const message: unknown = error.message;
      if (typeof code === 'string' && typeof message === 'string') return new Refused(code, message);
    }
  }
  return new Refused(fallbackCode, 'the op failed');
}
