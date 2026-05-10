use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use byteorder::{BigEndian, ReadBytesExt};
use flate2::read::GzDecoder;
use thiserror::Error;

use crate::message::{LogMessage, MessageId};
use crate::{FILE_HEADER, FILE_VERSION, INDEXED_MARKER};

/// Lightweight metadata about a log message.
#[derive(Debug, Clone, Copy)]
pub struct LogMessageInfo {
    pub timestamp_ns: i64,
    pub message_id: MessageId,
    pub payload_len: usize,
}

/// Progress information for an in-flight read.
#[derive(Debug, Clone, Copy)]
pub struct ReadProgress {
    pub bytes_read: u64,
    pub total_bytes: u64,
}

impl ReadProgress {
    pub fn fraction(self) -> f64 {
        if self.total_bytes == 0 {
            0.0
        } else {
            self.bytes_read as f64 / self.total_bytes as f64
        }
    }
}

/// Errors that can occur when reading log files.
#[derive(Debug, Error)]
pub enum ReadError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("Invalid log file header: expected 'SSL_LOG_FILE', found {found:?}")]
    InvalidHeader { found: String },

    #[error("Unsupported log file version: {version} (expected {FILE_VERSION})")]
    UnsupportedVersion { version: i32 },

    #[error("Invalid message length: {length}")]
    InvalidLength { length: i32 },

    #[error("Unknown message ID: {id}")]
    UnknownMessageId { id: i32 },

    #[error("Random access is not supported for compressed log files")]
    NoRandomAccessForCompressed,
}

/// A reader that can be either buffered file or buffered gzip.
enum ReaderInner {
    Plain(BufReader<CountingReader<File>>),
    Gzip(BufReader<GzDecoder<CountingReader<File>>>),
}

impl Read for ReaderInner {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            ReaderInner::Plain(r) => r.read(buf),
            ReaderInner::Gzip(r) => r.read(buf),
        }
    }
}

impl ReaderInner {
    fn bytes_read(&self) -> u64 {
        match self {
            ReaderInner::Plain(reader) => reader.get_ref().bytes_read(),
            ReaderInner::Gzip(reader) => reader.get_ref().get_ref().bytes_read(),
        }
    }
}

struct CountingReader<R> {
    inner: R,
    bytes_read: u64,
}

impl<R> CountingReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            bytes_read: 0,
        }
    }

    fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.bytes_read += read as u64;
        Ok(read)
    }
}

impl<R: Seek> Seek for CountingReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let offset = self.inner.seek(pos)?;
        self.bytes_read = offset;
        Ok(offset)
    }
}

/// Reads SSL log files, both plain and gzip-compressed.
///
/// # Examples
///
/// ```no_run
/// use loguna::LogReader;
///
/// let mut reader = LogReader::open("game.log").unwrap();
/// while let Some(msg) = reader.next_message().unwrap() {
///     println!("Message type: {}, timestamp: {}ns", msg.message_id, msg.timestamp_ns);
/// }
/// ```
pub struct LogReader {
    reader: ReaderInner,
    /// Keep a separate handle for random access / index reading.
    /// Only available for non-compressed files.
    file: Option<File>,
    compressed: bool,
    total_bytes: u64,
}

impl LogReader {
    /// Opens a log file for reading.
    ///
    /// Gzip-compressed files are detected by the `.gz` extension and
    /// decompressed transparently.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, ReadError> {
        let path = path.as_ref();
        let compressed = path.extension().map_or(false, |ext| ext == "gz");

        let total_bytes = path.metadata()?.len();
        let file = File::open(path)?;

        let (reader, random_access_file) = if compressed {
            let gz = GzDecoder::new(CountingReader::new(file));
            (ReaderInner::Gzip(BufReader::new(gz)), None)
        } else {
            let random_file = File::open(path)?; // second handle for random access
            (
                ReaderInner::Plain(BufReader::new(CountingReader::new(file))),
                Some(random_file),
            )
        };

        let mut log_reader = LogReader {
            reader,
            file: random_access_file,
            compressed,
            total_bytes,
        };

