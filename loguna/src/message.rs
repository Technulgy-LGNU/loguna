use std::fmt;

/// Identifies the type of protobuf message in a log entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum MessageId {
    /// Ignore this message.
    Blank = 0,
    /// Unknown message type (try to guess by parsing).
    Unknown = 1,
    /// Legacy SSL-Vision wrapper (2010 format).
    Vision2010 = 2,
    /// Game controller / referee message (2013+ format).
    Referee2013 = 3,
    /// SSL-Vision wrapper (2014+ format).
    Vision2014 = 4,
    /// Tracked vision data (2020+ format).
    VisionTracker2020 = 5,
    /// Index message (2021+ format).
    Index2021 = 6,
}

impl MessageId {
    /// Convert from a raw `i32` value.
    ///
    /// Returns `None` for unrecognized values.
    pub fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Blank),
            1 => Some(Self::Unknown),
            2 => Some(Self::Vision2010),
            3 => Some(Self::Referee2013),
            4 => Some(Self::Vision2014),
            5 => Some(Self::VisionTracker2020),
            6 => Some(Self::Index2021),
            _ => None,
        }
    }

    /// Convert to the raw `i32` value.
    pub fn as_i32(self) -> i32 {
        self as i32
    }
}

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Blank => write!(f, "Blank"),
            Self::Unknown => write!(f, "Unknown"),
            Self::Vision2010 => write!(f, "Vision2010"),
            Self::Referee2013 => write!(f, "Referee2013"),
            Self::Vision2014 => write!(f, "Vision2014"),
            Self::VisionTracker2020 => write!(f, "VisionTracker2020"),
            Self::Index2021 => write!(f, "Index2021"),
        }
    }
}

/// A single message read from or written to a log file.
#[derive(Debug, Clone)]
pub struct LogMessage {
    /// Receiver timestamp in nanoseconds since the Unix epoch.
    pub timestamp_ns: i64,
    /// The type of the protobuf payload.
    pub message_id: MessageId,
    /// The raw protobuf-encoded payload.
    pub payload: Vec<u8>,
}

impl LogMessage {
    /// Returns the timestamp as seconds (floating point) since the Unix epoch.
    pub fn timestamp_secs(&self) -> f64 {
        self.timestamp_ns as f64 / 1_000_000_000.0
    }
}
