import { spawn, type ChildProcess } from 'node:child_process'
import { createConnection, type Socket } from 'node:net'
import type { Workspace } from './workspace.js'

export interface OnlyneRequest {
  id: string
  op: string
  channel_id?: string
  message_id?: string
  text?: string
  format?: 'plain' | 'markdown'
  raw_text?: boolean
  limit?: number
}

export interface SendTarget {
  channelId: string
}

export interface SendResult extends SendTarget {
  ok: boolean
  error?: string
}

/** One request/reply round trip over the workspace-onlyne Unix socket. */
export function request(socketPath: string, req: OnlyneRequest): Promise<any> {
  return new Promise((resolve, reject) => {
    const socket = createConnection(socketPath)
    let data = ''
    socket.setEncoding('utf8')
    socket.on('error', reject)
    socket.on('connect', () => socket.write(`${JSON.stringify(req)}\n`))
    socket.on('data', (chunk) => {
      data += chunk
      const idx = data.indexOf('\n')
      if (idx >= 0) {
        socket.end()
        try { resolve(JSON.parse(data.slice(0, idx))) } catch (e) { reject(e) }
      }
    })
  })
}

/** Long-lived subscription to the daemon's event stream (NDJSON lines). */
export function subscribe(socketPath: string, onLine: (line: any) => void, onDisconnect?: () => void): Socket {
  const socket = createConnection(socketPath)
  let buf = ''
  let closed = false
  socket.setEncoding('utf8')
  const disconnect = () => { if (closed) return; closed = true; onDisconnect?.() }
  socket.on('error', disconnect)
  socket.on('close', disconnect)
  socket.on('connect', () => {
    if (!socket.destroyed) socket.write('{"id":"sub","op":"subscribe_events"}\n', () => {})
  })
  socket.on('data', (chunk) => {
    buf += chunk
    for (;;) {
      const idx = buf.indexOf('\n')
      if (idx < 0) break
      const raw = buf.slice(0, idx)
      buf = buf.slice(idx + 1)
      if (!raw.trim()) continue
      try { onLine(JSON.parse(raw)) } catch { /* ignore */ }
    }
  })
  return socket
}

export async function waitForSocket(socketPath: string, timeoutMs = 5000) {
  const deadline = Date.now() + timeoutMs
  let last: unknown
  while (Date.now() < deadline) {
    try { await request(socketPath, { id: 'ping', op: 'ping' }); return } catch (e) { last = e }
    await new Promise((r) => setTimeout(r, 100))
  }
  throw last instanceof Error ? last : new Error('onlyne socket not ready')
}

/** Spawn `onlyne run` supervised so it dies when this process dies. */
function spawnManagedDaemon(ws: Workspace): ChildProcess {
  const bin = process.env.ONLYNE_BIN || 'onlyne'
  const script = `
parent="$1"; shift
"$@" &
child=$!
trap 'kill "$child" 2>/dev/null; wait "$child" 2>/dev/null' INT TERM HUP EXIT
while kill -0 "$parent" 2>/dev/null; do
  kill -0 "$child" 2>/dev/null || { wait "$child"; exit $?; }
  sleep 1
done
kill "$child" 2>/dev/null
wait "$child" 2>/dev/null
`
  return spawn('sh', ['-c', script, 'onlyne-supervisor', String(process.pid), bin, '--workspace', ws.root, 'run'], { stdio: 'ignore' })
}

export async function connectDaemon(ws: Workspace, startIfMissing = true): Promise<{ owner: 'external' | 'extension'; process?: ChildProcess }> {
  try {
    await request(ws.socketPath, { id: 'ping', op: 'ping' })
    return { owner: 'external' }
  } catch (e) {
    if (!startIfMissing) throw new Error(`onlyne daemon is not running for ${ws.root}; start it with /onlyne daemon start`, { cause: e })
    const child = spawnManagedDaemon(ws)
    try {
      await waitForSocket(ws.socketPath)
      // The daemon we spawned already exited: we lost a startup race, the
      // socket belongs to someone else. Downgrade to external (subscribe only).
      if (child.exitCode !== null || child.signalCode !== null) return { owner: 'external' }
      return { owner: 'extension', process: child }
    } catch (err) { stopProcess(child); throw err }
  }
}

export async function shutdownDaemon(ws: Workspace, child?: ChildProcess) {
  try { await request(ws.socketPath, { id: `shutdown-${Date.now()}`, op: 'shutdown' }) } catch { /* may already be down */ }
  stopProcess(child)
}

export function stopProcess(child?: ChildProcess) {
  if (!child || child.killed) return
  try { child.kill('SIGTERM') } catch { /* ignore */ }
}

export async function loopback(socketPath: string, text: string, rawText = true): Promise<any> {
  return request(socketPath, { id: `loopback-${Date.now()}`, op: 'loopback', text, raw_text: rawText })
}

export async function markConsumed(socketPath: string, messageId: string): Promise<any> {
  return request(socketPath, { id: `consume-${Date.now()}`, op: 'mark_io_consumed', message_id: messageId })
}

export async function sendWithRetry(socketPath: string, target: SendTarget, text: string, attempts: number, rawText = false): Promise<SendResult> {
  let error = 'unknown error'
  for (let i = 0; i < Math.max(1, attempts); i++) {
    try {
      const res = await request(socketPath, { id: `send-${Date.now()}-${i}`, op: 'send_message', channel_id: target.channelId, text, raw_text: rawText })
      if (res.ok) return { ...target, ok: true }
      error = res.error?.message ?? JSON.stringify(res.error ?? res)
    } catch (e) { error = e instanceof Error ? e.message : String(e) }
  }
  return { ...target, ok: false, error }
}

export async function broadcast(socketPath: string, targets: SendTarget[], text: string, attempts: number, concurrency: number, rawText = false): Promise<SendResult[]> {
  const out: SendResult[] = []
  let next = 0
  async function worker() {
    for (;;) {
      const i = next++
      if (i >= targets.length) return
      out[i] = await sendWithRetry(socketPath, targets[i]!, text, attempts, rawText)
    }
  }
  await Promise.all(Array.from({ length: Math.max(1, Math.min(concurrency, targets.length)) }, worker))
  return out
}
