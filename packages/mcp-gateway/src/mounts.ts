import {
  OnlyneError, type Json, type Message, type NodeId, type OperationRequest, type OperationResult,
  type Resource, type ResourceDriver, type SendOutcome, type Storage, type ThreadRef,
} from '@onlyne/lib';
import type { McpOutcome } from '@onlyne/mcp-core';
import type { MountsConfig } from './config.js';
import { nextCronTime, parseCron } from './cron.js';
import { runCommand, stopChild, type RunningChild } from './exec.js';

/** The scheduler's resource ids carry this prefix, so one driver routes MCP servers and mounts apart. */
export const MOUNTS_RESOURCE_PREFIX = 'mounts:';
export const MOUNT_OPERATIONS = ['mount', 'update', 'list', 'inspect', 'runs', 'cancel', 'resume', 'remove'] as const;

export const mountsResourceId = (controller: NodeId): string => `${MOUNTS_RESOURCE_PREFIX}${controller}`;

export type MountSchedule =
  | { kind: 'interval'; every_ms: number }
  | { kind: 'cron'; expression: string }
  | { kind: 'once'; at: string };

export type MountAction =
  | { kind: 'command'; command: string }
  | { kind: 'mcp'; server: string; tool: string; arguments?: Json };

export type MountTaskState = 'active' | 'cancelled' | 'completed' | 'removed';
export type MountRunState = 'running' | 'succeeded' | 'failed' | 'unknown' | 'cancelled' | 'interrupted' | 'skipped';
export type MountPushState = 'pending' | 'unknown' | 'accepted' | 'refused' | 'expired';

export interface MountError { code: string; message: string }

export interface MountPush {
  message_id: string;
  state: MountPushState;
  attempts: number;
  retry_until: string;
  next_attempt_at?: string;
  receipt?: { thread: ThreadRef; seq: string };
  error?: string;
}

export interface MountRunSummary {
  id: string; state: MountRunState; started_at: string; finished_at: string | null; exit_code: number | null;
}

export interface MountChange {
  at: string; actor: NodeId; action: 'mount' | 'update' | 'cancel' | 'resume' | 'remove';
  title?: string; schedule?: MountSchedule; definition?: MountAction;
}

export interface MountTask {
  id: string;
  controller: NodeId;
  state: MountTaskState;
  title?: string;
  schedule: MountSchedule;
  action: MountAction;
  next_run_at: string | null;
  /** One thread per task: every run result lands there, so the controller reads a conversation. */
  thread: ThreadRef;
  created_at: string;
  updated_at: string;
  changes: MountChange[];
  last_run: MountRunSummary | null;
}

export interface MountRun {
  id: string;
  task_id: string;
  controller: NodeId;
  /** The definition snapshot this run started with; a later update never rewrites it. */
  action: MountAction;
  thread: ThreadRef;
  state: MountRunState;
  started_at: string;
  finished_at: string | null;
  /** The process exit status; `null` for an MCP tool call and for a run that never produced one. */
  exit_code: number | null;
  output_summary: string;
  output_bytes: number;
  error: MountError | null;
  /** Absent for a skipped occurrence: nothing ran, so there is nothing to push. */
  push?: MountPush;
}

/** The host seams the scheduler needs; the gateway binds them to its node, tests bind their own. */
export interface MountTransport {
  ensureThread(threadId: string, title: string): Promise<void>;
  resolveTarget(node: NodeId): Promise<void>;
  send(controller: NodeId, message: Message): Promise<SendOutcome>;
}

export interface MountConnection {
  readonly tools: ReadonlyArray<{ name: string; annotations?: { readOnlyHint?: boolean } }>;
  call(name: string, arguments_: Json): Promise<McpOutcome>;
}

export interface MountServiceOptions {
  owner: NodeId;
  config: MountsConfig | undefined;
  servers: ReadonlyArray<{ name: string; controllers?: NodeId[] }>;
  storage: Storage;
  connections: ReadonlyMap<string, MountConnection>;
  transport: MountTransport;
  log?(message: string, fields?: Record<string, unknown>): void;
  now?(): number;
}

export interface MountService {
  readonly driver: ResourceDriver;
  /** Loads the store, repairs interrupted runs, and schedules the first pass. */
  start(): Promise<void>;
  /** Stops the timer, signals every child it started, and records the runs as interrupted. */
  stop(): Promise<void>;
  /** One scheduling pass: retry unfinished pushes, then start every due task. */
  tick(): Promise<void>;
  /** Display lines for the node host's TUI: global categories first, per-task detail last. */
  status(): Promise<string[]>;
  resources(): Array<{ controller: NodeId; resource: Resource }>;
}

interface PushEntry {
  task_id: string; run_id: string; state: 'pending' | 'unknown'; attempts: number; next_attempt_at: string;
}

interface ActiveRun {
  task_id: string;
  child: RunningChild;
  done: Promise<void>;
}

const DAY_MS = 86_400_000;
/** How long a result message may keep being retried before the queue settles it as expired. */
const PUSH_WINDOW_MS = DAY_MS;
const MAX_CHANGES = 20;
const MAX_SUMMARY = 4_000;
const RETRY_CAP_MS = 60_000;
/** Push codes that no retry can fix; anything else stays in the queue. */
const TERMINAL_PUSH: Record<string, true> = {
  invalid_request: true, forbidden: true, not_a_member: true, id_conflict: true,
  revision_conflict: true, conflict: true, thread_closed: true, payload_too_large: true,
  unsupported_media_type: true, unsupported_delivery: true, ambiguous_target: true,
};

