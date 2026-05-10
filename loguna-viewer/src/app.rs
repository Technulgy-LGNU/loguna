use std::collections::HashSet;
use std::path::{Path, PathBuf};

use loguna::proto::{
    referee::{Command, Stage},
    Referee, SslWrapperPacket, TrackerWrapperPacket,
};
use loguna::{LogMessage, LogMessageInfo, LogReader, MessageId, ReadProgress};
use prost::Message;

#[derive(Debug, Clone)]
pub struct LogEntryMeta {
    pub index: usize,
    pub offset: Option<u64>,
    pub info: LogMessageInfo,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct LogEntryDetail {
    pub index: usize,
    pub raw: LogMessage,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct LoadingState {
    pub filename: String,
    pub progress: ReadProgress,
    pub messages_loaded: usize,
    pub phase: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Detail,
}

pub struct App {
    path: PathBuf,
    pub all_entries: Vec<LogEntryMeta>,
    pub filtered_indices: Vec<usize>,
    pub selected: usize,
    pub show_detail: bool,
    pub tab: Tab,
    pub enabled_types: HashSet<MessageId>,
    pub show_filter_menu: bool,
    pub total_messages: usize,
    pub filename: String,
    pub base_timestamp_ns: i64,
    pub page_size: usize,
    type_counts: Vec<(MessageId, usize)>,
    indexed_reader: Option<LogReader>,
    detail_cache: Option<LogEntryDetail>,
}

impl App {
    pub fn load_with_progress<F>(path: &Path, mut on_progress: F) -> anyhow::Result<Self>
    where
        F: FnMut(&LoadingState) -> anyhow::Result<()>,
    {
        let filename = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());

        let mut scan_reader = LogReader::open(path)?;
        let indexed_offsets = if scan_reader.is_indexed() {
            Some(scan_reader.read_index()?)
        } else {
            None
        };

        let mut all_entries = Vec::new();
        let mut type_counts = default_type_counts();
        let mut base_timestamp_ns = 0;

        match indexed_offsets {
            Some(offsets) => {
                let mut indexed_reader = LogReader::open(path)?;
                let total_offsets = offsets.len();
                let total_bytes = indexed_reader.total_bytes();
                for (i, offset) in offsets.into_iter().enumerate() {
                    let message = indexed_reader.read_message_at(offset)?;
                    let info = LogMessageInfo {
                        timestamp_ns: message.timestamp_ns,
                        message_id: message.message_id,
                        payload_len: message.payload.len(),
                    };
                    if i == 0 {
                        base_timestamp_ns = info.timestamp_ns;
                    }
                    increment_type_count(&mut type_counts, info.message_id);
                    all_entries.push(LogEntryMeta {
                        index: i,
                        offset: Some(offset),
                        summary: parse_message_summary(&message, base_timestamp_ns),
                        info,
                    });

                    if i % 512 == 0 || i + 1 == total_offsets {
                        let bytes_read = (offset + 16 + info.payload_len as u64).min(total_bytes);
                        on_progress(&LoadingState {
                            filename: filename.clone(),
                            progress: ReadProgress {
                                bytes_read,
                                total_bytes,
                            },
                            messages_loaded: i + 1,
                            phase: "Reading indexed headers".to_string(),
                        })?;
                    }
                }
            }
            None => {
                while let Some(message) = scan_reader.next_message()? {
                    let info = LogMessageInfo {
                        timestamp_ns: message.timestamp_ns,
                        message_id: message.message_id,
                        payload_len: message.payload.len(),
                    };
                    let index = all_entries.len();
                    if index == 0 {
                        base_timestamp_ns = info.timestamp_ns;
                    }
                    increment_type_count(&mut type_counts, info.message_id);
                    all_entries.push(LogEntryMeta {
                        index,
                        offset: None,
                        summary: parse_message_summary(&message, base_timestamp_ns),
                        info,
                    });

                    if index % 512 == 0 {
                        on_progress(&LoadingState {
                            filename: filename.clone(),
                            progress: scan_reader.progress(),
                            messages_loaded: index + 1,
                            phase: "Scanning log stream".to_string(),
                        })?;
                    }
                }

                on_progress(&LoadingState {
                    filename: filename.clone(),
                    progress: scan_reader.progress(),
                    messages_loaded: all_entries.len(),
                    phase: "Scanning log stream".to_string(),
                })?;
            }
        }

        let mut enabled_types = HashSet::new();
        enabled_types.insert(MessageId::Vision2014);
        enabled_types.insert(MessageId::Referee2013);
        enabled_types.insert(MessageId::VisionTracker2020);
        enabled_types.insert(MessageId::Vision2010);
        enabled_types.insert(MessageId::Blank);
        enabled_types.insert(MessageId::Unknown);
        enabled_types.insert(MessageId::Index2021);

        let mut app = App {
            path: path.to_path_buf(),
            all_entries,
            filtered_indices: Vec::new(),
            selected: 0,
            show_detail: false,
            tab: Tab::Overview,
            enabled_types,
            show_filter_menu: false,
            total_messages: 0,
            filename,
            base_timestamp_ns,
            page_size: 20,
            type_counts,
            indexed_reader: if scan_reader.is_indexed() {
                Some(LogReader::open(path)?)
            } else {
                None
            },
            detail_cache: None,
        };
        app.total_messages = app.all_entries.len();
        app.update_filter();
        Ok(app)
    }

    pub fn update_filter(&mut self) {
        self.filtered_indices = self
            .all_entries
            .iter()
            .enumerate()
            .filter(|(_, e)| self.enabled_types.contains(&e.info.message_id))
            .map(|(i, _)| i)
            .collect();

        if self.selected >= self.filtered_indices.len() {
            self.selected = self.filtered_indices.len().saturating_sub(1);
        }
    }

    pub fn selected_entry(&self) -> Option<&LogEntryMeta> {
        self.filtered_indices
            .get(self.selected)
            .and_then(|&i| self.all_entries.get(i))
    }

    pub fn selected_entry_detail(&mut self) -> anyhow::Result<Option<&LogEntryDetail>> {
        let Some(entry) = self.selected_entry().cloned() else {
            self.detail_cache = None;
            return Ok(None);
        };

        if self
            .detail_cache
            .as_ref()
            .map(|cached| cached.index == entry.index)
            .unwrap_or(false)
        {
            return Ok(self.detail_cache.as_ref());
        }

        let raw = if let Some(offset) = entry.offset {
            self.indexed_reader
                .as_mut()
                .expect("indexed reader missing for indexed entry")
                .read_message_at(offset)?
        } else {
            let mut reader = LogReader::open(&self.path)?;
            let mut current = 0usize;
            loop {
                match reader.next_message()? {
                    Some(message) if current == entry.index => break message,
                    Some(_) => current += 1,
                    None => anyhow::bail!("selected message index {} not found", entry.index),
                }
            }
        };

        let (_, detail) = parse_message_display(&raw, self.base_timestamp_ns);
        self.detail_cache = Some(LogEntryDetail {
            index: entry.index,
            raw,
            detail,
        });
        Ok(self.detail_cache.as_ref())
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
            self.selected = (self.selected + self.page_size).min(self.filtered_indices.len() - 1);
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
        self.next_tab();
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

    pub fn type_counts(&self) -> Vec<(MessageId, usize)> {
        self.type_counts
            .iter()
            .copied()
            .filter(|(_, count)| *count > 0)
            .collect()
    }
}

fn default_type_counts() -> Vec<(MessageId, usize)> {
    vec![
        (MessageId::Vision2014, 0),
        (MessageId::Referee2013, 0),
        (MessageId::VisionTracker2020, 0),
        (MessageId::Vision2010, 0),
        (MessageId::Blank, 0),
        (MessageId::Unknown, 0),
        (MessageId::Index2021, 0),
    ]
}

fn increment_type_count(type_counts: &mut [(MessageId, usize)], message_id: MessageId) {
    if let Some((_, count)) = type_counts.iter_mut().find(|(id, _)| *id == message_id) {
        *count += 1;
    } else {
        unreachable!("message type list is expected to be exhaustive");
    }
}

fn format_message_summary(message_id: MessageId, timestamp_ns: i64, payload_len: usize, base_ts: i64) -> String {
    let relative_ns = timestamp_ns - base_ts;
    let time_str = format_relative_time(relative_ns);
    format!(
        "{time_str}  {:<20}  {} bytes",
        message_id.to_string(),
        payload_len
    )
}

fn parse_message_summary(msg: &LogMessage, base_ts: i64) -> String {
    let time_str = format_relative_time(msg.timestamp_ns - base_ts);

    match msg.message_id {
        MessageId::Vision2014 => parse_vision_summary(msg, &time_str),
        MessageId::Referee2013 => parse_referee_summary(msg, &time_str),
        MessageId::VisionTracker2020 => parse_tracker_summary(msg, &time_str),
        _ => format_message_summary(msg.message_id, msg.timestamp_ns, msg.payload.len(), base_ts),
    }
}

pub fn format_relative_time(ns: i64) -> String {
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
                msg.message_id,
                msg.timestamp_ns,
                msg.payload.len()
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
                detail_lines.push(format!(
                    "  Goal: {}x{}mm",
                    field.goal_width, field.goal_depth
                ));
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

fn parse_vision_summary(msg: &LogMessage, time_str: &str) -> String {
    match SslWrapperPacket::decode(msg.payload.as_slice()) {
        Ok(wrapper) => {
            let mut parts = Vec::new();

            if let Some(ref det) = wrapper.detection {
                parts.push(format!(
                    "cam {} frame {} | {}B {}Y {}B",
                    det.camera_id,
                    det.frame_number,
                    det.balls.len(),
                    det.robots_yellow.len(),
                    det.robots_blue.len()
                ));
            }

            if wrapper.geometry.is_some() {
                parts.push("geometry".to_string());
            }

            let info = if parts.is_empty() {
                "empty".to_string()
            } else {
                parts.join(" | ")
            };

            format!("{time_str}  Vision2014          {info}")
        }
        Err(_) => format!("{time_str}  Vision2014          <decode error>"),
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
                        .and_then(|t| loguna::proto::game_event::Type::try_from(t).ok())
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

fn parse_referee_summary(msg: &LogMessage, time_str: &str) -> String {
    match Referee::decode(msg.payload.as_slice()) {
        Ok(referee) => {
            let command = Command::try_from(referee.command)
                .map(|c| format!("{c:?}"))
                .unwrap_or_else(|_| format!("Cmd({})", referee.command));
            format!(
                "{time_str}  Referee              {command} | {} {} - {} {}",
                referee.yellow.name, referee.yellow.score, referee.blue.score, referee.blue.name
            )
        }
        Err(_) => format!("{time_str}  Referee              <decode error>"),
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
                        pos.x,
                        pos.y,
                        pos.z,
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

fn parse_tracker_summary(msg: &LogMessage, time_str: &str) -> String {
    match TrackerWrapperPacket::decode(msg.payload.as_slice()) {
        Ok(wrapper) => {
            let source = wrapper.source_name.as_deref().unwrap_or("unknown");
            let mut parts = vec![format!("src={source}")];

            if let Some(ref frame) = wrapper.tracked_frame {
                parts.push(format!(
                    "frame {} | {}B {}R",
                    frame.frame_number,
                    frame.balls.len(),
                    frame.robots.len()
                ));
            }

            format!("{time_str}  Tracker              {}", parts.join(" | "))
        }
        Err(_) => format!("{time_str}  Tracker              <decode error>"),
    }
}
