//! The frame layout on the window's single multiplexed `Channel`.
//!
//! `[kind: u8][stream: u32 BE][len: u32 BE][payload]` — nine header bytes, then the
//! payload.
//!
//! ## Why this is not [`nysia_proto::frame`] verbatim
//!
//! The proto framing today is `[kind][len][payload]`, with no session discriminator,
//! because it was written when a `ClientRole::Stream` connection was assumed to carry one
//! session. §7.3 does not allow that on this leg: the webview gets **exactly one**
//! `Channel` for every session at once, so a frame that does not say which session it
//! belongs to cannot be delivered.
//!
//! The coordinator settled the shape rather than leaving it to be invented here: the
//! header gains a `u32` stream id, assigned by the daemon in response to a control-plane
//! attach verb, and W1 is landing that change in `nysia-proto`. The evidence it was always
//! the intent is in §7.3's own constants — a credit budget of *2 MiB total across streams*
//! alongside *512 KiB per stream* is meaningless unless one connection carries several.
//!
//! So this module encodes the shape proto is moving to, not a private invention, and
//! [`FrameKind`] is imported rather than re-declared. When W1's header lands, the encoder
//! and decoder here are deleted in favour of proto's and nothing above them moves — the
//! types on the boundary ([`ChannelFrame`], [`StreamId`]) are already the ones proto will
//! export.
//!
//! ## Two properties the decoder exists for
//!
//! **Frames arrive glued.** [`super::coalesce`] packs frames until the window closes,
//! because a payload under 1024 bytes goes through Tauri's `eval` path rather than its
//! fetch queue (traps register #4). "One read, one frame" is therefore never true.
//!
//! **Frames arrive torn.** The same buffer that holds three whole frames can end halfway
//! through a fourth. That is the normal case on a busy stream, not an error.

use nysia_proto::frame::{FrameKind, MAX_FRAME_PAYLOAD_BYTES};

/// The bytes a header occupies: a kind byte, a stream id, and a big-endian length.
pub const FRAME_HEADER_BYTES: usize = 1 + 4 + 4;

/// Which multiplexed stream a frame belongs to.
///
/// A `u32` rather than a [`nysia_proto::identity::SessionHandle`] because the header is on
/// the hot path: a handle is a 41-byte string, and repeating it on every 48 KiB chunk would
/// cost more than the chunk's own header. The daemon assigns the id when a session is
/// attached, and the mapping back to a handle travels once, over the control plane.
///
/// Zero is a valid id. There is no need for a reserved value here — unlike the kind byte,
/// a zero-filled buffer is already rejected by [`FrameKind::from_byte`] before the stream
/// id is ever read.
pub type StreamId = u32;

/// One frame as it rides the `Channel`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelFrame {
    /// What the payload is, using the kinds `nysia-proto` defines.
    pub kind: FrameKind,
    /// Which session's stream this belongs to.
    pub stream: StreamId,
    /// The bytes, without the header.
    pub payload: Vec<u8>,
}

impl ChannelFrame {
    /// A frame of `kind` on `stream`, carrying `payload`.
    pub fn new(kind: FrameKind, stream: StreamId, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            kind,
            stream,
            payload: payload.into(),
        }
    }

    /// The bytes this frame occupies on the wire, header included.
    #[cfg(test)]
    pub fn encoded_len(&self) -> usize {
        FRAME_HEADER_BYTES + self.payload.len()
    }
}

/// Why a frame could not be encoded or decoded.
///
/// Both variants are unrecoverable *for the channel*. There is no delimiter to scan
/// forward to, so a stream whose kind byte or length prefix is wrong cannot be
/// resynchronised; the only correct response is to tear the channel down and reattach.
///
/// "The buffer does not hold a whole frame yet" is deliberately absent: it is the normal
/// state of a busy stream, and modelling it as an error would have callers logging it
/// thousands of times a second.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FramingError {
    /// The first byte named no [`FrameKind`].
    ///
    /// Only the decoder can produce this, so it is scoped to the decoder's builds. The
    /// webview's decoder has the same case and is where it matters in production.
    #[cfg(test)]
    #[error("frame kind {0} is not one of 1..=5")]
    UnknownKind(u8),
    /// The length prefix asked for more than [`MAX_FRAME_PAYLOAD_BYTES`].
    ///
    /// The ceiling is not there to constrain the writer — §7.3 chunks output at 48 KiB, an
    /// order of magnitude under it. It is there so that four bytes of garbage ask for a
    /// bounded allocation instead of four gigabytes.
    #[error("a frame payload is at most {max} bytes, the header asked for {len}")]
    Oversized {
        /// What the header asked for.
        len: usize,
        /// The ceiling.
        max: usize,
    },
}

