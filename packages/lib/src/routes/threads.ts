import { fail } from '../core.ts';
import { eventStream } from '../stream.ts';
import {
  expect, headerMutation, json, memberPatch, messageQuery, mutation, pageOptions, respond, sendRequest, shape, string,
  type RouteRequest,
} from './shared.ts';

/** Message admission at the reply-authority route: the receiver owns the landing. */
async function replyAuthority(deps: RouteRequest): Promise<Response> {
  const { request, messages, auth, rawBody, limit } = deps;
  expect(request, 'POST');
  return respond(await messages.admit(auth, sendRequest(request, await json(request, limit, rawBody), undefined)), 201);
}

/**
 * Threads and everything one thread holds: its record, memberships, entries, obligations,
 * snapshots and event stream. The owner stores the thread, so every route here reads or
 * answers for that owner through the caller's credential.
 */
export async function threadRoutes(deps: RouteRequest): Promise<Response | undefined> {
  const { request, path, params, ctx, threads, messages, keys, trust, auth, rawBody, limit } = deps;

  if (path.length === 1 && path[0] === 'messages') return replyAuthority(deps);

  if (path.length === 1 && path[0] === 'threads') {
    if (request.method === 'GET') {
      const joined = params.get('joined');
      if (joined !== null) {
        if (joined !== '1') fail('invalid_request', 'joined takes the value 1.');
        return respond(await threads.joined(auth, pageOptions(params)));
      }
      return respond(await threads.list(auth, pageOptions(params)));
    }
    if (request.method === 'POST') {
      const raw = shape(await json(request, limit, rawBody), 'body', ['idempotency_key', 'retry_until', 'title', 'id', 'default']);
      if (raw.default !== undefined && typeof raw.default !== 'boolean') fail('invalid_request', 'default must be a boolean.');
      const id = raw.id === undefined ? undefined : string(raw.id, 'id');
      const created = await threads.create(auth, {
        ...mutation(request, raw),
        title: string(raw.title, 'title'),
        ...(id === undefined ? {} : { id }),
        ...(raw.default === undefined ? {} : { default: raw.default }),
      });
      return respond(created, 201);
    }
    fail('not_found', 'Unknown route.');
  }

  if (path.length >= 3 && path[0] === 'threads') {
    const ref = { owner: path[1]!, id: path[2]! };
    const tail = path[3];

    if (tail === undefined) {
      expect(request, 'GET');
      return respond(await threads.describe(auth, ref));
    }

    if (tail === 'close') {
      if (path.length !== 4) fail('not_found', 'Unknown route.');
      expect(request, 'POST');
      const raw = shape(await json(request, limit, rawBody), 'body', ['idempotency_key', 'retry_until']);
      return respond(await threads.close(auth, ref, mutation(request, raw)));
    }

    if (tail === 'memberships') {
      if (path.length === 4) {
        if (request.method === 'GET') return respond(await threads.members(auth, ref, pageOptions(params)));
        if (request.method === 'POST') {
          const raw = shape(await json(request, limit, rawBody), 'body', ['idempotency_key', 'retry_until', 'peer', 'state', 'access', 'read_scope']);
          return respond(await threads.member(auth, ref, string(raw.peer, 'peer'), memberPatch(raw, 'body'), mutation(request, raw)), 201);
        }
        fail('not_found', 'Unknown route.');
      }
      if (path.length === 5) {
        expect(request, 'DELETE');
        const peer = path[4] === 'self' ? auth.actor : path[4]!;
        return respond(await threads.member(auth, ref, peer, { state: 'revoked' }, headerMutation(request)));
      }
      fail('not_found', 'Unknown route.');
    }

    if (tail === 'messages') {
      if (path.length !== 4) fail('not_found', 'Unknown route.');
      if (request.method === 'GET') return respond(await messages.list(auth, ref, messageQuery(params)));
      if (request.method === 'POST') return respond(await messages.admit(auth, sendRequest(request, await json(request, limit, rawBody), ref)), 201);
      fail('not_found', 'Unknown route.');
    }

    if (tail === 'snapshot') {
      if (path.length !== 4) fail('not_found', 'Unknown route.');
      expect(request, 'GET');
      return respond(await messages.snapshot(auth, ref));
    }

    if (tail === 'attention') {
      if (path.length === 4) {
        expect(request, 'GET');
        return respond(await messages.attention(auth, ref, pageOptions(params)));
      }
      if (path.length === 6 && path[5] === 'ack') {
        expect(request, 'POST');
        const raw = shape(await json(request, limit, rawBody), 'body', ['idempotency_key', 'retry_until']);
        return respond(await messages.ack(auth, ref, path[4]!, mutation(request, raw)));
      }
      fail('not_found', 'Unknown route.');
    }

    if (tail === 'events') {
      if (path.length !== 4) fail('not_found', 'Unknown route.');
      expect(request, 'GET');
      // Awaited so an unauthorized or expired resume answers with a status, not a dead stream.
      return await eventStream({ ctx, messages, keys, trust }, request, auth, ref);
    }

    fail('not_found', 'Unknown route.');
  }

  return undefined;
}
