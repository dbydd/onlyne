import { randomBytes } from 'node:crypto';
import { join, resolve } from 'node:path';
import type { DesktopController, DesktopServer, ServerOptions } from './contracts.ts';

const UI_DIRECTORY = join(import.meta.dir, 'ui');
const EVENT_TOPIC = 'events';
const SESSION_COOKIE = 'onlyne_session';
const BASE_HEADERS = { 'cache-control': 'no-store', 'x-content-type-options': 'nosniff' } as const;

/**
 * The loopback surface the renderer talks to. Everything it serves lives in memory: the shell
 * HTML, the stylesheet, the browser bundle and every renderer plugin bundle are read or built
 * once here, so there is no build directory and no path a request can name on disk.
 */
export async function startDesktopServer(options: ServerOptions): Promise<DesktopServer> {
  const controller = options.controller;
  const hostname = options.hostname ?? '127.0.0.1';
  const launchSecret = randomBytes(32).toString('base64url');
  const sessionToken = randomBytes(32).toString('base64url');
  let launchPending = true;

  const pluginSources = options.rendererPlugins ?? controller.snapshot().settings.rendererPlugins;
  const [plugins, botPlugins, html, css, app] = await Promise.all([
    buildRendererPlugins(pluginSources),
    buildBotRendererPlugins(options.botRenderers ?? new Map()),
    readUiFile('index.html'),
    readUiFile('style.css'),
    buildBrowserBundle(join(UI_DIRECTORY, 'main.ts')),
  ]);
  const shell = injectBootstrap(html, [...plugins.keys()].map(index => `/plugins/${index}.js`));

  const authorized = (request: Request): boolean => containsSessionCookie(request, sessionToken);

  const server = Bun.serve({
    hostname,
    port: options.port ?? 0,
    async fetch(request, instance): Promise<Response | undefined> {
      const path = new URL(request.url).pathname;

      if (path.startsWith('/launch/')) {
        if (request.method !== 'GET') return methodNotAllowed();
        if (!launchPending || path.slice('/launch/'.length) !== launchSecret) return notFound();
        launchPending = false;
        return new Response(null, {
          status: 302,
          headers: {
            ...BASE_HEADERS,
            location: '/',
            'set-cookie': `${SESSION_COOKIE}=${sessionToken}; Path=/; HttpOnly; SameSite=Strict`,
          },
        });
      }

      if (path === '/api/events') {
        if (!authorized(request)) return failure(401, 'unauthorized');
        if (!isSameOrigin(request)) return failure(403, 'cross-origin request rejected');
        return instance.upgrade(request) ? undefined : failure(400, 'websocket upgrade failed');
      }

      if (path === '/api/state') {
        if (request.method !== 'GET') return methodNotAllowed();
        if (!authorized(request)) return failure(401, 'unauthorized');
        return jsonResponse(controller.snapshot());
      }

      if (path.startsWith('/api/media/')) {
        if (request.method !== 'GET') return methodNotAllowed();
        if (!authorized(request)) return failure(401, 'unauthorized');
        const id = decodeURIComponent(path.slice('/api/media/'.length));
        if (!/^[A-Za-z0-9_-]{1,64}$/.test(id)) return notFound();
        const media = options.media === undefined ? undefined : await options.media(id);
        if (media === undefined) return notFound();
        const file = Bun.file(media.path);
        if (!(await file.exists())) return notFound();
        const range = request.headers.get('range');
        const size = file.size;
        const headers = { ...BASE_HEADERS, 'content-type': media.contentType, 'accept-ranges': 'bytes', 'cache-control': 'private, max-age=31536000, immutable' };
        if (range !== null) {
          const match = /^bytes=(\d*)-(\d*)$/.exec(range.trim());
          if (match === null) return new Response('invalid range', { status: 416, headers: { ...headers, 'content-range': `bytes */${size}` } });
          const start = match[1] === '' ? Math.max(0, size - Number(match[2] ?? 0)) : Number(match[1]);
          const end = match[1] === '' || match[2] === '' ? size - 1 : Math.min(Number(match[2]), size - 1);
          if (!Number.isFinite(start) || !Number.isFinite(end) || start > end || start >= size) {
            return new Response('invalid range', { status: 416, headers: { ...headers, 'content-range': `bytes */${size}` } });
          }
          return new Response(file.slice(start, end + 1), {
            status: 206,
            headers: { ...headers, 'content-range': `bytes ${start}-${end}/${size}`, 'content-length': String(end - start + 1) },
          });
        }
        return new Response(file, { headers: { ...headers, 'content-length': String(size) } });
      }

      if (path === '/api/command') {
        if (request.method !== 'POST') return methodNotAllowed();
        if (!authorized(request)) return failure(401, 'unauthorized');
        if (!isSameOrigin(request)) return failure(403, 'cross-origin request rejected');
        return await runCommand(controller, request);
      }

      if (request.method !== 'GET') return methodNotAllowed();
      if (path === '/' || path === '/index.html') return textResponse('text/html; charset=utf-8', shell);
      if (path === '/style.css') return textResponse('text/css; charset=utf-8', css);
      if (path === '/app.js') return textResponse('text/javascript; charset=utf-8', app);
      const font = /^\/fonts\/([a-z0-9-]+\.woff2)$/.exec(path);
      if (font !== null) {
        const file = Bun.file(join(UI_DIRECTORY, 'fonts', font[1]!));
        if (await file.exists()) {
          return new Response(file, { headers: { ...BASE_HEADERS, 'content-type': 'font/woff2', 'cache-control': 'public, max-age=86400' } });
        }
      }

      const plugin = /^\/plugins\/(\d+)\.js$/.exec(path);
      if (plugin !== null) {
        const bundle = plugins.get(Number(plugin[1]));
        if (bundle !== undefined) return textResponse('text/javascript; charset=utf-8', bundle);
      }
      const botPlugin = /^\/bot-plugins\/([^/]+)\/(\d+)\.js$/.exec(path);
      if (botPlugin !== null) {
        const bundle = botPlugins.get(`${botPlugin[1]}/${botPlugin[2]}`);
        if (bundle !== undefined) return textResponse('text/javascript; charset=utf-8', bundle);
      }
      return notFound();
    },
    websocket: {
      open(socket) {
        socket.subscribe(EVENT_TOPIC);
        socket.send(JSON.stringify({ type: 'snapshot', state: controller.snapshot() }));
      },
      close(socket) {
        socket.unsubscribe(EVENT_TOPIC);
      },
    },
  });

  const unsubscribe = controller.subscribe(event => {
    server.publish(EVENT_TOPIC, JSON.stringify(event));
  });
  const port = server.port ?? 0;
  let closed = false;

  return {
    url: `http://${hostname}:${port}/launch/${launchSecret}`,
    port,
    async close(): Promise<void> {
      if (closed) return;
      closed = true;
      unsubscribe();
      server.stop(true);
    },
  };
}

