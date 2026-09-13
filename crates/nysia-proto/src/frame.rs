//! The binary output framing: `[kind: u8][stream: u32 big-endian][len: u32 big-endian][payload]`.
//!
//! Control traffic is newline-delimited JSON, but terminal output is not: it is bytes, it
//! is continuous, and base64 in JSON would cost a third of the bandwidth for nothing. So
//! output rides a separate length-prefixed binary stream (§3.1).
//!
//! **One connection carries every session.** The kind byte says what a frame is; the stream
//! id says which session it belongs to. That was always the design, and the header simply
//! never expressed it — §7.3 gives the credit window as per-stream 512 KiB initial and 2 MiB
//! max *and* a total of 2 and 8 MiB, and a total budget across streams is meaningless unless
//! one connection carries several. It also matches the rule that the webview gets exactly
//! one `Channel`: thirty sockets for thirty sessions would multiply the handshake and the
//! peer-credential work (§3.2) for nothing. See [`crate::stream`] for how an id is assigned.
//!
//! Two properties of the transport shape everything below.
//!
//! **Frames arrive glued together.** §7.3 coalesces to at least 1 KiB before flushing,
//! because Tauri sends raw payloads under 1024 bytes through `eval` rather than the fetch
//! queue. A 12-byte OSC 133 frame is therefore never delivered alone; it arrives packed
//! behind whatever else was in the window — now routinely behind frames for *other*
//! sessions, which multiplexing makes the normal case rather than an edge one.
//! [`FrameDecoder`] exists because "one read, one frame" is never true here.
//!
//! **Frames also arrive torn.** The same buffer that holds three whole frames can end
//! halfway through a fourth, including part-way through the nine-byte header.
//!
//! The kind byte, which a TypeScript decoder has to agree with:
//!
//! | Byte | Kind | Payload |
//! |---|---|---|
//! | 1 | [`FrameKind::Output`] | Raw PTY bytes, escape sequences intact. |
//! | 2 | [`FrameKind::Exit`] | The child's status, as JSON. |
//! | 3 | [`FrameKind::Bell`] | Empty. |
//! | 4 | [`FrameKind::Osc133`] | The shell-integration event, as JSON. |
//! | 5 | [`FrameKind::Credit`] | A credit grant or ack, as JSON. |
//!
//! Zero is deliberately not a kind, and [`StreamId::RESERVED`] is deliberately not a stream,
//! so a zero-filled buffer is rejected twice over rather than read as a run of empty frames.
//!
//! **Do not hand-copy those bytes into a decoder.** ts-rs exports types and not values, so
//! the numbers do not reach TypeScript through ts-rs — they are generated instead.
//! [`crate::bindings`] renders them into `apps/web/src/generated/wireConstants.ts` as
//! `FRAME_KIND`, `FRAME_KIND_BY_BYTE` and `FRAME_HEADER_BYTES`, in the same
//! `cargo test export_bindings` step as the types and under the same drift guard, so a
//! client imports them rather than restating them. The table above is a reader's summary of
//! what that generator emits, not the authority a client should copy from: a wrong kind byte
//! misroutes binary data instead of failing to compile, which is exactly why D-13 is
//! enforced here by codegen and not by a comment.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::stream::StreamId;

/// The bytes a frame header occupies: one kind byte, a four-byte stream id, and a four-byte
/// length.
///
/// Generated into TypeScript rather than restated there. It grew from 5 to 9 when the stream
/// id was added, which is exactly the kind of change a hand-copied constant survives quietly
/// and a generated one does not.
pub const FRAME_HEADER_BYTES: usize = 9;

/// Where the stream id starts in the header.
const STREAM_ID_OFFSET: usize = 1;

/// Where the payload length starts in the header.
const LENGTH_OFFSET: usize = 5;

// The offsets and the header size have to describe the same nine bytes. They are three
// constants that can be edited independently, so the agreement is asserted at compile time
// rather than left as a property someone has to notice: a header laid out differently is a
// build failure here, not a wrong length at runtime.
const _: () = assert!(STREAM_ID_OFFSET == 1, "the kind byte comes first");
const _: () = assert!(
    LENGTH_OFFSET - STREAM_ID_OFFSET == 4,
    "the stream id is a u32"
);
const _: () = assert!(
    FRAME_HEADER_BYTES - LENGTH_OFFSET == 4,
    "the payload length is a u32"
);

