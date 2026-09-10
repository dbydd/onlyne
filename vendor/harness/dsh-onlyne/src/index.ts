/**
 * dsh-onlyne: Onlyne IM inbox/outbox for DeepSeek Harness.
 *
 * Mirrors pi-onlyne's capability set as a Cordis plugin: model-facing tools to
 * send/broadcast/reply through an Onlyne workspace daemon, and a watch loop
 * that surfaces inbound IM messages into the running dsh agent via followup.
 *
 * Onlyne channels are singleton-routed: each enabled channel has one
 * `bind_conversation_id` set in config (or by sending `/handshake` from the
 * desired conversation), so the tools take `channelId` only.
 *
 * Config is shared with pi-onlyne (`.pi/onlyne.json`), so one workspace can be
 * bridged by either harness without reconfiguration.
 * @module dsh-onlyne
 */

import type { Context } from '@deepseek-ai/cordis'
import type {} from '@deepseek-ai/dsh-agent'
import type {} from '@deepseek-ai/dsh-commands'
import { createUserMessage } from '@deepseek-ai/dsh-llm'
import type {} from '@deepseek-ai/dsh-session'
import { defineTool } from '@deepseek-ai/dsh-tools'
import type { ChildProcess } from 'node:child_process'
import type { Socket } from 'node:net'
import { inboundModeFor, loadConfig, saveConfig } from './config.js'
import {
  broadcast,
  connectDaemon,
  loopback,
  markConsumed,
  sendWithRetry,
  shutdownDaemon,
  stopProcess,
  subscribe,
} from './onlyne.js'
import type { SendTarget } from './onlyne.js'
import { findWorkspace } from './workspace.js'
import type { Workspace } from './workspace.js'

/** Cordis plugin name used by loader diagnostics. */
export const name = 'dsh-onlyne'

/** Services required by the plugin. */
export const inject = ['tools', 'commands', 'agents']

interface Inbound {
  channelId: string
  conversationId: string
  messageId?: string
  text: string
  replied: boolean
  noReply: boolean
}

interface State {
  workspace: Workspace | null
  watching: boolean
  owner: 'external' | 'extension' | 'stopped'
  child?: ChildProcess
  socket?: Socket
  reconnectTimer?: ReturnType<typeof setTimeout>
  currentInbound?: Inbound
  lastValidOutput?: string
}

const state: State = { workspace: null, watching: false, owner: 'stopped' }

const currentConfig = () => loadConfig(state.workspace?.root ?? process.cwd())

/** Normalize an `inbound_message` event payload into Inbound fields. */
function inboundText(data: any): Omit<Inbound, 'replied' | 'noReply'> | null {
  const msg = data?.data?.data ?? data?.data ?? data
  const channelId = msg.channel_id ?? msg.channelId
  const conversationId = msg.conversation_id ?? msg.conversationId
  const messageId = msg.message_id ?? msg.messageId
  const text = msg.text ?? msg.content ?? msg.body
  return channelId && conversationId && typeof text === 'string'
    ? { channelId, conversationId, messageId, text }
    : null
}

function consumeIfNotified(inbound: { messageId?: string }) {
  if (state.workspace && inbound.messageId) void markConsumed(state.workspace.socketPath, inbound.messageId).catch(() => {})
}

function needsReply(inbound = state.currentInbound) {
  return !!inbound && !inbound.replied && !inbound.noReply && !!state.workspace
}

/** Surface text into the running dsh agent as a waking user message. */
function wakeAgent(ctx: Context, text: string): boolean {
  const agent = ctx.agents.list()[0]
  if (!agent) return false
  agent.followup(createUserMessage({ content: [{ type: 'text', text }], source: { kind: 'user' } }))
  return true
}

function scheduleReconnect(ctx: Context) {
  if (state.reconnectTimer || !state.watching || !state.workspace) return
  state.reconnectTimer = setTimeout(async () => {
    state.reconnectTimer = undefined
    if (!state.watching) return
    try { await startWatch(ctx) } catch { scheduleReconnect(ctx) }
  }, 1000)
}

async function startWatch(ctx: Context) {
  state.workspace = findWorkspace(state.workspace?.root ?? process.cwd())
  if (!state.workspace) throw new Error('current workspace has no .onlyne configuration')
  if (state.reconnectTimer) clearTimeout(state.reconnectTimer)
  state.reconnectTimer = undefined
  state.socket?.destroy()
  state.socket = undefined
  const conn = await connectDaemon(state.workspace)
  state.owner = conn.owner
  state.child = conn.process
  const socket = subscribe(
    state.workspace.socketPath,
    (line) => {
      if (!line?.event || line.type !== 'inbound_message') return
      const inbound = inboundText(line)
      if (!inbound) return
      const mode = inboundModeFor(currentConfig(), inbound.channelId, inbound.conversationId)
      if (mode === 'muted') return
      if (inbound.channelId === 'loopback') {
        if (mode === 'auto-handle') {
          wakeAgent(ctx, `Onlyne loopback activation${inbound.conversationId ? ` (${inbound.conversationId})` : ''}:\n\n${inbound.text}`)
        }
        consumeIfNotified(inbound)
        return
      }
      if (inbound.text.trim() === '/handshake') { consumeIfNotified(inbound); return }
      state.currentInbound = { ...inbound, replied: false, noReply: false }
      if (mode === 'auto-handle') {
        wakeAgent(ctx, `Onlyne inbound message from ${inbound.channelId}/${inbound.conversationId}:\n\n${inbound.text}\n\nReply with onlyne_reply, or call onlyne_mark_no_reply if no reply is needed.`)
        consumeIfNotified(inbound)
      }
    },
    () => { if (state.socket === socket) scheduleReconnect(ctx) },
  )
  state.socket = socket
  state.watching = true
  return `watching ${state.workspace.root} (${state.owner})`
}

