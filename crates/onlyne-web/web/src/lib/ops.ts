// Every mutation this surface can ask for. The browser sends ops and renders
// what comes back; nothing here decides policy, and nothing here folds state.
//
// Two rules run through the file. A failure is always told to the operator,
// because a button that did nothing and said nothing is worse than one that
// refused. And every spec edit is built from the read it is applied against:
// `set_targets` replaces a list wholesale, so an edit computed from a stale
// read would silently drop whatever else moved in the meantime.

import { short } from './format';
import type { Outcome } from '../gen/View';
import type { SpecEdit, WebOp } from '../gen/WebOp';
import { Refused, Unauthorized, postOp } from './net/api';
import { cluster } from './state/cluster.svelte';
import { spec, type SpecClient } from './state/spec.svelte';
import { ui } from './state/ui.svelte';

/// One refusal or failure as the operator reads it. The code travels with a
/// refusal because it is the word the protocol contract names.
export function messageOf(error: unknown): string {
  if (error instanceof Refused) return error.code === 'transport' ? error.message : `${error.message} (${error.code})`;
  if (error instanceof Unauthorized) return error.message;
  if (error instanceof Error) return error.message;
  return String(error);
}

/// Run one op. Nothing is announced on success: what an op is worth saying
/// about is the caller's business.
async function called(what: string, op: WebOp): Promise<boolean> {
  try {
    await postOp(cluster.token, op);
    return true;
  } catch (error) {
    ui.toast({ tone: 'fail', text: `${what}: ${messageOf(error)}` });
    return false;
  }
}

/// Write a task to a role. It goes as the operator, so its receipt lands on
/// the operator's own board.
export async function sendTask(to: string, text: string): Promise<boolean> {
  const ok = await called(`send to ${to}`, { op: 'send', args: { to, text } });
  if (ok) ui.toast({ tone: 'plain', text: `sent to ${to}` });
  return ok;
}

/// Point a role's session at a task it did not pull.
export async function focusSession(to: string, taskId: string): Promise<boolean> {
  const ok = await called('point a session at it', { op: 'focus', args: { to, task_id: taskId } });
  if (ok) ui.toast({ text: `a session of ${to} was pointed at task ${short(taskId, 8)}` });
  return ok;
}

/// File a conclusion on a task as the operator.
export async function reportTask(taskId: string, outcome: Outcome, head: string): Promise<boolean> {
  const ok = await called('file the report', { op: 'report', args: { task_id: taskId, outcome, head } });
  if (ok) ui.toast({ text: `filed ${outcome} on task ${short(taskId, 8)}` });
  return ok;
}

export type RepairKind = 'retry' | 'fail' | 'close';

/// The three repair verbs that move a task. Which one is offered is the
/// inspector's decision; what it does is the same admin op either way.
export async function repair(kind: RepairKind, taskId: string, reason: string): Promise<boolean> {
  const op: WebOp =
    kind === 'retry'
      ? { op: 'repair_retry', args: { task_id: taskId, reason } }
      : kind === 'fail'
        ? { op: 'repair_fail', args: { task_id: taskId, reason } }
        : { op: 'repair_close', args: { task_id: taskId, reason } };
  const ok = await called(`repair ${kind}`, op);
  if (ok) ui.toast({ text: `${kind} was filed for task ${short(taskId, 8)}` });
  return ok;
}

/// Read a session's reducer state without touching it. The answer is the
/// server's own JSON, shown as it came.
export async function inspectTask(taskId: string): Promise<unknown | null> {
  try {
    return await postOp(cluster.token, { op: 'repair_inspect', args: { task_id: taskId } });
  } catch (error) {
    ui.toast({ tone: 'fail', text: `inspect: ${messageOf(error)}` });
    return null;
  }
}

/// Mark a fault handled.
export async function ackFault(faultId: number, reason: string): Promise<boolean> {
  const ok = await called('acknowledge the fault', { op: 'repair_ack', args: { fault_id: faultId, reason } });
  if (ok) ui.toast({ text: `fault ${faultId} acknowledged` });
  return ok;
}

/// Build the edits for one change, from the spec as it is right now. Returning
/// a string instead means there was nothing to do, and the string says why.
export type SpecBuilder = (clients: SpecClient[]) => SpecEdit[] | string;

/// Apply typed edits to `spec.toml`.
///
/// The read and the edit are one step: the edits are built from the read whose
/// hash they name, and when the file moved under that hash the whole thing is
/// done again once. The operator's comments survive because the server applies
/// the edits with `toml_edit`; a reload follows, and the stream says so.
export async function editSpec(label: string, build: SpecBuilder): Promise<boolean> {
  for (let attempt = 0; attempt < 2; attempt += 1) {
    const hash = cluster.view.cluster?.spec_hash ?? '';
    if (!(await spec.refresh(cluster.token, hash))) {
      ui.toast({ tone: 'fail', text: `${label}: ${spec.error}` });
      return false;
    }
    const edits = build(spec.clients);
    if (typeof edits === 'string') {
      ui.toast({ text: edits });
      return false;
    }
    try {
      await postOp(cluster.token, { op: 'spec_apply', args: { base_hash: spec.sourceHash, edits } });
      // The reload is on its way. The held read no longer describes the file,
      // so the next hash the cluster publishes re-reads it.
      spec.forget();
      return true;
    } catch (error) {
      if (error instanceof Refused && error.code === 'conflict' && attempt === 0) continue;
      ui.toast({ tone: 'fail', text: `${label}: ${messageOf(error)}` });
      return false;
    }
  }
  return false;
}

/// Declare one allowed route: what a line dragged from one board to another
/// means.
export async function addRoute(role: string, target: string): Promise<boolean> {
  if (role === target) return false;
  const ok = await editSpec(`allow ${role} to reach ${target}`, (clients) => {
    const targets = clients.find((client) => client.role === role)?.allowedTargets ?? [];
    if (targets.includes(target)) return `${role} already reaches ${target}`;
    return [{ edit: 'set_targets', args: { role, targets: [...targets, target] } }];
  });
  if (ok) ui.toast({ tone: 'done', text: `${role} now reaches ${target}` });
  return ok;
}

/// Withdraw one declared route. Withdrawing is the one gesture on the canvas
/// that takes something away, so it comes back with an undo: a right-click on
/// a busy graph is easy to misplace.
export async function removeRoute(role: string, target: string): Promise<boolean> {
  const ok = await editSpec(`withdraw ${role} to ${target}`, (clients) => {
    const targets = clients.find((client) => client.role === role)?.allowedTargets ?? [];
    if (!targets.includes(target)) return `${role} does not reach ${target}`;
    return [{ edit: 'set_targets', args: { role, targets: targets.filter((candidate) => candidate !== target) } }];
  });
  if (ok) {
    ui.toast({
      text: `withdrew ${role} to ${target}`,
      action: { label: 'undo', run: () => void addRoute(role, target) },
    });
  }
  return ok;
}