/// The largest payload one frame may carry.
///
/// §7.3 chunks output at 48 KiB and flushes the coalescing window at 64 KiB, and the
/// pending cap is 256 KiB — so a legitimate frame is an order of magnitude under this. The
/// ceiling is not there to constrain the writer; it is there so a corrupted or hostile
/// length prefix asks for a bounded allocation. Without it, four bytes of garbage request
/// four gigabytes.
pub const MAX_FRAME_PAYLOAD_BYTES: usize = 1 << 20;

/// What a frame carries, named by its first byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
#[repr(u8)]
pub enum FrameKind {
    /// Raw PTY bytes, escape sequences intact, for the terminal surface to write.
    Output = 1,
    /// The child exited; the payload is its status.
    Exit = 2,
    /// The terminal rang. Empty payload.
    Bell = 3,
    /// A shell-integration event, intercepted by the VT handler before forwarding (§7.2).
    Osc133 = 4,
    /// Flow control: a credit grant from the reader, or an ack from the writer (§7.3).
    Credit = 5,
}

impl FrameKind {
    /// Every kind, in wire-byte order. The table the module docs describe.
    pub const ALL: [Self; 5] = [
        Self::Output,
        Self::Exit,
        Self::Bell,
        Self::Osc133,
        Self::Credit,
    ];

    /// The byte that names this kind in a header.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        self as u8
    }

    /// The wire spelling, which is also what this type exports to TypeScript.
    ///
    /// Kept in step with serde by a test rather than by convention, because
    /// [`crate::bindings`] keys the generated byte table on this and a disagreement would
    /// hand the webview a lookup table whose keys no client could produce.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Output => "output",
            Self::Exit => "exit",
            Self::Bell => "bell",
            Self::Osc133 => "osc133",
            Self::Credit => "credit",
        }
    }

    /// The kind a header byte names, or `None` if it names nothing.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Output),
            2 => Some(Self::Exit),
            3 => Some(Self::Bell),
            4 => Some(Self::Osc133),
            5 => Some(Self::Credit),
            _ => None,
        }
    }
}

/// One decoded frame.
///
/// Not exported to TypeScript on purpose. `payload` is opaque bytes, and ts-rs would render
/// it as `number[]` — an array of JavaScript numbers is not what arrives on a binary
/// channel, and publishing that shape would invite someone to build against it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// What the payload is.
    pub kind: FrameKind,
    /// Which session's stream this belongs to.
    pub stream: StreamId,
    /// The bytes, without the header.
    pub payload: Vec<u8>,
}

impl Frame {
    /// A frame of `kind` on `stream`, carrying `payload`.
    #[must_use]
    pub fn new(kind: FrameKind, stream: StreamId, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            kind,
            stream,
            payload: payload.into(),
        }
    }

    /// A frame with no payload — a [`FrameKind::Bell`], usually.
    #[must_use]
    pub fn empty(kind: FrameKind, stream: StreamId) -> Self {
        Self {
            kind,
            stream,
            payload: Vec::new(),
        }
    }

    /// The bytes this frame occupies on the wire, header included.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        FRAME_HEADER_BYTES + self.payload.len()
    }
}

