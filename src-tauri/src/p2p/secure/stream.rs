use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use anyhow::bail;
use iroh::SecretKey;
use iroh::endpoint::Connection;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::{
    HandshakeIdentity, PathKind, PathTrust, RECORD_CLOSE, RECORD_DATA, RECORD_HEADER_LEN,
    SecureError, SessionKeys, helper_handshake_request,
};

const LENGTH_PREFIX_LEN: usize = 4;
const TAG_LEN: usize = 16;
pub(super) const MAX_PLAINTEXT_CHUNK: usize = 60 * 1024;
const MIN_RECORD_LEN: usize = RECORD_HEADER_LEN + TAG_LEN;
const MAX_RECORD_LEN: usize = RECORD_HEADER_LEN + MAX_PLAINTEXT_CHUNK + TAG_LEN;

pub type GuestReader = Box<dyn AsyncRead + Unpin + Send>;
pub type GuestWriter = Box<dyn AsyncWrite + Unpin + Send>;

/// Opens one guest request stream, sealing helper-bridge paths end to end.
///
/// # Errors
///
/// Returns an error when the transport cannot open a stream or the helper
/// handshake fails identity verification.
pub async fn open_guest_stream(
    connection: &Connection,
    trust: &PathTrust,
    secret: &SecretKey,
) -> anyhow::Result<(GuestWriter, GuestReader)> {
    let (mut send, mut recv) = connection.open_bi().await?;
    if trust.kind() != PathKind::HelperBridge {
        return Ok((Box::new(send), Box::new(recv)));
    }
    if trust.helper().is_none() {
        bail!("helper path trust is missing the helper identity");
    }
    let identity = HandshakeIdentity::new(secret.clone());
    let keys = helper_handshake_request(&mut recv, &mut send, trust, &identity).await?;
    let sealed = SealedStream::new(recv, send, keys);
    let (reader, writer) = tokio::io::split(sealed);
    Ok((Box::new(writer), Box::new(reader)))
}

/// Bidirectional plaintext stream sealed with AES-GCM records in both directions.
///
/// Length prefixes and record buffers are bounded; partial reads and writes are
/// kept in stream state so cancellation never duplicates or drops a record.
pub struct SealedStream<R, W> {
    reader: R,
    writer: W,
    keys: SessionKeys,

    read_header: [u8; LENGTH_PREFIX_LEN],
    read_header_len: usize,
    read_record: Vec<u8>,
    read_record_len: usize,
    read_plain: Vec<u8>,
    read_plain_offset: usize,
    read_eof: bool,

    write_frame: Vec<u8>,
    write_frame_offset: usize,
    close_sent: bool,
}

impl<R, W> SealedStream<R, W> {
    /// Wraps a plaintext-capable byte stream pair with a negotiated session.
    #[must_use]
    pub fn new(reader: R, writer: W, keys: SessionKeys) -> Self {
        Self {
            reader,
            writer,
            keys,
            read_header: [0u8; LENGTH_PREFIX_LEN],
            read_header_len: 0,
            read_record: Vec::new(),
            read_record_len: 0,
            read_plain: Vec::new(),
            read_plain_offset: 0,
            read_eof: false,
            write_frame: Vec::new(),
            write_frame_offset: 0,
            close_sent: false,
        }
    }
}

impl<R, W> SealedStream<R, W>
where
    W: AsyncWrite + Unpin,
{
    fn poll_flush_frame(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        while self.write_frame_offset < self.write_frame.len() {
            match Pin::new(&mut self.writer)
                .poll_write(cx, &self.write_frame[self.write_frame_offset..])
            {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Ok(0)) => {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "sealed stream write made no progress",
                    )));
                }
                Poll::Ready(Ok(written)) => self.write_frame_offset += written,
                Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
            }
        }
        self.write_frame.clear();
        self.write_frame_offset = 0;
        Poll::Ready(Ok(()))
    }

    fn queue_record(&mut self, record: Vec<u8>) -> Result<(), SecureError> {
        let len = u32::try_from(record.len()).map_err(|_| SecureError::InvalidMessage)?;
        self.write_frame.clear();
        self.write_frame.extend_from_slice(&len.to_be_bytes());
        self.write_frame.extend_from_slice(&record);
        self.write_frame_offset = 0;
        Ok(())
    }
}

