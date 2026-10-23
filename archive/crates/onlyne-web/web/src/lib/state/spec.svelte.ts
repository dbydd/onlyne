// The spec as the browser is allowed to see it: the parsed `spec.toml` plus
// the source hash every edit has to name (`AdminOp::SpecGet`).
//
// The spec is read lazily — one admin round trip per read — and held until the
// cluster says its spec moved, which `spec_reloaded` does on the stream. Two
// hashes matter and they are not the same one: `source_hash` is the file's own
// bytes and is what an edit must name, while the cluster's `spec_hash` is the
// canonical hash the reload published, and is what this cache is keyed to.
//
// `key` is deliberately not part of the shape this surface keeps: it is the
// role's credential, no view renders it, and a field that is never read is a
// field that can never leak into a screenshot.

import type { Drive } from '../../gen/WebOp';
import { Refused, postOp } from '../net/api';

/// One `[[client]]` entry, narrowed to what this surface reads and writes.
export interface SpecClient {
  role: string;
  prose: string;
  admin: boolean;
  maxSessions: number;
  allowedSenders: string[];
  allowedTargets: string[];
  drive: Drive;
  command: string[];
  readyMs: number;
  idleMs: number;
  attempts: number;
  backoffMs: number[];
}

/// The `spec_get` answer, before narrowing.
interface SpecAnswer {
  path?: unknown;
  source_hash?: unknown;
  spec?: unknown;
}

function record(value: unknown): Record<string, unknown> {
  return value !== null && typeof value === 'object' ? (value as Record<string, unknown>) : {};
}

function num(value: unknown, fallback: number): number {
  return typeof value === 'number' && Number.isFinite(value) ? value : fallback;
}

function nums(value: unknown): number[] {
  return Array.isArray(value) ? value.filter((item): item is number => typeof item === 'number') : [];
}

function strings(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === 'string') : [];
}

function bool(value: unknown, fallback: boolean): boolean {
  return typeof value === 'boolean' ? value : fallback;
}

const DRIVES: readonly string[] = ['plugin', 'acp', 'exec'];

/// One entry of the parsed spec, flattened into the fields the editor edits.
function clientOf(entry: Record<string, unknown>): SpecClient {
  const runtime = record(entry.runtime);
  const timeout = record(entry.timeout);
  const intent = record(entry.intent);
  const drive = typeof runtime.drive === 'string' && DRIVES.includes(runtime.drive) ? runtime.drive : 'plugin';
  return {
    role: typeof entry.role === 'string' ? entry.role : '',
    prose: typeof entry.prose === 'string' ? entry.prose : '',
    admin: bool(entry.admin, false),
    maxSessions: num(entry.max_sessions, 1),
    allowedSenders: strings(entry.allowed_senders),
    allowedTargets: strings(entry.allowed_targets),
    drive: drive as Drive,
    command: strings(runtime.command),
    readyMs: num(timeout.ready_ms, 0),
    idleMs: num(timeout.idle_ms, 0),
    attempts: num(intent.attempts, 0),
    backoffMs: nums(intent.backoff_ms),
  };
}

class SpecStore {
  clients = $state.raw<SpecClient[]>([]);
  path = $state('');
  /// The file's own bytes, re-read with the spec and named by every edit.
  sourceHash = $state('');
  error = $state('');
  /// The cluster's canonical spec hash this held read describes.
  private forHash = '';

  byRole = $derived(new Map(this.clients.map((client) => [client.role, client])));

  clientOf = (role: string): SpecClient | undefined => this.byRole.get(role);

  /// Whether the held read still describes the spec the cluster is running.
  current = (specHash: string | null | undefined): boolean => this.forHash !== '' && this.forHash === (specHash ?? '');

  /// The held read no longer describes the file — an edit just landed and the
  /// reload is on its way. `current` says so until the next read.
  forget = () => {
    this.forHash = '';
  };

  /// Read the spec once, keyed by the cluster hash it was read for.
  refresh = async (token: string, specHash: string | null | undefined): Promise<boolean> => {
    try {
      const answer = (await postOp(token, { op: 'spec_get' })) as SpecAnswer;
      const tree = record(answer.spec);
      const entries = Array.isArray(tree.client) ? tree.client : [];
      this.clients = entries.map((entry) => clientOf(record(entry)));
      this.path = typeof answer.path === 'string' ? answer.path : '';
      this.sourceHash = typeof answer.source_hash === 'string' ? answer.source_hash : '';
      this.forHash = specHash ?? this.sourceHash;
      this.error = '';
      return true;
    } catch (error) {
      this.error = error instanceof Refused || error instanceof Error ? error.message : String(error);
      return false;
    }
  };
}

export const spec = new SpecStore();
