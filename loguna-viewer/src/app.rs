use std::collections::HashSet;
use std::path::Path;

use prost::Message;
use loguna::proto::{
    Referee, SslWrapperPacket, TrackerWrapperPacket,
    referee::{Command, Stage},
};
use loguna::{LogMessage, LogReader, MessageId};

/// A parsed and displayable log entry.
#[derive(Debug, Clone)]
pub struct LogEntry {
    /// Index in the original (unfiltered) message list.
    pub index: usize,
    /// The raw log message.
    pub raw: LogMessage,
    /// A human-readable summary line for the list view.
    pub summary: String,
    /// Detailed information shown in the detail panel.
    pub detail: String,
}

/// Which tab is active in the detail view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Detail,
}

pub struct App {
    /// All loaded log entries (unfiltered).
    pub all_entries: Vec<LogEntry>,
    /// Indices into `all_entries` that pass the current filter.
    pub filtered_indices: Vec<usize>,
    /// Currently selected index in `filtered_indices`.
    pub selected: usize,
    /// Whether the detail panel is shown.
    pub show_detail: bool,
    /// Current detail tab.
    pub tab: Tab,
    /// Which message types are enabled (shown).
    pub enabled_types: HashSet<MessageId>,
    /// Whether filter menu is shown.
    pub show_filter_menu: bool,
    /// Total messages loaded.
    pub total_messages: usize,
    /// Log file name for display.
    pub filename: String,
    /// First timestamp in the log (for relative time display).
    pub base_timestamp_ns: i64,
    /// Page size for page up/down.
    pub page_size: usize,
}

impl App {
    /// Load a log file and parse all messages.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let filename = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());

        let mut reader = LogReader::open(path)?;
        let messages = reader.read_all()?;
        let total_messages = messages.len();

        let base_timestamp_ns = messages.first().map(|m| m.timestamp_ns).unwrap_or(0);

        let all_entries: Vec<LogEntry> = messages
            .into_iter()
            .enumerate()
            .map(|(i, msg)| {
                let (summary, detail) = parse_message_display(&msg, base_timestamp_ns);
                LogEntry {
                    index: i,
                    raw: msg,
                    summary,
                    detail,
                }
            })
            .collect();

        let mut enabled_types = HashSet::new();
        enabled_types.insert(MessageId::Vision2014);
        enabled_types.insert(MessageId::Referee2013);
        enabled_types.insert(MessageId::VisionTracker2020);
        enabled_types.insert(MessageId::Vision2010);
        enabled_types.insert(MessageId::Blank);
        enabled_types.insert(MessageId::Unknown);
        enabled_types.insert(MessageId::Index2021);

