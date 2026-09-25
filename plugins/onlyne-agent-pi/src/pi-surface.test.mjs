// The pi-facing half of the surface: exactly what `wakeUser` hands
// `pi.sendUserMessage`. Every shape asserted here is pi's own declaration —
// `sendUserMessage(content: string | (TextContent | ImageContent)[])`, with
// `ImageContent` the flat `{ type: "image", data, mimeType }` of
// `@earendil-works/pi-ai` (measured against pi 0.87.1). pi normalizes each part
// before it builds the message, so a part in any other shape — the nested
// Anthropic `source: { type, media_type, data }` this plugin once sent — stops
// the whole delivery inside pi, and pi reports that in its own pane without
// handing anything back to the plugin.

import assert from "node:assert/strict";
import { test } from "node:test";

import { createSurface } from "./pi-surface.mjs";

/** The injection text `agent.mjs` builds for one assignment. */
const TASK_TEXT = "[onlyne] task 11111111-1111-4111-8111-111111111111 from role:planner";

/** One inline image part in the shape the agent hands the surface. */
const PNG_PART = { type: "image", mime: "image/png", data: "iVBORw0KGgoAAAANSUhEUg==", name: "shot.png" };

/** pi's own `UserMessage.content` for that text and that part, key for key. */
const PNG_CONTENT = [
  { type: "text", text: TASK_TEXT },
  { type: "image", data: "iVBORw0KGgoAAAANSUhEUg==", mimeType: "image/png" },
];

/**
 * A surface over a hand-written fake pi: every `sendUserMessage` arrives with
 * the arguments it was given, recorded whole, and every surface log line with
 * it. `refuse` makes one call throw synchronously, the way a pi that does not
 * know an option does.
 * @param {{ refuse?: (content: unknown, options: unknown) => boolean }} [options]
 */
function fakeSurface({ refuse = null } = {}) {
  const calls = [];
  const lines = [];
  const surface = createSurface({
    pi: {
      sendUserMessage(content, options) {
        calls.push({ content, options });
        if (refuse?.(content, options)) throw new Error("this pi does not take that option");
      },
    },
    log: (line) => lines.push(line),
    context: () => null,
  });
  return { calls, lines, surface };
}

test("an image attachment reaches pi as one flat image content part beside the task text", () => {
  const { calls, lines, surface } = fakeSurface();

  assert.equal(surface.wakeUser(TASK_TEXT, [PNG_PART]), true);
  assert.equal(calls.length, 1, "one message, not one call per part");
  // The whole array, key for key: a nested `source` / `mediaType` part carries
  // keys pi does not declare and lacks the two it does, so it cannot pass this.
  assert.deepEqual(calls[0].content, PNG_CONTENT);
  assert.deepEqual(lines, [], "a readable attachment is delivered, not reported");
});

test("task text with no attachments reaches pi as a bare string", () => {
  const { calls, surface } = fakeSurface();

  assert.equal(surface.wakeUser(TASK_TEXT, []), true);
  assert.equal(calls.length, 1);
  // pi takes the string path of `content`; a one-element array of text parts is
  // a different message and a needless normalization round trip.
  assert.equal(calls[0].content, TASK_TEXT);
});

test("an attachment pi cannot read is dropped, logged, and the task text still travels", () => {
  const { calls, lines, surface } = fakeSurface();

  // Each part below fails one clause of what pi needs: no base64 data, no media
  // type, an empty media type. Sending any of them stops the whole delivery
  // inside pi, so all three go and the text goes alone.
  assert.equal(
    surface.wakeUser(TASK_TEXT, [
      { type: "image", mime: "image/png", name: "no-data.png" },
      { type: "image", data: "iVBORw0KGgoAAAANSUhEUg==", name: "no-mime.png" },
      { type: "image", data: "iVBORw0KGgoAAAANSUhEUg==", mime: "" },
    ]),
    true,
  );
  assert.equal(calls.length, 1);
  assert.equal(calls[0].content, TASK_TEXT);
  assert.equal(lines.length, 1);
  assert.match(lines[0], /3 dropped/);
  assert.match(lines[0], /the task text still went/);

  // Dropping is per part: a usable image beside an unusable one still rides.
  assert.equal(surface.wakeUser(TASK_TEXT, [{ type: "image", mime: "image/png" }, PNG_PART]), true);
  assert.deepEqual(calls[1].content, PNG_CONTENT);
  assert.equal(lines.length, 2);
  assert.match(lines[1], /1 dropped/);
});

test("delivery asks for followUp and a refusal of that option retries with the same content", () => {
  const asked = fakeSurface();

  assert.equal(asked.surface.wakeUser(TASK_TEXT, [PNG_PART]), true);
  assert.deepEqual(asked.calls, [{ content: PNG_CONTENT, options: { deliverAs: "followUp" } }]);

  // An older pi throws on the option it does not know. The message itself is
  // fine, so the retry drops the option and keeps the content.
  const refusing = fakeSurface({ refuse: (_content, options) => Boolean(options) });

  assert.equal(refusing.surface.wakeUser(TASK_TEXT, [PNG_PART]), true);
  assert.equal(refusing.calls.length, 2);
  assert.deepEqual(refusing.calls[0].options, { deliverAs: "followUp" });
  assert.equal(refusing.calls[1].options, undefined);
  assert.deepEqual(refusing.calls[1].content, PNG_CONTENT);
  assert.deepEqual(refusing.lines, [], "a delivery that landed on the retry is not a failure");
});
