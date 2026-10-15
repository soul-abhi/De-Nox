use chrono::Local;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph, Wrap},
};
use std::{
    collections::HashMap,
    env, fs,
    io::{self, BufRead, BufReader, Stdout, Write},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, SystemTime},
};
use walkdir::WalkDir;

type AppTerminal = Terminal<CrosstermBackend<Stdout>>;

/// Directory names treated as regenerable build or dependency output.
/// Everything in this list must be reproducible by a standard project
/// command (`npm install`, `cargo build`, ...) — nothing here may hold
/// work that cannot be recreated.
const ARTIFACTS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".output",
    ".turbo",
    ".cache",
    "__pycache__",
    ".pytest_cache",
    "venv",
    ".venv",
];

/// A deletion is refused outright if the path has fewer components than
/// this, which keeps `/`, `/home` and `/home/user` out of reach.
const MIN_PATH_COMPONENTS: usize = 4;

/// Version tag on the first line of the history file, so a future format
/// change can be detected rather than silently misparsed.
const HISTORY_HEADER: &str = "# de-nox history v1";

#[derive(Clone)]
struct Entry {
    name: String,
    path: PathBuf,
    is_dir: bool,
    size: u64,
}

struct Artifact {
    path: PathBuf,
    /// Hours since the owning project was last modified, ignoring the
    /// contents of artifact directories. `None` means unknown.
    idle_hours: Option<u64>,
    /// Only measured for eligible artifacts; walking a live
    /// `node_modules` just to print a number is not worth the I/O.
    size: u64,
    eligible: bool,
    reason: String,
}

struct ScanResult {
    root: PathBuf,
    threshold_hours: u64,
    directories: Vec<PathBuf>,
    artifacts: Vec<Artifact>,
    errors: Vec<String>,
}

impl ScanResult {
    fn candidates(&self) -> impl Iterator<Item = &Artifact> {
        self.artifacts.iter().filter(|a| a.eligible)
    }

    fn reclaimable(&self) -> u64 {
        self.candidates().map(|a| a.size).sum()
    }
}

struct ScanProgress {
    current_path: PathBuf,
    directories: usize,
    artifacts: usize,
    candidates: usize,
}

enum ScanMessage {
    Progress(ScanProgress),
    Finished(ScanResult),
}

struct DeleteProgress {
    current_path: PathBuf,
    done: usize,
    total: usize,
}

struct DeleteOutcome {
    deleted: Vec<(PathBuf, u64)>,
    skipped: Vec<(PathBuf, String)>,
    freed: u64,
    /// A history write failure must never hide the deletion result, so
    /// it is reported alongside it instead of being returned as an error.
    history_error: Option<String>,
}

/// One line of a recorded session: a directory that was removed, or one
/// that was kept and why.
struct HistoryEntry {
    deleted: bool,
    path: PathBuf,
    note: String,
}

struct HistorySession {
    started: String,
    root: PathBuf,
    threshold_hours: u64,
    deleted_count: usize,
    kept_count: usize,
    freed: u64,
    entries: Vec<HistoryEntry>,
}

enum DeleteMessage {
    Progress(DeleteProgress),
    Finished(DeleteOutcome),
}

#[derive(Clone, Copy, PartialEq)]
enum Screen {
    Menu,
    Browser,
    ThresholdInput,
    Scanning,
    ScanResults,
    AllDirectories,
    ConfirmDelete,
    Deleting,
    DeleteReport,
    History,
    HistoryDetail,
}

fn read_entries(path: &Path) -> io::Result<Vec<Entry>> {
    let mut entries = Vec::new();

    for item in fs::read_dir(path)? {
        let item = match item {
            Ok(item) => item,
            Err(_) => continue,
        };

        let path = item.path();

        // symlink_metadata does not follow symbolic links.
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };

        entries.push(Entry {
            name: item.file_name().to_string_lossy().into_owned(),
            path,
            is_dir: metadata.is_dir(),
            size: metadata.len(),
        });
    }

    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Ok(entries)
}

fn file_icon(name: &str, is_dir: bool) -> &'static str {
    if is_dir {
        return match name {
            ".git" => " ",
            "node_modules" => " ",
            _ => "󰉋 ",
        };
    }

    match name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "rs" => "󱘗 ",
        "js" | "jsx" | "mjs" | "cjs" => "󰌞 ",
        "ts" | "tsx" => "󰛦 ",
        "json" => " ",
        "toml" => " ",
        "md" | "mdx" => "󰍔 ",
        "html" => "󰌝 ",
        "css" | "scss" => "󰌜 ",
        "py" => "󰌠 ",
        "java" => "󰬷 ",
        "c" | "h" => " ",
        "cpp" | "hpp" => " ",
        "go" => " ",
        "sh" | "bash" | "fish" => "󱆃 ",
        "yml" | "yaml" => "󰈙 ",
        "lock" => "󰌾 ",
        "png" | "jpg" | "jpeg" | "webp" | "svg" => "󰈟 ",
        "pdf" => "󰈦 ",
        "zip" | "gz" | "tar" => "󰗄 ",
        _ => "󰈔 ",
    }
}

fn format_size(size: u64) -> String {
    if size < 1024 {
        format!("{size} B")
    } else if size < 1024 * 1024 {
        format!("{:.1} KB", size as f64 / 1024.0)
    } else if size < 1024 * 1024 * 1024 {
        format!("{:.1} MB", size as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", size as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

fn format_hours(hours: u64) -> String {
    if hours < 48 {
        format!("{hours}h")
    } else {
        format!("{}d {}h", hours / 24, hours % 24)
    }
}

fn is_artifact_dir(name: &str) -> bool {
    ARTIFACTS.contains(&name)
}

fn is_artifact_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_artifact_dir)
}

/// Newest modification time among a project's own files, skipping the
/// contents of artifact directories. This is the activity signal: files
/// inside `node_modules` are written once at install time and say
/// nothing about whether the project is still in use.
///
/// Returns the timestamp and whether any error was encountered; a
/// partial reading must never be treated as authoritative.
fn project_last_modified(dir: &Path, errors: &mut Vec<String>) -> (Option<SystemTime>, bool) {
    let mut newest: Option<SystemTime> = None;
    let mut failed = false;

    let mut walker = WalkDir::new(dir).follow_links(false).into_iter();

    while let Some(item) = walker.next() {
        let item = match item {
            Ok(item) => item,
            Err(error) => {
                errors.push(error.to_string());
                failed = true;
                continue;
            }
        };

        if item.file_type().is_dir() {
            if item.depth() > 0 && is_artifact_path(item.path()) {
                walker.skip_current_dir();
            }
            continue;
        }

        if !item.file_type().is_file() {
            continue;
        }

        // walkdir::Error and io::Error are distinct types, so both are
        // reduced to a message before being combined.
        let modified = item
            .metadata()
            .map_err(|e| e.to_string())
            .and_then(|m| m.modified().map_err(|e| e.to_string()));

        match modified {
            Ok(time) => newest = Some(newest.map_or(time, |current: SystemTime| current.max(time))),
            Err(error) => {
                errors.push(error);
                failed = true;
            }
        }
    }

    (newest, failed)
}

fn directory_size(dir: &Path) -> u64 {
    WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|item| item.file_type().is_file())
        .filter_map(|item| item.metadata().ok())
        .map(|metadata| metadata.len())
        .sum()
}

fn hours_since(now: SystemTime, then: SystemTime) -> u64 {
    now.duration_since(then).unwrap_or(Duration::ZERO).as_secs() / 3600
}

