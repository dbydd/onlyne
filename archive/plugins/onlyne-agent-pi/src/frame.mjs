// Length-prefixed JSON framing, the onlyne-frame codec: a four-byte big-endian
// u32 body length followed by one UTF-8 JSON object. Zero dependencies: Node's
// Buffer is the whole implementation.
//
// Failure modes mirror `crates/onlyne-frame/src/lib.rs` so the plugin's log
// speaks the same vocabulary the host does: an oversize length prefix is
// `frame_too_large`, undecodable JSON is `bad_frame`.

/** Hard ceiling for one frame body, byte-for-byte `MAX_FRAME_BYTES`. */
export const MAX_FRAME_BYTES = 8 * 1024 * 1024;

/**
 * Encode one frame. Throws before any byte leaves the writer, so an oversize
 * payload never puts a partial frame on the wire.
 * @param {unknown} value
 * @returns {Buffer}
 */
export function encodeFrame(value) {
  const body = Buffer.from(JSON.stringify(value), "utf8");
  if (body.length > MAX_FRAME_BYTES) {
    throw new Error(`frame_too_large: ${body.length} > ${MAX_FRAME_BYTES}`);
  }
  const header = Buffer.allocUnsafe(4);
  header.writeUInt32BE(body.length, 0);
  return Buffer.concat([header, body]);
}

/**
 * Decode one framed body. Throws the same two error shapes `encodeFrame` and
 * the streaming decoder raise, for tests and for callers holding a whole frame.
 * @param {Buffer} frame
 * @returns {unknown}
 */
export function decodeFrame(frame) {
  if (frame.length < 4) throw new Error("bad_frame: truncated length prefix");
  const length = frame.readUInt32BE(0);
  if (length > MAX_FRAME_BYTES) throw new Error(`frame_too_large: ${length} > ${MAX_FRAME_BYTES}`);
  if (frame.length !== 4 + length) throw new Error("bad_frame: truncated body");
  try {
    return JSON.parse(frame.subarray(4).toString("utf8"));
  } catch (error) {
    throw new Error(`bad_frame: ${error instanceof Error ? error.message : String(error)}`);
  }
}

/**
 * A streaming decoder. `push` accepts whatever the socket delivered — half a
 * frame, several frames, or both — and calls `onFrame` once per complete body,
 * in order.
 *
 * The decoder dies on the first framing fault (oversize, undecodable body) the
 * way the connection must: framing cannot resynchronise after a corrupt body,
 * so `onError` is called once and every later `push` is a no-op.
 *
 * @param {{ maxBytes?: number, onFrame: (value: unknown) => void, onError?: (error: Error) => void }} options
 */
export function createFrameDecoder({ maxBytes = MAX_FRAME_BYTES, onFrame, onError }) {
  let buffer = Buffer.alloc(0);
  let dead = false;
  const fail = (error) => {
    dead = true;
    buffer = Buffer.alloc(0);
    onError?.(error);
  };
  return {
    /** @param {Buffer} chunk */
    push(chunk) {
      if (dead || chunk.length === 0) return;
      buffer = buffer.length === 0 ? chunk : Buffer.concat([buffer, chunk]);
      for (;;) {
        if (buffer.length < 4) return;
        const length = buffer.readUInt32BE(0);
        if (length > maxBytes) {
          fail(new Error(`frame_too_large: ${length} > ${maxBytes}`));
          return;
        }
        if (buffer.length < 4 + length) return;
        const body = buffer.subarray(4, 4 + length);
        buffer = buffer.subarray(4 + length);
        let value;
        try {
          value = JSON.parse(body.toString("utf8"));
        } catch (error) {
          fail(new Error(`bad_frame: ${error instanceof Error ? error.message : String(error)}`));
          return;
        }
        onFrame(value);
      }
    },
    /** Bytes held back waiting for the rest of a frame. */
    buffered() {
      return buffer.length;
    },
    /** Whether a framing fault has closed this decoder. */
    dead() {
      return dead;
    },
  };
}
