// The stream: one SSE subscription, resumed by cursor, with its own idea of
// whether it is up. The first frame is a plain `GET /api/view` because the
// stream only answers when something has moved — a subscriber that waits on
// it would draw nothing at all on a quiet cluster.
//
// The backoff is jittered: a cluster that lost its server and a browser that
// reconnects with it must not come back as a crowd.

import { getView, Unauthorized, type Payload } from './api';

export type Transport = 'connecting' | 'live' | 'reconnecting' | 'refused';

export interface StreamHandlers {
  frame: (payload: Payload) => void;
  transport: (state: Transport, detail: string) => void;
}

/// Long enough that a flapping server is not hammered, short enough that the
/// panel does not read as dead. The whole range is under two seconds.
const BACKOFF_MIN_MS = 400;
const BACKOFF_MAX_MS = 1600;

function backoffMs(): number {
  const spread = BACKOFF_MIN_MS + Math.random() * (BACKOFF_MAX_MS - BACKOFF_MIN_MS);
  return Math.round(spread);
}

/// Open the one subscription this tab has, and keep it open until `stop`.
export function openStream(token: string, handlers: StreamHandlers): () => void {
  let cursor = 0;
  let source: EventSource | null = null;
  let timer: number | null = null;
  let stopped = false;

  const connect = () => {
    if (stopped) return;
    getView(token)
      .then((payload) => {
        if (stopped) return;
        handlers.frame(payload);
        cursor = payload.cursor;
        handlers.transport('live', payload.link);
        const url = `/api/stream?token=${encodeURIComponent(token)}&cursor=${cursor}`;
        source = new EventSource(url);
        source.addEventListener('view', (event) => {
          if (stopped) return;
          const frame = JSON.parse((event as MessageEvent).data) as Payload;
          handlers.frame(frame);
          cursor = frame.cursor;
          handlers.transport('live', frame.link);
        });
        source.addEventListener('open', () => {
          if (!stopped) handlers.transport('live', '');
        });
        source.onerror = () => {
          source?.close();
          source = null;
          if (stopped) return;
          handlers.transport('reconnecting', 'the stream dropped; dialing again');
          timer = window.setTimeout(connect, backoffMs());
        };
      })
      .catch((error: unknown) => {
        if (stopped) return;
        if (error instanceof Unauthorized) {
          // A refused token cannot be retried into working: the operator has
          // to open the URL this process printed.
          handlers.transport('refused', error.message);
          return;
        }
        handlers.transport('reconnecting', error instanceof Error ? error.message : String(error));
        timer = window.setTimeout(connect, backoffMs());
      });
  };

  handlers.transport('connecting', '');
  connect();

  return () => {
    stopped = true;
    source?.close();
    source = null;
    if (timer !== null) window.clearTimeout(timer);
  };
}
