// One clock for every relative time on the screen. A module-level tick is
// deliberate: the alternative is a timer per row, and a cluster with a hundred
// rows would then re-render a hundred times a second to change nothing.

import type { Tone } from '../model';

/// How often the screen's relative times are allowed to move. Five seconds is
/// the display's own granularity: nothing on this screen resolves finer.
const TICK_MS = 5000;

export const tick = $state({ now: Date.now() });

if (typeof window !== 'undefined') {
  window.setInterval(() => {
    tick.now = Date.now();
  }, TICK_MS);
}

export type Transport = 'connecting' | 'live' | 'reconnecting' | 'refused';

export interface StatusReading {
  tone: Tone;
  label: string;
}

/// The one word the header shows about the link. Four states, and a refused
/// token is not one of the retrying ones: it needs the operator. A stream that
/// is up while the server link behind it is down reads as catching up, because
/// what this tab holds is real and incomplete.
export function statusOf(transport: Transport, stale: boolean): StatusReading {
  if (transport === 'refused') return { tone: 'fail', label: 'token refused' };
  if (transport !== 'live') return { tone: 'queue', label: transport === 'reconnecting' ? 'reconnecting' : 'connecting' };
  if (stale) return { tone: 'wait', label: 'catching up' };
  return { tone: 'done', label: 'live' };
}
