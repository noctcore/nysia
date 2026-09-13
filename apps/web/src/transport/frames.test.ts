import { describe, expect, it } from 'vitest';

import type { FrameKind } from '../generated/FrameKind';
import { FRAME_KIND, MAX_FRAME_PAYLOAD_BYTES } from '../generated/wireConstants';
import {
  bySession,
  CHANNEL_FRAME_HEADER_BYTES,
  decodeFrames,
  FrameDecoder,
  FrameError,
  type StreamId,
} from './frames';

/**
 * The exact bytes of one frame, as hex.
 *
 * The Rust encoder asserts this same literal in
 * `apps/desktop/src-tauri/src/channel/framing.rs` (`GOLDEN_HEX`). The two sides cannot
 * share a fixture file — `apps/web` is a browser bundle and may not import `node:fs`, which
 * the ESLint ban set enforces — so agreement is kept by this pair of tests naming the same
 * bytes. Change the layout and both fail, which is the property that matters: a layout
 * change that only one side noticed is the one bug this cannot be allowed to miss.
 *
 * Reading it: `01` output, `0000002a` stream 42, `00000003` three bytes, `686921` "hi!".
 */
const GOLDEN_HEX = '010000002a00000003686921';

/** Encode one frame the way the Rust side does, for the tests that need input. */
function encode(kind: FrameKind, stream: StreamId, payload: Uint8Array): Uint8Array {
  const frame = new Uint8Array(CHANNEL_FRAME_HEADER_BYTES + payload.length);
  const header = new DataView(frame.buffer);
  frame[0] = FRAME_KIND[kind];
  header.setUint32(1, stream, false);
  header.setUint32(5, payload.length, false);
  frame.set(payload, CHANNEL_FRAME_HEADER_BYTES);
  return frame;
}

function bytes(...parts: Uint8Array[]): Uint8Array {
  const total = parts.reduce((sum, part) => sum + part.length, 0);
  const joined = new Uint8Array(total);
  let offset = 0;
  for (const part of parts) {
    joined.set(part, offset);
    offset += part.length;
  }
  return joined;
}

function fromHex(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let index = 0; index < out.length; index += 1) {
    out[index] = Number.parseInt(hex.slice(index * 2, index * 2 + 2), 16);
  }
  return out;
}

function toHex(buffer: Uint8Array): string {
  return [...buffer].map((byte) => byte.toString(16).padStart(2, '0')).join('');
}

const text = new TextEncoder();
const decodeText = (payload: Uint8Array): string => new TextDecoder().decode(payload);

describe('the channel frame layout', () => {
  it('reads the exact bytes the Rust encoder writes', () => {
    const [frame] = decodeFrames(fromHex(GOLDEN_HEX));
    expect(frame?.kind).toBe('output');
    expect(frame?.stream).toBe(42);
    expect(decodeText(frame?.payload ?? new Uint8Array())).toBe('hi!');
  });

  it('round-trips through this file’s own encoder, which produces those bytes', () => {
    // Guards the helper the rest of the suite is built on: if `encode` drifted from the
    // layout, every case below would be testing a format nothing sends.
    expect(toHex(encode('output', 42, text.encode('hi!')))).toBe(GOLDEN_HEX);
  });

  it('reads a stream id with its high bit set as the id the daemon assigned', () => {
    // `<<` coerces to a signed 32-bit integer, so a hand-rolled decoder reads 0xFFFFFFFF as
    // -1 and the frame is routed to a pane that does not exist. The failure is silent: the
    // output simply never appears.
    const [frame] = decodeFrames(encode('output', 0xffffffff, text.encode('x')));
    expect(frame?.stream).toBe(4294967295);
  });

  it('reads every kind the wire defines', () => {
    const kinds = Object.keys(FRAME_KIND) as FrameKind[];
    const wire = bytes(...kinds.map((kind) => encode(kind, 1, new Uint8Array(2))));
    expect(decodeFrames(wire).map((frame) => frame.kind)).toEqual(kinds);
  });
});

