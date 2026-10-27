import { appendFileSync } from 'node:fs';

/** A bounded tail of the gateway's own log lines, so the TUI can show them. */
export interface LogRing {
  push(key: string, line: string): void;
  tail(count: number): string[];
}

export interface GatewayLogger {
  log(message: string, fields?: Record<string, unknown>): void;
  readonly ring: LogRing;
}

/**
 * The gateway logs JSON lines. Under the TUI those lines would corrupt the alternate
 * screen, so they go to a ring the TUI renders instead; without a TTY they stay on stdout.
 */
export function createLogger(toStdout: boolean, capacity = 200, file?: string): GatewayLogger {
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
  return {
    ring,
    log(message: string, fields?: Record<string, unknown>): void {
      const line = JSON.stringify({ at: new Date().toISOString(), message, ...fields });
      if (toStdout) console.log(line);
      else ring.push(`${message}|${JSON.stringify(fields ?? {})}`, line);
      // The TUI owns the screen, so the durable record is the file; without one the line would
      // exist only in the ring and vanish with the process.
      if (file !== undefined) {
        try { appendFileSync(file, `${line}\n`); } catch { /* a full disk must not stop the gateway */ }
      }
    },
  };
}