const iso = (ms: number): string => new Date(ms).toISOString();
const timeOf = (value: string | null): string => (value === null ? '—' : new Date(value).toTimeString().slice(0, 8));
/** Operation data is `Json`; the task and run interfaces are the typed face of the same shapes. */
const wire = (value: unknown): Json => JSON.parse(JSON.stringify(value)) as Json;

function failureOf(error: unknown): MountError {
  const reported = (error as { error?: { code?: unknown; message?: unknown } }).error;
  return {
    code: typeof reported?.code === 'string' ? reported.code : 'internal',
    message: typeof reported?.message === 'string' ? reported.message : (error instanceof Error ? error.message : String(error)),
  };
}

function recordOf(value: Json | undefined, what: string): Record<string, Json> {
  if (value === undefined || value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new OnlyneError('invalid_request', `${what} must be an object.`);
  }
  return value;
}

function textField(container: Record<string, Json>, field: string): string {
  const value = container[field];
  if (typeof value !== 'string' || value.length === 0) {
    throw new OnlyneError('invalid_request', `${field} must be a non-empty string.`);
  }
  return value;
}

function describeSchedule(schedule: MountSchedule): string {
  if (schedule.kind === 'cron') return `cron ${schedule.expression}`;
  if (schedule.kind === 'once') return `once ${schedule.at}`;
  return schedule.every_ms % 1_000 === 0 ? `every ${schedule.every_ms / 1_000}s` : `every ${schedule.every_ms}ms`;
}

/**
 * One run's readable output: stdout, then stderr, tail-bounded. `bytes` counts what the child
 * wrote even when the summary only keeps the end of it.
 */
function summarizeRun(stdout: string, stderr: string, bytes: number): { summary: string; bytes: number } {
  const joined = [stdout, stderr].filter(text => text.length > 0).join('\n');
  return { summary: joined.length > MAX_SUMMARY ? joined.slice(-MAX_SUMMARY) : joined, bytes };
}

/**
 * The persistent scheduler: task records, the run ledger and the result-push queue all live in
 * the node's store. A mount's definition is immutable for the run it started; a crash between
 * start and finish is recorded as `interrupted`, never silently retried, so an external effect
 * is never claimed to be exactly-once.
 */