/// Decides whether one artifact directory may be offered for deletion,
/// returning the idle time and a human-readable reason either way.
fn evaluate_artifact(
    path: &Path,
    threshold_hours: u64,
    now: SystemTime,
    project_times: &mut HashMap<PathBuf, (Option<SystemTime>, bool)>,
    errors: &mut Vec<String>,
) -> (Option<u64>, bool, String) {
    let Some(project) = path.parent().map(Path::to_path_buf) else {
        return (None, false, "has no parent project directory".to_string());
    };

    // Several artifacts commonly share one project (dist + node_modules
    // + .next), so the project walk is cached per parent.
    let (newest, failed) = project_times.entry(project.clone()).or_insert_with(|| {
        let mut local = Vec::new();
        let outcome = project_last_modified(&project, &mut local);
        errors.extend(local);
        outcome
    });

    let (newest, failed) = (*newest, *failed);

    if failed {
        return (
            newest.map(|t| hours_since(now, t)),
            false,
            "project could not be read completely".to_string(),
        );
    }

    let Some(newest) = newest else {
        return (
            None,
            false,
            "project has no readable timestamps".to_string(),
        );
    };

    let idle = hours_since(now, newest);

    if idle >= threshold_hours {
        (
            Some(idle),
            true,
            format!(
                "project idle {} (>= {threshold_hours}h)",
                format_hours(idle)
            ),
        )
    } else {
        (
            Some(idle),
            false,
            format!("project active {} ago", format_hours(idle)),
        )
    }
}

fn scan_directory(root: &Path, threshold_hours: u64, tx: &Sender<ScanMessage>) -> ScanResult {
    let mut result = ScanResult {
        root: root.to_path_buf(),
        threshold_hours,
        directories: Vec::new(),
        artifacts: Vec::new(),
        errors: Vec::new(),
    };

    let now = SystemTime::now();
    let mut project_times: HashMap<PathBuf, (Option<SystemTime>, bool)> = HashMap::new();
    let mut candidates = 0_usize;

    let mut walker = WalkDir::new(root).follow_links(false).into_iter();

    while let Some(item) = walker.next() {
        let item = match item {
            Ok(item) => item,
            Err(error) => {
                result.errors.push(error.to_string());
                continue;
            }
        };

        if !item.file_type().is_dir() {
            continue;
        }

        let path = item.path().to_path_buf();
        result.directories.push(path.clone());

        // The scan root itself is never a candidate, even if it happens
        // to be named like an artifact.
        let is_artifact = item.depth() > 0 && is_artifact_path(&path);

        if is_artifact {
            // Do not descend: an outer artifact already covers anything
            // nested inside it, and this is what keeps the scan fast.
            walker.skip_current_dir();

            let (idle_hours, eligible, reason) = evaluate_artifact(
                &path,
                threshold_hours,
                now,
                &mut project_times,
                &mut result.errors,
            );

            let size = if eligible { directory_size(&path) } else { 0 };

            if eligible {
                candidates += 1;
            }

            result.artifacts.push(Artifact {
                path: path.clone(),
                idle_hours,
                size,
                eligible,
                reason,
            });
        }

        // Report on every artifact and periodically otherwise, so the
        // channel does not carry one message per directory on a large tree.
        if is_artifact || result.directories.len().is_multiple_of(32) {
            let _ = tx.send(ScanMessage::Progress(ScanProgress {
                current_path: path,
                directories: result.directories.len(),
                artifacts: result.artifacts.len(),
                candidates,
            }));
        }
    }

    result
}

/// Every guard that stands between a path and `remove_dir_all`. This is
/// re-run immediately before deletion, not just at scan time, because
/// the filesystem may have changed while the confirmation was on screen.
fn check_deletable(path: &Path, root: &Path, threshold_hours: u64) -> Result<(), String> {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return Err("directory name is not readable".to_string());
    };

    if !is_artifact_dir(name) {
        return Err(format!("'{name}' is not a recognized artifact directory"));
    }

    if path == root {
        return Err("refusing to delete the scan root".to_string());
    }

    if !path.starts_with(root) {
        return Err("path lies outside the scan root".to_string());
    }

    if path.components().count() < MIN_PATH_COMPONENTS {
        return Err("path is too close to the filesystem root".to_string());
    }

    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;

    if metadata.file_type().is_symlink() {
        return Err("path is a symbolic link".to_string());
    }

    if !metadata.is_dir() {
        return Err("path is no longer a directory".to_string());
    }

    // Re-confirm the project is still idle. If the user started working
    // on it between the scan and the confirmation, this catches it.
    let Some(project) = path.parent() else {
        return Err("has no parent project directory".to_string());
    };

    let mut errors = Vec::new();
    let (newest, failed) = project_last_modified(project, &mut errors);

    if failed {
        return Err("project could not be re-read completely".to_string());
    }

    let Some(newest) = newest else {
        return Err("project has no readable timestamps".to_string());
    };

    let idle = hours_since(SystemTime::now(), newest);

    if idle < threshold_hours {
        return Err(format!(
            "project was modified {} ago, under the {threshold_hours}h threshold",
            format_hours(idle)
        ));
    }

    Ok(())
}

/// `history_log` of `None` means the default user location. It exists so
/// tests can record into a temporary file instead of the real log.
fn delete_artifacts(
    queue: Vec<(PathBuf, u64)>,
    root: PathBuf,
    threshold_hours: u64,
    history_log: Option<PathBuf>,
    tx: &Sender<DeleteMessage>,
) -> DeleteOutcome {
    let mut outcome = DeleteOutcome {
        deleted: Vec::new(),
        skipped: Vec::new(),
        freed: 0,
        history_error: None,
    };

    let total = queue.len();

    for (index, (path, size)) in queue.into_iter().enumerate() {
        let _ = tx.send(DeleteMessage::Progress(DeleteProgress {
            current_path: path.clone(),
            done: index,
            total,
        }));

        if let Err(reason) = check_deletable(&path, &root, threshold_hours) {
            outcome.skipped.push((path, reason));
            continue;
        }

        // One failure must not abort the batch; it is recorded and the
        // remaining directories are still processed.
        match fs::remove_dir_all(&path) {
            Ok(()) => {
                outcome.freed += size;
                outcome.deleted.push((path, size));
            }
            Err(error) => outcome.skipped.push((path, error.to_string())),
        }
    }

    // Recorded even when nothing was removed: "I ran a cleanup and it
    // kept everything" is useful history.
    let recorded = match &history_log {
        Some(path) => append_history_to(path, &root, threshold_hours, &outcome),
        None => append_history(&root, threshold_hours, &outcome),
    };

    if let Err(error) = recorded {
        outcome.history_error = Some(error.to_string());
    }

    outcome
}

// --- history -----------------------------------------------------------
//
// Sessions are appended to a tab-separated log rather than stored as
// JSON, which keeps the feature free of a serialisation dependency. The
// format is:
//
//   S <TAB> started <TAB> root <TAB> threshold <TAB> deleted <TAB> kept <TAB> freed
//   E <TAB> D|K     <TAB> path <TAB> size-or-reason
//
// `E` lines belong to the most recent `S` line. Appending means a crash
// mid-write can only ever truncate the tail, never corrupt earlier
// sessions.

fn history_path() -> Option<PathBuf> {
    let base = match env::var_os("XDG_DATA_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(env::var_os("HOME")?).join(".local/share"),
    };

    Some(base.join("de-nox").join("history.log"))
}

/// Paths and error messages may legally contain tabs and newlines, which
/// would break the line format, so those bytes are percent-escaped.
fn escape_field(value: &str) -> String {
    let mut out = String::with_capacity(value.len());

    for ch in value.chars() {
        match ch {
            '%' => out.push_str("%25"),
            '\t' => out.push_str("%09"),
            '\n' => out.push_str("%0A"),
            '\r' => out.push_str("%0D"),
            _ => out.push(ch),
        }
    }

    out
}

fn unescape_field(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();

    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }

        let hex: String = chars.clone().take(2).collect();

        match u8::from_str_radix(&hex, 16) {
            Ok(byte) if hex.len() == 2 => {
                out.push(byte as char);
                chars.next();
                chars.next();
            }
            // Not a valid escape: keep the '%' as written.
            _ => out.push('%'),
        }
    }

    out
}

fn append_history(root: &Path, threshold_hours: u64, outcome: &DeleteOutcome) -> io::Result<()> {
    let path = history_path()
        .ok_or_else(|| io::Error::other("cannot determine a data directory for history"))?;

    append_history_to(&path, root, threshold_hours, outcome)
}

