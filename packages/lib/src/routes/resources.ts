import { fail } from '../core.ts';
import type { ResourceEvent } from '../types.ts';
import {
  eventBody, expect, json, logQuery, mutation, operationRequest, pageOptions, probeBody, respond, resourceRecord, shape,
  type RouteRequest,
} from './shared.ts';

/**
 * Resources and the RFC 0006 handover: registration, the operation route, the append-only log
 * and the event ingress. The probe routes are the target side of a handover and pass the same
 * credential and signature checks before any of them is read.
 */
export async function resourceRoutes(deps: RouteRequest): Promise<Response | undefined> {
  const { request, path, params, resources, auth, rawBody, limit } = deps;

  // RFC 0006 §4.3: the target side of a handover. A probe is a peer request like any
  // other, so it passes the same signature check before it is read.
  if (path.length >= 2 && path[0] === 'handover' && path[1] === 'probes') {
    if (path.length === 2) {
      expect(request, 'POST');
      return respond(await resources.probe(auth, probeBody(await json(request, limit, rawBody))), 201);
    }
    if (path.length === 3) {
      expect(request, 'GET');
      return respond(await resources.probeAnswer(auth, path[2]!));
    }
    fail('not_found', 'Unknown route.');
  }

  if (path.length === 1 && path[0] === 'resources') {
    if (request.method === 'GET') return respond(await resources.list(auth, pageOptions(params)));
    if (request.method === 'POST') {
      const raw = shape(await json(request, limit, rawBody), 'body', ['idempotency_key', 'retry_until', 'resource']);
      mutation(request, raw);
      return respond(await resources.register(auth, resourceRecord(raw.resource)), 201);
    }
    fail('not_found', 'Unknown route.');
  }

  if (path.length >= 3 && path[0] === 'resources') {
    const ref = { owner: path[1]!, id: path[2]! };
    const tail = path[3];

    if (tail === undefined) {
      expect(request, 'GET');
      return respond(await resources.describe(auth, ref));
    }

    if (tail === 'operations') {
      if (path.length === 4) {
        if (request.method === 'GET') return respond(await resources.operations(auth, ref, pageOptions(params)));
        if (request.method === 'POST') {
          const operation = operationRequest(request, await json(request, limit, rawBody));
          await deps.ctx.options.plugins.prepareResourceOperation?.(auth, ref, operation);
          return respond(await resources.operate(auth, ref, operation, request.signal), 201);
        }
        fail('not_found', 'Unknown route.');
      }
      if (path.length === 5) {
        expect(request, 'GET');
        return respond(await resources.operation(auth, ref, path[4]!));
      }
      fail('not_found', 'Unknown route.');
    }

    // RFC 0006 §6: the log is read on its own route, from the endpoint's storage and
    // never through the binding, so it stays readable while the handler is away.
    if (tail === 'log') {
      if (path.length === 4) {
        expect(request, 'GET');
        return respond(await resources.log(auth, ref, logQuery(params)));
      }
      if (path.length === 5) {
        expect(request, 'GET');
        return respond(await resources.event(auth, ref, path[4]!));
      }
      fail('not_found', 'Unknown route.');
    }

    if (tail === 'events') {
      if (path.length === 4) {
        expect(request, 'POST');
        const event: ResourceEvent = await resources.receive(auth, ref, eventBody(await json(request, limit, rawBody)));
        return respond(event, 201);
      }
      if (path.length === 6 && path[5] === 'ack') {
        expect(request, 'POST');
        const raw = shape(await json(request, limit, rawBody), 'body', ['idempotency_key', 'retry_until']);
        return respond(await resources.ack(auth, ref, path[4]!, mutation(request, raw)));
      }
      fail('not_found', 'Unknown route.');
    }

    fail('not_found', 'Unknown route.');
  }

  return undefined;
}