impl<R, W> AsyncRead for SealedStream<R, W>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            if this.read_eof {
                return Poll::Ready(Ok(()));
            }
            if this.read_plain_offset < this.read_plain.len() {
                let remaining = this.read_plain.len() - this.read_plain_offset;
                let count = remaining.min(buf.remaining());
                buf.put_slice(
                    &this.read_plain[this.read_plain_offset..this.read_plain_offset + count],
                );
                this.read_plain_offset += count;
                return Poll::Ready(Ok(()));
            }

            while this.read_header_len < LENGTH_PREFIX_LEN {
                let mut scratch = [0u8; LENGTH_PREFIX_LEN];
                let mut scratch_buf =
                    ReadBuf::new(&mut scratch[..LENGTH_PREFIX_LEN - this.read_header_len]);
                match Pin::new(&mut this.reader).poll_read(cx, &mut scratch_buf) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Ok(())) => {
                        let filled = scratch_buf.filled();
                        if filled.is_empty() {
                            return Poll::Ready(Err(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "sealed stream ended inside a length prefix",
                            )));
                        }
                        this.read_header[this.read_header_len..this.read_header_len + filled.len()]
                            .copy_from_slice(filled);
                        this.read_header_len += filled.len();
                    }
                    Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
                }
            }
            let frame_len = u32::from_be_bytes(this.read_header);
            let frame_len = usize::try_from(frame_len).map_err(|_| invalid_length())?;
            if !(MIN_RECORD_LEN..=MAX_RECORD_LEN).contains(&frame_len) {
                return Poll::Ready(Err(invalid_length()));
            }
            this.read_header_len = 0;
            this.read_record.clear();
            this.read_record.resize(frame_len, 0);
            this.read_record_len = 0;

            while this.read_record_len < frame_len {
                let remaining = frame_len - this.read_record_len;
                let take = remaining.min(1024);
                let mut scratch = [0u8; 1024];
                let mut scratch_buf = ReadBuf::new(&mut scratch[..take]);
                match Pin::new(&mut this.reader).poll_read(cx, &mut scratch_buf) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Ok(())) => {
                        let filled = scratch_buf.filled();
                        if filled.is_empty() {
                            return Poll::Ready(Err(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "sealed stream ended inside a record",
                            )));
                        }
                        this.read_record[this.read_record_len..this.read_record_len + filled.len()]
                            .copy_from_slice(filled);
                        this.read_record_len += filled.len();
                    }
                    Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
                }
            }

            let record = std::mem::take(&mut this.read_record);
            this.read_record_len = 0;
            let (record_type, plaintext) = this.keys.open(&record).map_err(invalid_data)?;
            match record_type {
                RECORD_DATA => {
                    if plaintext.is_empty() {
                        continue;
                    }
                    this.read_plain = plaintext;
                    this.read_plain_offset = 0;
                }
                RECORD_CLOSE => {
                    if !plaintext.is_empty() {
                        return Poll::Ready(Err(invalid_data(SecureError::InvalidMessage)));
                    }
                    this.read_eof = true;
                    return Poll::Ready(Ok(()));
                }
                _ => return Poll::Ready(Err(invalid_data(SecureError::InvalidMessage))),
            }
        }
    }
}

impl<R, W> AsyncWrite for SealedStream<R, W>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.close_sent {
            return Poll::Ready(Err(broken_pipe()));
        }
        match this.poll_flush_frame(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
            Poll::Ready(Ok(())) => {}
        }
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let count = buf.len().min(MAX_PLAINTEXT_CHUNK);
        let record = match this.keys.seal(RECORD_DATA, &buf[..count]) {
            Ok(record) => record,
            Err(err) => return Poll::Ready(Err(invalid_data(err))),
        };
        if let Err(err) = this.queue_record(record) {
            return Poll::Ready(Err(invalid_data(err)));
        }
        Poll::Ready(Ok(count))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match this.poll_flush_frame(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
            Poll::Ready(Ok(())) => {}
        }
        Pin::new(&mut this.writer).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if !this.close_sent {
            match this.poll_flush_frame(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
                Poll::Ready(Ok(())) => {}
            }
            let record = match this.keys.seal(RECORD_CLOSE, &[]) {
                Ok(record) => record,
                Err(err) => return Poll::Ready(Err(invalid_data(err))),
            };
            if let Err(err) = this.queue_record(record) {
                return Poll::Ready(Err(invalid_data(err)));
            }
            this.close_sent = true;
        }
        match this.poll_flush_frame(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
            Poll::Ready(Ok(())) => {}
        }
        Pin::new(&mut this.writer).poll_shutdown(cx)
    }
}

fn invalid_length() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "sealed record length invalid")
}

fn invalid_data(error: SecureError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

fn broken_pipe() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "sealed stream is closed")
}
