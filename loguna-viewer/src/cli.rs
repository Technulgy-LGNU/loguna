use std::path::Path;

use loguna::proto::{
    referee::{Command, Stage},
    Referee, SslWrapperPacket, TrackerWrapperPacket,
};
use loguna::{LogReader, MessageId};
use prost::Message;

use crate::{MsgTypeFilter, OutputFormat};

/// Print summary statistics about the log file.
pub fn run_stats(log_file: &Path) -> anyhow::Result<()> {
    let mut reader = LogReader::open(log_file)?;

    let mut vision_count = 0u64;
    let mut vision2010_count = 0u64;
    let mut referee_count = 0u64;
    let mut tracker_count = 0u64;
    let mut other_count = 0u64;
    let mut first_ts: Option<i64> = None;
    let mut last_ts: i64 = 0;
    let mut total_bytes = 0u64;

    // Track team names from referee messages
    let mut yellow_team = String::new();
    let mut blue_team = String::new();
    let mut yellow_score = 0u32;
    let mut blue_score = 0u32;
    let mut last_stage = String::new();
    let mut last_command = String::new();

    while let Some(msg) = reader.next_message()? {
        if first_ts.is_none() && msg.timestamp_ns > 0 {
            first_ts = Some(msg.timestamp_ns);
        }
        if msg.timestamp_ns > 0 {
            last_ts = msg.timestamp_ns;
        }
        total_bytes += msg.payload.len() as u64;

        match msg.message_id {
            MessageId::Vision2014 => vision_count += 1,
            MessageId::Vision2010 => vision2010_count += 1,
            MessageId::Referee2013 => {
                referee_count += 1;
                if let Ok(ref_msg) = Referee::decode(msg.payload.as_slice()) {
                    yellow_team = ref_msg.yellow.name.clone();
                    blue_team = ref_msg.blue.name.clone();
                    yellow_score = ref_msg.yellow.score;
                    blue_score = ref_msg.blue.score;
                    last_stage = Stage::try_from(ref_msg.stage)
                        .map(|s| format!("{s:?}"))
                        .unwrap_or_else(|_| format!("{}", ref_msg.stage));
                    last_command = Command::try_from(ref_msg.command)
                        .map(|c| format!("{c:?}"))
                        .unwrap_or_else(|_| format!("{}", ref_msg.command));
                }
            }
            MessageId::VisionTracker2020 => tracker_count += 1,
            _ => other_count += 1,
        }
    }

    let total = vision_count + vision2010_count + referee_count + tracker_count + other_count;
    let duration_s = first_ts
        .map(|first| (last_ts - first) as f64 / 1e9)
        .unwrap_or(0.0);

    println!("=== SSL Log File Statistics ===");
    println!("File: {}", log_file.display());
    println!();
    println!("Total messages: {total}");
    println!("Total payload:  {:.1} MB", total_bytes as f64 / 1_048_576.0);
    println!(
        "Duration:       {:.1}s ({:.1} min)",
        duration_s,
        duration_s / 60.0
    );
    println!();
    println!("Message counts:");
    println!(
        "  Vision2014:       {vision_count:>10}  ({:.1}%)",
        vision_count as f64 / total as f64 * 100.0
    );
    if vision2010_count > 0 {
        println!(
            "  Vision2010:       {vision2010_count:>10}  ({:.1}%)",
            vision2010_count as f64 / total as f64 * 100.0
        );
    }
    println!(
        "  Referee2013:      {referee_count:>10}  ({:.1}%)",
        referee_count as f64 / total as f64 * 100.0
    );
    println!(
        "  VisionTracker:    {tracker_count:>10}  ({:.1}%)",
        tracker_count as f64 / total as f64 * 100.0
    );
    if other_count > 0 {
        println!("  Other:            {other_count:>10}");
    }
    println!();
    if !yellow_team.is_empty() {
        println!("Match: {yellow_team} (yellow) vs {blue_team} (blue)");
        println!("Final score: {yellow_score} - {blue_score}");
        println!("Last stage: {last_stage}");
        println!("Last command: {last_command}");
    }

    Ok(())
}

