import type { NodeId } from '@onlyne/lib';
import type { Connector } from '@onlyne/im-core';

/**
 * The optional platform-status surface. It is deliberately not part of `Connector`: a connector
 * that keeps no counters stays valid, and the gateway duck-types the method (RFC 0012 §4 keeps
 * the connector a translation layer; counters are host-side observations).
 */
export interface ConnectorStatus {
  /** `onboarding`, `connecting`, `connected`, `polling`, `paused`, `error`, `stopped`. */
  state: string;
  connected: boolean;
  /** Platform connection attempts after the first one. */
  reconnects: number;
  /** Platform rate-limit answers observed (HTTP 429 and equivalents). */
  rate_limits: number;
  /** The last inbound platform event's platform timestamp. */
  last_event_at?: string;
  /** Exact fields the operator still has to provide; empty once the channel can run. */
  onboarding?: string[];
  /** The last platform or lifecycle failure, verbatim. */
  error?: string;
}

export interface StatusConnector extends Connector {
  status?(): ConnectorStatus;
}

/** The gateway's own observation of the channel, merged with the connector's. */
export interface ChannelStatus extends ConnectorStatus {
  node: NodeId;
  platform: string;
  events: number;
  /** Platform calls that carried content: `send` plus declared operations. */
  sends: number;
  send_failures: number;
  last_send_at?: string;
  uptime_ms: number;
}

export interface StatusCounters {
  events: number;
  sends: number;
  send_failures: number;
  last_event_at?: string;
  last_send_at?: string;
}

const empty: ConnectorStatus = { state: 'unknown', connected: false, reconnects: 0, rate_limits: 0 };

const text = (value: unknown): string | undefined =>
  typeof value === 'string' && value.length > 0 ? value : undefined;

/** A connector's answer is display text, so a malformed field is dropped instead of rendered. */
function normalize(raw: unknown): ConnectorStatus {
  if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) return empty;
  const value = raw as Record<string, unknown>;
  const onboarding = Array.isArray(value.onboarding)
    ? value.onboarding.filter((entry): entry is string => typeof entry === 'string' && entry.length > 0)
    : undefined;
  return {
    state: text(value.state) ?? 'unknown',
    connected: value.connected === true,
    reconnects: typeof value.reconnects === 'number' && value.reconnects >= 0 ? Math.floor(value.reconnects) : 0,
    rate_limits: typeof value.rate_limits === 'number' && value.rate_limits >= 0 ? Math.floor(value.rate_limits) : 0,
    ...(text(value.last_event_at) === undefined ? {} : { last_event_at: text(value.last_event_at)! }),
    ...(onboarding === undefined || onboarding.length === 0 ? {} : { onboarding }),
    ...(text(value.error) === undefined ? {} : { error: text(value.error)! }),
  };
}

export function connectorStatus(connector: Connector): ConnectorStatus {
  const probe = (connector as Partial<StatusConnector>).status;
  if (typeof probe !== 'function') return { ...empty };
  try {
    return normalize(probe.call(connector));
  } catch (error) {
    return { ...empty, state: 'error', error: `status() failed: ${String(error)}` };
  }
}

/**
 * The host's status panel: four lines at most, so a 24-row terminal keeps the trust view.
 * Order is state, counters, onboarding, error; the first two always render.
 */
export function statusLines(status: ChannelStatus): string[] {
  const head = `im ${status.platform} · ${status.node} · ${status.state}`;
  const counters = [
    `events ${status.events}`,
    `sends ${status.sends}`,
    `reconnects ${status.reconnects}`,
    `rate-limited ${status.rate_limits}`,
    status.last_event_at === undefined ? 'last event none' : `last event ${status.last_event_at}`,
  ].join(' · ');
  const lines = [head, counters];
  if (status.onboarding !== undefined && status.onboarding.length > 0) {
    lines.push(`onboarding: ${status.onboarding.join(', ')}`);
  }
  if (status.error !== undefined) lines.push(`error: ${status.error}`);
  return lines;
}