/// Split from `append_history` so the log location is injectable; the
/// default path comes from the environment, which tests must not mutate.
fn append_history_to(
    path: &Path,
    root: &Path,
    threshold_hours: u64,
    outcome: &DeleteOutcome,
) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let is_new = !path.exists();
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;

    if is_new {
        writeln!(file, "{HISTORY_HEADER}")?;
    }

    writeln!(
        file,
        "S\t{}\t{}\t{}\t{}\t{}\t{}",
        Local::now().format("%Y-%m-%d %H:%M:%S"),
        escape_field(&root.to_string_lossy()),
        threshold_hours,
        outcome.deleted.len(),
        outcome.skipped.len(),
        outcome.freed,
    )?;

    for (path, size) in &outcome.deleted {
        writeln!(
            file,
            "E\tD\t{}\t{size}",
            escape_field(&path.to_string_lossy())
        )?;
    }

    for (path, reason) in &outcome.skipped {
        writeln!(
            file,
            "E\tK\t{}\t{}",
            escape_field(&path.to_string_lossy()),
            escape_field(reason),
        )?;
    }

    file.flush()
}

/// Reads every recorded session, newest first. A malformed line is
/// skipped rather than discarding the whole log.
fn load_history() -> (Vec<HistorySession>, Option<String>) {
    let Some(path) = history_path() else {
        return (Vec::new(), Some("no data directory available".to_string()));
    };

    load_history_from(&path)
}

fn load_history_from(path: &Path) -> (Vec<HistorySession>, Option<String>) {
    if !path.exists() {
        return (Vec::new(), None);
    }

    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) => return (Vec::new(), Some(error.to_string())),
    };

    let mut sessions: Vec<HistorySession> = Vec::new();
    let mut skipped = 0_usize;

    for line in BufReader::new(file).lines() {
        let Ok(line) = line else {
            skipped += 1;
            continue;
        };

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let fields: Vec<&str> = line.split('\t').collect();

        match fields.as_slice() {
            ["S", started, root, threshold, deleted, kept, freed] => {
                sessions.push(HistorySession {
                    started: (*started).to_string(),
                    root: PathBuf::from(unescape_field(root)),
                    threshold_hours: threshold.parse().unwrap_or(0),
                    deleted_count: deleted.parse().unwrap_or(0),
                    kept_count: kept.parse().unwrap_or(0),
                    freed: freed.parse().unwrap_or(0),
                    entries: Vec::new(),
                });
            }
            ["E", kind, path, note] => match sessions.last_mut() {
                Some(session) => session.entries.push(HistoryEntry {
                    deleted: *kind == "D",
                    path: PathBuf::from(unescape_field(path)),
                    note: unescape_field(note),
                }),
                // An entry before any session header cannot be placed.
                None => skipped += 1,
            },
            _ => skipped += 1,
        }
    }

    sessions.reverse();

    let warning = (skipped > 0).then(|| format!("{skipped} unreadable line(s) in history"));

    (sessions, warning)
}

/// Raw mode means Ctrl+C never becomes SIGINT: the terminal hands it over
/// as an ordinary key event, so quitting on it has to be implemented
/// explicitly or the key does nothing at all.
///
/// Only a press counts. Terminals using the kitty keyboard protocol also
/// report releases, and treating those as presses would make the
/// confirm-twice guard fire on a single keystroke.
fn is_quit_key(key: &KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
}

fn main() -> io::Result<()> {
    let start_dir = env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or(env::current_dir()?);

    let start_dir = fs::canonicalize(start_dir)?;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let result = run_app(&mut terminal, start_dir);

    let cleanup = (|| -> io::Result<()> {
        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        terminal.show_cursor()?;
        Ok(())
    })();

    result.and(cleanup)
}

