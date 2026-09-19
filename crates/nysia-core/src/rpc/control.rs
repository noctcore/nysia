//! Newline-delimited JSON: the handshake and every control frame after it.
//!
//! One frame per line, one JSON object per frame. That is the whole format, and the reason
//! it is worth a module is the bound: a peer that opens a connection and sends a megabyte
//! with no newline in it must not be able to make the daemon hold that megabyte, and then
//! the next one. [`MAX_CONTROL_LINE_BYTES`] is where the daemon stops reading and answers
//! rather than growing, and the refusal is the same one a malformed frame gets, because from
//! the outside they are the same mistake.
//!
//! The reader is deliberately *not* `tokio::io::AsyncBufReadExt::read_line`. That method
//! grows without limit, and the limit is the point.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::error::Category;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// The longest control frame the daemon will read before refusing the connection.
///
/// A megabyte is far more than any verb needs — the largest is a `terminal_send` carrying
/// pasted text — and far less than a peer could use to exhaust the daemon. It matches
/// [`nysia_proto::MAX_FRAME_PAYLOAD_BYTES`], so neither framing is the cheaper way to
/// misbehave.
pub const MAX_CONTROL_LINE_BYTES: usize = nysia_proto::MAX_FRAME_PAYLOAD_BYTES;

/// Why a control frame could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    /// The connection failed underneath.
    #[error("the control connection failed: {0}")]
    Io(#[from] std::io::Error),
    /// A line exceeded [`MAX_CONTROL_LINE_BYTES`] without a newline.
    #[error(
        "a control frame exceeded {MAX_CONTROL_LINE_BYTES} bytes with no newline; frames are \
         newline-delimited JSON, one per line"
    )]
    LineTooLong,
    /// The line was not the JSON this frame should have held.
    ///
    /// **Displayed as serde's category and position, never its message**, and not a
    /// `#[source]` either, so no chain printer reaches the message by another road.
    /// `serde_json::Error`'s `Display` echoes the scalar it choked on, and the one that
    /// arrives in practice is a path: a CLI from before `cwd` became an object sends
    /// `--cwd <folder>` as a string, and the message reads `invalid type: string
    /// "C:\\Users\\…\\a-clients-private-repo"`. The daemon writes this error into its log and
    /// into the answer, and `rpc::log_file` allows neither to carry a request body. The same
    /// split `rpc::tasks` makes for `gh`'s answer.
    #[error("could not read a control frame: {}", fault(.0))]
    Malformed(serde_json::Error),
    /// The peer closed the connection before finishing a frame.
    #[error("the peer closed the connection part way through a control frame")]
    Truncated,
}

/// What was wrong with a frame, as a phrase and a position that repeat nothing from it.
fn fault(err: &serde_json::Error) -> String {
    let what = match err.classify() {
        Category::Io => "the connection failed while it was being read",
        Category::Syntax => "it is not JSON",
        Category::Data => "it is JSON, but not in the shape this frame takes",
        Category::Eof => "it ends part way through a value",
    };
    format!("{what} (line {}, column {})", err.line(), err.column())
}

/// Reads newline-delimited JSON frames, bounded.
pub struct ControlReader<R> {
    inner: BufReader<R>,
}

impl<R: AsyncRead + Unpin> ControlReader<R> {
    /// Read frames from `reader`.
    pub fn new(reader: R) -> Self {
        Self {
            inner: BufReader::new(reader),
        }
    }

    /// Read the next line, or `None` at a clean end of stream.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::LineTooLong`] when the peer sends more than
    /// [`MAX_CONTROL_LINE_BYTES`] without a newline, [`ControlError::Truncated`] when the
    /// connection ends part way through a line, and [`ControlError::Io`] when it fails.
    pub async fn read_line(&mut self) -> Result<Option<String>, ControlError> {
        let mut line = Vec::new();
        loop {
            let available = self.inner.fill_buf().await?;
            if available.is_empty() {
                // A clean end of stream between frames is how every well-behaved client
                // leaves. In the middle of one it is a truncation, and those have to be told
                // apart — the first is the normal disconnect D-1 is about.
                return if line.is_empty() {
                    Ok(None)
                } else {
                    Err(ControlError::Truncated)
                };
            }
            match available.iter().position(|byte| *byte == b'\n') {
                Some(end) => {
                    if line.len() + end > MAX_CONTROL_LINE_BYTES {
                        return Err(ControlError::LineTooLong);
                    }
                    line.extend_from_slice(&available[..end]);
                    self.inner.consume(end + 1);
                    // A `\r\n` peer is not worth refusing over one byte.
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
                }
                None => {
                    let taken = available.len();
                    if line.len() + taken > MAX_CONTROL_LINE_BYTES {
                        // Stop here rather than consuming: the connection is about to be
                        // dropped, and buffering the rest of a frame nobody will read would
                        // be doing the peer's work for it.
                        return Err(ControlError::LineTooLong);
                    }
                    line.extend_from_slice(available);
                    self.inner.consume(taken);
                }
            }
        }
    }