        let mut app = App {
            all_entries,
            filtered_indices: Vec::new(),
            selected: 0,
            show_detail: false,
            tab: Tab::Overview,
            enabled_types,
            show_filter_menu: false,
            total_messages,
            filename,
            base_timestamp_ns,
            page_size: 20,
        };
        app.update_filter();
        Ok(app)
    }

    pub fn update_filter(&mut self) {
        self.filtered_indices = self
            .all_entries
            .iter()
            .enumerate()
            .filter(|(_, e)| self.enabled_types.contains(&e.raw.message_id))
            .map(|(i, _)| i)
            .collect();

        if self.selected >= self.filtered_indices.len() {
            self.selected = self.filtered_indices.len().saturating_sub(1);
        }
    }

    pub fn selected_entry(&self) -> Option<&LogEntry> {
        self.filtered_indices
            .get(self.selected)
            .and_then(|&i| self.all_entries.get(i))
    }

    pub fn next(&mut self) {
        if !self.filtered_indices.is_empty() {
            self.selected = (self.selected + 1).min(self.filtered_indices.len() - 1);
        }
    }

    pub fn previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn page_down(&mut self) {
        if !self.filtered_indices.is_empty() {
            self.selected =
                (self.selected + self.page_size).min(self.filtered_indices.len() - 1);
        }
    }

    pub fn page_up(&mut self) {
        self.selected = self.selected.saturating_sub(self.page_size);
    }

    pub fn first(&mut self) {
        self.selected = 0;
    }

    pub fn last(&mut self) {
        if !self.filtered_indices.is_empty() {
            self.selected = self.filtered_indices.len() - 1;
        }
    }

    pub fn toggle_detail(&mut self) {
        self.show_detail = !self.show_detail;
    }

    pub fn next_tab(&mut self) {
        self.tab = match self.tab {
            Tab::Overview => Tab::Detail,
            Tab::Detail => Tab::Overview,
        };
    }

    pub fn prev_tab(&mut self) {
        self.next_tab(); // only 2 tabs
    }

    pub fn toggle_filter_menu(&mut self) {
        self.show_filter_menu = !self.show_filter_menu;
    }

    pub fn toggle_message_filter(&mut self, msg_type: MessageId) {
        if self.enabled_types.contains(&msg_type) {
            self.enabled_types.remove(&msg_type);
        } else {
            self.enabled_types.insert(msg_type);
        }
        self.update_filter();
    }

    /// Get statistics about message type distribution.
    pub fn type_counts(&self) -> Vec<(MessageId, usize)> {
        let types = [
            MessageId::Vision2014,
            MessageId::Referee2013,
            MessageId::VisionTracker2020,
            MessageId::Vision2010,
            MessageId::Blank,
            MessageId::Unknown,
            MessageId::Index2021,
        ];
        types
            .iter()
            .map(|&t| {
                let count = self
                    .all_entries
                    .iter()
                    .filter(|e| e.raw.message_id == t)
                    .count();
                (t, count)
            })
            .filter(|(_, c)| *c > 0)
            .collect()
    }
}

/// Format a relative timestamp from nanoseconds.
fn format_relative_time(ns: i64) -> String {
    let total_secs = ns as f64 / 1_000_000_000.0;
    let hours = (total_secs / 3600.0) as u32;
    let mins = ((total_secs % 3600.0) / 60.0) as u32;
    let secs = total_secs % 60.0;
    if hours > 0 {
        format!("{hours}:{mins:02}:{secs:06.3}")
    } else {
        format!("{mins}:{secs:06.3}")
    }
}

fn parse_message_display(msg: &LogMessage, base_ts: i64) -> (String, String) {
    let relative_ns = msg.timestamp_ns - base_ts;
    let time_str = format_relative_time(relative_ns);

    match msg.message_id {
        MessageId::Vision2014 => parse_vision_display(msg, &time_str),
        MessageId::Referee2013 => parse_referee_display(msg, &time_str),
        MessageId::VisionTracker2020 => parse_tracker_display(msg, &time_str),
        _ => {
            let summary = format!(
                "{time_str}  {:<20}  {} bytes",
                msg.message_id.to_string(),
                msg.payload.len()
            );
            let detail = format!(
                "Message Type: {}\nTimestamp: {} ns\nPayload: {} bytes",
                msg.message_id, msg.timestamp_ns, msg.payload.len()
            );
            (summary, detail)
        }
    }
}