fn run_app(terminal: &mut AppTerminal, start_dir: PathBuf) -> io::Result<()> {
    let mut screen = Screen::Menu;
    let menu_labels = ["Safe Scan", "Deep Clean", "History", "Quit"];

    let mut menu_state = ListState::default();
    menu_state.select(Some(0));

    let mut current_dir = start_dir;
    let mut entries = read_entries(&current_dir).unwrap_or_default();

    let mut file_state = ListState::default();
    file_state.select(if entries.is_empty() { None } else { Some(0) });

    let mut threshold_input = String::new();
    let mut threshold_hours = 72_u64;
    let mut error_message: Option<String> = None;

    let mut scan: Option<ScanResult> = None;
    let mut scan_state = ListState::default();
    let mut directory_state = ListState::default();

    let mut scan_rx: Option<Receiver<ScanMessage>> = None;
    let mut scan_progress = ScanProgress {
        current_path: current_dir.clone(),
        directories: 0,
        artifacts: 0,
        candidates: 0,
    };

    let mut delete_queue: Vec<(PathBuf, u64)> = Vec::new();
    let mut delete_rx: Option<Receiver<DeleteMessage>> = None;
    let mut delete_progress = DeleteProgress {
        current_path: PathBuf::new(),
        done: 0,
        total: 0,
    };
    let mut delete_outcome: Option<DeleteOutcome> = None;

    // Reloaded from disk each time the History screen is opened, so a
    // session recorded in another running instance shows up too.
    let mut history: Vec<HistorySession> = Vec::new();
    let mut history_warning: Option<String> = None;
    let mut history_state = ListState::default();
    let mut history_detail_state = ListState::default();

    // Set by a first Ctrl+C during a deletion; a second one then exits.
    let mut force_quit_armed = false;

    let mut animation_frame = 0_usize;

    loop {
        // Receive worker updates without blocking the interface.
        if screen == Screen::Scanning {
            if let Some(rx) = &scan_rx {
                while let Ok(message) = rx.try_recv() {
                    match message {
                        ScanMessage::Progress(update) => scan_progress = update,
                        ScanMessage::Finished(result) => {
                            scan_state.select(if result.artifacts.is_empty() {
                                None
                            } else {
                                Some(0)
                            });
                            directory_state.select(Some(0));
                            scan = Some(result);
                            scan_rx = None;
                            screen = Screen::ScanResults;
                            break;
                        }
                    }
                }
            }
        }

        if screen == Screen::Deleting {
            if let Some(rx) = &delete_rx {
                while let Ok(message) = rx.try_recv() {
                    match message {
                        DeleteMessage::Progress(update) => delete_progress = update,
                        DeleteMessage::Finished(outcome) => {
                            delete_outcome = Some(outcome);
                            delete_rx = None;
                            force_quit_armed = false;
                            error_message = None;
                            // The scan is stale once anything is gone.
                            scan = None;
                            entries = read_entries(&current_dir).unwrap_or_default();
                            file_state.select(if entries.is_empty() { None } else { Some(0) });
                            screen = Screen::DeleteReport;
                            break;
                        }
                    }
                }
            }
        }

        animation_frame = animation_frame.wrapping_add(1);

        terminal.draw(|frame| {
            let sections = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(5),
                    Constraint::Length(3),
                ])
                .split(frame.area());

            let header = Paragraph::new("DE-NOX | Dev Brain Cleanup Tool")
                .block(Block::default().borders(Borders::ALL));

            let footer_text = match screen {
                Screen::Menu => "Up/Down Navigate | Enter Select | q or Ctrl+C Quit",
                Screen::Browser => {
                    "Up/Down | Enter Open | Backspace Parent | Shift+H Scan | Esc Menu | Ctrl+C Quit"
                }
                Screen::ThresholdInput => "Type hours | Enter Start Scan | Esc Cancel | Ctrl+C Quit",
                Screen::Scanning => "Scanning (read-only) | Esc Cancel | Ctrl+C Quit",
                Screen::ScanResults => {
                    "Up/Down | Shift+Enter (or D) Delete | a All Dirs | Esc Back | Ctrl+C Quit"
                }
                Screen::AllDirectories => "Up/Down Navigate | Esc Back to Results | Ctrl+C Quit",
                Screen::ConfirmDelete => "Enter CONFIRM DELETE | Esc Cancel | Ctrl+C Quit",
                Screen::Deleting => "Deleting... do not interrupt | Ctrl+C twice to force quit",
                Screen::DeleteReport => "Enter/Esc Back to Browser | q or Ctrl+C Quit",
                Screen::History => "Up/Down Navigate | Enter Open Session | Esc Menu | Ctrl+C Quit",
                Screen::HistoryDetail => "Up/Down Navigate | Esc Back | Ctrl+C Quit",
            };

            let footer_text = error_message.as_deref().unwrap_or(footer_text);
            let footer = Paragraph::new(footer_text).block(Block::default().borders(Borders::ALL));

            frame.render_widget(header, sections[0]);
            frame.render_widget(footer, sections[2]);

            match screen {
                Screen::Menu => {
                    let items: Vec<ListItem> = menu_labels
                        .iter()
                        .map(|label| ListItem::new(*label))
                        .collect();

                    let menu = List::new(items)
                        .block(Block::default().title(" Menu ").borders(Borders::ALL))
                        .highlight_symbol(">> ")
                        .highlight_style(
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        );

                    frame.render_stateful_widget(menu, sections[1], &mut menu_state);
                }

                Screen::Browser => {
                    let panels = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
                        .split(sections[1]);

                    let file_items: Vec<ListItem> = entries
                        .iter()
                        .map(|entry| {
                            ListItem::new(format!(
                                "{}{}",
                                file_icon(&entry.name, entry.is_dir),
                                entry.name
                            ))
                        })
                        .collect();

                    let list = List::new(file_items)
                        .block(
                            Block::default()
                                .title(format!(" {} ", current_dir.display()))
                                .borders(Borders::ALL),
                        )
                        .highlight_symbol(">> ")
                        .highlight_style(
                            Style::default()
                                .bg(Color::Rgb(100, 35, 70))
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        );

                    let details = match file_state.selected().and_then(|i| entries.get(i)) {
                        Some(entry) => format!(
                            "Name:\n{}\n\nType:\n{}\n\nSize:\n{}\n\nPath:\n{}",
                            entry.name,
                            if entry.is_dir { "Directory" } else { "File" },
                            if entry.is_dir {
                                "-".to_string()
                            } else {
                                format_size(entry.size)
                            },
                            entry.path.display(),
                        ),
                        None => "This directory is empty.".to_string(),
                    };

                    let preview = Paragraph::new(details)
                        .wrap(Wrap { trim: false })
                        .block(
                            Block::default()
                                .title(" Selected Item ")
                                .borders(Borders::ALL),
                        );

                    frame.render_stateful_widget(list, panels[0], &mut file_state);
                    frame.render_widget(preview, panels[1]);
                }

                Screen::ThresholdInput => {
                    let prompt = Paragraph::new(format!(
                        "Scan root:\n{}\n\nIdle threshold in hours: {}\n\n\
                         A build folder is offered for deletion only if its project\n\
                         has not been modified for at least this many hours.\n\n\
                         Example: 32 = anything whose project has been untouched\n\
                         for 32 hours or more.\n\n\
                         Press Enter to start a read-only scan.",
                        current_dir.display(),
                        threshold_input
                    ))
                    .wrap(Wrap { trim: false })
                    .block(Block::default().title(" Safe Scan ").borders(Borders::ALL));

                    frame.render_widget(prompt, sections[1]);
                }

                Screen::Scanning => {
                    let counters = format!(
                        "Directories visited: {}\nArtifact folders found: {}\n\
                         Deletion candidates: {}\nThreshold: {} hours",
                        scan_progress.directories,
                        scan_progress.artifacts,
                        scan_progress.candidates,
                        threshold_hours,
                    );

                    draw_worker_panel(
                        frame,
                        " DE-NOX // SCANNING ",
                        "Inspecting project directories...",
                        &format!("Scanning: {}", scan_progress.current_path.display()),
                        &counters,
                        "Read-only. Nothing is deleted during a scan.",
                        Color::Yellow,
                        animation_frame,
                    );
                }

                Screen::ScanResults => {
                    let panels = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
                        .split(sections[1]);

                    let artifact_items: Vec<ListItem> = scan
                        .as_ref()
                        .map(|s| {
                            s.artifacts
                                .iter()
                                .map(|a| {
                                    let name =
                                        a.path.file_name().and_then(|n| n.to_str()).unwrap_or("?");

                                    let (label, style) = if a.eligible {
                                        ("[CANDIDATE]", Style::default().fg(Color::Yellow))
                                    } else {
                                        ("[PROTECTED]", Style::default().fg(Color::DarkGray))
                                    };

                                    let size = if a.eligible {
                                        format!(" ({})", format_size(a.size))
                                    } else {
                                        String::new()
                                    };

                                    ListItem::new(format!(
                                        "{label} {name}{size} - {}",
                                        a.reason
                                    ))
                                    .style(style)
                                })
                                .collect()
                        })
                        .unwrap_or_default();

                    let list = List::new(artifact_items)
                        .block(
                            Block::default()
                                .title(" Artifact Folders ")
                                .borders(Borders::ALL),
                        )
                        .highlight_symbol(">> ")
                        .highlight_style(Style::default().add_modifier(Modifier::BOLD));

                    let summary = match &scan {
                        Some(s) => {
                            let detail = scan_state
                                .selected()
                                .and_then(|i| s.artifacts.get(i))
                                .map(|a| {
                                    let idle = a
                                        .idle_hours
                                        .map(format_hours)
                                        .unwrap_or_else(|| "unknown".to_string());

                                    format!(
                                        "\n\nSelected:\n{}\n\nProject idle: {}\nVerdict: {}",
                                        a.path.display(),
                                        idle,
                                        if a.eligible {
                                            "candidate for deletion"
                                        } else {
                                            "protected"
                                        },
                                    )
                                })
                                .unwrap_or_default();

                            format!(
                                "Root:\n{}\n\nThreshold: {} hours\n\
                                 Directories visited: {}\nArtifact folders: {}\n\
                                 Deletion candidates: {}\nReclaimable: {}\nWarnings: {}{}",
                                s.root.display(),
                                s.threshold_hours,
                                s.directories.len(),
                                s.artifacts.len(),
                                s.candidates().count(),
                                format_size(s.reclaimable()),
                                s.errors.len(),
                                detail,
                            )
                        }
                        None => "No scan results.".to_string(),
                    };

                    let summary = Paragraph::new(summary).wrap(Wrap { trim: false }).block(
                        Block::default()
                            .title(" Scan Summary ")
                            .borders(Borders::ALL),
                    );

                    frame.render_stateful_widget(list, panels[0], &mut scan_state);
                    frame.render_widget(summary, panels[1]);
                }

                Screen::AllDirectories => {
                    let items: Vec<ListItem> = scan
                        .as_ref()
                        .map(|s| {
                            s.directories
                                .iter()
                                .map(|p| ListItem::new(p.display().to_string()))
                                .collect()
                        })
                        .unwrap_or_default();

                    let list = List::new(items)
                        .block(
                            Block::default()
                                .title(" All Scanned Directories ")
                                .borders(Borders::ALL),
                        )
                        .highlight_symbol(">> ");

                    frame.render_stateful_widget(list, sections[1], &mut directory_state);
                }

                Screen::ConfirmDelete => {
                    let area = centered_area(frame.area(), 78, 70);
                    frame.render_widget(ratatui::widgets::Clear, area);

                    let outer = Block::default()
                        .title(" CONFIRM PERMANENT DELETION ")
                        .borders(Borders::ALL)
                        .border_style(
                            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                        );

                    frame.render_widget(outer, area);

                    let inner = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Length(5),
                            Constraint::Min(3),
                            Constraint::Length(4),
                        ])
                        .margin(1)
                        .split(area);

                    let total: u64 = delete_queue.iter().map(|(_, size)| *size).sum();

                    let warning = Paragraph::new(format!(
                        "{} folder(s) will be permanently deleted, freeing {}.\n\
                         This cannot be undone and does not use the trash.\n\
                         These are regenerable build folders: reinstall or rebuild\n\
                         to get them back.",
                        delete_queue.len(),
                        format_size(total),
                    ))
                    .wrap(Wrap { trim: false })
                    .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD));

                    let items: Vec<ListItem> = delete_queue
                        .iter()
                        .map(|(path, size)| {
                            ListItem::new(format!("{}  ({})", path.display(), format_size(*size)))
                        })
                        .collect();

                    let list = List::new(items).block(
                        Block::default()
                            .title(" Targets ")
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(Color::Red)),
                    );

                    let keys = Paragraph::new(
                        "Press Enter to delete permanently.\nPress Esc to cancel and keep everything.",
                    )
                    .wrap(Wrap { trim: false })
                    .style(Style::default().add_modifier(Modifier::BOLD));

                    frame.render_widget(warning, inner[0]);
                    frame.render_widget(list, inner[1]);
                    frame.render_widget(keys, inner[2]);
                }

                Screen::Deleting => {
                    let counters = format!(
                        "Deleted: {} of {}",
                        delete_progress.done, delete_progress.total
                    );

                    draw_worker_panel(
                        frame,
                        " DE-NOX // DELETING ",
                        "Removing build artifacts...",
                        &format!("Current: {}", delete_progress.current_path.display()),
                        &counters,
                        "Do not interrupt until this finishes.",
                        Color::Red,
                        animation_frame,
                    );
                }

                Screen::DeleteReport => {
                    let panels = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
                        .split(sections[1]);

                    let (items, summary) = match &delete_outcome {
                        Some(outcome) => {
                            let mut items: Vec<ListItem> = outcome
                                .deleted
                                .iter()
                                .map(|(path, size)| {
                                    ListItem::new(format!(
                                        "[DELETED] {}  ({})",
                                        path.display(),
                                        format_size(*size)
                                    ))
                                    .style(Style::default().fg(Color::Green))
                                })
                                .collect();

                            items.extend(outcome.skipped.iter().map(|(path, reason)| {
                                ListItem::new(format!("[KEPT] {} - {reason}", path.display()))
                                    .style(Style::default().fg(Color::Yellow))
                            }));

                            let recorded = match &outcome.history_error {
                                Some(error) => format!("\n\nHistory NOT recorded: {error}"),
                                None => "\n\nRecorded in History.".to_string(),
                            };

                            let summary = format!(
                                "Deleted: {}\nKept: {}\nSpace freed: {}\n\n\
                                 Folders listed as KEPT were refused by a safety\n\
                                 check or failed to delete. Nothing else was touched.{recorded}",
                                outcome.deleted.len(),
                                outcome.skipped.len(),
                                format_size(outcome.freed),
                            );

                            (items, summary)
                        }
                        None => (Vec::new(), "No deletion was performed.".to_string()),
                    };

                    let list = List::new(items).block(
                        Block::default()
                            .title(" Deletion Report ")
                            .borders(Borders::ALL),
                    );

                    let summary = Paragraph::new(summary).wrap(Wrap { trim: false }).block(
                        Block::default()
                            .title(" Result ")
                            .borders(Borders::ALL),
                    );

                    frame.render_widget(list, panels[0]);
                    frame.render_widget(summary, panels[1]);
                }

                Screen::History => {
                    let panels = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
                        .split(sections[1]);

                    let items: Vec<ListItem> = history
                        .iter()
                        .map(|session| {
                            let root = session
                                .root
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("/");

                            ListItem::new(format!(
                                "{}  {:>3} deleted  {:>3} kept  {:>9}  {root}",
                                session.started,
                                session.deleted_count,
                                session.kept_count,
                                format_size(session.freed),
                            ))
                        })
                        .collect();

                    let list = List::new(items)
                        .block(
                            Block::default()
                                .title(" Cleanup Sessions (newest first) ")
                                .borders(Borders::ALL),
                        )
                        .highlight_symbol(">> ")
                        .highlight_style(
                            Style::default()
                                .bg(Color::Rgb(100, 35, 70))
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        );

                    let detail = if history.is_empty() {
                        let location = history_path()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "unavailable".to_string());

                        format!(
                            "No cleanup sessions recorded yet.\n\n\
                             A session is written every time a deletion\n\
                             runs, including one that deleted nothing.\n\n\
                             Log file:\n{location}"
                        )
                    } else {
                        match history_state.selected().and_then(|i| history.get(i)) {
                            Some(session) => format!(
                                "When:\n{}\n\nRoot:\n{}\n\nThreshold: {} hours\n\
                                 Deleted: {}\nKept: {}\nSpace freed: {}\n\n\
                                 Press Enter to see every folder in this session.",
                                session.started,
                                session.root.display(),
                                session.threshold_hours,
                                session.deleted_count,
                                session.kept_count,
                                format_size(session.freed),
                            ),
                            None => "Select a session.".to_string(),
                        }
                    };

                    let detail = match &history_warning {
                        Some(warning) => format!("{detail}\n\nNote: {warning}"),
                        None => detail,
                    };

                    let detail = Paragraph::new(detail).wrap(Wrap { trim: false }).block(
                        Block::default()
                            .title(" Session Summary ")
                            .borders(Borders::ALL),
                    );

                    frame.render_stateful_widget(list, panels[0], &mut history_state);
                    frame.render_widget(detail, panels[1]);
                }

                Screen::HistoryDetail => {
                    let session = history_state.selected().and_then(|i| history.get(i));

                    let title = match session {
                        Some(s) => format!(" {} — {} ", s.started, s.root.display()),
                        None => " Session ".to_string(),
                    };

                    let items: Vec<ListItem> = session
                        .map(|s| {
                            s.entries
                                .iter()
                                .map(|entry| {
                                    if entry.deleted {
                                        let size = entry
                                            .note
                                            .parse::<u64>()
                                            .map(format_size)
                                            .unwrap_or_else(|_| entry.note.clone());

                                        ListItem::new(format!(
                                            "[DELETED] {}  ({size})",
                                            entry.path.display()
                                        ))
                                        .style(Style::default().fg(Color::Green))
                                    } else {
                                        ListItem::new(format!(
                                            "[KEPT] {} - {}",
                                            entry.path.display(),
                                            entry.note
                                        ))
                                        .style(Style::default().fg(Color::Yellow))
                                    }
                                })
                                .collect()
                        })
                        .unwrap_or_default();

                    let items = if items.is_empty() {
                        vec![ListItem::new(
                            "This session removed nothing and kept nothing.",
                        )]
                    } else {
                        items
                    };

                    let list = List::new(items)
                        .block(Block::default().title(title).borders(Borders::ALL))
                        .highlight_symbol(">> ")
                        .highlight_style(Style::default().add_modifier(Modifier::BOLD));

                    frame.render_stateful_widget(list, sections[1], &mut history_detail_state);
                }
            }
        })?;

        // Poll instead of blocking indefinitely, allowing animation and progress updates.
        if event::poll(Duration::from_millis(60))? {
            if let Event::Key(key) = event::read()? {
                if is_quit_key(&key) {
                    // Quitting mid-delete abandons a running remove_dir_all
                    // and can leave a half-removed folder, so that one case
                    // asks for confirmation instead of exiting instantly.
                    if screen == Screen::Deleting && !force_quit_armed {
                        force_quit_armed = true;
                        error_message = Some(
                            "Deletion in progress. Press Ctrl+C again to force quit \
                             (may leave a partly removed folder)."
                                .to_string(),
                        );
                    } else {
                        break;
                    }

                    continue;
                }

                match screen {
                    Screen::Menu => match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Up => {
                            let i = menu_state.selected().unwrap_or(0);
                            menu_state.select(Some(i.saturating_sub(1)));
                        }
                        KeyCode::Down => {
                            let i = menu_state.selected().unwrap_or(0);
                            menu_state.select(Some((i + 1).min(menu_labels.len() - 1)));
                        }
                        KeyCode::Enter => match menu_state.selected() {
                            Some(0) => {
                                error_message = None;
                                screen = Screen::Browser;
                            }
                            Some(1) => {
                                error_message =
                                    Some("Deep Clean is not implemented yet.".to_string())
                            }
                            Some(2) => {
                                let (sessions, warning) = load_history();
                                history = sessions;
                                history_warning = warning;
                                history_state.select(if history.is_empty() {
                                    None
                                } else {
                                    Some(0)
                                });
                                error_message = None;
                                screen = Screen::History;
                            }
                            Some(3) => break,
                            _ => {}
                        },
                        _ => {}
                    },

                    Screen::Browser => match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Esc => {
                            error_message = None;
                            screen = Screen::Menu;
                        }
                        KeyCode::Char('H') | KeyCode::Char('h')
                            if key.modifiers.contains(KeyModifiers::SHIFT) =>
                        {
                            threshold_input.clear();
                            threshold_input.push_str(&threshold_hours.to_string());
                            error_message = None;
                            screen = Screen::ThresholdInput;
                        }
                        KeyCode::Up => {
                            if !entries.is_empty() {
                                let i = file_state.selected().unwrap_or(0);
                                file_state.select(Some(i.saturating_sub(1)));
                            }
                        }
                        KeyCode::Down => {
                            if !entries.is_empty() {
                                let i = file_state.selected().unwrap_or(0);
                                file_state.select(Some((i + 1).min(entries.len() - 1)));
                            }
                        }
                        KeyCode::Enter => {
                            if let Some(entry) = file_state.selected().and_then(|i| entries.get(i))
                            {
                                if entry.is_dir {
                                    let next = entry.path.clone();
                                    match read_entries(&next) {
                                        Ok(new_entries) => {
                                            current_dir = next;
                                            entries = new_entries;
                                            file_state.select(if entries.is_empty() {
                                                None
                                            } else {
                                                Some(0)
                                            });
                                            error_message = None;
                                        }
                                        Err(e) => error_message = Some(e.to_string()),
                                    }
                                }
                            }
                        }
                        KeyCode::Backspace => {
                            if let Some(parent) = current_dir.parent() {
                                let parent = parent.to_path_buf();
                                match read_entries(&parent) {
                                    Ok(new_entries) => {
                                        current_dir = parent;
                                        entries = new_entries;
                                        file_state.select(if entries.is_empty() {
                                            None
                                        } else {
                                            Some(0)
                                        });
                                        error_message = None;
                                    }
                                    Err(e) => error_message = Some(e.to_string()),
                                }
                            }
                        }
                        _ => {}
                    },

                    Screen::ThresholdInput => match key.code {
                        KeyCode::Esc => {
                            error_message = None;
                            screen = Screen::Browser;
                        }
                        KeyCode::Char(c) if c.is_ascii_digit() => threshold_input.push(c),
                        KeyCode::Backspace => {
                            threshold_input.pop();
                        }
                        KeyCode::Enter => match threshold_input.parse::<u64>() {
                            Ok(hours) if hours > 0 => {
                                threshold_hours = hours;
                                error_message = None;
                                scan_progress = ScanProgress {
                                    current_path: current_dir.clone(),
                                    directories: 0,
                                    artifacts: 0,
                                    candidates: 0,
                                };
                                let root = current_dir.clone();
                                let (tx, rx) = mpsc::channel();
                                scan_rx = Some(rx);
                                scan = None;
                                screen = Screen::Scanning;

                                thread::spawn(move || {
                                    let result = scan_directory(&root, hours, &tx);
                                    let _ = tx.send(ScanMessage::Finished(result));
                                });
                            }
                            _ => {
                                error_message =
                                    Some("Enter a whole number greater than zero.".to_string())
                            }
                        },
                        _ => {}
                    },

                    Screen::Scanning => {
                        // Abandon the worker's results; it is read-only, so
                        // letting the thread finish on its own is harmless.
                        if key.code == KeyCode::Esc {
                            scan_rx = None;
                            screen = Screen::Browser;
                        }
                    }

                    Screen::ScanResults => match key.code {
                        KeyCode::Esc => {
                            error_message = None;
                            screen = Screen::Browser;
                        }
                        KeyCode::Up => {
                            let count = scan.as_ref().map(|s| s.artifacts.len()).unwrap_or(0);
                            if count > 0 {
                                let i = scan_state.selected().unwrap_or(0);
                                scan_state.select(Some(i.saturating_sub(1)));
                            }
                        }
                        KeyCode::Down => {
                            let count = scan.as_ref().map(|s| s.artifacts.len()).unwrap_or(0);
                            if count > 0 {
                                let i = scan_state.selected().unwrap_or(0);
                                scan_state.select(Some((i + 1).min(count - 1)));
                            }
                        }
                        KeyCode::Char('a') => {
                            if scan.is_some() {
                                directory_state.select(Some(0));
                                screen = Screen::AllDirectories;
                            }
                        }
                        // Shift+Enter is the documented key, but many terminals
                        // cannot report it, so D is an equivalent fallback.
                        KeyCode::Enter | KeyCode::Char('D')
                            if key.modifiers.contains(KeyModifiers::SHIFT) =>
                        {
                            match &scan {
                                Some(s) if s.candidates().next().is_some() => {
                                    delete_queue =
                                        s.candidates().map(|a| (a.path.clone(), a.size)).collect();
                                    error_message = None;
                                    screen = Screen::ConfirmDelete;
                                }
                                Some(_) => {
                                    error_message =
                                        Some("No deletion candidates in this scan.".to_string())
                                }
                                None => {}
                            }
                        }
                        _ => {}
                    },

                    Screen::AllDirectories => match key.code {
                        KeyCode::Esc => screen = Screen::ScanResults,
                        KeyCode::Up => {
                            let count = scan.as_ref().map(|s| s.directories.len()).unwrap_or(0);
                            if count > 0 {
                                let i = directory_state.selected().unwrap_or(0);
                                directory_state.select(Some(i.saturating_sub(1)));
                            }
                        }
                        KeyCode::Down => {
                            let count = scan.as_ref().map(|s| s.directories.len()).unwrap_or(0);
                            if count > 0 {
                                let i = directory_state.selected().unwrap_or(0);
                                directory_state.select(Some((i + 1).min(count - 1)));
                            }
                        }
                        _ => {}
                    },

                    Screen::ConfirmDelete => match key.code {
                        KeyCode::Esc => {
                            delete_queue.clear();
                            error_message = Some("Cancelled. Nothing was deleted.".to_string());
                            screen = Screen::ScanResults;
                        }
                        KeyCode::Enter => {
                            if delete_queue.is_empty() {
                                screen = Screen::ScanResults;
                            } else {
                                let root = scan
                                    .as_ref()
                                    .map(|s| s.root.clone())
                                    .unwrap_or_else(|| current_dir.clone());
                                let hours = threshold_hours;
                                let queue = std::mem::take(&mut delete_queue);

                                delete_progress = DeleteProgress {
                                    current_path: PathBuf::new(),
                                    done: 0,
                                    total: queue.len(),
                                };

                                let (tx, rx) = mpsc::channel();
                                delete_rx = Some(rx);
                                delete_outcome = None;
                                error_message = None;
                                screen = Screen::Deleting;

                                thread::spawn(move || {
                                    let outcome = delete_artifacts(queue, root, hours, None, &tx);
                                    let _ = tx.send(DeleteMessage::Finished(outcome));
                                });
                            }
                        }
                        _ => {}
                    },

                    Screen::Deleting => {
                        // Deliberately ignores input: interrupting a
                        // remove_dir_all would leave a half-deleted tree.
                    }

                    Screen::DeleteReport => match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Esc | KeyCode::Enter => {
                            error_message = None;
                            screen = Screen::Browser;
                        }
                        _ => {}
                    },

                    Screen::History => match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Esc => {
                            error_message = None;
                            screen = Screen::Menu;
                        }
                        KeyCode::Up => {
                            if !history.is_empty() {
                                let i = history_state.selected().unwrap_or(0);
                                history_state.select(Some(i.saturating_sub(1)));
                            }
                        }
                        KeyCode::Down => {
                            if !history.is_empty() {
                                let i = history_state.selected().unwrap_or(0);
                                history_state.select(Some((i + 1).min(history.len() - 1)));
                            }
                        }
                        KeyCode::Enter
                            if history_state
                                .selected()
                                .and_then(|i| history.get(i))
                                .is_some() =>
                        {
                            history_detail_state.select(Some(0));
                            screen = Screen::HistoryDetail;
                        }
                        _ => {}
                    },

                    Screen::HistoryDetail => match key.code {
                        KeyCode::Esc => screen = Screen::History,
                        KeyCode::Up => {
                            let i = history_detail_state.selected().unwrap_or(0);
                            history_detail_state.select(Some(i.saturating_sub(1)));
                        }
                        KeyCode::Down => {
                            let count = history_state
                                .selected()
                                .and_then(|i| history.get(i))
                                .map(|s| s.entries.len())
                                .unwrap_or(0);

                            if count > 0 {
                                let i = history_detail_state.selected().unwrap_or(0);
                                history_detail_state.select(Some((i + 1).min(count - 1)));
                            }
                        }
                        _ => {}
                    },
                }
            }
        }
    }

    Ok(())
}