/// Why a frame could not be encoded or decoded.
///
/// Every variant is unrecoverable *for that connection*. A stream whose length prefix, kind
/// byte or stream id is wrong cannot be resynchronised — there is no delimiter to scan
/// forward to — so the caller's only correct response is to drop the connection. That is why
/// "the buffer does not hold a whole frame yet" is not in here: it is a normal, expected
/// state, and modelling it as an error would have callers logging it thousands of times a
/// second.
///
/// A frame naming a stream id that is *syntactically* fine but not currently attached is the
/// same class of failure and gets the same response — see [`decode`], which cannot detect it,
/// and [`crate::stream`], which says who does.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    /// The first byte named no kind.
    #[error("frame kind {0} is not one of 1..=5")]
    UnknownKind(u8),
    /// The header carried [`StreamId::RESERVED`], which the daemon never assigns.
    #[error("stream id {0} is reserved and is never assigned to a session")]
    ReservedStream(u32),
    /// The length prefix asked for more than [`MAX_FRAME_PAYLOAD_BYTES`].
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
/// This, rather than a `Vec`-returning encoder, is what the coalescing writer wants: §7.3
/// packs frames into one buffer until it reaches 1 KiB, and allocating a fresh `Vec` per
/// frame to immediately copy it into another would be the opposite of the point.
///
/// # Errors
///
/// Returns [`FrameError::Oversized`] if the payload exceeds [`MAX_FRAME_PAYLOAD_BYTES`], and
/// [`FrameError::ReservedStream`] if the frame names [`StreamId::RESERVED`] — refused on the
/// way out as well as on the way in, so a writer cannot emit a frame its own decoder rejects.
pub fn encode_into(frame: &Frame, out: &mut Vec<u8>) -> Result<(), FrameError> {
    if frame.stream == StreamId::RESERVED {
        return Err(FrameError::ReservedStream(frame.stream.get()));
    }
    let len = frame.payload.len();
    let len = u32::try_from(len)
        .ok()
        .filter(|_| len <= MAX_FRAME_PAYLOAD_BYTES)
        .ok_or(FrameError::Oversized {
            len,
            max: MAX_FRAME_PAYLOAD_BYTES,
        })?;
    out.reserve(FRAME_HEADER_BYTES + frame.payload.len());
    out.push(frame.kind.as_byte());
    out.extend_from_slice(&frame.stream.get().to_be_bytes());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&frame.payload);
    Ok(())
}

/// Encode one frame on its own.
///
/// # Errors
///
/// The same as [`encode_into`].
pub fn encode(frame: &Frame) -> Result<Vec<u8>, FrameError> {
    let mut out = Vec::with_capacity(frame.encoded_len());
    encode_into(frame, &mut out)?;
    Ok(out)
}

/// Decode the frame at the front of `buf`.
///
/// `Ok(None)` means `buf` does not hold a whole frame *yet* — read more and call again.
/// `Ok(Some((frame, consumed)))` means `consumed` bytes were a frame and the rest is the
/// next one's beginning.
///
/// Each field is checked as soon as it has arrived, rather than after the whole frame has.
/// A stream whose very first byte is wrong is already unusable, and a decoder that waited
/// for a length it could not trust would sit on a dead connection until the buffer ceiling
/// stopped it, reporting nothing.
///
/// **What this cannot check.** A stream id that is well formed but names no attached session
/// is indistinguishable here from one that does: the decoder is pure and holds no session
/// table. The router owns that check, and the answer is the same one every other malformed
/// frame gets — drop the connection. See [`crate::stream`] for why it is not softer.
///
/// # Errors
///
/// Returns [`FrameError::UnknownKind`] if the first byte names no kind,
/// [`FrameError::ReservedStream`] if the stream id is [`StreamId::RESERVED`], and
/// [`FrameError::Oversized`] if the length prefix exceeds [`MAX_FRAME_PAYLOAD_BYTES`].
pub fn decode(buf: &[u8]) -> Result<Option<(Frame, usize)>, FrameError> {
    let Some(&kind_byte) = buf.first() else {
        return Ok(None);
    };
    let kind = FrameKind::from_byte(kind_byte).ok_or(FrameError::UnknownKind(kind_byte))?;

    // A four-element slice pattern rather than a fallible conversion with a default: if the
    // window is ever not four bytes the pattern simply does not match, so the decoder asks
    // for more input instead of fabricating a number. The `const` assertions above are what
    // stop that from being reachable in the first place.
    let Some(&[a, b, c, d]) = buf.get(STREAM_ID_OFFSET..LENGTH_OFFSET) else {
        return Ok(None);
    };
    let stream = StreamId(u32::from_be_bytes([a, b, c, d]));
    if stream == StreamId::RESERVED {
        return Err(FrameError::ReservedStream(stream.get()));
    }

    let Some(&[a, b, c, d]) = buf.get(LENGTH_OFFSET..FRAME_HEADER_BYTES) else {
        return Ok(None);
    };
    let len = u32::from_be_bytes([a, b, c, d]) as usize;
    if len > MAX_FRAME_PAYLOAD_BYTES {
        return Err(FrameError::Oversized {
            len,
            max: MAX_FRAME_PAYLOAD_BYTES,
        });
    }

    let end = FRAME_HEADER_BYTES + len;
    let Some(payload) = buf.get(FRAME_HEADER_BYTES..end) else {
        return Ok(None);
    };
    Ok(Some((Frame::new(kind, stream, payload), end)))
}