fn parse_vision_display(msg: &LogMessage, time_str: &str) -> (String, String) {
    match SslWrapperPacket::decode(msg.payload.as_slice()) {
        Ok(wrapper) => {
            let mut parts = Vec::new();
            let mut detail_lines = vec!["Type: Vision2014".to_string()];

            if let Some(ref det) = wrapper.detection {
                let n_balls = det.balls.len();
                let n_yellow = det.robots_yellow.len();
                let n_blue = det.robots_blue.len();
                parts.push(format!(
                    "cam {} frame {} | {}B {}Y {}B",
                    det.camera_id, det.frame_number, n_balls, n_yellow, n_blue
                ));

                detail_lines.push(format!("Camera ID: {}", det.camera_id));
                detail_lines.push(format!("Frame Number: {}", det.frame_number));
                detail_lines.push(format!("t_capture: {:.6}s", det.t_capture));
                detail_lines.push(format!("t_sent: {:.6}s", det.t_sent));
                if let Some(t_cam) = det.t_capture_camera {
                    detail_lines.push(format!("t_capture_camera: {:.6}s", t_cam));
                }
                detail_lines.push(format!("Balls: {n_balls}"));
                for (i, ball) in det.balls.iter().enumerate() {
                    detail_lines.push(format!(
                        "  Ball {i}: pos=({:.1}, {:.1}{}) conf={:.2}",
                        ball.x,
                        ball.y,
                        ball.z.map(|z| format!(", {z:.1}")).unwrap_or_default(),
                        ball.confidence
                    ));
                }
                detail_lines.push(format!("Yellow Robots: {n_yellow}"));
                for robot in &det.robots_yellow {
                    let id_str = robot
                        .robot_id
                        .map(|id| format!("#{id}"))
                        .unwrap_or_else(|| "?".to_string());
                    detail_lines.push(format!(
                        "  Robot {id_str}: pos=({:.1}, {:.1}) orient={} conf={:.2}",
                        robot.x,
                        robot.y,
                        robot
                            .orientation
                            .map(|o| format!("{o:.2}rad"))
                            .unwrap_or_else(|| "?".to_string()),
                        robot.confidence
                    ));
                }
                detail_lines.push(format!("Blue Robots: {n_blue}"));
                for robot in &det.robots_blue {
                    let id_str = robot
                        .robot_id
                        .map(|id| format!("#{id}"))
                        .unwrap_or_else(|| "?".to_string());
                    detail_lines.push(format!(
                        "  Robot {id_str}: pos=({:.1}, {:.1}) orient={} conf={:.2}",
                        robot.x,
                        robot.y,
                        robot
                            .orientation
                            .map(|o| format!("{o:.2}rad"))
                            .unwrap_or_else(|| "?".to_string()),
                        robot.confidence
                    ));
                }
            }
            if let Some(ref geo) = wrapper.geometry {
                let field = &geo.field;
                parts.push("geometry".to_string());
                detail_lines.push("Geometry:".to_string());
                detail_lines.push(format!(
                    "  Field: {}x{}mm",
                    field.field_length, field.field_width
                ));
                detail_lines.push(format!("  Goal: {}x{}mm", field.goal_width, field.goal_depth));
                detail_lines.push(format!("  Boundary: {}mm", field.boundary_width));
                detail_lines.push(format!("  Lines: {}", field.field_lines.len()));
                detail_lines.push(format!("  Arcs: {}", field.field_arcs.len()));
                detail_lines.push(format!("  Cameras: {}", geo.calib.len()));
            }

            let info = if parts.is_empty() {
                "empty".to_string()
            } else {
                parts.join(" | ")
            };
            let summary = format!("{time_str}  Vision2014          {info}");
            (summary, detail_lines.join("\n"))
        }
        Err(e) => {
            let summary = format!("{time_str}  Vision2014          <decode error>");
            let detail = format!("Decode error: {e}");
            (summary, detail)
        }
    }
}

