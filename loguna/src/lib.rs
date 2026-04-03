//! # ssl-log-parser
//!
//! A library for reading and writing RoboCup SSL log files.
//!
//! The SSL log format is a binary format consisting of:
//! - A 16-byte header: `"SSL_LOG_FILE"` (12 bytes) + version `1` (4 bytes, big-endian)
//! - A sequence of messages, each with:
//!   - Timestamp: `i64` (8 bytes, big-endian) — receiver timestamp in nanoseconds
//!   - Message ID: `i32` (4 bytes, big-endian) — identifies the protobuf message type
//!   - Length: `i32` (4 bytes, big-endian) — payload length
//!   - Payload: raw protobuf bytes
//! - An optional index at the end of the file for random access
//!
//! Gzip-compressed files (`.log.gz`) are transparently supported.
//!
//! ## Quick start
//!
//! ```no_run
//! use loguna::{LogReader, MessageId};
//! use loguna::proto::SslWrapperPacket;
//! use prost::Message;
//!
//! let mut reader = LogReader::open("game.log").unwrap();
//! while let Some(msg) = reader.next_message().unwrap() {
//!     match msg.message_id {
//!         MessageId::Vision2014 => {
//!             let wrapper = SslWrapperPacket::decode(msg.payload.as_slice()).unwrap();
//!             if let Some(detection) = wrapper.detection {
//!                 println!("Frame {} from camera {}", detection.frame_number, detection.camera_id);
//!             }
//!         }
//!         MessageId::Referee2013 => {
//!             let referee = loguna::proto::Referee::decode(msg.payload.as_slice()).unwrap();
//!             println!("Command: {:?}", referee.command());
//!         }
//!         _ => {}
//!     }
//! }
//! ```

mod message;
mod reader;
mod writer;

pub use message::{LogMessage, MessageId};
pub use reader::LogReader;
pub use writer::LogWriter;

/// Generated protobuf types for SSL messages.
///
/// All proto2 types without package declarations are compiled into a single module.
/// This includes vision, referee/game-controller, and tracked detection types.
pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

/// File header magic string.
pub const FILE_HEADER: &[u8; 12] = b"SSL_LOG_FILE";

/// File format version.
pub const FILE_VERSION: i32 = 1;

/// Header size in bytes (12 bytes magic + 4 bytes version).
pub const HEADER_SIZE: usize = 16;

/// Marker at the end of indexed files.
pub const INDEXED_MARKER: &[u8; 7] = b"INDEXED";