/// Shared chrome for the two long-running worker screens. The bar is an
/// indeterminate animation: the work has no knowable total, so no
/// percentage is invented.
#[allow(clippy::too_many_arguments)]
fn draw_worker_panel(
    frame: &mut Frame,
    title: &str,
    headline: &str,
    status: &str,
    counters: &str,
    hint: &str,
    accent: Color,
    animation_frame: usize,
) {
    let area = centered_area(frame.area(), 78, 60);
    frame.render_widget(ratatui::widgets::Clear, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(3),
            Constraint::Length(2),
            Constraint::Length(5),
            Constraint::Min(1),
        ])
        .margin(1)
        .split(area);

    let outer = Block::default()
        .title(title.to_string())
        .borders(Borders::ALL)
        .border_style(Style::default().fg(accent));

    frame.render_widget(outer, area);

    let headline = Paragraph::new(headline.to_string()).style(
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(headline, chunks[0]);

    let spinner = ["|", "/", "-", "\\"][animation_frame % 4];
    let status = Paragraph::new(format!("{spinner}  {status}"))
        .wrap(Wrap { trim: true })
        .style(Style::default().fg(Color::Cyan));
    frame.render_widget(status, chunks[1]);

    let bar_width = 24_usize;
    let position = animation_frame % (bar_width + 1);
    let bar: String = (0..bar_width)
        .map(|i| {
            if i >= position && i < position + 5 {
                '#'
            } else {
                '.'
            }
        })
        .collect();

    let gauge = Gauge::default()
        .gauge_style(Style::default().fg(accent))
        .label(format!("[{bar}]"))
        .ratio(1.0);
    frame.render_widget(gauge, chunks[2]);

    let counters = Paragraph::new(counters.to_string()).wrap(Wrap { trim: false });
    frame.render_widget(counters, chunks[3]);

    let hint = Paragraph::new(hint.to_string())
        .wrap(Wrap { trim: true })
        .style(Style::default().fg(Color::DarkGray));
    frame.render_widget(hint, chunks[4]);
}