fn parse_referee_display(msg: &LogMessage, time_str: &str) -> (String, String) {
    match Referee::decode(msg.payload.as_slice()) {
        Ok(referee) => {
            let stage = Stage::try_from(referee.stage).ok();
            let command = Command::try_from(referee.command).ok();

            let stage_str = stage
                .map(|s| format!("{s:?}"))
                .unwrap_or_else(|| format!("Stage({})", referee.stage));
            let cmd_str = command
                .map(|c| format!("{c:?}"))
                .unwrap_or_else(|| format!("Cmd({})", referee.command));

            let yellow = &referee.yellow;
            let blue = &referee.blue;

            let summary = format!(
                "{time_str}  Referee              {cmd_str} | {} {} - {} {}",
                yellow.name, yellow.score, blue.score, blue.name
            );

            let mut detail_lines = vec![
                "Type: Referee2013".to_string(),
                format!("Stage: {stage_str}"),
                format!("Command: {cmd_str}"),
                format!("Command Counter: {}", referee.command_counter),
            ];

            if let Some(stl) = referee.stage_time_left {
                let secs = stl as f64 / 1_000_000.0;
                detail_lines.push(format!("Stage Time Left: {secs:.1}s"));
            }

            detail_lines.push(format!("Yellow Team: {}", yellow.name));
            detail_lines.push(format!("  Score: {}", yellow.score));
            detail_lines.push(format!("  Yellow Cards: {}", yellow.yellow_cards));
            detail_lines.push(format!("  Red Cards: {}", yellow.red_cards));
            detail_lines.push(format!("  Timeouts: {}", yellow.timeouts));
            detail_lines.push(format!("  Goalkeeper: #{}", yellow.goalkeeper));
            if let Some(max) = yellow.max_allowed_bots {
                detail_lines.push(format!("  Max Bots: {max}"));
            }

            detail_lines.push(format!("Blue Team: {}", blue.name));
            detail_lines.push(format!("  Score: {}", blue.score));
            detail_lines.push(format!("  Yellow Cards: {}", blue.yellow_cards));
            detail_lines.push(format!("  Red Cards: {}", blue.red_cards));
            detail_lines.push(format!("  Timeouts: {}", blue.timeouts));
            detail_lines.push(format!("  Goalkeeper: #{}", blue.goalkeeper));
            if let Some(max) = blue.max_allowed_bots {
                detail_lines.push(format!("  Max Bots: {max}"));
            }

            if let Some(ref pos) = referee.designated_position {
                detail_lines.push(format!("Designated Position: ({:.1}, {:.1})", pos.x, pos.y));
            }
            if !referee.game_events.is_empty() {
                detail_lines.push(format!("Game Events: {}", referee.game_events.len()));
                for event in &referee.game_events {
                    let type_str = event
                        .r#type
                        .and_then(|t| {
                            loguna::proto::game_event::Type::try_from(t).ok()
                        })
                        .map(|t| format!("{t:?}"))
                        .unwrap_or_else(|| "?".to_string());
                    detail_lines.push(format!("  - {type_str}"));
                }
            }

            (summary, detail_lines.join("\n"))
        }
        Err(e) => {
            let summary = format!("{time_str}  Referee              <decode error>");
            let detail = format!("Decode error: {e}");
            (summary, detail)
        }
    }
}

fn parse_tracker_display(msg: &LogMessage, time_str: &str) -> (String, String) {
    match TrackerWrapperPacket::decode(msg.payload.as_slice()) {
        Ok(wrapper) => {
            let source = wrapper.source_name.as_deref().unwrap_or("unknown");

            let mut parts = vec![format!("src={source}")];
            let mut detail_lines = vec![
                "Type: VisionTracker2020".to_string(),
                format!("UUID: {}", wrapper.uuid),
                format!("Source: {source}"),
            ];

            if let Some(ref frame) = wrapper.tracked_frame {
                parts.push(format!(
                    "frame {} | {}B {}R",
                    frame.frame_number,
                    frame.balls.len(),
                    frame.robots.len()
                ));
                detail_lines.push(format!("Frame: {}", frame.frame_number));
                detail_lines.push(format!("Timestamp: {:.6}s", frame.timestamp));
                detail_lines.push(format!("Balls: {}", frame.balls.len()));
                for (i, ball) in frame.balls.iter().enumerate() {
                    let pos = &ball.pos;
                    detail_lines.push(format!(
                        "  Ball {i}: ({:.3}, {:.3}, {:.3})m vis={:.2}",
                        pos.x, pos.y, pos.z,
                        ball.visibility.unwrap_or(0.0)
                    ));
                }
                detail_lines.push(format!("Robots: {}", frame.robots.len()));
                for robot in &frame.robots {
                    let rid = &robot.robot_id;
                    let pos = &robot.pos;
                    let team = match rid.team {
                        Some(1) => "Y",
                        Some(2) => "B",
                        _ => "?",
                    };
                    detail_lines.push(format!(
                        "  {team}#{}: ({:.3}, {:.3})m orient={:.2}rad",
                        rid.id.unwrap_or(0),
                        pos.x,
                        pos.y,
                        robot.orientation
                    ));
                }
            }

            let summary = format!("{time_str}  Tracker              {}", parts.join(" | "));
            (summary, detail_lines.join("\n"))
        }
        Err(e) => {
            let summary = format!("{time_str}  Tracker              <decode error>");
            let detail = format!("Decode error: {e}");
            (summary, detail)
        }
    }
}