async function runCommand(controller: DesktopController, request: Request): Promise<Response> {
  let body: unknown;
  try {
    body = await request.json();
  } catch {
    return failure(400, 'body must be JSON');
  }
  if (typeof body !== 'object' || body === null || Array.isArray(body)) return failure(400, 'body must be an object');
  const envelope = body as { action?: unknown; payload?: unknown };
  if (typeof envelope.action !== 'string' || envelope.action.length === 0) return failure(400, 'action must be a non-empty string');
  const payload = envelope.payload === undefined || envelope.payload === null ? {} : envelope.payload;
  if (typeof payload !== 'object' || Array.isArray(payload)) return failure(400, 'payload must be an object');
  try {
    return jsonResponse({ ok: true, value: await controller.command(envelope.action, payload as Record<string, unknown>) });
  } catch (error) {
    return jsonResponse({ ok: false, error: error instanceof Error ? error.message : String(error) });
  }
}

function containsSessionCookie(request: Request, token: string): boolean {
  const header = request.headers.get('cookie');
  if (header === null) return false;
  for (const part of header.split(';')) {
    const separator = part.indexOf('=');
    if (separator < 0) continue;
    if (part.slice(0, separator).trim() !== SESSION_COOKIE) continue;
    if (part.slice(separator + 1).trim() === token) return true;
  }
  return false;
}