    /// Take the reader back, along with whatever it had already buffered.
    ///
    /// The buffered bytes come first and must not be dropped: the handshake and the frames
    /// after it arrive on one connection, so a single `read` can easily deliver the `hello`
    /// line *and* the first binary frame behind it. Returning the reader alone would discard
    /// that frame and leave a stream that never starts.
    pub fn into_parts(self) -> (Vec<u8>, R) {
        let buffered = self.inner.buffer().to_vec();
        (buffered, self.inner.into_inner())
    }

    /// Read the next frame as `T`, or `None` at a clean end of stream.
    ///
    /// # Errors
    ///
    /// As [`ControlReader::read_line`], plus [`ControlError::Malformed`] when the line is not
    /// the frame it should have been.
    pub async fn read_frame<T: DeserializeOwned>(&mut self) -> Result<Option<T>, ControlError> {
        let Some(line) = self.read_line().await? else {
            return Ok(None);
        };
        serde_json::from_str(&line)
            .map(Some)
            .map_err(ControlError::Malformed)
    }
}

/// Writes newline-delimited JSON frames.
pub struct ControlWriter<W> {
    inner: W,
}

impl<W: AsyncWrite + Unpin> ControlWriter<W> {
    /// Write frames to `writer`.
    pub fn new(writer: W) -> Self {
        Self { inner: writer }
    }

    /// Take the writer back.
    ///
    /// Safe at any point because [`ControlWriter::write_frame`] flushes every frame: there is
    /// never anything held back to lose.
    pub fn into_inner(self) -> W {
        self.inner
    }