function stopWatch() {
  if (state.reconnectTimer) clearTimeout(state.reconnectTimer)
  state.reconnectTimer = undefined
  state.socket?.destroy()
  state.socket = undefined
  stopProcess(state.child)
  state.child = undefined
  state.watching = false
  state.owner = 'stopped'
  return 'watch stopped'
}

async function startDaemon() {
  state.workspace = findWorkspace(state.workspace?.root ?? process.cwd())
  if (!state.workspace) throw new Error('current workspace has no .onlyne configuration')
  const conn = await connectDaemon(state.workspace, true)
  state.owner = conn.owner
  state.child = conn.process
  return `daemon ${state.owner === 'extension' ? 'started' : 'already running'} for ${state.workspace.root}`
}

async function stopDaemon() {
  if (!state.workspace) state.workspace = findWorkspace(process.cwd())
  if (!state.workspace) throw new Error('current workspace has no .onlyne configuration')
  state.socket?.destroy()
  state.socket = undefined
  await shutdownDaemon(state.workspace, state.child)
  state.child = undefined
  state.watching = false
  state.owner = 'stopped'
  return `daemon stopped for ${state.workspace.root}`
}

async function restartDaemon() {
  await stopDaemon().catch(() => {})
  return startDaemon()
}

async function reply(text: string) {
  if (!state.workspace) throw new Error('onlyne workspace not found')
  const inbound = state.currentInbound
  if (!inbound) throw new Error('no active inbound message')
  const res = await sendWithRetry(state.workspace.socketPath, { channelId: inbound.channelId }, text, currentConfig().outbound.retry.attempts)
  if (res.ok) { inbound.replied = true; state.currentInbound = undefined }
  return res
}

/** Text result helper: canonical value is the string, renderer shows it verbatim. */
function textOutput() {
  return {
    schema: { type: 'string' as const },
    render: (_args: Record<string, unknown>, value: string) => [{ type: 'text' as const, text: value }],
  }
}