/// Append `frame` to `out`.
///
/// This rather than a `Vec`-returning encoder, because that is what the coalescing writer
/// wants: [`super::coalesce`] packs frames into one buffer until the window closes, and
/// allocating a fresh `Vec` per frame only to copy it into another would be the opposite of
/// the point.
///
/// # Errors
///
/// Returns [`FramingError::Oversized`] if the payload exceeds [`MAX_FRAME_PAYLOAD_BYTES`].
pub fn encode_into(frame: &ChannelFrame, out: &mut Vec<u8>) -> Result<(), FramingError> {
    let len = frame.payload.len();
    let len = u32::try_from(len)
        .ok()
        .filter(|_| len <= MAX_FRAME_PAYLOAD_BYTES)
        .ok_or(FramingError::Oversized {
            len,
            max: MAX_FRAME_PAYLOAD_BYTES,
        })?;

    out.reserve(FRAME_HEADER_BYTES + frame.payload.len());
    out.push(frame.kind.as_byte());
    out.extend_from_slice(&frame.stream.to_be_bytes());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&frame.payload);
    Ok(())
}

/// Encode one frame on its own.
///
/// Production packs frames with [`encode_into`] and never encodes one alone — that is the
/// whole point of the coalescing window — so this exists for the round-trip tests.
///
/// # Errors
///
/// Returns [`FramingError::Oversized`] if the payload exceeds [`MAX_FRAME_PAYLOAD_BYTES`].
#[cfg(test)]
pub fn encode(frame: &ChannelFrame) -> Result<Vec<u8>, FramingError> {
    let mut out = Vec::with_capacity(frame.encoded_len());
    encode_into(frame, &mut out)?;
    Ok(out)
}

/// Decode the frame at the front of `buf`.
///
/// See the module docs: the production decoder for this leg is in TypeScript.
///
/// `Ok(None)` means `buf` does not hold a whole frame *yet* — read more and call again.
/// `Ok(Some((frame, consumed)))` means `consumed` bytes were a frame and whatever follows
/// is the next one's beginning.
///
/// The kind byte is validated before the payload is waited for. It has to be: a stream
/// whose first byte is wrong is already unusable, and a decoder that waited for a length it
/// could not trust would sit on a dead channel reporting nothing until a buffer ceiling
/// stopped it.
///
/// # Errors
///
/// Returns [`FramingError::UnknownKind`] if the first byte names no kind, and
/// [`FramingError::Oversized`] if the length prefix exceeds [`MAX_FRAME_PAYLOAD_BYTES`].
#[cfg(test)]
pub fn decode(buf: &[u8]) -> Result<Option<(ChannelFrame, usize)>, FramingError> {
    let Some(&kind_byte) = buf.first() else {
        return Ok(None);
    };
    let kind = FrameKind::from_byte(kind_byte).ok_or(FramingError::UnknownKind(kind_byte))?;

    let Some(header) = buf.get(1..FRAME_HEADER_BYTES) else {
        return Ok(None);
    };
    // Both slices are exactly four bytes long, so the conversions are total. `unwrap_or`
    // keeps the crate free of `expect` without inventing an error path that cannot happen.
    let stream = u32::from_be_bytes(
        header
            .get(0..4)
            .unwrap_or(&[0; 4])
            .try_into()
            .unwrap_or([0; 4]),
    );
    let len = u32::from_be_bytes(
        header
            .get(4..8)
            .unwrap_or(&[0; 4])
            .try_into()
            .unwrap_or([0; 4]),
    ) as usize;
    if len > MAX_FRAME_PAYLOAD_BYTES {
        return Err(FramingError::Oversized {
            len,
            max: MAX_FRAME_PAYLOAD_BYTES,
        });
    }

    let end = FRAME_HEADER_BYTES + len;
    let Some(payload) = buf.get(FRAME_HEADER_BYTES..end) else {
        return Ok(None);
    };
    Ok(Some((ChannelFrame::new(kind, stream, payload), end)))
}

/// Reassembles frames from a byte stream that neither starts nor stops on frame boundaries.
///
/// Pure: it owns a buffer and nothing else. Push whatever a read produced, then pull frames
/// until it says there are none left.
///
/// On an error the decoder does **not** consume the offending bytes, so the same error
/// comes back on every subsequent call. That is deliberate — with no delimiter to
/// resynchronise on, skipping ahead would invent frames out of payload bytes.
#[cfg(test)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
}