    /// Write one frame, terminated and flushed.
    ///
    /// Flushed every time rather than on a timer: a control frame is an answer somebody is
    /// blocked on, and a buffered reply is an answer that has not arrived.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::Malformed`] if the value will not serialise, and
    /// [`ControlError::Io`] if the write fails.
    pub async fn write_frame<T: Serialize>(&mut self, value: &T) -> Result<(), ControlError> {
        let mut line = serde_json::to_vec(value).map_err(ControlError::Malformed)?;
        line.push(b'\n');
        self.inner.write_all(&line).await?;
        self.inner.flush().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nysia_proto::{ClientRole, HelloRequest, PROTOCOL_VERSION};

    fn hello() -> HelloRequest {
        HelloRequest::new(
            PROTOCOL_VERSION,
            ClientRole::Control,
            "nysia-test".parse().expect("a well-formed client id"),
        )
    }

    #[tokio::test]
    async fn a_frame_written_is_the_frame_read_back() {
        let mut buffer = Vec::new();
        ControlWriter::new(&mut buffer)
            .write_frame(&hello())
            .await
            .expect("writes");
        assert!(buffer.ends_with(b"\n"), "frames are newline-delimited");

        let mut reader = ControlReader::new(buffer.as_slice());
        let read: HelloRequest = reader.read_frame().await.expect("reads").expect("is there");
        assert_eq!(read, hello());
        assert!(
            reader
                .read_frame::<HelloRequest>()
                .await
                .expect("reads")
                .is_none(),
            "a clean end of stream is not an error"
        );
    }

    #[tokio::test]
    async fn several_frames_arrive_in_order_however_the_bytes_were_chunked() {
        let mut buffer = Vec::new();
        {
            let mut writer = ControlWriter::new(&mut buffer);
            for _ in 0..3 {
                writer.write_frame(&hello()).await.expect("writes");
            }
        }
        let mut reader = ControlReader::new(buffer.as_slice());
        for _ in 0..3 {
            assert_eq!(
                reader
                    .read_frame::<HelloRequest>()
                    .await
                    .expect("reads")
                    .expect("is there"),
                hello()
            );
        }
    }

    #[tokio::test]
    async fn a_peer_that_never_sends_a_newline_is_refused_rather_than_buffered() {
        // Without the bound, this is a peer that makes the daemon hold a megabyte, and then
        // another, for as long as it feels like.
        let flood = vec![b'x'; MAX_CONTROL_LINE_BYTES + 1];
        let mut reader = ControlReader::new(flood.as_slice());
        assert!(matches!(
            reader.read_line().await,
            Err(ControlError::LineTooLong)
        ));
    }

    #[tokio::test]
    async fn a_line_that_just_fits_is_still_read() {
        // The bound has to refuse the flood without refusing a large legitimate paste, so
        // the boundary itself is worth a test rather than an assumption.
        let mut line = vec![b'"'; 1];
        line.extend(std::iter::repeat_n(b'x', MAX_CONTROL_LINE_BYTES - 2));
        line.push(b'"');
        line.push(b'\n');
        let mut reader = ControlReader::new(line.as_slice());
        let read: String = reader.read_frame().await.expect("reads").expect("is there");
        assert_eq!(read.len(), MAX_CONTROL_LINE_BYTES - 2);
    }

    #[tokio::test]
    async fn a_truncated_frame_is_not_mistaken_for_a_clean_disconnect() {
        let mut reader = ControlReader::new(&b"{\"type\":\"hel"[..]);
        assert!(matches!(
            reader.read_line().await,
            Err(ControlError::Truncated)
        ));
    }

    #[tokio::test]
    async fn a_line_that_is_not_the_frame_it_should_be_is_malformed_and_not_fatal_to_the_reader() {
        let mut reader = ControlReader::new(&b"{\"type\":\"goodbye\"}\n{}\n"[..]);
        assert!(matches!(
            reader.read_frame::<HelloRequest>().await,
            Err(ControlError::Malformed(_))
        ));
        // The connection is still readable: the frame was bad, not the stream. That is what
        // lets the daemon answer a malformed `hello` with a rejection instead of a hang-up.
        assert!(reader.read_line().await.expect("reads").is_some());
    }

    #[test]
    fn the_old_shape_fixture_is_json_in_the_wrong_shape() {
        // The leak checks that use the fixture are only as good as the fixture: a frame
        // serde rejects as a syntax error has a message that quotes nothing, and every check
        // fed one passes whatever the daemon does with that message. So this holds it to the
        // case that *can* leak — serde's own message, unfiltered, repeats the folder.
        let fixture = crate::rpc::testing::OLD_SHAPE_CREATE;
        serde_json::from_str::<serde_json::Value>(fixture).expect("the fixture is JSON");
        let serde_said = serde_json::from_str::<nysia_proto::RequestEnvelope>(fixture)
            .expect_err("a string where an object belongs does not parse");
        assert_eq!(serde_said.classify(), Category::Data);
        assert!(
            serde_said.to_string().contains("a-clients-private-repo"),
            "serde no longer quotes the value it could not read, so the leak checks below \
             prove nothing: {serde_said}"
        );
    }

    #[tokio::test]
    async fn a_frame_that_will_not_parse_is_described_without_repeating_what_it_held() {
        // The mechanism behind the daemon's log line and its answer to a malformed frame:
        // both are this error's `Display`. serde's own message would print the folder whole.
        // `rpc::server`'s `a_malformed_frame_leaves_no_path_in_the_log_or_the_answer` holds
        // the two places it is written.
        let mut reader = ControlReader::new(crate::rpc::testing::OLD_SHAPE_CREATE.as_bytes());
        let err = reader
            .read_frame::<nysia_proto::RequestEnvelope>()
            .await
            .expect_err("a string where an object belongs does not parse");
        assert!(matches!(err, ControlError::Malformed(_)), "{err:?}");

        let said = err.to_string();
        assert!(
            !said.contains("a-clients-private-repo") && !said.contains("Projekty"),
            "the error repeats the frame it could not read: {said}"
        );
        // What it says instead, so this cannot pass by saying nothing.
        assert!(said.contains("not in the shape this frame takes"), "{said}");
        assert!(said.contains("line 1, column"), "{said}");
        assert!(
            std::error::Error::source(&err).is_none(),
            "a source hands serde's message to any printer that walks the chain"
        );
    }
}