/// Reassembles frames from a stream that neither starts nor stops on frame boundaries.
///
/// Pure: it owns a buffer and nothing else. Push whatever a read produced, then pull frames
/// until it says there are none left. Consecutive frames routinely belong to different
/// sessions, so a caller routes on [`Frame::stream`] rather than assuming a run belongs
/// together.
///
/// On an error the decoder does **not** consume the offending bytes, so the same error
/// comes back on every subsequent call. That is deliberate — there is no delimiter to
/// resynchronise on, so silently skipping ahead would invent frames out of payload bytes.
/// Drop the connection.
///
/// ```
/// use nysia_proto::frame::{encode_into, Frame, FrameDecoder, FrameKind};
/// use nysia_proto::stream::StreamId;
///
/// let shell = StreamId(1);
/// let agent = StreamId(2);
///
/// let mut wire = Vec::new();
/// encode_into(&Frame::new(FrameKind::Output, shell, b"hel".as_slice()), &mut wire)?;
/// encode_into(&Frame::empty(FrameKind::Bell, agent), &mut wire)?;
///
/// // Delivered in two pieces that fall wherever the socket felt like — here, part-way
/// // through the first frame's header.
/// let (first, rest) = wire.split_at(6);
/// let mut decoder = FrameDecoder::new();
/// decoder.push(first);
/// assert_eq!(decoder.next_frame()?, None);
///
/// decoder.push(rest);
/// let output = decoder.next_frame()?.expect("the first frame is whole now");
/// assert_eq!(output.stream, shell);
/// assert_eq!(decoder.next_frame()?, Some(Frame::empty(FrameKind::Bell, agent)));
/// assert_eq!(decoder.next_frame()?, None);
/// # Ok::<(), nysia_proto::frame::FrameError>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
}