export function apply(ctx: Context) {
  // Workspace discovery: the dsh process uses its invoking directory as the
  // default workspace root (same rule as the launcher).
  state.workspace = findWorkspace(process.cwd())

  // Track the last valid assistant output for fallback replies.
  ctx.on('session/event', (_session, event) => {
    if (event.type !== 'assistant/message') return
    const text = event.data.message.content
      .filter((block) => block.type === 'text')
      .map((block) => (block as { text: string }).text)
      .join('')
      .trim()
    if (text && !text.startsWith('{') && !text.startsWith('[onlyne-internal]')) state.lastValidOutput = text
  })

  // One lean reply reminder when a turn ends with an unconsumed inbound.
  ctx.on('session/event', (_session, event) => {
    if (event.type !== 'turn/end') return
    const inbound = state.currentInbound
    if (!inbound || !needsReply()) return
    if (currentConfig().outbound.defaultReplyMode === 'explicit-only') return
    wakeAgent(ctx, `Onlyne reminder: reply to ${inbound.channelId}/${inbound.conversationId} with onlyne_reply, or call onlyne_mark_no_reply.`)
  })

  ctx.on('agent/session-start', () => {
    // Refresh workspace once a session is live; keeps autoStart working when
    // the process cwd differs from the session cwd.
    const refreshed = findWorkspace(process.cwd())
    if (refreshed) state.workspace = refreshed
    if (currentConfig().watch.autoStart && !state.watching && state.workspace) {
      void startWatch(ctx).catch(() => {})
    }
  })

  ctx.commands.register({
    name: 'onlyne',
    description: 'Onlyne watch/status/daemon commands',
    handler: async (invocation) => {
      const [cmd, sub] = invocation.rawInput.trim().split(/\s+/)
      try {
        if (cmd === 'watch' && sub === 'on') return { kind: 'success', text: await startWatch(ctx) }
        if (cmd === 'watch' && sub === 'off') return { kind: 'success', text: stopWatch() }
        if (cmd === 'daemon' && sub === 'start') return { kind: 'success', text: await startDaemon() }
        if (cmd === 'daemon' && sub === 'stop') return { kind: 'success', text: await stopDaemon() }
        if (cmd === 'daemon' && sub === 'restart') return { kind: 'success', text: await restartDaemon() }
        if (cmd === 'status') return { kind: 'success', text: `onlyne ${state.watching ? 'watching' : 'stopped'}; owner=${state.owner}; workspace=${state.workspace?.root ?? 'none'}` }
        if (cmd === 'config' && sub === 'auto-start') {
          const cfg = currentConfig()
          cfg.watch.autoStart = !cfg.watch.autoStart
          saveConfig(state.workspace?.root ?? process.cwd(), cfg)
          return { kind: 'success', text: `autoStart=${cfg.watch.autoStart}` }
        }
        return { kind: 'error', text: 'usage: /onlyne status | watch on|off | daemon start|stop|restart | config auto-start' }
      } catch (e) {
        return { kind: 'error', text: e instanceof Error ? e.message : String(e) }
      }
    },
  })

  ctx.tools.register(defineTool({
    name: 'onlyne_daemon_start',
    description: 'Start or connect to the current workspace-local Onlyne daemon.',
    parameters: {},
    output: textOutput(),
    async execute() {
      const res = await startDaemon()
      return JSON.stringify({ ok: true, message: res, owner: state.owner, workspace: state.workspace?.root })
    },
  }))

  ctx.tools.register(defineTool({
    name: 'onlyne_daemon_stop',
    description: 'Stop the current workspace-local Onlyne daemon when dsh-onlyne manages it, without shelling out to pkill/nohup.',
    parameters: {},
    output: textOutput(),
    async execute() {
      const res = await stopDaemon()
      return JSON.stringify({ ok: true, message: res })
    },
  }))

  ctx.tools.register(defineTool({
    name: 'onlyne_daemon_restart',
    description: 'Restart the current workspace-local Onlyne daemon through dsh-onlyne lifecycle management.',
    parameters: {},
    output: textOutput(),
    async execute() {
      const res = await restartDaemon()
      return JSON.stringify({ ok: true, message: res })
    },
  }))

  ctx.tools.register(defineTool({
    name: 'onlyne_reply',
    description: 'Reply with plain text to the current Onlyne inbound message.',
    parameters: {
      text: { type: 'string', required: true, description: 'Reply body as plain text.' },
    },
    output: textOutput(),
    async execute(args) {
      return JSON.stringify(await reply(args.text))
    },
  }))

  ctx.tools.register(defineTool({
    name: 'onlyne_send',
    description: 'Send Markdown to the channel\'s configured Onlyne conversation. Set rawText=true only for literal plain text.',
    parameters: {
      channelId: { type: 'string', required: true, description: 'Channel id as configured in the Onlyne workspace.' },
      text: { type: 'string', required: true, description: 'Message body (Markdown unless rawText).' },
      rawText: { type: 'boolean', description: 'Send the text literally instead of as Markdown.' },
    },
    output: textOutput(),
    async execute(args) {
      if (!state.workspace) throw new Error('onlyne workspace not found')
      const res = await sendWithRetry(state.workspace.socketPath, { channelId: args.channelId }, args.text, currentConfig().outbound.retry.attempts, args.rawText ?? false)
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'onlyne_broadcast',
    description: 'Send Markdown to many configured Onlyne channels concurrently. Set rawText=true only for literal plain text.',
    parameters: {
      targets: {
        type: 'array',
        required: true,
        items: {
          type: 'object',
          additionalProperties: false,
          properties: {
            channelId: { type: 'string', required: true, description: 'Channel id as configured in the Onlyne workspace.' },
          },
        },
      },
      text: { type: 'string', required: true, description: 'Message body (Markdown unless rawText).' },
      rawText: { type: 'boolean', description: 'Send the text literally instead of as Markdown.' },
    },
    output: textOutput(),
    async execute(args) {
      if (!state.workspace) throw new Error('onlyne workspace not found')
      const cfg = currentConfig()
      const results = await broadcast(state.workspace.socketPath, args.targets as SendTarget[], args.text, cfg.outbound.retry.attempts, cfg.outbound.retry.concurrency, args.rawText ?? false)
      return JSON.stringify({ ok: results.every((r) => r.ok), results })
    },
  }))

  ctx.tools.register(defineTool({
    name: 'onlyne_loopback',
    description: 'Inject a local loopback activation message so scripts can wake the current dsh session. Set rawText=false for Markdown. FIFO alternative: write to .onlyne/channels/loopback/in.',
    parameters: {
      text: { type: 'string', required: true, description: 'Message body.' },
      rawText: { type: 'boolean', description: 'Send the text literally instead of as Markdown.' },
    },
    output: textOutput(),
    async execute(args) {
      if (!state.workspace) throw new Error('onlyne workspace not found')
      const res = await loopback(state.workspace.socketPath, args.text, args.rawText ?? true)
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'onlyne_mark_no_reply',
    description: 'Mark the current Onlyne inbound message as intentionally not replied.',
    parameters: {
      reason: { type: 'string', description: 'Optional reason (not sent to the channel).' },
    },
    output: textOutput(),
    async execute(args) {
      if (state.currentInbound) {
        state.currentInbound.noReply = true
        state.currentInbound = undefined
      }
      return JSON.stringify({ ok: true, reason: args.reason ?? null })
    },
  }))
}
