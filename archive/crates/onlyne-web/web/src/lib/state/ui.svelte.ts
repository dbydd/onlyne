// The surface's own state: what is selected, which margin is open, and what it
// said last. None of it belongs to the cluster, and none of it is sent
// anywhere — an op is built at the moment an operator asks for one.

import type { Route, Tone } from '../model';
import { load, save } from '../persist';
import { cluster } from './cluster.svelte';

/// What the inspector is showing. One surface, five shapes: a board, one
/// delivery, one fault, one declared route, or the form that declares a role.
export type Selection =
  | { kind: 'role'; role: string }
  | { kind: 'task'; msgId: string; role: string }
  | { kind: 'fault'; id: number }
  | { kind: 'route'; source: string; target: string }
  | { kind: 'declare' };

export type DockTab = 'ledger' | 'sessions' | 'events' | 'faults';
/// What the ledger shows: what is still owed, everything this link holds, or
/// the operator's own receipts.
export type LedgerScope = 'live' | 'all' | 'inbox';

export interface Toast {
  id: number;
  tone: Tone;
  text: string;
  action?: { label: string; run: () => void };
  at: number;
}

export interface ToastInput {
  tone?: Tone;
  text: string;
  action?: { label: string; run: () => void };
  /// How long it stays, in milliseconds. An action holds longer by default.
  ttl?: number;
}

const DOCK_KEY = 'onlyne.dock';

interface DockMemory {
  tab?: DockTab;
  open?: boolean;
  height?: number;
}

const remembered = load<DockMemory>(DOCK_KEY, {});

class UiStore {
  selection = $state<Selection | null>(null);
  dockTab = $state<DockTab>(remembered.tab ?? 'ledger');
  dockOpen = $state(remembered.open ?? true);
  dockHeight = $state(remembered.height ?? 216);
  ledgerScope = $state<LedgerScope>('live');
  query = $state('');
  paletteOpen = $state(false);
  toasts = $state.raw<Toast[]>([]);
  /// Bumped when the canvas is asked to fit what it just revealed.
  revealSeq = $state(0);
  revealRoles = $state.raw<string[]>([]);
  /// Bumped when the canvas is asked to forget the places it remembers.
  layoutSeq = $state(0);

  private toastSeq = 0;
  private timers = new Map<number, number>();

  /// The family trace behind the current selection, when it is a task.
  trace = $derived(this.selection?.kind === 'task' ? cluster.traceFor(this.selection.msgId) : null);

  /// The roles worth looking at right now: the canvas dims every board that is
  /// not in here. Null means nothing is selected and nothing is dimmed.
  focus = $derived.by<Set<string> | null>(() => {
    const selection = this.selection;
    if (!selection || selection.kind === 'declare') return null;
    if (selection.kind === 'role') {
      const roles = new Set([selection.role]);
      for (const route of cluster.routes) {
        if (route.source === selection.role) roles.add(route.target);
        if (route.target === selection.role) roles.add(route.source);
      }
      return roles;
    }
    if (selection.kind === 'task') {
      const trace = this.trace;
      return trace ? new Set(trace.roles) : null;
    }
    if (selection.kind === 'route') return new Set([selection.source, selection.target]);
    const fault = cluster.faults.find((candidate) => candidate.id === selection.id);
    return fault?.role ? new Set([fault.role]) : null;
  });

  /// Select something. `reveal` also asks the canvas to bring it into view.
  select = (selection: Selection, reveal = false) => {
    this.selection = this.selectionKey(this.selection) === this.selectionKey(selection) ? null : selection;
    this.query = '';
    if (reveal || this.selection) this.revealRoles = this.rolesToReveal();
    if (reveal) this.revealSeq += 1;
  };

  clear = () => {
    this.selection = null;
  };

  setDock = (patch: Partial<{ tab: DockTab; open: boolean; height: number }>) => {
    if (patch.tab !== undefined) this.dockTab = patch.tab;
    if (patch.open !== undefined) this.dockOpen = patch.open;
    if (patch.height !== undefined) this.dockHeight = Math.min(560, Math.max(120, patch.height));
    save(DOCK_KEY, { tab: this.dockTab, open: this.dockOpen, height: this.dockHeight });
  };

  /// Ask the canvas to forget the places it remembers and lay out again.
  resetLayout = () => {
    this.layoutSeq += 1;
  };

  toast = (input: ToastInput) => {
    const id = ++this.toastSeq;
    const ttl = input.ttl ?? (input.action ? 8000 : input.tone === 'fail' ? 8000 : 3200);
    this.toasts = [...this.toasts, { id, tone: input.tone ?? 'plain', text: input.text, action: input.action, at: Date.now() }].slice(-4);
    this.timers.set(
      id,
      window.setTimeout(() => {
        this.timers.delete(id);
        this.dismiss(id);
      }, ttl),
    );
  };

  dismiss = (id: number) => {
    const timer = this.timers.get(id);
    if (timer !== undefined) {
      window.clearTimeout(timer);
      this.timers.delete(id);
    }
    this.toasts = this.toasts.filter((toast) => toast.id !== id);
  };

  /// A selection is worth dimming down to: a role and what it talks to, the
  /// roles a trace crosses, both ends of a route, or a fault's board.
  private rolesToReveal(): string[] {
    const selection = this.selection;
    if (!selection || selection.kind === 'declare') return [];
    if (selection.kind === 'role') return [selection.role];
    if (selection.kind === 'route') return [selection.source, selection.target];
    if (selection.kind === 'task') {
      const trace = this.trace;
      return trace ? trace.roles.filter((role) => cluster.canvasRoles.has(role)) : [selection.role];
    }
    const fault = cluster.faults.find((candidate) => candidate.id === selection.id);
    return fault?.role ? [fault.role] : [];
  }

  /// The identity of a selection, so asking for the thing already selected
  /// closes it and asking for a different thing switches to it.
  private selectionKey(selection: Selection | null): string {
    if (!selection) return '';
    if (selection.kind === 'role') return `role:${selection.role}`;
    if (selection.kind === 'task') return `task:${selection.msgId}`;
    if (selection.kind === 'fault') return `fault:${selection.id}`;
    if (selection.kind === 'route') return `route:${selection.source}->${selection.target}`;
    return 'declare';
  }
}

/// The routes a role may address, for the inspector's route editor.
export function routeFrom(role: string, target: string): Route {
  return { id: `${role}->${target}`, source: role, target };
}

export const ui = new UiStore();
