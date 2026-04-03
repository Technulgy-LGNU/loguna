use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::Path;

use byteorder::{BigEndian, WriteBytesExt};
use flate2::write::GzEncoder;
use flate2::Compression;
use thiserror::Error;

use crate::message::{LogMessage, MessageId};
use crate::{FILE_HEADER, FILE_VERSION, INDEXED_MARKER};

/// Errors that can occur when writing log files.
#[derive(Debug, Error)]
pub enum WriteError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Inner writer, either plain or gzip-compressed.
enum WriterInner {
    Plain(BufWriter<File>),
    Gzip(BufWriter<GzEncoder<File>>),
}

impl Write for WriterInner {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            WriterInner::Plain(w) => w.write(buf),
            WriterInner::Gzip(w) => w.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            WriterInner::Plain(w) => w.flush(),
            WriterInner::Gzip(w) => w.flush(),
        }
    }
}

/// Writes SSL log files, both plain and gzip-compressed.
///
/// # Examples
///
/// ```no_run
/// use loguna::{LogWriter, LogMessage, MessageId};
///
/// let mut writer = LogWriter::create("output.log").unwrap();
/// writer.write_message(&LogMessage {
///     timestamp_ns: 1234567890_000_000_000,
///     message_id: MessageId::Vision2014,
///     payload: vec![/* protobuf bytes */],
/// }).unwrap();
/// writer.close().unwrap();
/// ```
pub struct LogWriter {
    writer: WriterInner,
    #[allow(dead_code)]
    compressed: bool,
}

impl LogWriter {
    /// Creates a new log file, writing the header.
    ///
    /// If the file already exists it will be truncated.
    /// Gzip compression is detected by `.gz` extension.
    pub fn create<P: AsRef<Path>>(path: P) -> Result<Self, WriteError> {
        let path = path.as_ref();
        let compressed = path.extension().map_or(false, |ext| ext == "gz");

        let file = File::create(path)?;

        let writer = if compressed {
            let gz = GzEncoder::new(file, Compression::default());
            WriterInner::Gzip(BufWriter::new(gz))
        } else {
            WriterInner::Plain(BufWriter::new(file))
        };

        let mut log_writer = LogWriter { writer, compressed };
        log_writer.write_header()?;
        Ok(log_writer)
    }

    /// Opens an existing log file for appending.
    ///
    /// Does **not** write a header (assumes it already exists).
    pub fn append<P: AsRef<Path>>(path: P) -> Result<Self, WriteError> {
        let path = path.as_ref();
        let compressed = path.extension().map_or(false, |ext| ext == "gz");

        let file = OpenOptions::new().append(true).open(path)?;

        let writer = if compressed {
            let gz = GzEncoder::new(file, Compression::default());
            WriterInner::Gzip(BufWriter::new(gz))
        } else {
            WriterInner::Plain(BufWriter::new(file))
        };

        Ok(LogWriter { writer, compressed })
    }

    /// Writes a single message to the log file.
    pub fn write_message(&mut self, msg: &LogMessage) -> Result<(), WriteError> {
        self.writer.write_i64::<BigEndian>(msg.timestamp_ns)?;
        self.writer
            .write_i32::<BigEndian>(msg.message_id.as_i32())?;
        self.writer
            .write_i32::<BigEndian>(msg.payload.len() as i32)?;
        self.writer.write_all(&msg.payload)?;
        Ok(())
    }

    /// Writes an index to the end of the file.
    ///
    /// The index contains byte offsets to each message, enabling random access.
    /// Format matches the Go implementation:
    /// - Message header (timestamp=0, id=Index2021, length)
    /// - Array of i64 offsets (big-endian)
    /// - i64 backward seek offset (total message size including header)
    /// - "INDEXED" marker (7 bytes)
    pub fn write_index(&mut self, offsets: &[u64]) -> Result<(), WriteError> {
        let payload_len = offsets.len() * 8;
        let trailing_size = 8 + INDEXED_MARKER.len(); // seek-back + marker
        let msg_len = payload_len + 16 + trailing_size; // payload + header + trailing

        // Write message header
        self.writer.write_i64::<BigEndian>(0)?; // timestamp = 0
        self.writer
            .write_i32::<BigEndian>(MessageId::Index2021.as_i32())?;
        self.writer
            .write_i32::<BigEndian>((payload_len + trailing_size) as i32)?;

        // Write offsets
        for &offset in offsets {
            self.writer.write_i64::<BigEndian>(offset as i64)?;
        }

        // Write backward offset (total message length)
        self.writer.write_i64::<BigEndian>(msg_len as i64)?;

        // Write indexed marker
        self.writer.write_all(INDEXED_MARKER)?;

        Ok(())
    }

    /// Flushes and closes the writer.
    pub fn close(mut self) -> Result<(), WriteError> {
        match self.writer {
            WriterInner::Plain(ref mut w) => w.flush()?,
            WriterInner::Gzip(w) => {
                let gz = w.into_inner().map_err(|e| e.into_error())?;
                gz.finish()?;
                return Ok(());
            }
        }
        Ok(())
    }

    fn write_header(&mut self) -> Result<(), WriteError> {
        self.writer.write_all(FILE_HEADER)?;
        self.writer.write_i32::<BigEndian>(FILE_VERSION)?;
        Ok(())
    }
}
