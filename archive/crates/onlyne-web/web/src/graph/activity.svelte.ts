import type { DeliveryView } from '../gen/View';
import { principalRole, workOf, type Tone } from '../lib/model';
import type { Payload } from '../lib/net/api';
import { cluster } from '../lib/state/cluster.svelte';

const PULSE_TTL = 2500;

export interface ActivityPulse {
  id: string;
  from: string;
  to: string;
  tone: Tone;
  at: number;
}

function changed(previous: DeliveryView | undefined, next: DeliveryView): boolean {
  if (!previous) return true;
  return (
    previous.acked_at !== next.acked_at ||
    previous.attempt !== next.attempt ||
    previous.enqueued_at !== next.enqueued_at ||
    previous.family !== next.family ||
    previous.hop !== next.hop ||
    previous.kind !== next.kind ||
    previous.op_id !== next.op_id ||
    previous.origin !== next.origin ||
    previous.out_head !== next.out_head ||
    previous.outcome !== next.outcome ||
    previous.reason !== next.reason ||
    previous.state !== next.state ||
    previous.task_id !== next.task_id
  );
}

function deliveriesOf(payload: Payload): Record<string, DeliveryView> {
  return payload.view.deliveries ?? {};
}

class ActivityStore {
  pulses = $state.raw<ActivityPulse[]>([]);
  byRoute = $derived.by<Record<string, ActivityPulse[]>>(() => {
    const grouped: Record<string, ActivityPulse[]> = {};
    for (const pulse of this.pulses) {
      const route = `${pulse.from}->${pulse.to}`;
      (grouped[route] ??= []).push(pulse);
    }
    return grouped;
  });

  private pruneTimer: ReturnType<typeof setTimeout> | null = null;

  constructor() {
    cluster.onFrame((previous, next) => this.adopt(previous, next));
  }

  pulseOf(route: string): ActivityPulse[] {
    return this.byRoute[route] ?? [];
  }

  private adopt(previous: Payload, next: Payload): void {
    const before = deliveriesOf(previous);
    const now = Date.now();
    const fresh: ActivityPulse[] = [];
    for (const delivery of Object.values(deliveriesOf(next))) {
      if (!changed(before[delivery.msg_id], delivery)) continue;
      const from = principalRole(delivery.from);
      const to = principalRole(delivery.to);
      if (!from || !to || from === to) continue;
      fresh.push({ id: delivery.msg_id, from, to, tone: workOf(delivery).tone, at: now });
    }
    if (fresh.length === 0) return;
    this.pulses = [...this.pulses, ...fresh].filter((pulse) => now - pulse.at <= PULSE_TTL);
    this.schedulePrune();
  }

  private schedulePrune(): void {
    if (this.pruneTimer !== null) return;
    this.pruneTimer = setTimeout(() => {
      this.pruneTimer = null;
      const now = Date.now();
      this.pulses = this.pulses.filter((pulse) => now - pulse.at <= PULSE_TTL);
      if (this.pulses.length > 0) this.schedulePrune();
    }, PULSE_TTL + 20);
  }
}

export const activity = new ActivityStore();