/// Dump messages to stdout with filters.
pub fn run_dump(
    log_file: &Path,
    types: &[MsgTypeFilter],
    limit: Option<usize>,
    offset: usize,
    after: Option<f64>,
    before: Option<f64>,
    format: &OutputFormat,
    detail: bool,
) -> anyhow::Result<()> {
    let mut reader = LogReader::open(log_file)?;

    let type_filter = resolve_type_filter(types);
    let mut base_ts: Option<i64> = None;
    let mut matched = 0usize;
    let mut emitted = 0usize;

    while let Some(msg) = reader.next_message()? {
        if base_ts.is_none() {
            base_ts = Some(msg.timestamp_ns);
        }
        let base = base_ts.unwrap();
        let relative_s = (msg.timestamp_ns - base) as f64 / 1e9;

        // Type filter
        if !type_filter.contains(&msg.message_id) {
            continue;
        }

        // Time filters
        if let Some(after_s) = after {
            if relative_s < after_s {
                continue;
            }
        }
        if let Some(before_s) = before {
            if relative_s > before_s {
                continue;
            }
        }

        // Offset
        if matched < offset {
            matched += 1;
            continue;
        }
        matched += 1;

        // Emit
        match format {
            OutputFormat::Text => print_message_text(&msg, relative_s),
            OutputFormat::Full => print_message_full(&msg, relative_s, detail),
        }

        emitted += 1;
        if let Some(lim) = limit {
            if emitted >= lim {
                break;
            }
        }
    }

    Ok(())
}

/// Show referee commands and game state transitions.
pub fn run_referee(
    log_file: &Path,
    limit: Option<usize>,
    changes_only: bool,
) -> anyhow::Result<()> {
    let mut reader = LogReader::open(log_file)?;

    let mut base_ts: Option<i64> = None;
    let mut emitted = 0usize;
    let mut prev_command: Option<i32> = None;
    let mut prev_stage: Option<i32> = None;

    while let Some(msg) = reader.next_message()? {
        if base_ts.is_none() {
            base_ts = Some(msg.timestamp_ns);
        }

        if msg.message_id != MessageId::Referee2013 {
            continue;
        }

        let ref_msg = match Referee::decode(msg.payload.as_slice()) {
            Ok(r) => r,
            Err(_) => continue,
        };

        if changes_only {
            let changed =
                prev_command != Some(ref_msg.command) || prev_stage != Some(ref_msg.stage);
            if !changed {
                continue;
            }
            prev_command = Some(ref_msg.command);
            prev_stage = Some(ref_msg.stage);
        }

        let base = base_ts.unwrap();
        let relative_s = (msg.timestamp_ns - base) as f64 / 1e9;

        let stage = Stage::try_from(ref_msg.stage)
            .map(|s| format!("{s:?}"))
            .unwrap_or_else(|_| format!("Stage({})", ref_msg.stage));
        let command = Command::try_from(ref_msg.command)
            .map(|c| format!("{c:?}"))
            .unwrap_or_else(|_| format!("Cmd({})", ref_msg.command));

        let yellow = &ref_msg.yellow;
        let blue = &ref_msg.blue;

        let time_left = ref_msg
            .stage_time_left
            .map(|t| format!(" time_left={:.1}s", t as f64 / 1e6))
            .unwrap_or_default();

        println!(
            "[{relative_s:>10.3}s] {stage} | {command}{time_left} | {yellow_name} {ys} - {bs} {blue_name} | yellow_cards={yc} red_cards={yr} | blue_cards={bc} red_cards={br}",
            yellow_name = yellow.name,
            ys = yellow.score,
            bs = blue.score,
            blue_name = blue.name,
            yc = yellow.yellow_cards,
            yr = yellow.red_cards,
            bc = blue.yellow_cards,
            br = blue.red_cards,
        );

        if !ref_msg.game_events.is_empty() {
            for event in &ref_msg.game_events {
                let type_str = event
                    .r#type
                    .and_then(|t| loguna::proto::game_event::Type::try_from(t).ok())
                    .map(|t| format!("{t:?}"))
                    .unwrap_or_else(|| "?".to_string());
                println!("             game_event: {type_str}");
            }
        }

        emitted += 1;
        if let Some(lim) = limit {
            if emitted >= lim {
                break;
            }
        }
    }

    Ok(())
}

fn resolve_type_filter(types: &[MsgTypeFilter]) -> Vec<MessageId> {
    if types.is_empty() || types.iter().any(|t| matches!(t, MsgTypeFilter::All)) {
        return vec![
            MessageId::Vision2014,
            MessageId::Referee2013,
            MessageId::VisionTracker2020,
            MessageId::Vision2010,
            MessageId::Blank,
            MessageId::Unknown,
            MessageId::Index2021,
        ];
    }

    let mut ids = Vec::new();
    for t in types {
        match t {
            MsgTypeFilter::Vision => ids.push(MessageId::Vision2014),
            MsgTypeFilter::Referee => ids.push(MessageId::Referee2013),
            MsgTypeFilter::Tracker => ids.push(MessageId::VisionTracker2020),
            MsgTypeFilter::Vision2010 => ids.push(MessageId::Vision2010),
            MsgTypeFilter::All => unreachable!(),
        }
    }
    ids.sort_by_key(|id| id.as_i32());
    ids.dedup();
    ids
}