        log_reader.verify_header()?;
        Ok(log_reader)
    }

    /// Whether the underlying file is gzip-compressed.
    pub fn is_compressed(&self) -> bool {
        self.compressed
    }

    /// Total input size in bytes.
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// Bytes consumed from the input stream.
    pub fn bytes_read(&self) -> u64 {
        self.reader.bytes_read()
    }

    /// Current read progress.
    pub fn progress(&self) -> ReadProgress {
        ReadProgress {
            bytes_read: self.bytes_read(),
            total_bytes: self.total_bytes,
        }
    }

    /// Checks whether the file has an index appended at the end.
    ///
    /// Returns `false` for compressed files (index not supported).
    pub fn is_indexed(&self) -> bool {
        if self.compressed {
            return false;
        }
        let file = match &self.file {
            Some(f) => f,
            None => return false,
        };

        let Ok(metadata) = file.metadata() else {
            return false;
        };

        let file_size = metadata.len() as i64;
        let marker_len = INDEXED_MARKER.len() as i64;
        if file_size < marker_len {
            return false;
        }

        let mut buf = [0u8; 7];
        let offset = file_size - marker_len;

        // Use pread-style reading via the random access file
        use std::os::unix::fs::FileExt;
        if file.read_exact_at(&mut buf, offset as u64).is_err() {
            return false;
        }

        &buf == INDEXED_MARKER
    }

    /// Reads the next message from the log file.
    ///
    /// Returns `Ok(None)` when the end of the file is reached.
    pub fn next_message(&mut self) -> Result<Option<LogMessage>, ReadError> {
        let Some(info) = read_message_info(&mut self.reader)? else {
            return Ok(None);
        };

        let mut payload = vec![0u8; info.payload_len];
        self.reader.read_exact(&mut payload)?;

        Ok(Some(LogMessage {
            timestamp_ns: info.timestamp_ns,
            message_id: info.message_id,
            payload,
        }))
    }

    /// Reads lightweight metadata for the next message and skips its payload.
    pub fn next_message_info(&mut self) -> Result<Option<LogMessageInfo>, ReadError> {
        let Some(info) = read_message_info(&mut self.reader)? else {
            return Ok(None);
        };

        io::copy(
            &mut self.reader.by_ref().take(info.payload_len as u64),
            &mut io::sink(),
        )?;

        Ok(Some(info))
    }

    /// Reads a message at a specific byte offset in the file.
    ///
    /// Only available for non-compressed files.
    pub fn read_message_at(&mut self, offset: u64) -> Result<LogMessage, ReadError> {
        if self.compressed {
            return Err(ReadError::NoRandomAccessForCompressed);
        }

        let reader = match &mut self.reader {
            ReaderInner::Plain(r) => r,
            _ => unreachable!(),
        };

        reader.seek(SeekFrom::Start(offset))?;

        let info = read_message_info_required(reader)?;

        let mut payload = vec![0u8; info.payload_len];
        reader.read_exact(&mut payload)?;

        Ok(LogMessage {
            timestamp_ns: info.timestamp_ns,
            message_id: info.message_id,
            payload,
        })
    }

    /// Reads timestamp, message type, and payload length at a given offset.
    pub fn read_info_at(&mut self, offset: u64) -> Result<LogMessageInfo, ReadError> {
        if self.compressed {
            return Err(ReadError::NoRandomAccessForCompressed);
        }

        let reader = match &mut self.reader {
            ReaderInner::Plain(r) => r,
            _ => unreachable!(),
        };

        reader.seek(SeekFrom::Start(offset))?;
        read_message_info_required(reader)
    }

    /// Reads only the timestamp and message type at a given offset, without reading the payload.
    ///
    /// This is useful for quickly scanning through an indexed file.
    /// Only available for non-compressed files.
    pub fn read_header_at(&mut self, offset: u64) -> Result<(i64, MessageId), ReadError> {
        let info = self.read_info_at(offset)?;
        Ok((info.timestamp_ns, info.message_id))
    }

    /// Reads the index from the end of the file.
    ///
    /// The index contains byte offsets to each message, enabling random access.
    /// Only available for non-compressed, indexed files.
    pub fn read_index(&self) -> Result<Vec<u64>, ReadError> {
        if self.compressed {
            return Err(ReadError::NoRandomAccessForCompressed);
        }

        let file = self
            .file
            .as_ref()
            .ok_or(ReadError::NoRandomAccessForCompressed)?;

        use std::os::unix::fs::FileExt;

        let metadata = file.metadata()?;
        let file_size = metadata.len();

        // Read the seek-back value: 8 bytes before the "INDEXED" marker
        let seek_back_offset = file_size - INDEXED_MARKER.len() as u64 - 8;
        let mut buf = [0u8; 8];
        file.read_exact_at(&mut buf, seek_back_offset)?;
        let seek_back = u64::from_be_bytes(buf);

        // The offsets start after the header of the index message (16 bytes)
        let offsets_start = file_size - seek_back + crate::HEADER_SIZE as u64;
        let offsets_end = seek_back_offset;
        let offsets_len = offsets_end - offsets_start;

        let mut data = vec![0u8; offsets_len as usize];
        file.read_exact_at(&mut data, offsets_start)?;

        let num_offsets = offsets_len / 8;
        let mut offsets = Vec::with_capacity(num_offsets as usize);
        for i in 0..num_offsets {
            let start = (i * 8) as usize;
            let bytes: [u8; 8] = data[start..start + 8].try_into().unwrap();
            offsets.push(u64::from_be_bytes(bytes));
        }

        Ok(offsets)
    }

    /// Skips the next message without allocating memory for its payload.
    ///
    /// Returns the total number of bytes skipped (header + payload), or `Ok(None)` at EOF.
    pub fn skip_message(&mut self) -> Result<Option<usize>, ReadError> {
        let Some(info) = read_message_info(&mut self.reader)? else {
            return Ok(None);
        };

        // Discard the payload
        io::copy(
            &mut self.reader.by_ref().take(info.payload_len as u64),
            &mut io::sink(),
        )?;

        Ok(Some(16 + info.payload_len))
    }

    /// Collects all messages into a `Vec`.
    ///
    /// This reads the entire file into memory. For large files, prefer
    /// iterating with [`next_message`](Self::next_message).
    pub fn read_all(&mut self) -> Result<Vec<LogMessage>, ReadError> {
        let mut messages = Vec::new();
        while let Some(msg) = self.next_message()? {
            messages.push(msg);
        }
        Ok(messages)
    }

    /// Returns an iterator over all messages in the log file.
    pub fn iter(&mut self) -> LogMessageIter<'_> {
        LogMessageIter { reader: self }
    }

    fn verify_header(&mut self) -> Result<(), ReadError> {
        let mut header = [0u8; 12];
        self.reader.read_exact(&mut header)?;

        if &header != FILE_HEADER {
            return Err(ReadError::InvalidHeader {
                found: String::from_utf8_lossy(&header).into_owned(),
            });
        }

        let version = self.reader.read_i32::<BigEndian>()?;
        if version != FILE_VERSION {
            return Err(ReadError::UnsupportedVersion { version });
        }

        Ok(())
    }
}

