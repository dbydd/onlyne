import type { RpcFrame, RuntimeConnection, RuntimeContext, RuntimeDriver } from './contracts.ts';
import { prepareRuntime } from './runtime-config.ts';

const FRAME_LIMIT = 1024 * 1024;
const REASSEMBLY_LIMIT = 64 * 1024 * 1024;

export class RpcCommandError extends Error {
  constructor(readonly response: RpcFrame) {
    super(typeof response.error === 'string' ? response.error : `OMP rejected ${String(response.command)}`);
    this.name = 'RpcCommandError';
  }
}

export class RpcFrameDecoder {
  private chunk: { id: string; count: number; bytes: number; parts: Buffer[]; received: number } | undefined;
  private maxReassembled = REASSEMBLY_LIMIT;

  decode(frame: RpcFrame): RpcFrame | undefined {
    if (frame.type === 'ready') {
      const advertised = frame.maxReassembledFrameBytes;
      if (typeof advertised === 'number' && Number.isSafeInteger(advertised) && advertised > 0) {
        this.maxReassembled = Math.min(advertised, REASSEMBLY_LIMIT);
      }
    }
    if (frame.type !== 'rpc_chunk') {
      if (this.chunk !== undefined) throw new Error('OMP interrupted a chunked frame');
      return frame;
    }
    const { chunkId, index, count, byteLength, data } = frame;
    if (typeof chunkId !== 'string' || typeof index !== 'number' || !Number.isSafeInteger(index)
      || typeof count !== 'number' || !Number.isSafeInteger(count) || count <= 0
      || typeof byteLength !== 'number' || !Number.isSafeInteger(byteLength) || byteLength <= 0
      || byteLength > this.maxReassembled || count > byteLength || index < 0 || index >= count
      || typeof data !== 'string' || data.length % 4 !== 0 || !/^[A-Za-z0-9+/]*={0,2}$/.test(data)) {
      throw new Error('OMP sent an invalid chunk header');
    }
    const bytes = Buffer.from(data, 'base64');
    if (bytes.toString('base64') !== data) throw new Error('OMP sent invalid base64');
    if (this.chunk === undefined) {
      if (index !== 0) throw new Error('OMP chunk sequence did not start at zero');
      this.chunk = { id: chunkId, count, bytes: byteLength, parts: [], received: 0 };
    }
    const pending = this.chunk;
    if (pending.id !== chunkId || pending.count !== count || pending.bytes !== byteLength
      || pending.parts.length !== index || pending.received + bytes.length > byteLength) {
      throw new Error('OMP chunk sequence is inconsistent');
    }
    pending.parts.push(bytes);
    pending.received += bytes.length;
    if (index + 1 !== count) return undefined;
    this.chunk = undefined;
    if (pending.received !== pending.bytes) throw new Error('OMP chunk byte length differs');
    const text = new TextDecoder('utf-8', { fatal: true }).decode(Buffer.concat(pending.parts, pending.bytes));
    return parseFrame(text);
  }

  finish(): void {
    if (this.chunk !== undefined) throw new Error('OMP closed during a chunked frame');
  }
}

function parseFrame(text: string): RpcFrame {
  const value: unknown = JSON.parse(text);
  if (value === null || typeof value !== 'object' || Array.isArray(value)
    || typeof (value as Record<string, unknown>).type !== 'string') {
    throw new Error('OMP sent a frame without a type');
  }
  return value as RpcFrame;
}