fn format_time(secs: f64) -> String {
    let hours = (secs / 3600.0) as u32;
    let mins = ((secs % 3600.0) / 60.0) as u32;
    let s = secs % 60.0;
    if hours > 0 {
        format!("{hours}:{mins:02}:{s:06.3}")
    } else {
        format!("{mins}:{s:06.3}")
    }
}

fn print_message_text(msg: &loguna::LogMessage, relative_s: f64) {
    let time = format_time(relative_s);
    match msg.message_id {
        MessageId::Vision2014 => {
            if let Ok(wrapper) = SslWrapperPacket::decode(msg.payload.as_slice()) {
                if let Some(ref det) = wrapper.detection {
                    println!(
                        "{time}  vision    cam={} frame={} balls={} yellow={} blue={}",
                        det.camera_id,
                        det.frame_number,
                        det.balls.len(),
                        det.robots_yellow.len(),
                        det.robots_blue.len(),
                    );
                }
                if wrapper.geometry.is_some() {
                    println!("{time}  vision    geometry_update");
                }
            }
        }
        MessageId::Referee2013 => {
            if let Ok(r) = Referee::decode(msg.payload.as_slice()) {
                let cmd = Command::try_from(r.command)
                    .map(|c| format!("{c:?}"))
                    .unwrap_or_else(|_| format!("{}", r.command));
                let stage = Stage::try_from(r.stage)
                    .map(|s| format!("{s:?}"))
                    .unwrap_or_else(|_| format!("{}", r.stage));
                println!(
                    "{time}  referee   stage={stage} cmd={cmd} {} {}-{} {}",
                    r.yellow.name, r.yellow.score, r.blue.score, r.blue.name,
                );
            }
        }
        MessageId::VisionTracker2020 => {
            if let Ok(wrapper) = TrackerWrapperPacket::decode(msg.payload.as_slice()) {
                if let Some(ref frame) = wrapper.tracked_frame {
                    println!(
                        "{time}  tracker   frame={} balls={} robots={}",
                        frame.frame_number,
                        frame.balls.len(),
                        frame.robots.len(),
                    );
                }
            }
        }
        _ => {
            println!(
                "{time}  {:<9} payload_bytes={}",
                msg.message_id.to_string().to_lowercase(),
                msg.payload.len(),
            );
        }
    }
}