describe('demultiplexing', () => {
  it('takes one delivery apart into the sessions that produced it', () => {
    // The case coalescing creates, and the reason this module exists: the 16 ms window
    // collected output from three panes, and one delivery carries all of it.
    const wire = bytes(
      encode('output', 1, text.encode('one')),
      encode('output', 2, text.encode('two')),
      encode('bell', 1, new Uint8Array(0)),
      encode('output', 3, text.encode('three')),
      encode('output', 2, text.encode('more')),
    );

    const grouped = bySession(decodeFrames(wire));
    expect([...grouped.keys()].sort()).toEqual([1, 2, 3]);
    expect(grouped.get(1)?.map((frame) => frame.kind)).toEqual(['output', 'bell']);
    expect(grouped.get(2)?.map((frame) => decodeText(frame.payload))).toEqual([
      'two',
      'more',
    ]);
    expect(grouped.get(3)).toHaveLength(1);
  });

  it('preserves order within a session', () => {
    // Across sessions the interleaving is not meaningful. Within one it is everything: two
    // halves of an escape sequence delivered out of order paint something no program wrote.
    const wire = bytes(
      ...['\u001b[3', '1mred', '\u001b[0m'].map((part) =>
        encode('output', 7, text.encode(part)),
      ),
    );
    const painted = (bySession(decodeFrames(wire)).get(7) ?? [])
      .map((frame) => decodeText(frame.payload))
      .join('');
    expect(painted).toBe('\u001b[31mred\u001b[0m');
  });

  it('gives an empty delivery no sessions rather than an empty one', () => {
    expect(bySession(decodeFrames(new Uint8Array(0))).size).toBe(0);
  });
});

describe('a delivery that does not end on a frame boundary', () => {
  it('completes a frame torn at any offset', () => {
    const whole = encode('output', 9, new Uint8Array(300).fill(0xab));

    for (let split = 0; split < whole.length; split += 1) {
      const decoder = new FrameDecoder();
      expect(decoder.push(whole.subarray(0, split)), `split at ${split}`).toHaveLength(0);
      const frames = decoder.push(whole.subarray(split));
      expect(frames, `split at ${split}`).toHaveLength(1);
      expect(frames[0]?.payload).toHaveLength(300);
      expect(decoder.buffered).toBe(0);
    }
  });

  it('keeps the whole frames and retains only the remainder', () => {
    const wire = bytes(
      encode('output', 1, text.encode('first')),
      encode('output', 2, text.encode('second')),
    );
    const cut = wire.length - 3;

    const decoder = new FrameDecoder();
    const first = decoder.push(wire.subarray(0, cut));
    expect(first).toHaveLength(1);
    expect(decodeText(first[0]?.payload ?? new Uint8Array())).toBe('first');
    expect(decoder.buffered).toBeGreaterThan(0);

    const second = decoder.push(wire.subarray(cut));
    expect(decodeText(second[0]?.payload ?? new Uint8Array())).toBe('second');
    expect(decoder.buffered).toBe(0);
  });

  it('does not alias a delivery the channel may reuse', () => {
    // A retained payload that pointed into the caller's buffer would be overwritten by the
    // next delivery, and a terminal would paint the wrong bytes with nothing to show why.
    const whole = encode('output', 1, text.encode('kept'));
    const delivery = new Uint8Array(whole.subarray(0, 6));

    const decoder = new FrameDecoder();
    decoder.push(delivery);
    delivery.fill(0);

    const frames = decoder.push(whole.subarray(6));
    expect(decodeText(frames[0]?.payload ?? new Uint8Array())).toBe('kept');
  });

  it('forgets what it retained when the channel is torn down', () => {
    const decoder = new FrameDecoder();
    decoder.push(encode('output', 1, text.encode('abc')).subarray(0, 4));
    expect(decoder.buffered).toBeGreaterThan(0);
    decoder.reset();
    expect(decoder.buffered).toBe(0);
  });
});

describe('an unreadable channel', () => {
  it('reports a kind byte the wire does not define', () => {
    const wire = encode('output', 1, text.encode('ok'));
    wire[0] = 0;
    expect(() => decodeFrames(wire)).toThrow(FrameError);
  });

  it('refuses a length prefix past the ceiling rather than allocating for it', () => {
    // Four bytes of garbage would otherwise ask for four gigabytes.
    const wire = new Uint8Array(CHANNEL_FRAME_HEADER_BYTES);
    wire[0] = FRAME_KIND.output;
    new DataView(wire.buffer).setUint32(5, MAX_FRAME_PAYLOAD_BYTES + 1, false);
    expect(() => decodeFrames(wire)).toThrow(/at most/);
  });

  it('keeps reporting rather than resynchronising on a guess', () => {
    // There is no delimiter to scan forward to, so skipping ahead would invent frames out
    // of payload bytes — which is worse than stopping, because it looks like it worked.
    const decoder = new FrameDecoder();
    const wire = encode('output', 1, text.encode('ok'));
    wire[0] = 0xff;
    expect(() => decoder.push(wire)).toThrow(FrameError);
    expect(() => decoder.push(new Uint8Array(0))).toThrow(FrameError);
  });
});