fn centered_area(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn unique_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        let base =
            env::temp_dir().join(format!("de-nox-test-{tag}-{}-{nanos}", std::process::id()));

        fs::create_dir_all(&base).unwrap();
        base
    }

    /// Creates a file and backdates its modification time.
    fn write_file(path: &Path, hours_ago: u64) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = fs::File::create(path).unwrap();
        file.write_all(b"content").unwrap();

        let when = SystemTime::now() - Duration::from_secs(hours_ago * 3600);
        file.set_times(fs::FileTimes::new().set_modified(when))
            .unwrap();
    }

    fn evaluate(path: &Path, threshold: u64) -> (Option<u64>, bool, String) {
        let mut cache = HashMap::new();
        let mut errors = Vec::new();
        evaluate_artifact(path, threshold, SystemTime::now(), &mut cache, &mut errors)
    }

    #[test]
    fn project_activity_ignores_artifact_contents() {
        let root = unique_dir("activity");
        let project = root.join("app");

        write_file(&project.join("src/index.js"), 100);
        // Freshly installed dependencies must not make the project look active.
        write_file(&project.join("node_modules/left-pad/index.js"), 0);

        let (newest, failed) = project_last_modified(&project, &mut Vec::new());

        assert!(!failed);
        let idle = hours_since(SystemTime::now(), newest.unwrap());
        assert!(
            idle >= 99,
            "expected the 100h source file to win, got {idle}h"
        );

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn idle_project_yields_a_candidate() {
        let root = unique_dir("idle");
        let project = root.join("app");

        write_file(&project.join("src/index.js"), 100);
        write_file(&project.join("node_modules/pkg/index.js"), 0);

        let (idle, eligible, reason) = evaluate(&project.join("node_modules"), 32);

        assert!(eligible, "expected a candidate, reason was: {reason}");
        assert!(idle.unwrap() >= 99);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn active_project_is_protected() {
        let root = unique_dir("active");
        let project = root.join("app");

        write_file(&project.join("src/index.js"), 1);
        write_file(&project.join("node_modules/pkg/index.js"), 500);

        let (_, eligible, reason) = evaluate(&project.join("node_modules"), 32);

        assert!(!eligible, "an active project must be protected");
        assert!(reason.contains("active"), "unexpected reason: {reason}");

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn project_without_timestamps_is_protected() {
        let root = unique_dir("empty");
        let project = root.join("app");
        fs::create_dir_all(project.join("node_modules")).unwrap();

        let (_, eligible, _) = evaluate(&project.join("node_modules"), 32);

        assert!(!eligible, "no readable timestamp must never qualify");

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn delete_guard_accepts_a_stale_artifact() {
        let root = unique_dir("guard-ok");
        let project = root.join("app");

        write_file(&project.join("src/index.js"), 100);
        write_file(&project.join("node_modules/pkg/index.js"), 0);

        assert_eq!(
            check_deletable(&project.join("node_modules"), &root, 32),
            Ok(())
        );

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn delete_guard_rejects_unknown_directory_names() {
        let root = unique_dir("guard-name");
        let project = root.join("app");

        write_file(&project.join("src/index.js"), 100);
        fs::create_dir_all(project.join("src")).unwrap();

        let error = check_deletable(&project.join("src"), &root, 32).unwrap_err();
        assert!(error.contains("not a recognized artifact"), "{error}");

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn delete_guard_rejects_the_scan_root() {
        let root = unique_dir("guard-root").join("target");
        fs::create_dir_all(&root).unwrap();

        let error = check_deletable(&root, &root, 32).unwrap_err();
        assert!(error.contains("scan root"), "{error}");

        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[test]
    fn delete_guard_rejects_paths_outside_the_root() {
        let root = unique_dir("guard-outside");
        let other = unique_dir("guard-elsewhere");

        write_file(&other.join("app/src/index.js"), 100);
        fs::create_dir_all(other.join("app/node_modules")).unwrap();

        let error = check_deletable(&other.join("app/node_modules"), &root, 32).unwrap_err();
        assert!(error.contains("outside the scan root"), "{error}");

        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&other).unwrap();
    }

    #[test]
    fn delete_guard_rejects_a_project_touched_since_the_scan() {
        let root = unique_dir("guard-raced");
        let project = root.join("app");

        write_file(&project.join("src/index.js"), 100);
        fs::create_dir_all(project.join("node_modules")).unwrap();

        // The scan saw an idle project; the user then edited a file.
        write_file(&project.join("src/index.js"), 0);

        let error = check_deletable(&project.join("node_modules"), &root, 32).unwrap_err();
        assert!(error.contains("under the 32h threshold"), "{error}");

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn delete_guard_rejects_shallow_paths() {
        let shallow = Path::new("/target");
        let error = check_deletable(shallow, Path::new("/"), 32).unwrap_err();
        assert!(
            error.contains("too close to the filesystem root"),
            "{error}"
        );
    }

    #[test]
    fn scan_skips_nested_artifacts() {
        let root = unique_dir("nested");
        let project = root.join("app");

        write_file(&project.join("src/index.js"), 100);
        write_file(&project.join("node_modules/a/node_modules/b/index.js"), 0);

        let (tx, _rx) = mpsc::channel();
        let result = scan_directory(&root, 32, &tx);

        // The inner node_modules is covered by the outer one, so only
        // one artifact may be reported.
        assert_eq!(
            result.artifacts.len(),
            1,
            "nested artifacts must not be listed separately"
        );
        assert!(result.artifacts[0].eligible);
        assert!(
            result.reclaimable() > 0,
            "candidate size should be measured"
        );

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn delete_removes_only_stale_artifacts_and_reports_the_rest() {
        let root = unique_dir("delete");

        // Idle project: its node_modules must go.
        let stale = root.join("idle-app");
        write_file(&stale.join("src/index.js"), 100);
        write_file(&stale.join("node_modules/pkg/index.js"), 0);

        // Active project: its node_modules must survive.
        let active = root.join("live-app");
        write_file(&active.join("src/index.js"), 1);
        write_file(&active.join("node_modules/pkg/index.js"), 0);

        let queue = vec![
            (stale.join("node_modules"), 10),
            (active.join("node_modules"), 20),
        ];

        let (tx, _rx) = mpsc::channel();
        let log = root.join("history.log");
        let outcome = delete_artifacts(queue, root.clone(), 32, Some(log.clone()), &tx);

        assert_eq!(outcome.deleted.len(), 1);
        assert_eq!(outcome.skipped.len(), 1);
        assert_eq!(outcome.freed, 10);

        assert!(
            !stale.join("node_modules").exists(),
            "stale artifact should be gone"
        );
        assert!(
            active.join("node_modules").exists(),
            "active artifact must survive"
        );
        assert!(
            stale.join("src/index.js").exists(),
            "source files must never be touched"
        );

        fs::remove_dir_all(&root).unwrap();
    }

    fn outcome(deleted: Vec<(&str, u64)>, skipped: Vec<(&str, &str)>) -> DeleteOutcome {
        DeleteOutcome {
            freed: deleted.iter().map(|(_, size)| *size).sum(),
            deleted: deleted
                .into_iter()
                .map(|(p, size)| (PathBuf::from(p), size))
                .collect(),
            skipped: skipped
                .into_iter()
                .map(|(p, r)| (PathBuf::from(p), r.to_string()))
                .collect(),
            history_error: None,
        }
    }

    #[test]
    fn history_escaping_survives_awkward_paths() {
        // Tabs and newlines are legal in filenames and would otherwise
        // break the line-based format.
        for raw in [
            "/plain/path",
            "/with\ttab",
            "/with\nnewline",
            "/with%percent",
            "/with%09literal-escape-text",
            "/mixed\t%\n/end",
        ] {
            assert_eq!(
                unescape_field(&escape_field(raw)),
                raw,
                "roundtrip failed for {raw:?}"
            );
        }

        assert!(!escape_field("/a\tb").contains('\t'));
        assert!(!escape_field("/a\nb").contains('\n'));
    }

    #[test]
    fn history_roundtrips_a_session() {
        let dir = unique_dir("history-roundtrip");
        let log = dir.join("history.log");

        let first = outcome(
            vec![("/proj/node_modules", 2048), ("/proj/dist", 512)],
            vec![("/other/target", "project was modified 2h ago")],
        );

        append_history_to(&log, Path::new("/proj"), 32, &first).unwrap();

        let (sessions, warning) = load_history_from(&log);
        assert_eq!(warning, None);
        assert_eq!(sessions.len(), 1);

        let s = &sessions[0];
        assert_eq!(s.root, PathBuf::from("/proj"));
        assert_eq!(s.threshold_hours, 32);
        assert_eq!(s.deleted_count, 2);
        assert_eq!(s.kept_count, 1);
        assert_eq!(s.freed, 2560);
        assert_eq!(s.entries.len(), 3);

        assert!(s.entries[0].deleted);
        assert_eq!(s.entries[0].path, PathBuf::from("/proj/node_modules"));
        assert_eq!(s.entries[0].note, "2048");

        let kept = s.entries.iter().find(|e| !e.deleted).unwrap();
        assert_eq!(kept.note, "project was modified 2h ago");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn history_lists_newest_session_first() {
        let dir = unique_dir("history-order");
        let log = dir.join("history.log");

        append_history_to(&log, Path::new("/first"), 10, &outcome(vec![], vec![])).unwrap();
        append_history_to(
            &log,
            Path::new("/second"),
            20,
            &outcome(vec![("/second/target", 100)], vec![]),
        )
        .unwrap();

        let (sessions, _) = load_history_from(&log);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].root, PathBuf::from("/second"));
        assert_eq!(sessions[1].root, PathBuf::from("/first"));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn history_records_a_session_that_deleted_nothing() {
        let dir = unique_dir("history-empty-session");
        let log = dir.join("history.log");

        let kept_everything = outcome(
            vec![],
            vec![("/proj/node_modules", "project active 1h ago")],
        );
        append_history_to(&log, Path::new("/proj"), 32, &kept_everything).unwrap();

        let (sessions, _) = load_history_from(&log);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].deleted_count, 0);
        assert_eq!(sessions[0].kept_count, 1);
        assert_eq!(sessions[0].freed, 0);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn history_survives_a_corrupt_line() {
        let dir = unique_dir("history-corrupt");
        let log = dir.join("history.log");

        append_history_to(
            &log,
            Path::new("/good"),
            32,
            &outcome(vec![("/good/dist", 1)], vec![]),
        )
        .unwrap();

        // Simulate a truncated write followed by a later good session.
        let mut file = fs::OpenOptions::new().append(true).open(&log).unwrap();
        writeln!(file, "S\tbroken-row-with-too-few-fields").unwrap();
        writeln!(file, "garbage").unwrap();
        drop(file);

        append_history_to(&log, Path::new("/later"), 48, &outcome(vec![], vec![])).unwrap();

        let (sessions, warning) = load_history_from(&log);
        assert_eq!(sessions.len(), 2, "good sessions must still load");
        assert!(warning.is_some(), "the skipped lines should be reported");
        assert_eq!(sessions[0].root, PathBuf::from("/later"));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn history_is_empty_when_no_log_exists() {
        let dir = unique_dir("history-absent");
        let (sessions, warning) = load_history_from(&dir.join("does-not-exist.log"));

        assert!(sessions.is_empty());
        assert_eq!(warning, None);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn delete_writes_a_history_session() {
        let dir = unique_dir("history-from-delete");
        let root = dir.join("bed");
        let project = root.join("app");

        write_file(&project.join("src/index.js"), 100);
        write_file(&project.join("node_modules/pkg/index.js"), 0);

        let (tx, _rx) = mpsc::channel();
        let queue = vec![(project.join("node_modules"), 1234)];
        let log = dir.join("history.log");
        let result = delete_artifacts(queue, root.clone(), 32, Some(log.clone()), &tx);

        assert_eq!(result.deleted.len(), 1);
        assert_eq!(
            result.history_error, None,
            "history write failed: {:?}",
            result.history_error
        );

        // The wiring is the point: a delete must leave a readable session.
        let (sessions, warning) = load_history_from(&log);
        assert_eq!(warning, None);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].root, root);
        assert_eq!(sessions[0].deleted_count, 1);
        assert_eq!(sessions[0].freed, 1234);
        assert_eq!(sessions[0].entries.len(), 1);
        assert!(sessions[0].entries[0].deleted);
        assert_eq!(sessions[0].entries[0].path, project.join("node_modules"));

        fs::remove_dir_all(&dir).unwrap();
    }

    fn key(code: KeyCode, modifiers: KeyModifiers, kind: KeyEventKind) -> KeyEvent {
        let mut event = KeyEvent::new(code, modifiers);
        event.kind = kind;
        event
    }

    #[test]
    fn ctrl_c_is_recognised_as_the_quit_key() {
        assert!(is_quit_key(&key(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
            KeyEventKind::Press
        )));

        // Some terminals deliver Ctrl+Shift+C as an uppercase C.
        assert!(is_quit_key(&key(
            KeyCode::Char('C'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            KeyEventKind::Press
        )));
    }

    #[test]
    fn other_keys_are_not_the_quit_key() {
        // A bare 'c' must stay usable as ordinary input.
        assert!(!is_quit_key(&key(
            KeyCode::Char('c'),
            KeyModifiers::NONE,
            KeyEventKind::Press
        )));

        assert!(!is_quit_key(&key(
            KeyCode::Char('d'),
            KeyModifiers::CONTROL,
            KeyEventKind::Press
        )));

        assert!(!is_quit_key(&key(
            KeyCode::Enter,
            KeyModifiers::CONTROL,
            KeyEventKind::Press
        )));
    }

    #[test]
    fn key_release_is_not_a_quit() {
        // Otherwise one keystroke would count twice and skip the
        // confirm-again guard shown during a deletion.
        for kind in [KeyEventKind::Release, KeyEventKind::Repeat] {
            assert!(
                !is_quit_key(&key(KeyCode::Char('c'), KeyModifiers::CONTROL, kind)),
                "{kind:?} must not quit"
            );
        }
    }
}
