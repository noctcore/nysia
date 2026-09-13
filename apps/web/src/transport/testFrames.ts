import type { FrameKind } from '../generated/FrameKind';
import { FRAME_HEADER_BYTES, FRAME_KIND } from '../generated/wireConstants';
import type { StreamId } from './frames';

/**
 * Encoding frames the way the daemon does, for tests that need input.
 *
 * A plain module rather than a `.test.ts`, following `store/storeContract.ts`: vitest picks
 * up `src/**\/*.test.ts` only, so this is a helper the suites import rather than a suite of
 * its own.
 *
 * It exists so the layout is written down **once** on this side. Two test files previously
 * carried their own copy of the header size, the field offsets and the kind bytes — and a
 * duplicated layout is precisely what survives a change to the real one: the header grew
 * from five bytes to nine when stream multiplexing landed, and a test that kept its own
 * copy would have gone on passing against a format nothing sends.
 *
 * Every value comes from `generated/wireConstants`, which `nysia-proto` writes (D-13).
 */

/** Where the stream id starts, derived from the generated total — see `frames.ts`. */
const STREAM_OFFSET = 1;

/** Where the payload length starts: the last four bytes of the header. */
const LENGTH_OFFSET = FRAME_HEADER_BYTES - 4;

/** One frame, byte for byte as the Rust encoder writes it. */
export function encodeFrame(
  kind: FrameKind,
  stream: StreamId,
  payload: Uint8Array,
): Uint8Array {
  const frame = new Uint8Array(FRAME_HEADER_BYTES + payload.length);
  const header = new DataView(frame.buffer);
  frame[0] = FRAME_KIND[kind];
  header.setUint32(STREAM_OFFSET, stream, false);
  header.setUint32(LENGTH_OFFSET, payload.length, false);
  frame.set(payload, FRAME_HEADER_BYTES);
  return frame;
}

/** Several frames, glued the way one coalescing window delivers them. */
export function encodeFrames(
  frames: readonly (readonly [FrameKind, StreamId, Uint8Array])[],
): Uint8Array {
  const parts = frames.map(([kind, stream, payload]) =>
    encodeFrame(kind, stream, payload),
  );
  const total = parts.reduce((sum, part) => sum + part.length, 0);
  const joined = new Uint8Array(total);
  let offset = 0;
  for (const part of parts) {
    joined.set(part, offset);
    offset += part.length;
  }
  return joined;
}

/** A header whose length prefix asks for `length` bytes, for the ceiling tests. */
export function encodeHeaderOnly(kindByte: number, length: number): Uint8Array {
  const wire = new Uint8Array(FRAME_HEADER_BYTES);
  wire[0] = kindByte;
  new DataView(wire.buffer).setUint32(LENGTH_OFFSET, length, false);
  return wire;
}
