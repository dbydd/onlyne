// Framing tests against the Rust encoder's own wire vectors.

import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

import { MAX_FRAME_BYTES, createFrameDecoder, decodeFrame, encodeFrame } from "./frame.mjs";

const VECTOR_DIR = fileURLToPath(new URL("../../../crates/onlyne-proto/tests/wire_vectors/", import.meta.url));
const hasVectors = existsSync(VECTOR_DIR);
const vector = (name) => JSON.parse(readFileSync(`${VECTOR_DIR}${name}`, "utf8"));

/** Collect every frame one decoder yields from a sequence of chunks. */
function decodeAll(chunks, options = {}) {
  const frames = [];
  const errors = [];
  const decoder = createFrameDecoder({
    onFrame: (value) => frames.push(value),
    onError: (error) => errors.push(error),
    ...options,
  });
  for (const chunk of chunks) decoder.push(chunk);
  return { frames, errors, decoder };
}

test("a frame round trips through encode and decode", () => {
  const value = { id: 1, op: "hello", args: { protocol: 1, capabilities: ["register", "report"] } };
  const frame = encodeFrame(value);
  assert.equal(frame.readUInt32BE(0), frame.length - 4);
  assert.deepEqual(decodeFrame(frame), value);
  assert.deepEqual(decodeAll([frame]).frames, [value]);
});

test("a half frame is held until its remainder arrives", () => {
  const frame = encodeFrame({ op: "assign", args: { task_id: "t1" } });
  const first = decodeAll([frame.subarray(0, 4)]);
  assert.deepEqual(first.frames, []);
  assert.equal(first.decoder.buffered(), 4);
  const rest = decodeAll([frame.subarray(0, 6), frame.subarray(6)]);
  assert.deepEqual(rest.frames, [{ op: "assign", args: { task_id: "t1" } }]);
});

test("several frames in one chunk are emitted in order", () => {
  const frames = [encodeFrame({ id: 1 }), encodeFrame({ id: 2 }), encodeFrame({ id: 3 })];
  const { frames: decoded } = decodeAll([Buffer.concat(frames)]);
  assert.deepEqual(decoded, [{ id: 1 }, { id: 2 }, { id: 3 }]);
});

test("a frame split across many chunks still decodes once", () => {
  const frame = encodeFrame({ op: "report", args: { kind: "heartbeat", data: { seq: 1001 } } });
  const chunks = [...frame].map((byte) => Buffer.from([byte]));
  const { frames, errors } = decodeAll(chunks);
  assert.deepEqual(errors, []);
  assert.deepEqual(frames, [{ op: "report", args: { kind: "heartbeat", data: { seq: 1001 } } }]);
});

test("an oversize length prefix kills the decoder without emitting a frame", () => {
  const header = Buffer.alloc(4);
  header.writeUInt32BE(MAX_FRAME_BYTES + 1, 0);
  const { frames, errors, decoder } = decodeAll([header]);
  assert.deepEqual(frames, []);
  assert.equal(errors.length, 1);
  assert.match(errors[0].message, /^frame_too_large: /);
  // A dead decoder stays dead: framing cannot resynchronise after a corrupt length.
  assert.equal(decoder.dead(), true);
  assert.equal(decoder.buffered(), 0);
});

test("undecodable JSON is a bad_frame fault", () => {
  const body = Buffer.from("{not json", "utf8");
  const header = Buffer.alloc(4);
  header.writeUInt32BE(body.length, 0);
  const { frames, errors } = decodeAll([Buffer.concat([header, body])]);
  assert.deepEqual(frames, []);
  assert.match(errors[0].message, /^bad_frame: /);
});

test("the encoder refuses a body above the ceiling before writing anything", () => {
  assert.throws(
    () => encodeFrame({ text: "x".repeat(MAX_FRAME_BYTES + 1) }),
    /frame_too_large/,
  );
});

test("the Rust encoder's own vectors decode through this decoder", { skip: !hasVectors }, () => {
  const names = ["adapter_plugin_hello.json", "adapter_plugin_report.json", "adapter_host_assign.json"];
  for (const name of names) {
    const entry = vector(name);
    assert.equal(entry.encoding, "u32_be_length_prefix_plus_utf8_json");
    const body = Buffer.from(entry.frame, "utf8");
    const framed = encodeFrame(JSON.parse(entry.frame));
    assert.equal(framed.readUInt32BE(0), body.length, `${name}: length prefix must be the body byte count`);
    assert.deepEqual(decodeFrame(framed), JSON.parse(entry.frame), `${name}: round trip`);
    assert.deepEqual(decodeAll([framed]).frames, [JSON.parse(entry.frame)], `${name}: streamed decode`);
  }
});