/** Same-origin mutations: the browser's Origin header has to name the URL it actually reached. */
function isSameOrigin(request: Request): boolean {
  const header = request.headers.get('origin');
  if (header === null) return false;
  let actual: string;
  try {
    actual = originOf(new URL(header));
  } catch {
    return false;
  }
  return originOf(new URL(request.url)) === actual;
}

function originOf(url: URL): string {
  const protocol = url.protocol === 'ws:' ? 'http:' : url.protocol === 'wss:' ? 'https:' : url.protocol;
  return `${protocol}//${url.host}`;
}

function jsonResponse(value: unknown, status = 200): Response {
  return new Response(JSON.stringify(value), {
    status,
    headers: { ...BASE_HEADERS, 'content-type': 'application/json; charset=utf-8' },
  });
}

function failure(status: number, error: string): Response {
  return jsonResponse({ ok: false, error }, status);
}

function textResponse(contentType: string, body: string | Uint8Array<ArrayBuffer>): Response {
  return new Response(body, { headers: { ...BASE_HEADERS, 'content-type': contentType } });
}

function notFound(): Response {
  return new Response('not found', { status: 404, headers: { ...BASE_HEADERS, 'content-type': 'text/plain; charset=utf-8' } });
}

function methodNotAllowed(): Response {
  return failure(405, 'method not allowed');
}

async function readUiFile(name: string): Promise<string> {
  const file = Bun.file(join(UI_DIRECTORY, name));
  if (!(await file.exists())) throw new Error(`desktop ui asset is missing: ${file.name}`);
  return await file.text();
}

async function buildBrowserBundle(entry: string): Promise<Uint8Array<ArrayBuffer>> {
  const result = await Bun.build({ entrypoints: [entry], target: 'browser', format: 'esm', throw: false });
  const output = result.outputs.find(item => item.kind === 'entry-point') ?? result.outputs[0];
  if (!result.success || output === undefined) {
    const detail = result.logs.map(log => log.message).join('; ');
    throw new Error(`browser bundle failed for ${entry}: ${detail === '' ? 'no output' : detail}`);
  }
  return new Uint8Array(await output.arrayBuffer());
}

/** Plugin sources come from the server options or the stored settings; a URL never names one. */
async function buildRendererPlugins(sources: string[]): Promise<Map<number, Uint8Array<ArrayBuffer>>> {
  const built = new Map<number, Uint8Array<ArrayBuffer>>();
  for (const [index, source] of sources.entries()) {
    try {
      built.set(index, await buildBrowserBundle(resolve(source)));
    } catch (error) {
      console.error(`onlyne desktop: renderer plugin ${source} did not build: ${error instanceof Error ? error.message : String(error)}`);
    }
  }
  return built;
}

async function buildBotRendererPlugins(sources: Map<string, string[]>): Promise<Map<string, Uint8Array<ArrayBuffer>>> {
  const built = new Map<string, Uint8Array<ArrayBuffer>>();
  for (const [botId, paths] of sources) {
    for (const [index, path] of paths.entries()) {
      if (path === '') continue;
      try { built.set(`${encodeURIComponent(botId)}/${index}`, await buildBrowserBundle(resolve(path))); }
      catch (error) {
        console.error(`onlyne desktop: renderer plugin ${path} for ${botId} did not build: ${error instanceof Error ? error.message : String(error)}`);
      }
    }
  }
  return built;
}

function injectBootstrap(html: string, plugins: string[]): string {
  // Escaped so a configured path containing `</script>` cannot close the metadata tag early.
  const json = JSON.stringify({ plugins }).replace(/[<>&\u2028\u2029]/g, character => {
    return `\\u${character.charCodeAt(0).toString(16).padStart(4, '0')}`;
  });
  const tag = `<script id="onlyne-bootstrap" type="application/json">${json}</script>`;
  return html.includes('</head>') ? html.replace('</head>', `${tag}\n</head>`) : `${html}\n${tag}\n`;
}
