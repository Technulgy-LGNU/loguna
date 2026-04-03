use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use byteorder::{BigEndian, ReadBytesExt};
use flate2::read::GzDecoder;
use thiserror::Error;

use crate::message::{LogMessage, MessageId};
use crate::{FILE_HEADER, FILE_VERSION, INDEXED_MARKER};

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
    Plain(BufReader<File>),
    Gzip(BufReader<GzDecoder<File>>),
}

impl Read for ReaderInner {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            ReaderInner::Plain(r) => r.read(buf),
            ReaderInner::Gzip(r) => r.read(buf),
        }
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
}

impl LogReader {
    /// Opens a log file for reading.
    ///
    /// Gzip-compressed files are detected by the `.gz` extension and
    /// decompressed transparently.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, ReadError> {
        let path = path.as_ref();
        let compressed = path
            .extension()
            .map_or(false, |ext| ext == "gz");

        let file = File::open(path)?;

        let (reader, random_access_file) = if compressed {
            let gz = GzDecoder::new(file);
            (ReaderInner::Gzip(BufReader::new(gz)), None)
        } else {
            let random_file = File::open(path)?; // second handle for random access
            (ReaderInner::Plain(BufReader::new(file)), Some(random_file))
        };

        let mut log_reader = LogReader {
            reader,
            file: random_access_file,
            compressed,
        };

        log_reader.verify_header()?;
        Ok(log_reader)
    }

    /// Whether the underlying file is gzip-compressed.
    pub fn is_compressed(&self) -> bool {
        self.compressed
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
        // Try to read the timestamp; EOF here means we're done
        let timestamp_ns = match self.reader.read_i64::<BigEndian>() {
            Ok(v) => v,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(ReadError::Io(e)),
        };

        let message_id_raw = self.reader.read_i32::<BigEndian>()?;
        let message_id = MessageId::from_i32(message_id_raw)
            .ok_or(ReadError::UnknownMessageId { id: message_id_raw })?;

        let length = self.reader.read_i32::<BigEndian>()?;
        if length < 0 {
            return Err(ReadError::InvalidLength { length });
        }

        let mut payload = vec![0u8; length as usize];
        self.reader.read_exact(&mut payload)?;

        Ok(Some(LogMessage {
            timestamp_ns,
            message_id,
            payload,
        }))
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

        let timestamp_ns = reader.read_i64::<BigEndian>()?;
        let message_id_raw = reader.read_i32::<BigEndian>()?;
        let message_id = MessageId::from_i32(message_id_raw)
            .ok_or(ReadError::UnknownMessageId { id: message_id_raw })?;

        let length = reader.read_i32::<BigEndian>()?;
        if length < 0 {
            return Err(ReadError::InvalidLength { length });
        }

        let mut payload = vec![0u8; length as usize];
        reader.read_exact(&mut payload)?;

        Ok(LogMessage {
            timestamp_ns,
            message_id,
            payload,
        })
    }

    /// Reads only the timestamp and message type at a given offset, without reading the payload.
    ///
    /// This is useful for quickly scanning through an indexed file.
    /// Only available for non-compressed files.
    pub fn read_header_at(&mut self, offset: u64) -> Result<(i64, MessageId), ReadError> {
        if self.compressed {
            return Err(ReadError::NoRandomAccessForCompressed);
        }

        let reader = match &mut self.reader {
            ReaderInner::Plain(r) => r,
            _ => unreachable!(),
        };

        reader.seek(SeekFrom::Start(offset))?;

        let timestamp_ns = reader.read_i64::<BigEndian>()?;
        let message_id_raw = reader.read_i32::<BigEndian>()?;
        let message_id = MessageId::from_i32(message_id_raw)
            .ok_or(ReadError::UnknownMessageId { id: message_id_raw })?;

        Ok((timestamp_ns, message_id))
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
        // Read and discard 12 bytes (timestamp + message_id)
        let mut header = [0u8; 12];
        match self.reader.read_exact(&mut header) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(ReadError::Io(e)),
        }

        let length = self.reader.read_i32::<BigEndian>()?;
        if length < 0 {
            return Err(ReadError::InvalidLength { length });
        }

        // Discard the payload
        io::copy(
            &mut self.reader.by_ref().take(length as u64),
            &mut io::sink(),
        )?;

        Ok(Some(16 + length as usize))
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