impl FrameDecoder {
    /// An empty decoder.
    #[must_use]
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
    #[must_use]
    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }

    /// Take the next whole frame, or `None` while one is still arriving.
    ///
    /// # Errors
    ///
    /// Propagates [`decode`]'s errors, without consuming the bytes that caused them.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, FrameError> {
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

    /// A seeded xorshift64, so the property tests are random in shape and identical run to
    /// run. Deliberately hand-rolled: a generator crate would mean touching
    /// `[workspace.dependencies]` and `Cargo.lock`, which are coordinator-owned.
    struct Rng(u64);

    impl Rng {
        fn new(seed: u64) -> Self {
            // xorshift is a fixed point at zero; any other seed is fine.
            Self(seed | 1)
        }

        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        /// A value in `0..n`.
        fn below(&mut self, n: usize) -> usize {
            if n == 0 {
                0
            } else {
                usize::try_from(self.next_u64() % n as u64).unwrap_or(0)
            }
        }
    }

    const SHELL: StreamId = StreamId(1);
    const AGENT: StreamId = StreamId(2);

    fn sample_frames() -> Vec<Frame> {
        vec![
            Frame::new(FrameKind::Output, SHELL, b"$ cargo test\r\n".as_slice()),
            Frame::empty(FrameKind::Bell, AGENT),
            Frame::new(
                FrameKind::Osc133,
                SHELL,
                br#"{"event":"prompt"}"#.as_slice(),
            ),
            Frame::new(
                FrameKind::Exit,
                AGENT,
                br#"{"outcome":"exited","code":0}"#.as_slice(),
            ),
            Frame::new(FrameKind::Credit, SHELL, br#"{"bytes":196608}"#.as_slice()),
        ]
    }

    #[test]
    fn the_header_is_a_kind_byte_a_stream_id_and_a_big_endian_length() {
        let frame = Frame::new(FrameKind::Output, StreamId(0x0102_0304), vec![0xAB; 258]);
        let wire = encode(&frame).unwrap();
        assert_eq!(wire[0], 1);
        // Both multi-byte fields are big-endian, and the stream id comes first.
        assert_eq!(&wire[1..5], &[0x01, 0x02, 0x03, 0x04]);
        assert_eq!(&wire[5..9], &[0x00, 0x00, 0x01, 0x02]);
        assert_eq!(wire.len(), FRAME_HEADER_BYTES + 258);
        assert_eq!(wire.len(), frame.encoded_len());
        assert_eq!(FRAME_HEADER_BYTES, 9);
    }

    #[test]
    fn every_kind_has_a_distinct_byte_and_round_trips() {
        let mut seen = Vec::new();
        for kind in FrameKind::ALL {
            assert_eq!(FrameKind::from_byte(kind.as_byte()), Some(kind));
            assert!(!seen.contains(&kind.as_byte()), "{kind:?} reuses a byte");
            seen.push(kind.as_byte());
        }
        // Zero is not a kind, so a zero-filled buffer is rejected rather than read as a
        // run of empty frames.
        assert_eq!(FrameKind::from_byte(0), None);
        assert_eq!(FrameKind::from_byte(6), None);
        assert_eq!(FrameKind::from_byte(u8::MAX), None);
    }

    #[test]
    fn a_kind_names_itself_in_json() {
        assert_eq!(
            serde_json::to_string(&FrameKind::Osc133).unwrap(),
            "\"osc133\""
        );
        for kind in FrameKind::ALL {
            let text = serde_json::to_string(&kind).unwrap();
            assert_eq!(serde_json::from_str::<FrameKind>(&text).unwrap(), kind);
            // `as_str` is what the generated byte table is keyed on, and ts-rs derives the
            // TypeScript union from the same serde attributes. If these two ever drift, the
            // webview gets a table whose keys no client could produce.
            assert_eq!(text, format!("\"{}\"", kind.as_str()));
        }
    }

    #[test]
    fn an_empty_payload_is_a_whole_frame() {
        let wire = encode(&Frame::empty(FrameKind::Bell, SHELL)).unwrap();
        assert_eq!(wire, [3, 0, 0, 0, 1, 0, 0, 0, 0]);
        let (frame, consumed) = decode(&wire).unwrap().unwrap();
        assert_eq!(consumed, FRAME_HEADER_BYTES);
        assert_eq!(frame, Frame::empty(FrameKind::Bell, SHELL));
        assert!(frame.payload.is_empty());
    }

    #[test]
    fn every_short_prefix_asks_for_more_rather_than_failing() {
        // The header is nine bytes now, so there are four more places a read can stop
        // inside it than there used to be. Every one of them has to mean "not yet".
        let wire = encode(&Frame::new(FrameKind::Output, SHELL, b"hello".as_slice())).unwrap();
        for cut in 0..wire.len() {
            assert_eq!(
                decode(&wire[..cut]),
                Ok(None),
                "a {cut}-byte prefix should not decode"
            );
        }
        assert!(decode(&wire).unwrap().is_some());
    }

    #[test]
    fn an_unknown_kind_fails_immediately_rather_than_waiting_for_a_payload() {
        // One byte, nothing else. A decoder that waited for the header before checking the
        // kind would sit on a dead connection reporting nothing.
        assert_eq!(decode(&[0]), Err(FrameError::UnknownKind(0)));
        assert_eq!(decode(&[99]), Err(FrameError::UnknownKind(99)));
        assert_eq!(decode(&[7, 0, 0]), Err(FrameError::UnknownKind(7)));
    }

    #[test]
    fn the_reserved_stream_is_refused_as_soon_as_it_has_arrived() {
        // Five bytes: enough for the kind and the stream id, and not the length. The check
        // lands there rather than after the whole header, for the same reason the kind
        // check lands at byte one.
        assert_eq!(
            decode(&[FrameKind::Output.as_byte(), 0, 0, 0, 0]),
            Err(FrameError::ReservedStream(0))
        );
        // Four bytes is still "not yet" — the id is not complete.
        assert_eq!(decode(&[FrameKind::Output.as_byte(), 0, 0, 0]), Ok(None));

        // A zero-filled buffer is now refused twice over: byte 0 is not a kind, and if it
        // somehow were, stream 0 is not a stream.
        assert_eq!(decode(&[0; 16]), Err(FrameError::UnknownKind(0)));

        // And a writer cannot produce what its own decoder would refuse.
        assert_eq!(
            encode(&Frame::empty(FrameKind::Bell, StreamId::RESERVED)),
            Err(FrameError::ReservedStream(0))
        );
    }

    #[test]
    fn an_oversized_length_is_refused_before_anything_is_allocated() {
        let mut header = vec![FrameKind::Output.as_byte()];
        header.extend_from_slice(&SHELL.get().to_be_bytes());
        header.extend_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(
            decode(&header),
            Err(FrameError::Oversized {
                len: u32::MAX as usize,
                max: MAX_FRAME_PAYLOAD_BYTES,
            })
        );

        // One byte over is also one byte over.
        let mut just_over = vec![FrameKind::Output.as_byte()];
        just_over.extend_from_slice(&SHELL.get().to_be_bytes());
        let len = u32::try_from(MAX_FRAME_PAYLOAD_BYTES + 1).unwrap();
        just_over.extend_from_slice(&len.to_be_bytes());
        assert!(matches!(
            decode(&just_over),
            Err(FrameError::Oversized { .. })
        ));

        // Exactly at the ceiling is legal, and the header alone is not yet the frame.
        let mut at_ceiling = vec![FrameKind::Output.as_byte()];
        at_ceiling.extend_from_slice(&SHELL.get().to_be_bytes());
        let len = u32::try_from(MAX_FRAME_PAYLOAD_BYTES).unwrap();
        at_ceiling.extend_from_slice(&len.to_be_bytes());
        assert_eq!(decode(&at_ceiling), Ok(None));
    }

    #[test]
    fn encoding_refuses_a_payload_the_decoder_would_refuse() {
        let too_big = Frame::new(
            FrameKind::Output,
            SHELL,
            vec![0; MAX_FRAME_PAYLOAD_BYTES + 1],
        );
        assert_eq!(
            encode(&too_big),
            Err(FrameError::Oversized {
                len: MAX_FRAME_PAYLOAD_BYTES + 1,
                max: MAX_FRAME_PAYLOAD_BYTES,
            })
        );
        assert!(
            encode(&Frame::new(
                FrameKind::Output,
                SHELL,
                vec![0; MAX_FRAME_PAYLOAD_BYTES]
            ))
            .is_ok()
        );
    }

    #[test]
    fn frames_for_several_streams_arriving_in_one_buffer_all_come_back() {
        // The ≥1 KiB coalescing case (§7.3), which multiplexing makes the *normal* case:
        // consecutive frames in one window routinely belong to different sessions.
        let frames = sample_frames();
        let mut wire = Vec::new();
        for frame in &frames {
            encode_into(frame, &mut wire).unwrap();
        }
        assert!(
            wire.len() < 1024,
            "the fixtures should be small enough to pack"
        );

        let mut decoder = FrameDecoder::new();
        decoder.push(&wire);
        let mut decoded = Vec::new();
        while let Some(frame) = decoder.next_frame().unwrap() {
            decoded.push(frame);
        }
        assert_eq!(decoded, frames);
        assert_eq!(decoder.buffered(), 0);

        // Interleaved, not grouped: a router that assumed a run belonged to one session
        // would send the bell to the shell.
        let streams: Vec<StreamId> = decoded.iter().map(|frame| frame.stream).collect();
        assert_eq!(streams, [SHELL, AGENT, SHELL, AGENT, SHELL]);
    }

    #[test]
    fn one_frame_split_across_buffers_is_reassembled() {
        let frame = Frame::new(
            FrameKind::Output,
            StreamId(0xDEAD_BEEF),
            b"a moderately long payload".as_slice(),
        );
        let wire = encode(&frame).unwrap();
        for split in 0..=wire.len() {
            let mut decoder = FrameDecoder::new();
            decoder.push(&wire[..split]);
            if split < wire.len() {
                assert_eq!(decoder.next_frame().unwrap(), None, "split at {split}");
                assert_eq!(decoder.buffered(), split);
            }
            decoder.push(&wire[split..]);
            assert_eq!(decoder.next_frame().unwrap(), Some(frame.clone()));
            assert_eq!(decoder.next_frame().unwrap(), None);
            assert_eq!(decoder.buffered(), 0);
        }
    }

    #[test]
    fn a_split_inside_the_header_is_reassembled_for_every_field() {
        // The header is nine bytes across three fields, so a read can now stop with a
        // half-read stream id or a half-read length. Both used to be impossible.
        let frame = Frame::new(FrameKind::Exit, StreamId(0x0A0B_0C0D), b"xy".as_slice());
        let wire = encode(&frame).unwrap();
        for split in 1..FRAME_HEADER_BYTES {
            let mut decoder = FrameDecoder::new();
            decoder.push(&wire[..split]);
            assert_eq!(
                decoder.next_frame().unwrap(),
                None,
                "a header cut at {split} should not decode"
            );
            decoder.push(&wire[split..]);
            assert_eq!(
                decoder.next_frame().unwrap(),
                Some(frame.clone()),
                "a header cut at {split} should reassemble"
            );
        }
    }

    #[test]
    fn a_poisoned_decoder_keeps_reporting_rather_than_resynchronising() {
        let mut decoder = FrameDecoder::new();
        decoder.push(&[0xFF, 0, 0, 0, 1, 0, 0, 0, 0]);
        assert_eq!(decoder.next_frame(), Err(FrameError::UnknownKind(0xFF)));
        // Same answer, same bytes: there is nothing to resynchronise on, so skipping ahead
        // would invent frames out of payload bytes.
        assert_eq!(decoder.next_frame(), Err(FrameError::UnknownKind(0xFF)));
        assert_eq!(decoder.buffered(), 9);
    }

    #[test]
    fn random_frames_survive_random_chunking() {
        let mut rng = Rng::new(0x5EED_1234_ABCD_0001);
        for round in 0..200 {
            let count = 1 + rng.below(8);
            let frames: Vec<Frame> = (0..count)
                .map(|_| {
                    let kind = FrameKind::ALL[rng.below(FrameKind::ALL.len())];
                    // Never zero: that is the reserved id, and the encoder refuses it.
                    let stream = StreamId(u32::try_from(1 + rng.below(6)).unwrap_or(1));
                    let len = rng.below(600);
                    let payload: Vec<u8> = (0..len)
                        .map(|_| u8::try_from(rng.below(256)).unwrap_or(0))
                        .collect();
                    Frame::new(kind, stream, payload)
                })
                .collect();

            let mut wire = Vec::new();
            for frame in &frames {
                encode_into(frame, &mut wire).unwrap();
            }

            let mut decoder = FrameDecoder::new();
            let mut decoded = Vec::new();
            let mut at = 0;
            while at < wire.len() {
                // Chunks land wherever the socket felt like, including mid-header.
                let take = (1 + rng.below(97)).min(wire.len() - at);
                decoder.push(&wire[at..at + take]);
                at += take;
                while let Some(frame) = decoder.next_frame().unwrap() {
                    decoded.push(frame);
                }
            }
            assert_eq!(decoded, frames, "round {round}");
            assert_eq!(decoder.buffered(), 0, "round {round}");
        }
    }

    #[test]
    fn a_payload_that_looks_like_a_header_is_still_a_payload() {
        // The length prefix is the only framing; there is no delimiter to scan for, which
        // is exactly why a desynchronised stream cannot be recovered.
        let inner = encode(&Frame::new(FrameKind::Bell, AGENT, b"".as_slice())).unwrap();
        let outer = Frame::new(FrameKind::Output, SHELL, inner);
        let wire = encode(&outer).unwrap();

        let mut decoder = FrameDecoder::new();
        decoder.push(&wire);
        assert_eq!(decoder.next_frame().unwrap(), Some(outer));
        assert_eq!(decoder.next_frame().unwrap(), None);
    }
}
