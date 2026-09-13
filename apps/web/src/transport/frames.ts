import type { FrameKind } from '../generated/FrameKind';
import {
  FRAME_KIND_BY_BYTE,
  MAX_FRAME_PAYLOAD_BYTES,
} from '../generated/wireConstants';

/**
 * Taking the one multiplexed Channel apart again.
 *
 * Terminal output for every session arrives on a single `tauri::ipc::Channel` — never
 * `emit`, because tauri#12724 leaks on sustained emits and terminal output is the
 * definition of a sustained emit. One channel means this module is the only thing standing
 * between a delivery of bytes and thirty panes that each want their own share of it.
 *
 * Two properties of the transport shape everything here, and both come from the Rust side
 * having a coalescing window (§7.3):
 *
 * - **Frames arrive glued.** A delivery holds every frame the 16 ms window collected, from
 *   however many sessions were producing output. "One delivery, one frame" is never true.
 * - **Frames arrive torn.** `Channel` preserves message boundaries, so a delivery is whole
 *   today — but the decoder is written not to depend on that, because the cost is one
 *   retained buffer and the failure mode if it ever stops being true is silent corruption
 *   of a terminal's escape-sequence parser.
 *
 * The byte values are imported from `generated/wireConstants`, which `nysia-proto`'s
 * `bindings` module writes from the same Rust definitions the daemon uses (D-13). Nothing
 * here restates a number Rust already owns.
 */

/**
 * Which multiplexed stream a frame belongs to.
 *
 * Assigned by the daemon, and a small integer rather than a `SessionHandle` because it is
 * on the hot path: a handle is a 41-character string, and repeating it on every 48 KiB
 * chunk would cost more than the chunk's own header.
 */
export type StreamId = number;

/**
 * The bytes a channel frame's header occupies: a kind byte, a stream id, and a length.
 *
 * Deliberately **not** `FRAME_HEADER_BYTES` from `generated/wireConstants`. That constant
 * is 5 and describes `nysia-proto`'s current socket header, `[kind][len][payload]`, which
 * has no session discriminator because it was written when a stream connection was assumed
 * to carry one session. This leg carries all of them over one Channel, so the header has a
 * stream id in it and is four bytes longer. When W1 lands the stream-tagged header in proto
 * the two converge and this constant is replaced by the generated one.
 */
export const CHANNEL_FRAME_HEADER_BYTES = 9;

/** One frame, as the Rust side packed it. */
export interface ChannelFrame {
  readonly kind: FrameKind;
  readonly stream: StreamId;
  /** A view into the delivery buffer. Not copied — see {@link decodeFrames}. */
  readonly payload: Uint8Array;
}

/**
 * Why a delivery could not be read.
 *
 * There is no delimiter to scan forward to, so a stream whose kind byte or length prefix is
 * wrong cannot be resynchronised. Both cases mean the same thing to a caller: tear the
 * channel down and reattach. That is why this is one class with a reason rather than a
 * hierarchy nobody would branch on.
 */
export class FrameError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'FrameError';
  }
}

/**
 * Reassembles frames from deliveries that need not start or stop on frame boundaries.
 *
 * Push whatever arrived, then pull frames until it says there are none left. A partial
 * frame is retained and completed by the next delivery; it is not an error, and modelling
 * it as one would have callers logging thousands of times a second on a busy session.
 *
 * On an error the decoder does not consume the offending bytes, so the same error comes
 * back on every subsequent call. Skipping ahead would invent frames out of payload bytes.
 */
export class FrameDecoder {
  /** Retained bytes that did not yet form a whole frame. */
  #buffer = new Uint8Array(0);

  /** Bytes held but not yet forming a whole frame. */
  get buffered(): number {
    return this.#buffer.length;
  }

  /**
   * Decode everything `delivery` completes.
   *
   * Returns frames in the order they were packed, which is the order the daemon read them
   * off the PTYs. Order is the one thing a terminal cannot tolerate losing: two chunks of
   * one escape sequence delivered out of order paint something the program never wrote.
   *
   * @throws {FrameError} if the stream is unreadable.
   */
  push(delivery: Uint8Array): ChannelFrame[] {
    // The fast path — nothing retained — hands `decodeFrames` the delivery untouched, so
    // the common case copies no bytes at all.
    const buffer =
      this.#buffer.length === 0 ? delivery : concat(this.#buffer, delivery);

    const frames: ChannelFrame[] = [];
    let offset = 0;
    try {
      for (;;) {
        const decoded = decodeAt(buffer, offset);
        if (decoded === null) {
          break;
        }
        frames.push(decoded.frame);
        offset = decoded.end;
      }
    } catch (cause) {
      // Retain the bytes that could not be read, so the same error comes back on every
      // subsequent call. Dropping them would let the next delivery decode cleanly from a
      // position that is not a frame boundary, which is far worse than staying broken: the
      // channel would look recovered while painting bytes no program wrote.
      this.#retain(buffer, offset);
      throw cause;
    }

    this.#retain(buffer, offset);
    return frames;
  }