#[cfg(test)]
impl FrameDecoder {
    /// An empty decoder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add whatever the last read produced.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// Bytes held but not yet forming a whole frame.
    ///
    /// A caller that watches this can tell "the peer is quiet" from "the peer sent half a
    /// frame and stopped", which are different failures.
    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }

    /// Take the next whole frame, or `None` while one is still arriving.
    ///
    /// # Errors
    ///
    /// Propagates [`decode`]'s errors, without consuming the bytes that caused them.
    pub fn next_frame(&mut self) -> Result<Option<ChannelFrame>, FramingError> {
        match decode(&self.buffer)? {
            Some((frame, consumed)) => {
                self.buffer.drain(..consumed);
                Ok(Some(frame))
            }
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact bytes of one frame, as hex.
    ///
    /// The TypeScript decoder asserts the same literal in
    /// `apps/web/src/transport/frames.test.ts`. The two sides cannot share a fixture file —
    /// `apps/web` is a browser bundle and may not import `node:fs` — so agreement is kept
    /// by this pair of tests naming the same bytes. If you change the layout, both fail.
    const GOLDEN_HEX: &str = "010000002a00000003686921";

    fn golden_frame() -> ChannelFrame {
        ChannelFrame::new(FrameKind::Output, 42, b"hi!".as_slice())
    }

    fn to_hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn the_header_is_a_kind_a_stream_id_and_a_big_endian_length() {
        let wire = encode(&golden_frame()).unwrap();
        assert_eq!(to_hex(&wire), GOLDEN_HEX);
        assert_eq!(wire[0], FrameKind::Output.as_byte());
        assert_eq!(&wire[1..5], &[0, 0, 0, 42]);
        assert_eq!(&wire[5..9], &[0, 0, 0, 3]);
        assert_eq!(wire.len(), golden_frame().encoded_len());
    }

    #[test]
    fn every_proto_kind_survives_a_round_trip_on_every_stream() {
        for kind in FrameKind::ALL {
            for stream in [0, 1, 7, u32::MAX] {
                let frame = ChannelFrame::new(kind, stream, vec![kind.as_byte(); 5]);
                let wire = encode(&frame).unwrap();
                let (decoded, consumed) = decode(&wire).unwrap().unwrap();
                assert_eq!(decoded, frame);
                assert_eq!(consumed, wire.len());
            }
        }
    }

    #[test]
    fn frames_glued_together_demultiplex_back_to_their_streams() {
        // The case coalescing creates: three sessions' output in one delivery.
        let frames = vec![
            ChannelFrame::new(FrameKind::Output, 1, b"one".as_slice()),
            ChannelFrame::new(FrameKind::Output, 2, b"two".as_slice()),
            ChannelFrame::new(FrameKind::Bell, 1, Vec::new()),
            ChannelFrame::new(FrameKind::Output, 3, b"three".as_slice()),
        ];
        let mut wire = Vec::new();
        for frame in &frames {
            encode_into(frame, &mut wire).unwrap();
        }

        let mut decoder = FrameDecoder::new();
        decoder.push(&wire);
        let mut seen = Vec::new();
        while let Some(frame) = decoder.next_frame().unwrap() {
            seen.push(frame);
        }
        assert_eq!(seen, frames);
        assert_eq!(decoder.buffered(), 0);
    }

    #[test]
    fn a_frame_torn_at_every_offset_still_arrives_whole() {
        let frame = ChannelFrame::new(FrameKind::Output, 9, vec![0xAB; 300]);
        let wire = encode(&frame).unwrap();

        for split in 0..wire.len() {
            let mut decoder = FrameDecoder::new();
            decoder.push(&wire[..split]);
            assert_eq!(decoder.next_frame().unwrap(), None, "split at {split}");
            decoder.push(&wire[split..]);
            assert_eq!(decoder.next_frame().unwrap(), Some(frame.clone()));
            assert_eq!(decoder.buffered(), 0);
        }
    }

    #[test]
    fn an_unknown_kind_byte_is_reported_and_not_consumed() {
        let mut decoder = FrameDecoder::new();
        decoder.push(&[0x00, 0, 0, 0, 1, 0, 0, 0, 0]);
        assert_eq!(decoder.next_frame(), Err(FramingError::UnknownKind(0)));
        // Reported again rather than skipped: there is nothing to resynchronise on.
        assert_eq!(decoder.next_frame(), Err(FramingError::UnknownKind(0)));
    }

    #[test]
    fn a_hostile_length_prefix_asks_for_a_bounded_allocation() {
        let mut wire = vec![FrameKind::Output.as_byte()];
        wire.extend_from_slice(&0u32.to_be_bytes());
        wire.extend_from_slice(&u32::MAX.to_be_bytes());

        assert_eq!(
            decode(&wire),
            Err(FramingError::Oversized {
                len: u32::MAX as usize,
                max: MAX_FRAME_PAYLOAD_BYTES,
            })
        );
    }

    #[test]
    fn a_payload_over_the_ceiling_is_refused_by_the_encoder_too() {
        let frame = ChannelFrame::new(FrameKind::Output, 0, vec![0; MAX_FRAME_PAYLOAD_BYTES + 1]);
        assert!(matches!(
            encode(&frame),
            Err(FramingError::Oversized { .. })
        ));
    }
}
