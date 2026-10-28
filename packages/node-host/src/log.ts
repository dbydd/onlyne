import { appendFileSync } from 'node:fs';

/** A bounded tail of the gateway's own log lines, so the TUI can show them. */
export interface LogRing {
  push(key: string, line: string): void;
  tail(count: number): string[];
}

/** The four levels of the convention: `debug` loops, `info` milestones, `warn` degradation, `error` loss. */
export type LogLevel = 'debug' | 'info' | 'warn' | 'error';

/** One JSON line about to be written. */
export type LogLine = (message: string, fields?: Record<string, unknown>) => void;

/**
 * A host's log sink. Calling it writes `info`, the level every existing call site uses; a sink
 * that also carries `debug`/`warn`/`error` records those levels where the host put them.
 */
export type LogSink = LogLine & {
  readonly debug?: LogLine;
  readonly warn?: LogLine;
  readonly error?: LogLine;
};

export type LeveledLog = LogLine & {
  log: LogLine;
  debug: LogLine;
  warn: LogLine;
  error: LogLine;
};

export type GatewayLogger = LeveledLog & { readonly level: LogLevel; readonly ring: LogRing };

const ORDER: Record<LogLevel, number> = { debug: 0, info: 1, warn: 2, error: 3 };

/** `ONLYNE_LOG_LEVEL` selects a level; anything unrecognised (and unset) keeps `info`. */
export function resolveLogLevel(value: string | undefined): LogLevel {
  return value === 'debug' || value === 'warn' || value === 'error' ? value : 'info';
}

/**
 * Normalizes a sink to the four entry points. A sink without level methods records every line
 * it is handed: it cannot filter, so the host's own store decides what the level means.
 */
export function leveled(sink?: LogSink): LeveledLog {
  const call: LogLine = (message, fields) => { if (typeof sink === 'function') sink(message, fields); };
  return Object.assign(call, {
    log: call,
    debug: sink?.debug ?? call,
    warn: sink?.warn ?? call,
    error: sink?.error ?? call,
  });
}

/**
 * The gateway logs JSON lines. Under the TUI those lines would corrupt the alternate
 * screen, so they go to a ring the TUI renders instead; without a TTY they stay on stdout.
 * The returned function is the `info` line — `logger.log` is the same entry point — and
 * `debug`, `warn`, `error` are the other levels of the same convention.
 */
export function createLogger(toStdout: boolean, capacity = 200, file?: string, level?: LogLevel): GatewayLogger {
  const selected = level ?? resolveLogLevel(process.env.ONLYNE_LOG_LEVEL);
  const threshold = ORDER[selected];
  // One entry per distinct message: a gateway that retries every five seconds would otherwise
  // fill the pane with the same line and hide everything else.
  const lines: Array<{ key: string; line: string; count: number }> = [];
  const ring: LogRing = {
    push(key: string, line: string): void {
      const last = lines.at(-1);
      if (last !== undefined && last.key === key) {
        last.line = line;
        last.count += 1;
        return;
      }
      lines.push({ key, line, count: 1 });
      if (lines.length > capacity) lines.splice(0, lines.length - capacity);
    },
    tail(count: number): string[] {
      return lines.slice(Math.max(0, lines.length - count))
        .map(entry => (entry.count === 1 ? entry.line : `${entry.line} ×${entry.count}`));
    },
  };
  const emit = (at: LogLevel, message: string, fields?: Record<string, unknown>): void => {
    if (ORDER[at] < threshold) return;
    const line = JSON.stringify({ at: new Date().toISOString(), level: at, message, ...fields });
    if (toStdout) console.log(line);
    else ring.push(`${at}|${message}|${JSON.stringify(fields ?? {})}`, line);
    // The TUI owns the screen, so the durable record is the file; without one the line would
    // exist only in the ring and vanish with the process.
    if (file !== undefined) {
      try { appendFileSync(file, `${line}\n`); } catch { /* a full disk must not stop the gateway */ }
    }
  };
  const info: LogLine = (message, fields) => emit('info', message, fields);
  return Object.assign(info, {
    level: selected,
    log: info,
    debug: (message: string, fields?: Record<string, unknown>) => emit('debug', message, fields),
    warn: (message: string, fields?: Record<string, unknown>) => emit('warn', message, fields),
    error: (message: string, fields?: Record<string, unknown>) => emit('error', message, fields),
    ring,
  });
}