  /** Forget everything retained. For a channel being torn down and reattached. */
  reset(): void {
    this.#buffer = new Uint8Array(0);
  }

  /**
   * Keep whatever follows `offset` for the next delivery.
   *
   * Always a copy, never a view: `buffer` may be the caller’s delivery, and the Channel
   * is entitled to reuse that memory the moment we return. A retained view would be
   * overwritten in place and the terminal would paint the wrong bytes.
   */
  #retain(buffer: Uint8Array, offset: number): void {
    this.#buffer =
      offset === buffer.length ? new Uint8Array(0) : buffer.slice(offset);
  }
}

/**
 * Every whole frame in `buffer`, for a caller that knows it holds complete frames.
 *
 * Payloads are **views** into `buffer`, not copies: a 48 KiB chunk copied on the way past
 * would double the cost of the hot path for nothing, since xterm's `write` consumes the
 * bytes synchronously. A caller that retains a payload past the turn must copy it itself.
 *
 * @throws {FrameError} if the stream is unreadable.
 */
export function decodeFrames(buffer: Uint8Array): ChannelFrame[] {
  const frames: ChannelFrame[] = [];
  let offset = 0;
  for (;;) {
    const decoded = decodeAt(buffer, offset);
    if (decoded === null) {
      return frames;
    }
    frames.push(decoded.frame);
    offset = decoded.end;
  }
}

/**
 * Group frames by the session they belong to, preserving order within each.
 *
 * The demultiplexing step proper. Across sessions the interleaving is not meaningful —
 * three panes producing output in the same 16 ms did so concurrently — but within one
 * session it is everything.
 */
export function bySession(
  frames: readonly ChannelFrame[],
): Map<StreamId, ChannelFrame[]> {
  const grouped = new Map<StreamId, ChannelFrame[]>();
  for (const frame of frames) {
    const existing = grouped.get(frame.stream);
    if (existing) {
      existing.push(frame);
    } else {
      grouped.set(frame.stream, [frame]);
    }
  }
  return grouped;
}

/** The frame beginning at `offset`, or `null` while it is still arriving. */
function decodeAt(
  buffer: Uint8Array,
  offset: number,
): { frame: ChannelFrame; end: number } | null {
  const kindByte = buffer[offset];
  if (kindByte === undefined) {
    return null;
  }

  // The kind is checked before the length is waited for. It has to be: a stream whose first
  // byte is wrong is already unusable, and waiting for a length that cannot be trusted
  // would sit on a dead channel reporting nothing.
  const kind = FRAME_KIND_BY_BYTE[kindByte];
  if (kind === undefined) {
    throw new FrameError(
      `frame kind ${kindByte} is not one the wire defines; the channel is unreadable`,
    );
  }

  if (buffer.length - offset < CHANNEL_FRAME_HEADER_BYTES) {
    return null;
  }

  // A DataView rather than hand-rolled shifts: `<<` coerces to a signed 32-bit integer, so
  // a stream id with its high bit set would read as negative and never match the id the
  // daemon assigned.
  const header = new DataView(
    buffer.buffer,
    buffer.byteOffset + offset,
    CHANNEL_FRAME_HEADER_BYTES,
  );
  const stream = header.getUint32(1, false);
  const length = header.getUint32(5, false);

  if (length > MAX_FRAME_PAYLOAD_BYTES) {
    throw new FrameError(
      `a frame payload is at most ${MAX_FRAME_PAYLOAD_BYTES} bytes, the header asked for ${length}`,
    );
  }

  const start = offset + CHANNEL_FRAME_HEADER_BYTES;
  const end = start + length;
  if (end > buffer.length) {
    return null;
  }

  return {
    frame: { kind, stream, payload: buffer.subarray(start, end) },
    end,
  };
}

/** One buffer holding `first` then `second`. */
function concat(first: Uint8Array, second: Uint8Array): Uint8Array {
  const joined = new Uint8Array(first.length + second.length);
  joined.set(first, 0);
  joined.set(second, first.length);
  return joined;
}
