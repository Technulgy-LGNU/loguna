mod app;
mod cli;
mod ui;

use std::io;
use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::prelude::*;

use app::App;

#[derive(Parser)]
#[command(name = "ssl-log-viewer", about = "TUI viewer and CLI explorer for RoboCup SSL log files")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Path to the SSL log file (opens TUI if no subcommand given)
    log_file: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Commands {
    /// Open the interactive TUI viewer
    Tui {
        /// Path to the SSL log file
        log_file: PathBuf,
    },

    /// Print log summary statistics
    Stats {
        /// Path to the SSL log file
        log_file: PathBuf,
    },

    /// Dump messages to stdout (designed for LLM consumption)
    Dump {
        /// Path to the SSL log file
        log_file: PathBuf,

        /// Message types to include (comma-separated or repeated)
        #[arg(short = 't', long = "type", value_delimiter = ',')]
        types: Vec<MsgTypeFilter>,

        /// Maximum number of messages to output
        #[arg(short = 'n', long)]
        limit: Option<usize>,

        /// Skip the first N matching messages
        #[arg(long, default_value = "0")]
        offset: usize,

        /// Only show messages after this relative time (seconds from log start)
        #[arg(long)]
        after: Option<f64>,

        /// Only show messages before this relative time (seconds from log start)
        #[arg(long)]
        before: Option<f64>,

        /// Output format
        #[arg(short = 'f', long, default_value = "text")]
        format: OutputFormat,

        /// Show detailed protobuf fields (not just summary)
        #[arg(short = 'd', long)]
        detail: bool,
    },

    /// Show referee commands and game state transitions
    Referee {
        /// Path to the SSL log file
        log_file: PathBuf,

        /// Maximum number of entries to output
        #[arg(short = 'n', long)]
        limit: Option<usize>,

        /// Only show when command changes (deduplicate repeated identical states)
        #[arg(long)]
        changes_only: bool,
    },
}

#[derive(Clone, Debug, ValueEnum)]
enum MsgTypeFilter {
    Vision,
    Referee,
    Tracker,
    Vision2010,
    All,
}

#[derive(Clone, Debug, ValueEnum)]
enum OutputFormat {
    /// Human-readable text with one line per message
    Text,
    /// Structured text with full detail (good for LLM analysis)
    Full,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Tui { log_file }) => run_tui(&log_file),
        Some(Commands::Stats { log_file }) => cli::run_stats(&log_file),
        Some(Commands::Dump {
            log_file,
            types,
            limit,
            offset,
            after,
            before,
            format,
            detail,
        }) => cli::run_dump(&log_file, &types, limit, offset, after, before, &format, detail),
        Some(Commands::Referee {
            log_file,
            limit,
            changes_only,
        }) => cli::run_referee(&log_file, limit, changes_only),
        None => {
            // Default: if a log file was provided positionally, open TUI
            match cli.log_file {
                Some(log_file) => run_tui(&log_file),
                None => {
                    eprintln!("Usage: ssl-log-viewer <LOG_FILE> or ssl-log-viewer <COMMAND>");
                    eprintln!("Try --help for more info.");
                    std::process::exit(1);
                }
            }
        }
    }
}

fn run_tui(log_file: &PathBuf) -> anyhow::Result<()> {
    let app = App::load(log_file)?;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, app);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }

    Ok(())
}

fn run_app<B: Backend>(terminal: &mut Terminal<B>, mut app: App) -> anyhow::Result<()> {
    loop {
        terminal.draw(|f| ui::draw(f, &app))?;

        if let Event::Key(key) = event::read()? {
            match (key.modifiers, key.code) {
                (KeyModifiers::CONTROL, KeyCode::Char('c')) | (_, KeyCode::Char('q')) => {
                    return Ok(());
                }
                (_, KeyCode::Down) | (_, KeyCode::Char('j')) => app.next(),
                (_, KeyCode::Up) | (_, KeyCode::Char('k')) => app.previous(),
                (_, KeyCode::PageDown) => app.page_down(),
                (_, KeyCode::PageUp) => app.page_up(),
                (_, KeyCode::Home) => app.first(),
                (_, KeyCode::End) => app.last(),
                (_, KeyCode::Enter) => app.toggle_detail(),
                (_, KeyCode::Tab) => app.next_tab(),
                (KeyModifiers::SHIFT, KeyCode::BackTab) => app.prev_tab(),
                (_, KeyCode::Char('f')) => app.toggle_filter_menu(),
                (_, KeyCode::Char('1')) => app.toggle_message_filter(loguna::MessageId::Vision2014),
                (_, KeyCode::Char('2')) => app.toggle_message_filter(loguna::MessageId::Referee2013),
                (_, KeyCode::Char('3')) => app.toggle_message_filter(loguna::MessageId::VisionTracker2020),
                (_, KeyCode::Char('4')) => app.toggle_message_filter(loguna::MessageId::Vision2010),
                _ => {}
            }
        }
    }
}
