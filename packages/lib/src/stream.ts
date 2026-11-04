import { OnlyneError, canonical, fail, same } from './core.ts';
import type { Context } from './core.ts';
import type { Auth, Event, ThreadRef } from './types.ts';
import type { MessageService } from './messages.ts';
import type { KeyService, TrustService } from './keys.ts';

const STREAM_TICK_MS = 15_000;
const STREAM_MAX_PAGES = 1_000;

/** A resume cursor this binding issues: a decimal event position bound to one stream scope. */
function eventCursor(value: unknown): { scope: unknown; after: string } | undefined {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) return undefined;
  if (!('after' in value) || !('scope' in value)) return undefined;
  const { after, scope } = value;
  if (typeof after !== 'string' || !/^(0|[1-9][0-9]*)$/.test(after)) return undefined;
  return { scope, after };
}

export interface StreamDeps {
  ctx: Context;
  messages: MessageService;
  keys?: KeyService;
  trust?: TrustService;
}

/**
 * Polls the event page on a timer and writes SSE frames. Every frame carries its own `id:`: a
 * cursor bound to this node, actor, ThreadRef and query scope, positioned after the event the
 * frame delivered. A frame that reused one page cursor would replay the whole page on resume.
 * The resume value is validated against that same scope before the response exists, the first
 * page is fetched up front so a lost credential or expired cursor answers with an HTTP error
 * instead of a 200 stream, and every later page re-checks the credential.
 */
export async function eventStream({ ctx, messages, keys, trust }: StreamDeps, request: Request, auth: Auth, ref: ThreadRef): Promise<Response> {
  const encoder = new TextEncoder();
  const scope = { ref, actor: auth.actor, kind: 'events' as const };
  const resume = request.headers.get('last-event-id');
  let cursor: string | undefined;
  if (resume !== null && resume.length > 0) {
    let decoded: unknown;
    try {
      decoded = JSON.parse(decodeURIComponent(resume));
    } catch {
      fail('invalid_request', 'Last-Event-ID is not a cursor this binding issues.');
    }
    const parsed = eventCursor(decoded);
    if (parsed === undefined || !same(parsed.scope, scope)) {
      fail('invalid_request', 'Last-Event-ID belongs to another node, actor or thread.');
    }
    cursor = resume;
  }

  let tick: ReturnType<typeof setInterval> | undefined;
  let running = false;
  let done = false;
  let detach = () => {};

  const write = (controller: ReadableStreamDefaultController<Uint8Array>, text: string) => {
    if (done) return;
    try {
      controller.enqueue(encoder.encode(text));
    } catch {
      // The consumer closed the stream; release the tick and the abort listener with it.
      done = true;
      clearInterval(tick);
      detach();
    }
  };
  const stop = (controller: ReadableStreamDefaultController<Uint8Array>) => {
    if (done) return;
    done = true;
    clearInterval(tick);
    detach();
    try { controller.close(); } catch { /* the consumer already closed it */ }
  };
  const emit = (controller: ReadableStreamDefaultController<Uint8Array>, event: Event) => {
    cursor = encodeURIComponent(canonical({ scope, after: event.id }));
    write(controller, `id: ${cursor}\nevent: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`);
  };
  const drain = async (controller: ReadableStreamDefaultController<Uint8Array>) => {
    for (let pages = 0; pages < STREAM_MAX_PAGES && !done; pages++) {
      const page = await messages.events(auth, ref, cursor === undefined ? {} : { cursor });
      if (page.items.length === 0) {
        write(controller, ': idle\n\n');
        return;
      }
      for (const event of page.items) emit(controller, event);
      if (page.next_cursor === null) return;
    }
  };
  const poll = async (controller: ReadableStreamDefaultController<Uint8Array>) => {
    if (running || done) return;
    running = true;
    try {
      const current = keys?.enabled === true
        ? ((await trust?.entry(auth.actor))?.state === 'trusted' ? { actor: auth.actor } : null)
        : await ctx.options.plugins.authenticate(request);
      if (current === null || current.actor !== auth.actor) {
        write(controller, `event: error\ndata: ${JSON.stringify({ error: new OnlyneError('forbidden', 'The credential is no longer valid for this stream.').error })}\n\n`);
        stop(controller);
        return;
      }
      await drain(controller);
    } catch (error) {
      const onlyne = error instanceof OnlyneError ? error : new OnlyneError('internal', 'The event stream failed.');
      write(controller, `event: error\ndata: ${JSON.stringify({ error: onlyne.error })}\n\n`);
      stop(controller);
    } finally {
      running = false;
    }
  };

  // The first page settles authorization and cursor validity while a status code can still say so.
  const first = await messages.events(auth, ref, cursor === undefined ? {} : { cursor });
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      for (const event of first.items) emit(controller, event);
      if (first.items.length === 0) write(controller, ': idle\n\n');
      if (request.signal.aborted) {
        stop(controller);
        return;
      }
      const onAbort = () => stop(controller);
      detach = () => request.signal.removeEventListener('abort', onAbort);
      request.signal.addEventListener('abort', onAbort, { once: true });
      tick = setInterval(() => { void poll(controller); }, STREAM_TICK_MS);
      if (first.next_cursor !== null) void poll(controller);
    },
    cancel() {
      done = true;
      clearInterval(tick);
      detach();
    },
  });
  return new Response(body, {
    status: 200,
    headers: { 'content-type': 'text/event-stream', 'cache-control': 'no-store', connection: 'keep-alive' },
  });
}