export const ompRpcDriver: RuntimeDriver = {
  id: 'omp-rpc',
  async connect(context: RuntimeContext): Promise<RuntimeConnection> {
    const prepared = await prepareRuntime(context);
    const child = Bun.spawn([context.bot.runtimeOptions.command ?? 'omp',
      ...(context.bot.runtimeOptions.args ?? []), '--mode', 'rpc-ui', ...prepared.args], {
      cwd: prepared.cwd,
      env: { ...process.env, ...prepared.env, ...context.bot.runtimeOptions.env,
        PI_RPC_EMIT_TITLE: '1', PI_NO_TITLE: '1', PI_NO_PTY: '1' },
      stdin: 'pipe', stdout: 'pipe', stderr: 'pipe',
    });
    const pending = new Map<string, { resolve(frame: RpcFrame): void; reject(error: Error): void }>();
    const decoder = new RpcFrameDecoder();
    let sequence = 0;
    let closed = false;
    let failure: Error | undefined;
    const { promise: ready, resolve: readyResolve, reject: readyReject } = Promise.withResolvers<RpcFrame>();
    const fail = (error: Error): void => {
      if (failure !== undefined) return;
      failure = error;
      readyReject(error);
      for (const waiter of pending.values()) waiter.reject(error);
      pending.clear();
      context.onFrame({ type: 'runtime_error', error: error.message });
    };
    const send = (frame: RpcFrame): void => {
      if (closed) throw new Error('OMP RPC connection is closed');
      if (failure !== undefined) throw failure;
      const line = `${JSON.stringify(frame)}\n`;
      if (Buffer.byteLength(line) > FRAME_LIMIT) throw new Error('OMP RPC input exceeds its physical frame limit');
      child.stdin.write(line);
      child.stdin.flush();
    };
    const request = (command: RpcFrame): Promise<RpcFrame> => {
      const id = `desktop_${++sequence}`;
      const { promise, resolve, reject } = Promise.withResolvers<RpcFrame>();
      pending.set(id, { resolve, reject });
      try { send({ ...command, id }); }
      catch (error) {
        pending.delete(id);
        reject(error);
      }
      return promise;
    };
    context.onEarlySend?.(send);
    const stdout = (async (): Promise<void> => {
      const reader = child.stdout.getReader();
      let parts: Uint8Array[] = [];
      let physicalBytes = 0;
      const accept = (line: Uint8Array): void => {
        const text = new TextDecoder('utf-8', { fatal: true }).decode(line);
        if (text.trim().length === 0) return;
        const frame = decoder.decode(parseFrame(text));
        if (frame === undefined) return;
        if (frame.type === 'ready') readyResolve(frame);
        if (frame.type === 'response' && typeof frame.id === 'string') {
          const waiter = pending.get(frame.id);
          if (waiter !== undefined) {
            pending.delete(frame.id);
            if (frame.success === false) waiter.reject(new RpcCommandError(frame));
            else waiter.resolve(frame);
          }
        }
        context.onFrame(frame);
      };
      try {
        for (;;) {
          const next = await reader.read();
          if (next.done) break;
          const chunk = next.value;
          let start = 0;
          for (let position = 0; position < chunk.length; position++) {
            if (chunk[position] !== 10) continue;
            const segment = next.value.subarray(start, position);
            physicalBytes += segment.length;
            if (physicalBytes + 1 > FRAME_LIMIT) throw new Error('OMP stdout frame exceeds the physical frame limit');
            parts.push(segment);
            accept(Buffer.concat(parts, physicalBytes));
            parts = [];
            physicalBytes = 0;
            start = position + 1;
          }
          if (start < chunk.length) {
            const rest = chunk.subarray(start);
            physicalBytes += rest.length;
            if (physicalBytes + 1 > FRAME_LIMIT) throw new Error('OMP stdout frame exceeds the physical frame limit');
            parts.push(rest);
          }
        }
        if (physicalBytes > 0) throw new Error('OMP stdout ended without a newline');
        decoder.finish();
      } catch (error) {
        fail(error instanceof Error ? error : new Error(String(error)));
        child.stdin.end();
      } finally {
        reader.releaseLock();
      }
    })();
    const stderr = (async (): Promise<void> => {
      const reader = child.stderr.getReader();
      const text = new TextDecoder();
      try {
        for (;;) {
          const next = await reader.read();
          if (next.done) break;
          const message = text.decode(next.value, { stream: true });
          if (message.length > 0) context.onFrame({ type: 'runtime_diagnostic', message });
        }
      } finally { reader.releaseLock(); }
    })();
    const exited = child.exited.then(async code => {
      await Promise.all([stdout, stderr]);
      if (!closed) fail(new Error(`OMP exited with code ${code}`));
      context.onFrame({ type: 'runtime_exit', code });
    });
    try {
      const initial = await ready;
      if (Array.isArray(initial.supportedProtocolVersions) && initial.supportedProtocolVersions.includes(2)) {
        await request({ type: 'negotiate_protocol', protocolVersion: 2 });
      }
      await request({ type: 'set_ask_dialog', enabled: true });
      await request({ type: 'set_subagent_subscription', level: 'events' });
      return {
        request, send,
        async close(): Promise<void> {
          if (closed) return;
          closed = true;
          const error = new Error('OMP RPC connection closed');
          for (const waiter of pending.values()) waiter.reject(error);
          pending.clear();
          child.stdin.end();
          await exited;
        },
      };
    } catch (error) {
      closed = true;
      child.stdin.end();
      await exited;
      throw error;
    }
  },
};
