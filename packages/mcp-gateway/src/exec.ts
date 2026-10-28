import { OnlyneError } from '@onlyne/lib';

/** Output kept per stream: enough for one readable result line, bounded so a chatty script cannot bloat the store. */
export const OUTPUT_CAP = 4_096;
/** How long a cancelled or stopping run gets to exit before the subtree is killed outright. */
const KILL_GRACE_MS = 1_500;

export interface CommandResult {
  exit_code: number | null;
  stdout: string;
  stderr: string;
  output_bytes: number;
}

export interface CommandTarget {
  command: string;
  args?: readonly string[];
  cwd?: string;
}

/** A handle the scheduler keeps so it can signal exactly the process it started, never a name match. */
export interface RunningChild {
  cancelled: boolean;
  pid?: number;
}

const safeKill = (pid: number, signal: NodeJS.Signals): void => {
  // Never signal this process or init: a scheduler cancel must not take the gateway down.
  if (pid <= 1 || pid === process.pid) return;
  try { process.kill(pid, signal); } catch { /* the process is already gone */ }
};

/** The parent the kernel currently reports for a pid, used to prove the process is still ours. */
function parentOf(pid: number): number | undefined {
  try {
    const text = Bun.spawnSync({
      cmd: ['ps', '-o', 'ppid=', '-p', String(pid)], stdout: 'pipe', stderr: 'ignore',
    }).stdout.toString().trim();
    const parent = Number(text);
    return Number.isInteger(parent) ? parent : undefined;
  } catch {
    return undefined;
  }
}

/**
 * The live descendants of `pid`, read from `ps` once per signal so the list is the kernel's
 * current view and not a stale snapshot from before the parent exited. Every pid is exact;
 * nothing is matched by name.
 */
async function descendantsOf(pid: number): Promise<number[]> {
  let output: string;
  try {
    output = Bun.spawnSync({
      cmd: ['ps', '-axo', 'pid=,ppid='], stdout: 'pipe', stderr: 'ignore',
    }).stdout.toString();
  } catch {
    return [];
  }
  const children = new Map<number, number[]>();
  for (const line of output.split('\n')) {
    const [childText, parentText] = line.trim().split(/\s+/);
    const child = Number(childText);
    const parent = Number(parentText);
    if (!Number.isInteger(child) || !Number.isInteger(parent)) continue;
    const list = children.get(parent) ?? [];
    list.push(child);
    children.set(parent, list);
  }
  const found: number[] = [];
  const queue = [...(children.get(pid) ?? [])];
  while (queue.length > 0) {
    const next = queue.shift()!;
    if (next === process.pid || found.includes(next)) continue;
    found.push(next);
    queue.push(...(children.get(next) ?? []));
  }
  return found;
}

/** Signals the process group the scheduler built: the exact child first, then its verified descendants. */
export async function signalTree(child: RunningChild, signal: NodeJS.Signals): Promise<void> {
  const pid = child.pid;
  if (pid === undefined) return;
  // A pid that no longer descends from this process has exited and its number may now belong to
  // an unrelated process: drop the handle instead of signalling whatever took the slot.
  if (parentOf(pid) !== process.pid) {
    child.pid = undefined;
    return;
  }
  const descendants = await descendantsOf(pid);
  for (const descendant of descendants.reverse()) safeKill(descendant, signal);
  safeKill(pid, signal);
}

/**
 * Stops one run: SIGTERM for a clean exit, then SIGKILL of the same subtree while its root is
 * still alive after the grace period. The escalation is not awaited by the caller, so a cancel
 * answers as soon as the signal is sent; the run settles when its process actually exits.
 */
export function stopChild(child: RunningChild): void {
  if (child.cancelled) return;
  child.cancelled = true;
  const pid = child.pid;
  if (pid === undefined) return;
  void signalTree(child, 'SIGTERM');
  const timer = setTimeout(() => {
    // Re-derive from the live root: a dead root's pid may already belong to another process,
    // and a reused pid must never receive this run's signal.
    if (child.pid === pid) void signalTree(child, 'SIGKILL');
  }, KILL_GRACE_MS);
  timer.unref?.();
}

async function drain(stream: ReadableStream<Uint8Array>, cap: number): Promise<{ text: string; bytes: number }> {
  const decoder = new TextDecoder();
  let text = '';
  let bytes = 0;
  for await (const chunk of stream as unknown as AsyncIterable<Uint8Array>) {
    bytes += chunk.byteLength;
    text += decoder.decode(chunk, { stream: true });
    if (text.length > cap) text = text.slice(-cap);
  }
  text += decoder.decode();
  if (text.length > cap) text = text.slice(-cap);
  return { text, bytes };
}

/**
 * Runs one allowlisted executable with argv directly, never through a shell: no quoting, no
 * metacharacters, no remote string ever reaches `/bin/sh`. The child's pid is recorded before
 * any output is read so a cancel that arrives first still has a handle to signal.
 */
export async function runCommand(target: CommandTarget, child: RunningChild): Promise<CommandResult> {
  let proc: ReturnType<typeof Bun.spawn>;
  try {
    proc = Bun.spawn({
      cmd: [target.command, ...(target.args ?? [])],
      cwd: target.cwd ?? process.cwd(),
      env: process.env,
      stdio: ['ignore', 'pipe', 'pipe'],
    });
  } catch (error) {
    throw new OnlyneError('invalid_request', `The command ${target.command} could not be started: ${error instanceof Error ? error.message : String(error)}`);
  }
  child.pid = proc.pid;
  try {
    const [stdout, stderr] = await Promise.all([
      drain(proc.stdout as ReadableStream<Uint8Array>, OUTPUT_CAP),
      drain(proc.stderr as ReadableStream<Uint8Array>, OUTPUT_CAP),
      proc.exited,
    ]);
    return {
      exit_code: proc.exitCode ?? null,
      stdout: stdout.text,
      stderr: stderr.text,
      output_bytes: stdout.bytes + stderr.bytes,
    };
  } finally {
    // The child is gone; releasing the handle keeps a late cancel from signalling a reused pid.
    if (proc.exitCode !== null) child.pid = undefined;
  }
}