fn read_message_info<R: Read>(reader: &mut R) -> Result<Option<LogMessageInfo>, ReadError> {
    let timestamp_ns = match reader.read_i64::<BigEndian>() {
        Ok(v) => v,
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(ReadError::Io(e)),
    };

    let message_id_raw = reader.read_i32::<BigEndian>()?;
    let message_id = MessageId::from_i32(message_id_raw)
        .ok_or(ReadError::UnknownMessageId { id: message_id_raw })?;

    let length = reader.read_i32::<BigEndian>()?;
    if length < 0 {
        return Err(ReadError::InvalidLength { length });
    }

    Ok(Some(LogMessageInfo {
        timestamp_ns,
        message_id,
        payload_len: length as usize,
    }))
}

fn read_message_info_required<R: Read>(reader: &mut R) -> Result<LogMessageInfo, ReadError> {
    read_message_info(reader)?.ok_or_else(|| {
        ReadError::Io(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected end of file while reading message header",
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tempfile::tempdir;

    use super::*;
    use crate::{LogMessage, LogWriter};

    #[test]
    fn next_message_info_skips_payload_without_allocating_messages() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sample.log");
        write_sample_log(&path);

        let mut reader = LogReader::open(&path).unwrap();
        let first = reader.next_message_info().unwrap().unwrap();
        let second = reader.next_message_info().unwrap().unwrap();

        assert_eq!(first.timestamp_ns, 100);
        assert_eq!(first.message_id, MessageId::Vision2014);
        assert_eq!(first.payload_len, 3);
        assert_eq!(second.timestamp_ns, 200);
        assert_eq!(second.message_id, MessageId::Referee2013);
        assert_eq!(second.payload_len, 2);
        assert!(reader.next_message_info().unwrap().is_none());
    }

    #[test]
    fn read_info_at_reports_header_fields() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sample.log");
        write_sample_log(&path);

        let mut reader = LogReader::open(&path).unwrap();
        let first = reader.read_info_at(crate::HEADER_SIZE as u64).unwrap();
        let second = reader
            .read_info_at(crate::HEADER_SIZE as u64 + 16 + 3)
            .unwrap();

        assert_eq!(first.timestamp_ns, 100);
        assert_eq!(first.message_id, MessageId::Vision2014);
        assert_eq!(first.payload_len, 3);
        assert_eq!(second.timestamp_ns, 200);
        assert_eq!(second.message_id, MessageId::Referee2013);
        assert_eq!(second.payload_len, 2);
    }

    fn write_sample_log(path: &Path) {
        let mut writer = LogWriter::create(path).unwrap();
        writer
            .write_message(&LogMessage {
                timestamp_ns: 100,
                message_id: MessageId::Vision2014,
                payload: vec![1, 2, 3],
            })
            .unwrap();
        writer
            .write_message(&LogMessage {
                timestamp_ns: 200,
                message_id: MessageId::Referee2013,
                payload: vec![4, 5],
            })
            .unwrap();
        writer.close().unwrap();
    }
}

/// An iterator over log messages.
pub struct LogMessageIter<'a> {
    reader: &'a mut LogReader,
}

impl<'a> Iterator for LogMessageIter<'a> {
    type Item = Result<LogMessage, ReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.reader.next_message() {
            Ok(Some(msg)) => Some(Ok(msg)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }
}