fn print_message_full(msg: &loguna::LogMessage, relative_s: f64, detail: bool) {
    let time = format_time(relative_s);
    println!(
        "--- message at {time} ({relative_s:.6}s) type={} payload_bytes={} ---",
        msg.message_id,
        msg.payload.len()
    );

    match msg.message_id {
        MessageId::Vision2014 => {
            if let Ok(wrapper) = SslWrapperPacket::decode(msg.payload.as_slice()) {
                if let Some(ref det) = wrapper.detection {
                    println!("  detection:");
                    println!("    camera_id: {}", det.camera_id);
                    println!("    frame_number: {}", det.frame_number);
                    println!("    t_capture: {:.6}", det.t_capture);
                    println!("    t_sent: {:.6}", det.t_sent);
                    println!("    balls: {}", det.balls.len());
                    if detail {
                        for (i, ball) in det.balls.iter().enumerate() {
                            println!(
                                "      ball[{i}]: x={:.1} y={:.1} z={} conf={:.3}",
                                ball.x,
                                ball.y,
                                ball.z
                                    .map(|z| format!("{z:.1}"))
                                    .unwrap_or_else(|| "none".into()),
                                ball.confidence
                            );
                        }
                    }
                    println!("    robots_yellow: {}", det.robots_yellow.len());
                    if detail {
                        for robot in &det.robots_yellow {
                            println!(
                                "      id={} x={:.1} y={:.1} orient={} conf={:.3}",
                                robot
                                    .robot_id
                                    .map(|id| id.to_string())
                                    .unwrap_or_else(|| "?".into()),
                                robot.x,
                                robot.y,
                                robot
                                    .orientation
                                    .map(|o| format!("{o:.3}"))
                                    .unwrap_or_else(|| "?".into()),
                                robot.confidence
                            );
                        }
                    }
                    println!("    robots_blue: {}", det.robots_blue.len());
                    if detail {
                        for robot in &det.robots_blue {
                            println!(
                                "      id={} x={:.1} y={:.1} orient={} conf={:.3}",
                                robot
                                    .robot_id
                                    .map(|id| id.to_string())
                                    .unwrap_or_else(|| "?".into()),
                                robot.x,
                                robot.y,
                                robot
                                    .orientation
                                    .map(|o| format!("{o:.3}"))
                                    .unwrap_or_else(|| "?".into()),
                                robot.confidence
                            );
                        }
                    }
                }
                if let Some(ref geo) = wrapper.geometry {
                    let f = &geo.field;
                    println!("  geometry:");
                    println!("    field_length: {}", f.field_length);
                    println!("    field_width: {}", f.field_width);
                    println!("    goal_width: {}", f.goal_width);
                    println!("    goal_depth: {}", f.goal_depth);
                    println!("    boundary_width: {}", f.boundary_width);
                    println!("    field_lines: {}", f.field_lines.len());
                    println!("    field_arcs: {}", f.field_arcs.len());
                    println!("    cameras: {}", geo.calib.len());
                }
            }
        }
        MessageId::Referee2013 => {
            if let Ok(r) = Referee::decode(msg.payload.as_slice()) {
                let stage = Stage::try_from(r.stage)
                    .map(|s| format!("{s:?}"))
                    .unwrap_or_else(|_| format!("{}", r.stage));
                let command = Command::try_from(r.command)
                    .map(|c| format!("{c:?}"))
                    .unwrap_or_else(|_| format!("{}", r.command));
                println!("  stage: {stage}");
                println!("  command: {command}");
                println!("  command_counter: {}", r.command_counter);
                if let Some(stl) = r.stage_time_left {
                    println!("  stage_time_left: {:.1}s", stl as f64 / 1e6);
                }
                let y = &r.yellow;
                println!("  yellow: name={} score={} yellow_cards={} red_cards={} timeouts={} goalkeeper={}",
                    y.name, y.score, y.yellow_cards, y.red_cards, y.timeouts, y.goalkeeper);
                if let Some(max) = y.max_allowed_bots {
                    println!("    max_allowed_bots: {max}");
                }
                let b = &r.blue;
                println!("  blue: name={} score={} yellow_cards={} red_cards={} timeouts={} goalkeeper={}",
                    b.name, b.score, b.yellow_cards, b.red_cards, b.timeouts, b.goalkeeper);
                if let Some(max) = b.max_allowed_bots {
                    println!("    max_allowed_bots: {max}");
                }
                if let Some(ref pos) = r.designated_position {
                    println!("  designated_position: x={:.1} y={:.1}", pos.x, pos.y);
                }
                if !r.game_events.is_empty() {
                    println!("  game_events:");
                    for event in &r.game_events {
                        let type_str = event
                            .r#type
                            .and_then(|t| loguna::proto::game_event::Type::try_from(t).ok())
                            .map(|t| format!("{t:?}"))
                            .unwrap_or_else(|| "?".to_string());
                        println!("    - {type_str}");
                    }
                }
            }
        }
        MessageId::VisionTracker2020 => {
            if let Ok(wrapper) = TrackerWrapperPacket::decode(msg.payload.as_slice()) {
                println!("  uuid: {}", wrapper.uuid);
                if let Some(ref name) = wrapper.source_name {
                    println!("  source_name: {name}");
                }
                if let Some(ref frame) = wrapper.tracked_frame {
                    println!("  frame_number: {}", frame.frame_number);
                    println!("  timestamp: {:.6}", frame.timestamp);
                    println!("  balls: {}", frame.balls.len());
                    if detail {
                        for (i, ball) in frame.balls.iter().enumerate() {
                            let p = &ball.pos;
                            println!(
                                "    ball[{i}]: x={:.3} y={:.3} z={:.3} vis={:.2}",
                                p.x,
                                p.y,
                                p.z,
                                ball.visibility.unwrap_or(0.0)
                            );
                        }
                    }
                    println!("  robots: {}", frame.robots.len());
                    if detail {
                        for robot in &frame.robots {
                            let rid = &robot.robot_id;
                            let p = &robot.pos;
                            let team = match rid.team {
                                Some(1) => "yellow",
                                Some(2) => "blue",
                                _ => "unknown",
                            };
                            println!(
                                "    {team} id={} x={:.3} y={:.3} orient={:.3}",
                                rid.id.unwrap_or(0),
                                p.x,
                                p.y,
                                robot.orientation
                            );
                        }
                    }
                }
            }
        }
        _ => {
            println!("  (no structured decoding for this message type)");
        }
    }
}
