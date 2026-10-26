import type { Connector, ImEvent, SendOutcome } from '@onlyne/im-core';

/**
 * A scripted fake connector for tests and acceptance fixtures. It replaces the v2 fake
 * backend's four environment variables (archive/crates/onlyne-im-backend-fake/src/main.rs)
 * with an injected script object: events to replay inbound, answers to dequeue on send.
 */
export interface FakeScript {
  /** Inbound events, delivered to listeners in order once `start` is called. */
  events: ImEvent[];
  /** Outbound answers, dequeued one per `send`. An empty queue throws. */
  answers: SendOutcome[];
}

/**
 * Inbound events that carry no platform handle get a synthetic `om_fake_N` one, because the
 * driver bridge requires a reply target on every message event.
 */
export function createFakeConnector(script: FakeScript): Connector {
  const listeners: Array<(event: ImEvent) => void> = [];
  let handleCounter = 0;

  return {
    // The bridge routes by platform, so the script's own events name it; a script with no
    // events serves no platform and can only be sent to.
    platform: script.events[0]?.platform ?? 'fake',
    async start(): Promise<void> {
      for (const event of script.events) {
        const delivered: ImEvent = event.message_handle === undefined
          ? { ...event, message_handle: `om_fake_${++handleCounter}` }
          : event;
        // Microtasks keep the replay asynchronous while preserving order.
        void Promise.resolve().then(() => {
          for (const listener of listeners) listener(delivered);
        });
      }
    },
    onEvent(listener: (event: ImEvent) => void): void {
      listeners.push(listener);
    },
    async send(): Promise<SendOutcome> {
      const answer = script.answers.shift();
      if (answer === undefined) {
        throw new Error('The fake script has no answer for this send.');
      }
      return answer;
    },
    async stop(): Promise<void> {
      listeners.length = 0;
    },
  };
}
