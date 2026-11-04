import { DEFAULT_MAX_AUTO_TURNS, type Conversation } from './contracts.ts';

export type AutoTurnDecision =
  | { kind: 'advance'; botId: string; turn: number; max: number }
  | { kind: 'limit'; turn: number; max: number }
  | { kind: 'idle' };

/**
 * Decides which group member speaks after a completed reply. The counter is
 * process state, not conversation state: a restart must not resume an
 * unattended chain, and every user message resets it explicitly.
 */
export class GroupTurnScheduler {
  private readonly turns = new Map<string, number>();
  private readonly paused = new Set<string>();
  private readonly active = new Set<string>();

  reset(conversationId: string): void {
    this.turns.delete(conversationId);
    this.paused.delete(conversationId);
  }

  pause(conversationId: string): void {
    this.paused.add(conversationId);
    this.turns.delete(conversationId);
  }

  isPaused(conversationId: string): boolean {
    return this.paused.has(conversationId);
  }

  isActive(conversationId: string): boolean {
    return this.active.has(conversationId);
  }

  decide(conversation: Conversation, fromBotId: string): AutoTurnDecision {
    if (conversation.kind !== 'group') return { kind: 'idle' };
    const group = conversation.scene?.group;
    if (group?.auto !== true || group.mode === 'manual') return { kind: 'idle' };
    if (this.paused.has(conversation.id)) return { kind: 'idle' };
    const order = (group.order?.length ? group.order : conversation.botIds)
      .filter(id => conversation.botIds.includes(id));
    if (order.length === 0) return { kind: 'idle' };
    const max = normalizeLimit(group.maxAutoTurns);
    const turn = this.turns.get(conversation.id) ?? 0;
    if (turn >= max) return { kind: 'limit', turn, max };
    const position = order.indexOf(fromBotId);
    const botId = order[(position + 1) % order.length] ?? conversation.adminBotId;
    return { kind: 'advance', botId, turn: turn + 1, max };
  }

  begin(conversationId: string, turn: number): boolean {
    if (this.active.has(conversationId)) return false;
    this.active.add(conversationId);
    this.turns.set(conversationId, turn);
    return true;
  }

  end(conversationId: string): void {
    this.active.delete(conversationId);
  }

  counts(): Array<{ conversationId: string; turns: number }> {
    return [...this.turns.entries()].map(([conversationId, turns]) => ({ conversationId, turns }));
  }
}

function normalizeLimit(value: number | undefined): number {
  return typeof value === 'number' && Number.isInteger(value) && value >= 1 ? value : DEFAULT_MAX_AUTO_TURNS;
}