export function createMountService(options: MountServiceOptions): MountService {
  const owner = options.owner;
  const config = options.config;
  const storage = options.storage;
  const now = options.now ?? (() => Date.now());
  const log = options.log ?? (() => undefined);
  const commands = new Map((config?.commands ?? []).map(entry => [entry.id, entry]));
  const tools = new Set((config?.tools ?? []).map(entry => `${entry.server}\u0000${entry.tool}`));
  const controllers = new Set(config?.controllers ?? []);
  const servers = new Map(options.servers.map(entry => [entry.name, entry]));
  const tickMs = config?.tick_ms ?? 1_000;

  const tasks = new Map<string, MountTask>();
  /** Running runs only; finished history is read from the store, so memory stays bounded. */
  const active = new Map<string, ActiveRun>();
  const push = new Map<string, PushEntry>();
  const pushing = new Set<string>();
  const settled = { accepted: 0, refused: 0, expired: 0 };
  let recent: MountRun[] = [];
  let timer: ReturnType<typeof setInterval> | undefined;
  let ticking = false;
  let stopping = false;

  const taskKey = (id: string) => `mounts/task/${id}`;
  const runKey = (taskId: string, runId: string) => `mounts/run/${taskId}/${runId}`;
  const runPrefix = (taskId: string) => `mounts/run/${taskId}/`;
  const pushKey = (taskId: string, runId: string) => `mounts/push/${taskId}/${runId}`;

  const saveTask = (task: MountTask): Promise<void> => storage.transaction(tx => tx.put(taskKey(task.id), task));
  const saveRun = (run: MountRun): Promise<void> => storage.transaction(tx => tx.put(runKey(run.task_id, run.id), run));
  const loadRun = (taskId: string, runId: string): Promise<MountRun | undefined> =>
    storage.transaction(tx => tx.get<MountRun>(runKey(taskId, runId)));
  const savePush = (entry: PushEntry): Promise<void> => storage.transaction(tx => tx.put(pushKey(entry.task_id, entry.run_id), entry));
  const dropPush = (entry: PushEntry): Promise<void> => storage.transaction(tx => tx.delete(pushKey(entry.task_id, entry.run_id)));

  function change(task: MountTask, actor: NodeId, action: MountChange['action'], extra: Partial<MountChange> = {}): void {
    task.changes.push({ at: iso(now()), actor, action, ...extra });
    if (task.changes.length > MAX_CHANGES) task.changes = task.changes.slice(-MAX_CHANGES);
  }

  function runSummary(run: MountRun): MountRunSummary {
    return { id: run.id, state: run.state, started_at: run.started_at, finished_at: run.finished_at, exit_code: run.exit_code };
  }

  function noteRecent(run: MountRun): void {
    recent = [run, ...recent.filter(item => item.id !== run.id)]
      .sort((a, b) => ((a.finished_at ?? a.started_at) < (b.finished_at ?? b.started_at) ? 1 : -1))
      .slice(0, 5);
  }

  // ---- request parsing and authorization -------------------------------------

  function controllerOf(resource: Resource): NodeId {
    const id = resource.resource_ref.id;
    if (!id.startsWith(MOUNTS_RESOURCE_PREFIX)) {
      throw new OnlyneError('forbidden', 'This driver only serves the scheduler resource.');
    }
    const controller = id.slice(MOUNTS_RESOURCE_PREFIX.length);
    if (!controllers.has(controller)) {
      throw new OnlyneError('forbidden', 'Only a configured mount controller may operate this resource.');
    }
    return controller;
  }

  function parseSchedule(value: Json | undefined): MountSchedule {
    const record = recordOf(value, 'schedule');
    const kind = textField(record, 'kind');
    if (kind === 'interval') {
      const every = record.every_ms;
      if (typeof every !== 'number' || !Number.isInteger(every) || every < 1_000) {
        throw new OnlyneError('invalid_request', 'An interval schedule needs every_ms as an integer of at least 1000.');
      }
      return { kind: 'interval', every_ms: every };
    }
    if (kind === 'cron') {
      const expression = textField(record, 'expression');
      if (nextCronTime(parseCron(expression), now()) === undefined) {
        throw new OnlyneError('invalid_request', `The cron expression ${expression} has no occurrence in the next five years.`);
      }
      return { kind: 'cron', expression };
    }
    if (kind === 'once') {
      const at = textField(record, 'at');
      const when = Date.parse(at);
      if (!Number.isFinite(when)) throw new OnlyneError('invalid_request', 'A one-shot schedule needs at as an ISO timestamp.');
      if (when < now()) throw new OnlyneError('invalid_request', 'A one-shot schedule needs a time in the future.');
      return { kind: 'once', at: iso(when) };
    }
    throw new OnlyneError('invalid_request', `The schedule kind ${JSON.stringify(kind)} is interval, cron or once.`);
  }

  function parseAction(controller: NodeId, value: Json | undefined): MountAction {
    const record = recordOf(value, 'action');
    const kind = textField(record, 'kind');
    if (kind === 'command') {
      const command = textField(record, 'command');
      if (!commands.has(command)) {
        throw new OnlyneError('forbidden', `The mount command ${command} is not in the configured command allowlist.`);
      }
      return { kind: 'command', command };
    }
    if (kind === 'mcp') {
      const server = textField(record, 'server');
      const tool = textField(record, 'tool');
      if (!servers.has(server)) {
        throw new OnlyneError('invalid_request', `No configured MCP server is named ${server}.`);
      }
      const permitted = servers.get(server)?.controllers ?? [];
      // A scheduled tool call runs with the mounting controller's authority, so the server's
      // own controller list decides whether that controller could have called it at all.
      if (!permitted.includes(controller)) {
        throw new OnlyneError('forbidden', `The server ${server} does not list ${controller} as a controller.`);
      }
      if (!tools.has(`${server}\u0000${tool}`)) {
        throw new OnlyneError('forbidden', `The tool ${server}/${tool} is not in the configured mount tool allowlist.`);
      }
      const argument = record.arguments;
      if (argument !== undefined && argument !== null && (typeof argument !== 'object' || Array.isArray(argument))) {
        throw new OnlyneError('invalid_request', 'The tool arguments for a mount are an object.');
      }
      return {
        kind: 'mcp', server, tool,
        ...(argument === undefined || argument === null ? {} : { arguments: argument }),
      };
    }
    throw new OnlyneError('invalid_request', `The mount action kind ${JSON.stringify(kind)} is command or mcp.`);
  }

  function parseTitle(value: Json | undefined): string | undefined {
    if (value === undefined || value === null) return undefined;
    if (typeof value !== 'string' || value.trim().length === 0) {
      throw new OnlyneError('invalid_request', 'title must be a non-empty string when present.');
    }
    if (value.length > 200) throw new OnlyneError('invalid_request', 'title is at most 200 characters.');
    return value;
  }

  function parseTaskId(record: Record<string, Json>): string {
    const id = textField(record, 'task');
    if (!tasks.has(id)) throw new OnlyneError('not_found', `No mount task is named ${id}.`);
    return id;
  }

  function requireTask(controller: NodeId, id: string): MountTask {
    const task = tasks.get(id);
    if (task === undefined || task.controller !== controller) {
      throw new OnlyneError('not_found', `No mount task is named ${id}.`);
    }
    return task;
  }

  /** The next instant for a schedule; `immediate` fires interval/once at `fromMs` instead of later. */
  function nextFor(schedule: MountSchedule, fromMs: number, immediate: boolean): string | null {
    if (schedule.kind === 'once') return immediate ? iso(fromMs) : schedule.at;
    if (schedule.kind === 'interval') return iso(immediate ? fromMs : fromMs + schedule.every_ms);
    const next = nextCronTime(parseCron(schedule.expression), fromMs);
    return next === undefined ? null : iso(next);
  }

  // ---- run lifecycle ----------------------------------------------------------

  async function recordSkipped(task: MountTask, atMs: number, reason: string): Promise<void> {
    const run: MountRun = {
      id: `${atMs.toString(36)}-${crypto.randomUUID().slice(0, 8)}`,
      task_id: task.id, controller: task.controller, action: structuredClone(task.action), thread: task.thread,
      state: 'skipped', started_at: iso(atMs), finished_at: iso(atMs), exit_code: null,
      output_summary: '', output_bytes: 0,
      error: { code: 'overlap', message: `The previous run was still active at this occurrence (${reason}).` },
    };
    await saveRun(run);
    noteRecent(run);
  }

  /** Re-arms a task whose occurrence passed while its previous run was still active. */
  function advanceOverdue(task: MountTask, atMs: number): void {
    if (task.schedule.kind === 'interval') {
      task.next_run_at = iso(atMs + task.schedule.every_ms);
      return;
    }
    if (task.schedule.kind === 'cron') {
      const next = nextCronTime(parseCron(task.schedule.expression), atMs);
      task.next_run_at = next === undefined ? null : iso(next);
      return;
    }
    task.next_run_at = null;
  }

  async function startRun(task: MountTask, atMs: number): Promise<void> {
    const runId = `${atMs.toString(36)}-${crypto.randomUUID().slice(0, 8)}`;
    const run: MountRun = {
      id: runId,
      task_id: task.id, controller: task.controller, action: structuredClone(task.action), thread: task.thread,
      state: 'running', started_at: iso(atMs), finished_at: null, exit_code: null,
      output_summary: '', output_bytes: 0, error: null,
      // The message id is the run id: a re-enqueue of the same run is the same message, so a
      // peer dedupes it instead of receiving the result twice.
      push: { message_id: runId, state: 'pending', attempts: 0, retry_until: iso(atMs + PUSH_WINDOW_MS) },
    };
    // The schedule advances before the external effect starts. A crash in between leaves a
    // running record that startup repairs to `interrupted`; it is never re-fired as success.
    if (task.schedule.kind === 'once') task.next_run_at = null;
    else if (task.schedule.kind === 'interval') task.next_run_at = iso(atMs + task.schedule.every_ms);
    else {
      const next = nextCronTime(parseCron(task.schedule.expression), atMs);
      task.next_run_at = next === undefined ? null : iso(next);
    }
    task.updated_at = iso(atMs);
    const child: RunningChild = { cancelled: false };
    let release = (): void => undefined;
    const done = new Promise<void>(resolve => { release = resolve; });
    // Insert the running record before any await: a concurrent tick sees the overlap, and the
    // durable record lands before the child exists.
    active.set(runId, { task_id: task.id, child, done });
    try {
      await saveRun(run);
      await saveTask(task);
    } catch (error) {
      // Nothing was started, so no run is left owing a record.
      active.delete(runId);
      release();
      throw error;
    }
    log('mount.run.started', { task: task.id, run: runId, kind: task.action.kind });
    void executeRun(task, run, child)
      .catch(error => log('mount.run.failed', { task: task.id, run: runId, error: String(error) }))
      .finally(() => {
        active.delete(runId);
        release();
      });
  }

  async function executeRun(task: MountTask, run: MountRun, child: RunningChild): Promise<void> {
    let state: MountRunState = 'failed';
    let exitCode: number | null = null;
    let summary = '';
    let bytes = 0;
    let error: MountError | null = null;
    const action = run.action;
    try {
      if (action.kind === 'command') {
        const spec = commands.get(action.command);
        if (spec === undefined) {
          throw new OnlyneError('invalid_request', `The allowlist no longer holds the command ${action.command}.`);
        }
        const result = await runCommand(spec, child);
        const combined = summarizeRun(result.stdout, result.stderr, result.output_bytes);
        summary = combined.summary;
        bytes = combined.bytes;
        exitCode = result.exit_code;
        if (stopping) {
          state = 'interrupted';
          error = { code: 'interrupted', message: 'The gateway stopped before this run finished; the external effect is unconfirmed.' };
        } else if (child.cancelled) {
          state = 'cancelled';
          error = { code: 'cancelled', message: 'The mount was cancelled while this run was active.' };
        } else if (exitCode === 0) {
          state = 'succeeded';
        } else {
          state = 'failed';
          error = { code: 'exit_nonzero', message: `The command exited with status ${exitCode ?? 'unknown'}.` };
        }
      } else {
        const connection = options.connections.get(action.server);
        if (connection === undefined) {
          throw new OnlyneError('unavailable', `No configured MCP server is named ${action.server}.`);
        }
        const tool = connection.tools.find(candidate => candidate.name === action.tool);
        if (tool === undefined) {
          throw new OnlyneError('unsupported_delivery', `The tool ${action.tool} is not in ${action.server}'s tools/list.`);
        }
        const outcome = await connection.call(action.tool, action.arguments ?? {});
        if (outcome.kind === 'result') {
          const text = JSON.stringify({
            ...(outcome.content === undefined ? {} : { content: outcome.content }),
            ...(outcome.structuredContent === undefined ? {} : { structuredContent: outcome.structuredContent }),
          });
          summary = text.length > MAX_SUMMARY ? text.slice(0, MAX_SUMMARY) : text;
          bytes = text.length;
          state = 'succeeded';
        } else if (outcome.kind === 'rejected') {
          summary = outcome.text.slice(0, MAX_SUMMARY);
          bytes = outcome.text.length;
          const readOnly = tool.annotations?.readOnlyHint === true;
          state = readOnly ? 'failed' : 'unknown';
          error = {
            code: 'tool_rejected',
            message: readOnly
              ? 'The MCP tool reported failure.'
              : 'The MCP tool reported failure; the side effect is unconfirmed.',
          };
        } else {
          summary = outcome.text.slice(0, MAX_SUMMARY);
          bytes = outcome.text.length;
          state = 'unknown';
          error = { code: 'unavailable', message: 'The MCP server gave no answer; the side effect is unconfirmed.' };
        }
        // A cancel or shutdown cannot abort the in-flight tool call, so the observed result is
        // kept, but the run is recorded for what it is: stopped before its answer was settled.
        if (stopping) {
          state = 'interrupted';
          error = { code: 'interrupted', message: 'The gateway stopped before this tool call settled; the external effect is unconfirmed.' };
        } else if (child.cancelled) {
          state = 'cancelled';
          error = { code: 'cancelled', message: 'The mount was cancelled while this tool call was in flight.' };
        }
      }
    } catch (err) {
      error = failureOf(err);
      state = error.code === 'unavailable' ? 'unknown' : 'failed';
    } finally {
      await finishRun(task, run, { state, exitCode, summary, bytes, error });
    }
  }

  async function finishRun(
    task: MountTask,
    run: MountRun,
    outcome: { state: MountRunState; exitCode: number | null; summary: string; bytes: number; error: MountError | null },
  ): Promise<void> {
    run.state = outcome.state;
    run.exit_code = outcome.exitCode;
    run.output_summary = outcome.summary;
    run.output_bytes = outcome.bytes;
    run.error = outcome.error;
    run.finished_at = iso(now());
    task.last_run = runSummary(run);
    task.updated_at = run.finished_at;
    if (task.state === 'active') {
      if (task.schedule.kind === 'once') {
        task.state = 'completed';
      } else if (task.next_run_at !== null && Date.parse(task.next_run_at) <= now()) {
        // The run overran its slot: one honest skip record per missed occurrence, then re-arm.
        await recordSkipped(task, now(), 'run overran its schedule');
        advanceOverdue(task, now());
      }
    }
    const pushState = run.push;
    if (pushState !== undefined) {
      pushState.state = 'pending';
      pushState.attempts = 0;
      // The retry window opens when the result exists, not when a long run started.
      pushState.retry_until = iso(Date.parse(run.finished_at) + PUSH_WINDOW_MS);
      const entry: PushEntry = {
        task_id: run.task_id, run_id: run.id, state: 'pending', attempts: 0, next_attempt_at: run.finished_at,
      };
      push.set(pushKey(run.task_id, run.id), entry);
      await savePush(entry);
    }
    await saveRun(run);
    await saveTask(task);
    noteRecent(run);
    log('mount.run.finished', { task: task.id, run: run.id, state: run.state, exit_code: run.exit_code });
  }

  // ---- result push queue ------------------------------------------------------

  function resultMessage(run: MountRun): Message {
    const exit = run.exit_code === null ? '' : ` exit=${run.exit_code}`;
    const reason = run.error === null ? '' : `: ${run.error.message}`;
    const body = run.output_summary.trim();
    return {
      id: run.push?.message_id ?? run.id,
      sender: owner,
      delivery_kind: 'mount.run',
      msg: `mount run ${run.id} (task ${run.task_id}) ${run.state}${exit}${reason}${body.length > 0 ? `\n${body}` : ''}`.slice(0, 3_500),
      meta: {
        thread: run.thread,
        data: {
          task_id: run.task_id,
          run_id: run.id,
          state: run.state,
          exit_code: run.exit_code,
          started_at: run.started_at,
          finished_at: run.finished_at,
          output_summary: run.output_summary.slice(0, 2_000),
          error: run.error === null ? null : { ...run.error },
        },
      },
    };
  }

  async function attemptPush(entry: PushEntry): Promise<void> {
    const run = await loadRun(entry.task_id, entry.run_id);
    if (run?.push === undefined) {
      push.delete(pushKey(entry.task_id, entry.run_id));
      return;
    }
    const current = run.push;
    current.attempts += 1;
    const settle = async (state: MountPushState, message: string | undefined): Promise<void> => {
      current.state = state;
      if (message === undefined) delete current.error;
      else current.error = message;
      if (state === 'pending' || state === 'unknown') {
        const delay = Math.min(RETRY_CAP_MS, 1_000 * 2 ** Math.min(current.attempts, 6));
        current.next_attempt_at = iso(now() + delay);
        entry.state = state;
        entry.attempts = current.attempts;
        entry.next_attempt_at = current.next_attempt_at;
        push.set(pushKey(entry.task_id, entry.run_id), entry);
        await savePush(entry);
      } else {
        delete current.next_attempt_at;
        push.delete(pushKey(entry.task_id, entry.run_id));
        await dropPush(entry);
        if (state === 'refused') settled.refused += 1;
        else settled.expired += 1;
      }
      await saveRun(run);
    };
    const expired = (): boolean => now() > Date.parse(current.retry_until);
    try {
      await options.transport.resolveTarget(run.controller);
    } catch (error) {
      const failure = failureOf(error);
      // An undiscovered controller is not a refusal: the queue keeps the result until the peer
      // becomes resolvable or the window closes.
      if (expired()) await settle('expired', failure.message);
      else await settle('pending', `target-unresolved: ${failure.message}`);
      return;
    }
    try {
      const outcome = await options.transport.send(run.controller, resultMessage(run));
      if (outcome.admission === 'accepted') {
        current.state = 'accepted';
        delete current.error;
        delete current.next_attempt_at;
        if (outcome.receipt !== undefined) {
          current.receipt = { thread: outcome.receipt.thread_ref, seq: outcome.receipt.seq };
        }
        push.delete(pushKey(entry.task_id, entry.run_id));
        await dropPush(entry);
        await saveRun(run);
        settled.accepted += 1;
        return;
      }
      const message = outcome.error?.message ?? 'The delivery admission is unsettled.';
      if (outcome.admission === 'refused') await settle('refused', message);
      else if (expired()) await settle('expired', message);
      else await settle('unknown', message);
    } catch (error) {
      const failure = failureOf(error);
      if (TERMINAL_PUSH[failure.code] === true) await settle('refused', failure.message);
      else if (expired()) await settle('expired', failure.message);
      else await settle('unknown', failure.message);
    }
  }

  function schedulePushes(): void {
    const at = now();
    for (const [key, entry] of push) {
      if (pushing.has(key)) continue;
      if (Date.parse(entry.next_attempt_at) > at) continue;
      pushing.add(key);
      void attemptPush(entry)
        .catch(error => log('mount.push.failed', { task: entry.task_id, run: entry.run_id, error: String(error) }))
        .finally(() => pushing.delete(key));
    }
  }

  // ---- scheduling -------------------------------------------------------------

  async function startDue(nowMs: number): Promise<void> {
    const due = [...tasks.values()]
      .filter(task => task.state === 'active' && task.next_run_at !== null && Date.parse(task.next_run_at) <= nowMs)
      .sort((a, b) => ((a.next_run_at ?? '') < (b.next_run_at ?? '') ? -1 : 1));
    for (const task of due) {
      if (stopping) return;
      const running = [...active.values()].find(entry => entry.task_id === task.id);
      if (running !== undefined) {
        await recordSkipped(task, nowMs, 'the previous run is still active');
        advanceOverdue(task, nowMs);
        task.updated_at = iso(nowMs);
        await saveTask(task);
        continue;
      }
      await startRun(task, nowMs);
    }
  }

  async function loadStore(): Promise<void> {
    const at = now();
    const stored = await storage.transaction(async tx => ({
      tasks: (await tx.scan<MountTask>('mounts/task/')).map(row => row.value),
      runs: (await tx.scan<MountRun>('mounts/run/')).map(row => row.value),
      pushes: (await tx.scan<PushEntry>('mounts/push/')).map(row => row.value),
    }));
    for (const task of stored.tasks) tasks.set(task.id, task);
    for (const entry of stored.pushes) push.set(pushKey(entry.task_id, entry.run_id), entry);
    const runsByTask = new Map<string, MountRun[]>();
    for (const run of stored.runs) {
      const list = runsByTask.get(run.task_id) ?? [];
      list.push(run);
      runsByTask.set(run.task_id, list);
      if (run.state === 'running') {
        const task = tasks.get(run.task_id);
        run.state = 'interrupted';
        run.finished_at = iso(at);
        run.error = { code: 'interrupted', message: 'The gateway stopped before this run finished; the external effect is unconfirmed.' };
        if (task !== undefined) {
          task.last_run = runSummary(run);
          task.updated_at = run.finished_at;
        }
        if (run.push !== undefined && run.push.state !== 'accepted') {
          run.push.state = 'pending';
          run.push.attempts = 0;
          const entry: PushEntry = {
            task_id: run.task_id, run_id: run.id, state: 'pending', attempts: 0, next_attempt_at: run.finished_at,
          };
          push.set(pushKey(run.task_id, run.id), entry);
          await savePush(entry);
        }
        await saveRun(run);
        log('mount.run.interrupted', { task: run.task_id, run: run.id });
      }
      noteRecent(run);
    }
    for (const task of tasks.values()) {
      if (task.state !== 'active') continue;
      const runs = runsByTask.get(task.id) ?? [];
      if (task.schedule.kind === 'once') {
        // A one-shot mount fires once. A finished or interrupted attempt counts as fired, so a
        // restart never repeats a side effect it cannot prove did not happen.
        if (runs.length > 0) {
          task.state = 'completed';
          task.next_run_at = null;
          await saveTask(task);
        } else if (task.next_run_at === null) {
          task.next_run_at = iso(at);
          await saveTask(task);
        }
        continue;
      }
      const next = task.next_run_at === null ? undefined : Date.parse(task.next_run_at);
      if (task.schedule.kind === 'interval') {
        // Missed occurrences coalesce into one immediate run: the scheduler never bursts through
        // a backlog it cannot prove the side effects of.
        if (next === undefined || next <= at) {
          task.next_run_at = iso(at);
          await saveTask(task);
        }
      } else if (next === undefined || next <= at) {
        const computed = nextCronTime(parseCron(task.schedule.expression), at);
        task.next_run_at = computed === undefined ? null : iso(computed);
        if (task.next_run_at === null) task.state = 'completed';
        await saveTask(task);
      }
    }
  }

  // ---- operations -------------------------------------------------------------

  async function mount(controller: NodeId, data: Record<string, Json>): Promise<Json> {
    const at = now();
    const schedule = parseSchedule(data.schedule);
    const action = parseAction(controller, data.action);
    const title = parseTitle(data.title);
    const id = `mnt-${at.toString(36)}-${crypto.randomUUID().slice(0, 6)}`;
    const task: MountTask = {
      id, controller, state: 'active',
      ...(title === undefined ? {} : { title }),
      schedule, action,
      // A fixed interval starts now: the first run fires on the next pass, then start-to-start.
      next_run_at: nextFor(schedule, at, schedule.kind === 'interval'),
      thread: { owner, id: `mnt-${id}` },
      created_at: iso(at), updated_at: iso(at),
      changes: [], last_run: null,
    };
    change(task, controller, 'mount', { schedule, definition: action, ...(title === undefined ? {} : { title }) });
    await options.transport.ensureThread(task.thread.id, `mount ${id}`);
    tasks.set(id, task);
    await saveTask(task);
    log('mount.created', { task: id, controller, schedule: describeSchedule(schedule) });
    return wire({ task });
  }

  async function update(controller: NodeId, data: Record<string, Json>): Promise<Json> {
    const task = requireTask(controller, parseTaskId(data));
    if (task.state === 'removed') throw new OnlyneError('invalid_request', 'A removed mount cannot be updated.');
    const title = parseTitle(data.title);
    const schedule = data.schedule === undefined ? undefined : parseSchedule(data.schedule);
    const action = data.action === undefined ? undefined : parseAction(controller, data.action);
    if (title === undefined && schedule === undefined && action === undefined) {
      throw new OnlyneError('invalid_request', 'An update names at least one of title, schedule or action.');
    }
    if (title !== undefined) task.title = title;
    if (action !== undefined) task.action = action;
    if (schedule !== undefined) {
      task.schedule = schedule;
      // A schedule change takes effect from now, never by replaying occurrences missed while the
      // old definition was in force. An interval waits one full period so an update is not
      // itself a trigger; an active run keeps the definition it started with.
      task.next_run_at = task.state === 'active' ? nextFor(schedule, now(), false) : null;
    }
    task.updated_at = iso(now());
    change(task, controller, 'update', {
      ...(title === undefined ? {} : { title }),
      ...(schedule === undefined ? {} : { schedule }),
      ...(action === undefined ? {} : { definition: action }),
    });
    await saveTask(task);
    log('mount.updated', { task: task.id, controller });
    return wire({ task });
  }

  function stopRunsOf(taskId: string): void {
    for (const [runId, entry] of active) {
      if (entry.task_id !== taskId) continue;
      stopChild(entry.child);
      log('mount.run.cancelled', { task: taskId, run: runId });
    }
  }

  async function cancel(controller: NodeId, data: Record<string, Json>): Promise<Json> {
    const task = requireTask(controller, parseTaskId(data));
    if (task.state === 'active') {
      task.state = 'cancelled';
      task.next_run_at = null;
      task.updated_at = iso(now());
      change(task, controller, 'cancel');
      await saveTask(task);
      stopRunsOf(task.id);
      log('mount.cancelled', { task: task.id, controller });
    }
    return wire({ task });
  }

  async function resume(controller: NodeId, data: Record<string, Json>): Promise<Json> {
    const task = requireTask(controller, parseTaskId(data));
    if (task.state === 'removed') throw new OnlyneError('invalid_request', 'A removed mount cannot be resumed.');
    if (task.state !== 'active') {
      task.state = 'active';
      task.next_run_at = nextFor(task.schedule, now(), task.schedule.kind !== 'cron');
      task.updated_at = iso(now());
      change(task, controller, 'resume');
      await saveTask(task);
      log('mount.resumed', { task: task.id, controller });
    }
    return wire({ task });
  }

  async function remove(controller: NodeId, data: Record<string, Json>): Promise<Json> {
    const task = requireTask(controller, parseTaskId(data));
    if (task.state !== 'removed') {
      task.state = 'removed';
      task.next_run_at = null;
      task.updated_at = iso(now());
      change(task, controller, 'remove');
      await saveTask(task);
      stopRunsOf(task.id);
      log('mount.removed', { task: task.id, controller });
    }
    return wire({ task });
  }

  async function list(controller: NodeId, data: Record<string, Json>): Promise<Json> {
    const includeRemoved = data.include_removed === true;
    const items = [...tasks.values()]
      .filter(task => task.controller === controller && (includeRemoved || task.state !== 'removed'))
      .sort((a, b) => (a.created_at < b.created_at ? 1 : -1))
      .map(task => structuredClone(task));
    return wire({ tasks: items });
  }

  async function inspect(controller: NodeId, data: Record<string, Json>): Promise<Json> {
    const task = requireTask(controller, parseTaskId(data));
    const runs = (await storage.transaction(tx => tx.scan<MountRun>(runPrefix(task.id))))
      .map(row => row.value)
      .sort((a, b) => (a.started_at < b.started_at ? 1 : -1))
      .slice(0, 10);
    return wire({ task, runs, audit: task.changes });
  }

  async function runs(controller: NodeId, data: Record<string, Json>): Promise<Json> {
    const task = requireTask(controller, parseTaskId(data));
    const limit = typeof data.limit === 'number' && Number.isInteger(data.limit) && data.limit > 0 && data.limit <= 100
      ? data.limit
      : 20;
    const items = (await storage.transaction(tx => tx.scan<MountRun>(runPrefix(task.id))))
      .map(row => row.value)
      .sort((a, b) => (a.started_at < b.started_at ? 1 : -1))
      .slice(0, limit);
    return wire({ task_id: task.id, runs: items });
  }

  const OPERATIONS: Record<string, (controller: NodeId, data: Record<string, Json>) => Promise<Json>> = {
    mount, update, list, inspect, runs, cancel, resume, remove,
  };

  // ---- driver -----------------------------------------------------------------

  const driver: ResourceDriver = {
    async validate(resource: Resource, operation: string, data: Json | undefined): Promise<void> {
      const controller = controllerOf(resource);
      if (!(operation in OPERATIONS)) {
        throw new OnlyneError('unsupported_delivery', `The scheduler does not declare the operation ${operation}.`);
      }
      const value = data === undefined ? {} : recordOf(data, 'The operation payload');
      if (operation === 'mount') {
        if (value.schedule === undefined) throw new OnlyneError('invalid_request', 'A mount names its schedule.');
        if (value.action === undefined) throw new OnlyneError('invalid_request', 'A mount names its action.');
        parseSchedule(value.schedule);
        parseAction(controller, value.action);
        parseTitle(value.title);
        return;
      }
      if (operation === 'update') {
        parseTaskId(value);
        if (value.title === undefined && value.schedule === undefined && value.action === undefined) {
          throw new OnlyneError('invalid_request', 'An update names at least one of title, schedule or action.');
        }
        if (value.schedule !== undefined) parseSchedule(value.schedule);
        if (value.action !== undefined) parseAction(controller, value.action);
        parseTitle(value.title);
        return;
      }
      if (operation === 'list') {
        if (value.include_removed !== undefined && typeof value.include_removed !== 'boolean') {
          throw new OnlyneError('invalid_request', 'include_removed is a boolean.');
        }
        return;
      }
      parseTaskId(value);
      if (operation === 'runs' && value.limit !== undefined) {
        const limit = value.limit;
        if (typeof limit !== 'number' || !Number.isInteger(limit) || limit < 1 || limit > 100) {
          throw new OnlyneError('invalid_request', 'limit is an integer from 1 to 100.');
        }
      }
    },

    async execute(resource: Resource, request: OperationRequest): Promise<Omit<OperationResult, 'operation_id'>> {
      const controller = controllerOf(resource);
      const run = OPERATIONS[request.operation];
      if (run === undefined) throw new OnlyneError('unsupported_delivery', `The scheduler does not declare the operation ${request.operation}.`);
      const data = request.data === undefined ? {} : recordOf(request.data, 'The operation payload');
      return { state: 'committed', data: await run(controller, data) };
    },
  };

  const service: MountService = {
    driver,

    async start(): Promise<void> {
      if (config === undefined || config.controllers.length === 0) return;
      await loadStore();
      timer = setInterval(() => { void service.tick(); }, tickMs);
      log('mounts.started', { controllers: config.controllers, tick_ms: tickMs });
    },

    async stop(): Promise<void> {
      if (stopping) return;
      stopping = true;
      clearInterval(timer);
      timer = undefined;
      const pending = [...active.values()];
      for (const entry of pending) stopChild(entry.child);
      // The child may outlive SIGTERM; the escalation timer is unref'd, so shutdown itself does
      // not wait for it. Give the normal exit path a moment, then repair whatever is left.
      const bounded = Promise.withResolvers<void>();
      const deadline = setTimeout(bounded.resolve, 2_000);
      deadline.unref?.();
      await Promise.race([Promise.allSettled(pending.map(entry => entry.done)), bounded.promise]);
      clearTimeout(deadline);
      for (const [runId, entry] of active) {
        const run = await loadRun(entry.task_id, runId);
        if (run !== undefined && run.state === 'running') {
          run.state = 'interrupted';
          run.finished_at = iso(now());
          run.error = { code: 'interrupted', message: 'The gateway stopped before this run finished; the external effect is unconfirmed.' };
          if (run.push !== undefined && run.push.state !== 'accepted') {
            run.push.state = 'pending';
            run.push.attempts = 0;
            const queued: PushEntry = {
              task_id: run.task_id, run_id: run.id, state: 'pending', attempts: 0, next_attempt_at: run.finished_at,
            };
            push.set(pushKey(run.task_id, run.id), queued);
            await savePush(queued);
          }
          const task = tasks.get(run.task_id);
          if (task !== undefined) {
            task.last_run = runSummary(run);
            task.updated_at = run.finished_at;
            if (task.state === 'active' && task.schedule.kind === 'once') task.state = 'completed';
            await saveTask(task);
          }
          await saveRun(run);
        }
        active.delete(runId);
      }
      log('mounts.stopped', {});
    },

    async tick(): Promise<void> {
      if (stopping || ticking || config === undefined) return;
      ticking = true;
      try {
        schedulePushes();
        await startDue(now());
      } catch (error) {
        log('mounts.tick.failed', { error: String(error) });
      } finally {
        ticking = false;
      }
    },

    async status(): Promise<string[]> {
      const lines: string[] = [];
      const names = [...options.connections.entries()].map(([name, connection]) => `${name}(${connection.tools.length})`);
      lines.push(`servers: ${names.length === 0 ? 'none' : names.join(' ')}`);
      const activeTasks = [...tasks.values()].filter(task => task.state === 'active');
      const running = activeTasks.filter(task => [...active.values()].some(entry => entry.task_id === task.id));
      lines.push(`mounts: ${activeTasks.length} active, ${running.length} running`);
      const queued = [...push.values()];
      const pending = queued.filter(entry => entry.state === 'pending').length;
      const unknown = queued.filter(entry => entry.state === 'unknown').length;
      lines.push(`push: ${pending} pending, ${unknown} unknown, ${settled.refused} refused`);
      const last = recent[0];
      lines.push(last === undefined
        ? 'last: none'
        : `last: ${last.task_id} ${last.state} exit=${last.exit_code ?? '-'} ${timeOf(last.finished_at ?? last.started_at)}`);
      // The four global lines come first so a 24-row screen shows categories before detail.
      const shown = activeTasks.slice(0, activeTasks.length > 4 ? 3 : 4);
      for (const task of shown) {
        const runningNow = running.includes(task) ? ' running' : '';
        lines.push(`  ${task.id} next=${timeOf(task.next_run_at)} ${describeSchedule(task.schedule)}${runningNow}`);
      }
      if (activeTasks.length > shown.length) lines.push(`  +${activeTasks.length - shown.length} more`);
      return lines;
    },

    resources(): Array<{ controller: NodeId; resource: Resource }> {
      if (config === undefined) return [];
      return config.controllers.map(controller => ({
        controller,
        resource: {
          resource_ref: { owner, id: mountsResourceId(controller) },
          kind: 'mounts',
          operations: [...MOUNT_OPERATIONS],
          binding: { handler: controller, cursor: '0' },
          log_readers: [...new Set([owner, controller])],
          revision: 'rev-1',
          event_seq: '0',
          retention_ms: 0,
          data: { kind: 'mounts', controller },
        },
      }));
    },
  };
  return service;
}
