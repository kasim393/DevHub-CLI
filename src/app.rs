use crate::config::{self, Config};
use crate::proc::{self, Log, Msg, Sink};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::ListState;
use ratatui::DefaultTerminal;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Idle,
    Starting,
    Running,
    Stopping,
    Done,
    Failed(i32),
}

impl Status {
    pub fn is_live(self) -> bool {
        matches!(self, Status::Starting | Status::Running | Status::Stopping)
    }
}

pub struct Unit {
    pub key: String,
    pub name: String,
    pub path: PathBuf,
    pub cmd: String,
    /// None for a group's step (listed under its group instead).
    pub category: Option<String>,
    pub group: Option<usize>,
    pub cfg_port: Option<u16>,
    pub stop_cmd: Option<String>,
    pub status: Status,
    pub pid: Option<u32>,
    pub run: u64,
    pub stopping: bool,
    pub port: Option<u16>,
    pub branch: Option<String>,
    pub log: Log,
    /// Projects started first (indices into `units`), resolved from `needs` names.
    pub needs: Vec<usize>,
    needs_names: Vec<String>,
    /// Set while this unit waits for its needs' ports before it starts.
    waiting: Option<(u64, Arc<AtomicBool>)>,
    /// Place within its section (from the section's `items`); MAX sorts by name after.
    pub pos: usize,
    pub metrics: Metrics,
    /// When the current run started.
    pub started: Option<Instant>,
    /// Whether its port answered the last check (None: no port, or not checked yet).
    pub up: Option<bool>,
    /// While running: the process's start time and its log file (for `running.toml`).
    proc_started: Option<u64>,
    log_file: Option<PathBuf>,
}

/// Samples kept for the charts: 150 × 2 s = the last 5 minutes.
pub const HISTORY: usize = 150;

/// What the dashboard charts for one project, sampled every 2 s while it runs.
#[derive(Default, Clone)]
pub struct Metrics {
    pub cpu: VecDeque<f32>,
    pub mem: VecDeque<u64>,
    /// Average request time per sample; None when no request came in.
    pub rt: VecDeque<Option<f64>>,
    /// Output lines per second, per sample.
    pub rate: VecDeque<f32>,
    /// Error lines per minute, the last 60 minutes.
    pub errors: VecDeque<u32>,
    pub rt_peak: f64,
    pub server_errors: u64,
    last_total: u64,
    last_errors: u64,
    minute_started: Option<Instant>,
}

fn push_capped<T>(q: &mut VecDeque<T>, v: T, cap: usize) {
    if q.len() >= cap {
        q.pop_front();
    }
    q.push_back(v);
}

impl Metrics {
    /// One 2 s sample: CPU and memory from the stats thread, the rest from the log counters.
    fn sample(&mut self, cpu: f32, mem: u64, log: &Log) {
        push_capped(&mut self.cpu, cpu, HISTORY);
        push_capped(&mut self.mem, mem, HISTORY);
        let Ok(mut l) = log.lock() else { return };
        let lines = l.total.saturating_sub(self.last_total);
        let errs = l.errors.saturating_sub(self.last_errors) as u32;
        self.last_total = l.total;
        self.last_errors = l.errors;
        push_capped(&mut self.rate, lines as f32 / 2.0, HISTORY);
        let req = l.take_requests();
        if let Some((_, _, max)) = req {
            self.rt_peak = self.rt_peak.max(max);
        }
        self.server_errors = l.server_errors;
        push_capped(&mut self.rt, req.map(|(_, avg, _)| avg), HISTORY);
        let new_minute = self.minute_started.is_none_or(|t| t.elapsed() >= Duration::from_secs(60));
        if new_minute {
            push_capped(&mut self.errors, 0, 60);
            self.minute_started = Some(Instant::now());
        }
        if let Some(last) = self.errors.back_mut() {
            *last += errs;
        }
    }

    /// Starts counting from the log's current totals (after a restart).
    fn reset_from(log: &Log) -> Self {
        let mut m = Metrics::default();
        if let Ok(mut l) = log.lock() {
            m.last_total = l.total;
            m.last_errors = l.errors;
            l.take_requests();
        }
        m
    }
}

/// One server devhub started, as remembered in `running.toml` so a later devhub can
/// find it again after this one closes.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct RunEntry {
    pub key: String,
    pub pid: u32,
    /// The process's own start time, to tell it apart from a reused pid.
    pub started: u64,
    pub log: PathBuf,
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Registry {
    #[serde(default)]
    run: Vec<RunEntry>,
}

pub fn registry_path() -> PathBuf {
    proc::data_dir().join("running.toml")
}

fn load_registry() -> Vec<RunEntry> {
    std::fs::read_to_string(registry_path())
        .ok()
        .and_then(|t| toml::from_str::<Registry>(&t).ok())
        .map(|r| r.run)
        .unwrap_or_default()
}

pub struct Step {
    pub unit: usize,
    pub wait_exit: bool,
    pub wait_port: Option<u16>,
}

pub struct Group {
    /// Unique: `section/name`, so two sections may each have a group called "stack".
    pub id: String,
    pub name: String,
    pub section: Option<String>,
    /// Place within its section: 0 = top, otherwise its index in the section's items + 1.
    pub pos: usize,
    pub steps: Vec<Step>,
    pub log: Log,
    pub runner: Option<(u64, Arc<AtomicBool>)>,
    /// Units the running runner may still start; reset if it ends before starting them.
    pending: Vec<usize>,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Row {
    Header(String),
    Group(usize),
    Unit(usize),
    /// A unit listed under group `.0` (its own step, or a shared project).
    Step(usize, usize),
}

impl Row {
    pub fn unit(&self) -> Option<usize> {
        match self {
            Row::Unit(u) | Row::Step(_, u) => Some(*u),
            _ => None,
        }
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Mode {
    Normal,
    Filter,
    Help,
    ConfirmQuit,
    Search,
    Branches,
}

/// The branch picker (`b`): one project's branches, filtered by what was typed.
pub struct Picker {
    pub path: PathBuf,
    pub project: String,
    pub items: Vec<proc::BranchItem>,
    pub filter: String,
    pub sel: usize,
    pub changed: usize,
    pub busy: bool,
    pub error: Option<String>,
    /// A background `git fetch` is running; the list refreshes when it ends.
    pub fetching: bool,
    /// Why the last fetch failed (offline, no access…); the list is then the local one.
    pub fetch_error: Option<String>,
}

impl Picker {
    /// Indices of the branches that match the filter.
    pub fn visible(&self) -> Vec<usize> {
        let f = self.filter.to_lowercase();
        (0..self.items.len())
            .filter(|&i| f.is_empty() || self.items[i].name.to_lowercase().contains(&f))
            .collect()
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Tab {
    Log,
    Git,
}

pub struct App {
    /// Servers still running from an earlier session whose project is no longer in the config.
    pub orphans: Vec<RunEntry>,
    /// Set by "leave running" on quit: servers keep going after devhub ends.
    leave_running: bool,
    /// The background last sent to the terminal itself (OSC 11); None = never sent.
    painted_bg: Option<Option<ratatui::style::Color>>,
    /// The neon colours (solid, or see-through with `background = "none"`).
    pub theme: crate::theme::Theme,
    pub picker: Option<Picker>,
    /// Right-hand panel: the log or git info.
    pub tab: Tab,
    /// CPU % and memory bytes per root pid, from the stats thread.
    pub stats: HashMap<u32, (f32, u64)>,
    stat_pids: Arc<std::sync::Mutex<Vec<u32>>>,
    health_targets: Arc<std::sync::Mutex<Vec<(u32, u16)>>>,
    /// Something changed since the last frame: draw again.
    dirty: bool,
    last_draw: Instant,
    /// Git info per repo folder, and when it was read.
    pub git: HashMap<PathBuf, (proc::GitInfo, Instant)>,
    git_loading: HashSet<PathBuf>,
    /// Find-in-log text and the log line it currently points at.
    pub search: String,
    pub search_hit: Option<usize>,
    /// Rows the log panel showed last frame, and which log / how long it was then.
    pub log_rows: usize,
    pub log_seen: (Option<String>, usize),
    pub cfg_path: PathBuf,
    pub units: Vec<Unit>,
    pub groups: Vec<Group>,
    pub open: Vec<(char, String)>,
    pub editor: String,
    pub rows: Vec<Row>,
    pub selected: usize,
    pub list_state: ListState,
    pub filter: String,
    pub mode: Mode,
    pub scroll: usize,
    pub toast: Option<(String, Instant)>,
    pub error: Option<String>,
    quit: bool,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    restart_units: HashSet<String>,
    restart_groups: HashSet<String>,
    /// Section names, top to bottom.
    pub cats: Vec<String>,
    /// Sections folded shut (remembered in state.toml beside the config).
    pub collapsed: HashSet<String>,
    next_token: u64,
}

impl App {
    pub fn new(cfg_path: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        let stat_pids = Arc::new(std::sync::Mutex::new(Vec::new()));
        proc::spawn_stats(stat_pids.clone(), tx.clone());
        let health_targets = Arc::new(std::sync::Mutex::new(Vec::new()));
        proc::spawn_health(health_targets.clone(), tx.clone());
        let mut app = App {
            orphans: Vec::new(),
            leave_running: false,
            painted_bg: None,
            theme: crate::theme::NEON,
            picker: None,
            tab: Tab::Log,
            stats: HashMap::new(),
            stat_pids,
            health_targets,
            dirty: true,
            last_draw: Instant::now(),
            git: HashMap::new(),
            git_loading: HashSet::new(),
            search: String::new(),
            search_hit: None,
            log_rows: 20,
            log_seen: (None, 0),
            cfg_path,
            units: Vec::new(),
            groups: Vec::new(),
            open: Vec::new(),
            editor: "code".into(),
            rows: Vec::new(),
            selected: 0,
            list_state: ListState::default(),
            filter: String::new(),
            mode: Mode::Normal,
            scroll: 0,
            toast: None,
            error: None,
            quit: false,
            tx,
            rx,
            restart_units: HashSet::new(),
            restart_groups: HashSet::new(),
            cats: Vec::new(),
            collapsed: HashSet::new(),
            next_token: 1,
        };
        app.load_state();
        app.reload();
        app.reattach();
        app
    }

    // ---------------------------------------------------------------- loop

    pub fn run(&mut self, term: &mut DefaultTerminal) -> anyhow::Result<()> {
        while !self.quit {
            if self.drain() {
                self.dirty = true;
            }
            self.tick();
            // Draw when something changed, and once a second for the clock and timers.
            if self.dirty || self.last_draw.elapsed() >= Duration::from_secs(1) {
                self.paint_terminal();
                term.draw(|f| crate::ui::draw(f, self))?;
                self.dirty = false;
                self.last_draw = Instant::now();
            }
            if event::poll(Duration::from_millis(80))? {
                match event::read()? {
                    Event::Key(k) if k.kind == KeyEventKind::Press => {
                        self.on_key(k);
                        self.dirty = true;
                    }
                    Event::Resize(..) => self.dirty = true,
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// Gives the terminal itself the theme's background, so the padding around the
    /// grid matches. Sent only when it changes; nothing until a theme has a colour.
    fn paint_terminal(&mut self) {
        let bg = self.theme.background;
        if self.painted_bg == Some(bg) || (self.painted_bg.is_none() && bg.is_none()) {
            self.painted_bg = Some(bg);
            return;
        }
        crate::theme::emit(&crate::theme::terminal_background_code(bg));
        self.painted_bg = Some(bg);
    }

    /// Whether devhub changed the terminal's background (and must put it back on exit).
    pub fn painted_terminal(&self) -> bool {
        matches!(self.painted_bg, Some(Some(_)))
    }

    /// Stops everything this session started. Called on exit.
    pub fn shutdown(&mut self) {
        if self.leave_running {
            // They keep running; the next devhub finds them through running.toml.
            self.save_registry();
            return;
        }
        for g in &self.groups {
            if let Some((_, c)) = &g.runner {
                c.store(true, Ordering::Relaxed);
            }
        }
        for u in &self.units {
            if let Some((_, c)) = &u.waiting {
                c.store(true, Ordering::Relaxed);
            }
        }
        let pids: Vec<u32> = self
            .units
            .iter()
            .filter_map(|u| u.pid)
            .chain(self.orphans.iter().map(|o| o.pid))
            .collect();
        for pid in pids {
            proc::kill_tree(pid);
        }
        let _ = std::fs::remove_file(registry_path());
        for u in &self.units {
            if u.group.is_some() && u.status != Status::Idle {
                if let Some(stop) = &u.stop_cmd {
                    proc::detach(stop, Some(&u.path));
                }
            }
        }
    }

    /// Writes what runs now (plus orphans) to running.toml, or removes it when nothing runs.
    fn save_registry(&self) {
        let mut run: Vec<RunEntry> = self
            .units
            .iter()
            .filter_map(|u| {
                Some(RunEntry { key: u.key.clone(), pid: u.pid?, started: u.proc_started?, log: u.log_file.clone()? })
            })
            .collect();
        run.extend(self.orphans.iter().cloned());
        let path = registry_path();
        if run.is_empty() {
            let _ = std::fs::remove_file(path);
            return;
        }
        if let (Some(dir), Ok(text)) = (path.parent(), toml::to_string(&Registry { run })) {
            let _ = std::fs::create_dir_all(dir);
            let _ = std::fs::write(
                path,
                format!("# Servers devhub started that are still running. devhub keeps this up to date.\n{text}"),
            );
        }
    }

    /// Finds servers an earlier devhub left running and follows them again.
    fn reattach(&mut self) {
        let mut found = 0;
        for e in load_registry() {
            if !proc::is_alive(e.pid, e.started) {
                continue;
            }
            match self.unit_idx(&e.key) {
                Some(i) if self.units[i].pid.is_none() => {
                    let sinks = self.sinks_for(i);
                    let run = proc::attach(e.key.clone(), e.pid, e.started, e.log.clone(), sinks, self.tx.clone());
                    let age = Duration::from_secs(proc::now_secs().saturating_sub(e.started));
                    let u = &mut self.units[i];
                    u.pid = Some(e.pid);
                    u.run = run;
                    u.status = Status::Running;
                    u.started = Instant::now().checked_sub(age).or(Some(Instant::now()));
                    u.metrics = Metrics::reset_from(&u.log);
                    u.proc_started = Some(e.started);
                    u.log_file = Some(e.log);
                    found += 1;
                }
                _ => self.orphans.push(e),
            }
        }
        self.save_registry();
        match (found, self.orphans.len()) {
            (0, 0) => {}
            (n, 0) => self.flash(format!("Reattached {n} running server{}", if n == 1 { "" } else { "s" })),
            (n, o) => self.flash(format!("Reattached {n}; {o} more from an old config still run (S stops them)")),
        }
    }

    pub fn flash(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    pub fn live_count(&self) -> usize {
        self.units.iter().filter(|u| u.status.is_live()).count()
    }

    /// Handles every waiting message; true when there was any.
    fn drain(&mut self) -> bool {
        let mut any = false;
        while let Ok(msg) = self.rx.try_recv() {
            self.on_msg(msg);
            any = true;
        }
        any
    }

    fn tick(&mut self) {
        if let Ok(mut p) = self.stat_pids.lock() {
            *p = self.units.iter().filter_map(|u| u.pid).collect();
        }
        if let Ok(mut t) = self.health_targets.lock() {
            *t = self.units.iter().filter_map(|u| Some((u.pid?, u.port?))).collect();
        }
        // Branch and changed-file count for the selection (cached 15 s, read in the background).
        self.load_git(false);
        // While scrolled up, new lines must not move what you are reading.
        let id = self.selected_id();
        let len = self.current_log().and_then(|l| l.lock().ok().map(|b| b.lines.len())).unwrap_or(0);
        if self.log_seen.0 == id && self.scroll > 0 && len > self.log_seen.1 {
            self.scroll += len - self.log_seen.1;
        }
        if self.log_seen.0 != id || self.log_seen.1 != len {
            self.dirty = true;
        }
        self.log_seen = (id, len);
        if let Some((_, t)) = &self.toast {
            if t.elapsed() > Duration::from_secs(4) {
                self.toast = None;
                self.dirty = true;
            }
        }
        let pending: Vec<String> = self.restart_groups.iter().cloned().collect();
        for name in pending {
            match self.groups.iter().position(|g| g.id == name) {
                Some(gi) if !self.group_active(gi) => {
                    self.restart_groups.remove(&name);
                    self.start_group(gi);
                }
                Some(_) => {}
                None => {
                    self.restart_groups.remove(&name);
                }
            }
        }
    }

    fn unit_idx(&self, key: &str) -> Option<usize> {
        self.units.iter().position(|u| u.key == key)
    }

    fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Fetched { path, result } => {
                let Some(p) = self.picker.as_mut().filter(|p| p.path == path) else { return };
                p.fetching = false;
                match result {
                    Ok(items) => {
                        // Stay on the branch that was highlighted, wherever it moved to.
                        let keep = p.visible().get(p.sel).map(|&i| p.items[i].name.clone());
                        p.items = items;
                        let vis = p.visible();
                        p.sel = keep
                            .and_then(|k| vis.iter().position(|&i| p.items[i].name == k))
                            .unwrap_or(0)
                            .min(vis.len().saturating_sub(1));
                    }
                    Err(e) => p.fetch_error = Some(e),
                }
            }
            Msg::Switched { path, result } => match result {
                Ok(branch) => {
                    let project = self.picker.take().map(|p| p.project).unwrap_or_default();
                    self.mode = Mode::Normal;
                    for u in self.units.iter_mut().filter(|u| u.path == path) {
                        u.branch = Some(branch.clone());
                    }
                    self.git.remove(&path);
                    self.load_git(true);
                    self.flash(format!("✓ {project} is on {branch}"));
                }
                Err(e) => {
                    if let Some(p) = self.picker.as_mut() {
                        p.busy = false;
                        p.error = Some(e.clone());
                    }
                    self.flash(format!("✗ {e}"));
                }
            },
            Msg::Stats(list) => {
                self.stats = list.into_iter().map(|(pid, cpu, mem)| (pid, (cpu, mem))).collect();
                for u in self.units.iter_mut() {
                    if let Some((cpu, mem)) = u.pid.and_then(|p| self.stats.get(&p)) {
                        u.metrics.sample(*cpu, *mem, &u.log);
                    }
                }
            }
            Msg::Health(list) => {
                for (pid, up) in list {
                    if let Some(u) = self.units.iter_mut().find(|u| u.pid == Some(pid)) {
                        u.up = Some(up);
                    }
                }
            }
            Msg::Git { path, info } => {
                self.git_loading.remove(&path);
                for u in self.units.iter_mut().filter(|u| u.path == path) {
                    u.branch = Some(info.branch.clone());
                }
                self.git.insert(path, (info, Instant::now()));
            }
            Msg::Started { key, pid, run, started, log } => {
                if let Some(i) = self.unit_idx(&key) {
                    let u = &mut self.units[i];
                    u.pid = Some(pid);
                    u.run = run;
                    u.status = Status::Running;
                    u.stopping = false;
                    u.started = Some(Instant::now());
                    u.up = None;
                    u.metrics = Metrics::reset_from(&u.log);
                    u.proc_started = Some(started);
                    u.log_file = Some(log);
                    self.save_registry();
                }
            }
            Msg::Exited { key, run, code } => {
                let Some(i) = self.unit_idx(&key) else { return };
                let u = &mut self.units[i];
                if u.run != run {
                    return;
                }
                // Ended outside devhub (no exit code known): just stopped, not failed.
                let by_user = u.stopping || code == proc::UNKNOWN_EXIT;
                u.pid = None;
                u.stopping = false;
                u.port = u.cfg_port;
                u.started = None;
                u.up = None;
                u.proc_started = None;
                u.log_file = None;
                u.status = if by_user {
                    Status::Idle
                } else if code == 0 {
                    Status::Done
                } else {
                    Status::Failed(code)
                };
                let name = u.name.clone();
                self.save_registry();
                if self.restart_units.remove(&key) {
                    self.start_with_needs(i);
                } else if !by_user && code != 0 {
                    self.flash(format!("✗ {name} exited with code {code}"));
                }
            }
            Msg::Port { key, run, port } => {
                if let Some(i) = self.unit_idx(&key) {
                    let u = &mut self.units[i];
                    if u.run == run && u.cfg_port.is_none() {
                        u.port = Some(port);
                    }
                }
            }
            Msg::Branch { path, branch } => {
                for u in self.units.iter_mut().filter(|u| u.path == path) {
                    u.branch = Some(branch.clone());
                }
            }
            Msg::Ready { key, token, error } => {
                let Some(i) = self.unit_idx(&key) else { return };
                let u = &mut self.units[i];
                if !matches!(&u.waiting, Some((t, _)) if *t == token) {
                    return;
                }
                u.waiting = None;
                u.status = Status::Idle;
                match error {
                    None => self.start_unit(i),
                    Some(e) => {
                        u.status = Status::Failed(-1);
                        proc::log_line(&u.log, format!("── not started: {e}"));
                        let msg = format!("✗ {}: {e}", u.name);
                        self.flash(msg);
                    }
                }
            }
            Msg::GroupDone { group, token, error } => {
                if let Some(gi) = self.groups.iter().position(|g| g.id == group) {
                    if matches!(&self.groups[gi].runner, Some((t, _)) if *t == token) {
                        self.groups[gi].runner = None;
                        self.reset_pending(gi);
                        let g = &self.groups[gi];
                        match &error {
                            Some(e) => proc::log_line(&g.log, format!("── stopped: {e}")),
                            None => proc::log_line(&g.log, "── all steps started"),
                        }
                    }
                }
                match error {
                    Some(e) if e != "cancelled" => self.flash(format!("✗ {group}: {e}")),
                    Some(_) => {}
                    None => self.flash(format!("✓ {group} is up")),
                }
            }
        }
    }

    // ---------------------------------------------------------------- config

    pub fn reload(&mut self) {
        let sel = self.selected_id();
        match config::load(&self.cfg_path) {
            Ok((cfg, created)) => {
                self.error = None;
                if created {
                    self.flash("Created a starter config: press , to edit it");
                }
                self.apply(cfg);
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
        self.rebuild_rows(sel);
    }

    fn apply(&mut self, cfg: Config) {
        let (mut units, mut groups, warnings, cats) = build(&cfg);
        self.cats = cats;
        if let Some(w) = warnings.first() {
            let more = if warnings.len() > 1 { format!(" (+{} more)", warnings.len() - 1) } else { String::new() };
            self.flash(format!("✗ config: {w}{more}"));
        }

        // Keep what is running across a reload.
        let old_keys: Vec<String> = self.units.iter().map(|u| u.key.clone()).collect();
        let mut old: HashMap<String, Unit> =
            self.units.drain(..).map(|u| (u.key.clone(), u)).collect();
        for u in units.iter_mut() {
            if let Some(o) = old.remove(&u.key) {
                u.status = o.status;
                u.pid = o.pid;
                u.run = o.run;
                u.stopping = o.stopping;
                u.port = u.cfg_port.or(o.port);
                u.log = o.log;
                u.branch = o.branch;
                u.waiting = o.waiting;
                u.metrics = o.metrics;
                u.started = o.started;
                u.up = o.up;
            }
        }
        for (_, o) in old {
            if let Some(pid) = o.pid {
                std::thread::spawn(move || proc::kill_tree(pid));
            }
        }
        let mut old_groups: HashMap<String, Group> =
            self.groups.drain(..).map(|g| (g.id.clone(), g)).collect();
        for g in groups.iter_mut() {
            if let Some(o) = old_groups.remove(&g.id) {
                g.log = o.log;
                g.runner = o.runner;
                // Indices may have moved: map the old pending units by key.
                let keys: HashSet<String> = o.pending.iter().filter_map(|&i| old_keys.get(i).cloned()).collect();
                g.pending = (0..units.len()).filter(|&i| keys.contains(&units[i].key)).collect();
            }
        }

        self.units = units;
        self.groups = groups;
        self.editor = cfg.editor.clone().unwrap_or_else(|| "code".into());
        let clear = cfg.background.as_deref().is_some_and(|b| {
            matches!(b.trim().to_lowercase().as_str(), "none" | "transparent")
        });
        self.theme = crate::theme::neon(clear);

        let mut open: Vec<(char, String)> = vec![
            ('v', "code {path}".into()),
            ('z', "zed {path}".into()),
            ('a', "agy {path}".into()),
            ('e', explorer().into()),
        ];
        for (k, cmd) in &cfg.open {
            let Some(c) = k.chars().next() else { continue };
            open.retain(|(o, _)| *o != c);
            if !cmd.trim().is_empty() && !RESERVED.contains(c) {
                open.push((c, cmd.clone()));
            }
        }
        self.open = open;
        self.load_branches();
    }

    fn load_branches(&self) {
        let mut seen = HashSet::new();
        for u in &self.units {
            if seen.insert(u.path.clone()) {
                let (path, tx) = (u.path.clone(), self.tx.clone());
                std::thread::spawn(move || {
                    if let Some(branch) = proc::git_branch(&path) {
                        let _ = tx.send(Msg::Branch { path, branch });
                    }
                });
            }
        }
    }

    // ---------------------------------------------------------------- rows

    fn selected_id(&self) -> Option<String> {
        self.row_id(self.rows.get(self.selected)?)
    }

    fn row_id(&self, row: &Row) -> Option<String> {
        match row {
            Row::Group(gi) => Some(format!("g:{}", self.groups.get(*gi)?.id)),
            Row::Unit(ui) => Some(self.units.get(*ui)?.key.clone()),
            Row::Step(gi, ui) => Some(format!(
                "s:{}:{}",
                self.groups.get(*gi)?.id,
                self.units.get(*ui)?.key
            )),
            Row::Header(name) => Some(format!("h:{name}")),
        }
    }

    pub fn rebuild_rows(&mut self, keep: Option<String>) {
        let f = self.filter.to_lowercase();
        let hit = |s: &str| f.is_empty() || s.to_lowercase().contains(&f);
        let group_hit = |gi: usize| {
            let g = &self.groups[gi];
            hit(&g.name) || g.steps.iter().any(|s| hit(&self.units[s.unit].name))
        };
        // While filtering, every match shows, folded sections included.
        let folded = |name: &str| f.is_empty() && self.collapsed.contains(name);
        let push_group = |rows: &mut Vec<Row>, gi: usize| {
            rows.push(Row::Group(gi));
            for s in &self.groups[gi].steps {
                rows.push(Row::Step(gi, s.unit));
            }
        };
        let mut rows = Vec::new();

        // Groups in no section get one of their own, on top.
        let loose: Vec<usize> = (0..self.groups.len())
            .filter(|&gi| self.groups[gi].section.is_none() && group_hit(gi))
            .collect();
        if !loose.is_empty() {
            rows.push(Row::Header(LOOSE.into()));
            if !folded(LOOSE) {
                for gi in loose {
                    push_group(&mut rows, gi);
                }
            }
        }

        for cat in &self.cats {
            // (place, groups before projects at the same place, which)
            let mut items: Vec<(usize, u8, Row)> = Vec::new();
            for gi in 0..self.groups.len() {
                if self.groups[gi].section.as_deref() == Some(cat.as_str()) && group_hit(gi) {
                    items.push((self.groups[gi].pos, 0, Row::Group(gi)));
                }
            }
            for (ui, u) in self.units.iter().enumerate() {
                if u.category.as_deref() == Some(cat.as_str()) && hit(&u.name) {
                    items.push((u.pos, 1, Row::Unit(ui)));
                }
            }
            if items.is_empty() {
                continue;
            }
            items.sort_by_key(|(pos, kind, _)| (*pos, *kind));
            rows.push(Row::Header(cat.clone()));
            if folded(cat) {
                continue;
            }
            for (_, _, row) in items {
                match row {
                    Row::Group(gi) => push_group(&mut rows, gi),
                    other => rows.push(other),
                }
            }
        }
        self.rows = rows;

        let found = keep.and_then(|id| {
            self.rows
                .iter()
                .position(|r| self.row_id(r).as_deref() == Some(id.as_str()))
        });
        self.selected = match found {
            Some(i) => i,
            None => self
                .rows
                .iter()
                .position(|r| !matches!(r, Row::Header(_)))
                .unwrap_or(0),
        };
    }

    pub fn current(&self) -> Option<&Row> {
        self.rows
            .get(self.selected)
            .filter(|r| !matches!(r, Row::Header(_)))
    }

    fn move_by(&mut self, delta: isize) {
        let i = self.selected as isize + delta;
        if i >= 0 && (i as usize) < self.rows.len() {
            self.selected = i as usize;
            self.scroll = 0;
        }
    }

    fn move_to_edge(&mut self, end: bool) {
        if !self.rows.is_empty() {
            self.selected = if end { self.rows.len() - 1 } else { 0 };
            self.scroll = 0;
        }
    }

    /// The section the selection is in: its header's row index and name.
    pub fn section_of_selected(&self) -> Option<(usize, String)> {
        let last = self.rows.len().checked_sub(1)?;
        (0..=self.selected.min(last))
            .rev()
            .find_map(|i| match &self.rows[i] {
                Row::Header(name) => Some((i, name.clone())),
                _ => None,
            })
    }

    /// Folds (`Some(true)`), unfolds (`Some(false)`) or flips the selected section,
    /// leaving the selection on its header.
    fn fold(&mut self, shut: Option<bool>) {
        let Some((_, name)) = self.section_of_selected() else { return };
        let is_shut = self.collapsed.contains(&name);
        let want = shut.unwrap_or(!is_shut);
        if want == is_shut {
            return;
        }
        if want {
            self.collapsed.insert(name.clone());
        } else {
            self.collapsed.remove(&name);
        }
        self.rebuild_rows(Some(format!("h:{name}")));
        self.save_state();
    }

    fn state_path(&self) -> PathBuf {
        self.cfg_path.with_file_name("state.toml")
    }

    fn load_state(&mut self) {
        #[derive(serde::Deserialize, Default)]
        struct State {
            #[serde(default)]
            collapsed: Vec<String>,
        }
        let state: State = std::fs::read_to_string(self.state_path())
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default();
        self.collapsed = state.collapsed.into_iter().collect();
    }

    fn save_state(&self) {
        let mut list: Vec<&String> = self.collapsed.iter().collect();
        list.sort();
        let body = list.iter().map(|s| format!("{s:?}")).collect::<Vec<_>>().join(", ");
        let _ = std::fs::write(
            self.state_path(),
            format!("# devhub remembers this itself; safe to delete.\ncollapsed = [{body}]\n"),
        );
    }

    // ---------------------------------------------------------------- run / stop

    pub fn group_active(&self, gi: usize) -> bool {
        let g = &self.groups[gi];
        g.runner.is_some() || g.steps.iter().any(|s| self.units[s.unit].status.is_live())
    }

    pub fn group_status(&self, gi: usize) -> Status {
        let g = &self.groups[gi];
        let steps = g.steps.iter().map(|s| self.units[s.unit].status);
        if g.runner.is_some() {
            Status::Starting
        } else if steps.clone().any(|s| s == Status::Stopping) {
            Status::Stopping
        } else if steps.clone().any(|s| s == Status::Running) {
            Status::Running
        } else if let Some(Status::Failed(c)) = steps.clone().find(|s| matches!(s, Status::Failed(_))) {
            Status::Failed(c)
        } else {
            Status::Idle
        }
    }

    /// A unit's own log, plus the log of every group that lists it.
    fn sinks_for(&self, ui: usize) -> Vec<Sink> {
        let u = &self.units[ui];
        let mut sinks = vec![Sink { log: u.log.clone(), prefix: None }];
        for g in &self.groups {
            if g.steps.iter().any(|s| s.unit == ui) {
                sinks.push(Sink { log: g.log.clone(), prefix: Some(format!("[{}] ", u.name)) });
            }
        }
        sinks
    }

    /// Everything `ui` needs, deepest first, each once, `ui` itself left out.
    pub fn collect_needs(&self, ui: usize) -> Vec<usize> {
        fn visit(app: &App, ui: usize, out: &mut Vec<usize>, seen: &mut HashSet<usize>) {
            for &n in &app.units[ui].needs {
                if seen.insert(n) {
                    visit(app, n, out, seen);
                    out.push(n);
                }
            }
        }
        let mut out = Vec::new();
        let mut seen = HashSet::from([ui]);
        visit(self, ui, &mut out, &mut seen);
        out
    }

    /// Starts what `ui` needs (skipping what already runs), waits for their ports, then `ui`.
    pub fn start_with_needs(&mut self, ui: usize) {
        if self.units[ui].status.is_live() {
            return;
        }
        let needs = self.collect_needs(ui);
        if needs.is_empty() {
            return self.start_unit(ui);
        }
        let mut waits = Vec::new();
        for n in needs {
            if !self.units[n].status.is_live() {
                self.start_unit(n);
            }
            if let Some(p) = self.units[n].cfg_port {
                waits.push((self.units[n].name.clone(), p));
            }
        }
        if waits.is_empty() {
            return self.start_unit(ui);
        }

        let token = self.next_token;
        self.next_token += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        let u = &mut self.units[ui];
        u.status = Status::Starting;
        u.waiting = Some((token, cancel.clone()));
        let list: Vec<String> = waits.iter().map(|(n, p)| format!("{n} :{p}")).collect();
        proc::log_line(&u.log, format!("── waiting for {}", list.join(", ")));

        let (key, tx) = (u.key.clone(), self.tx.clone());
        std::thread::spawn(move || {
            let mut error = None;
            for (name, port) in waits {
                if !proc::wait_port(port, &cancel, Duration::from_secs(180)) {
                    error = Some(if cancel.load(Ordering::Relaxed) {
                        "cancelled".into()
                    } else {
                        format!("{name} never opened port {port}")
                    });
                    break;
                }
            }
            let _ = tx.send(Msg::Ready { key, token, error });
        });
    }

    /// Units a group runner meant to start but never did go back to idle.
    fn reset_pending(&mut self, gi: usize) {
        for ui in std::mem::take(&mut self.groups[gi].pending) {
            let u = &mut self.units[ui];
            if u.status == Status::Starting && u.pid.is_none() && u.waiting.is_none() {
                u.status = Status::Idle;
            }
        }
    }

    pub fn start_unit(&mut self, ui: usize) {
        if self.units[ui].status.is_live() {
            return;
        }
        if !self.units[ui].path.is_dir() {
            let msg = format!("Folder not found: {}", self.units[ui].path.display());
            self.flash(msg);
            return;
        }
        let sinks = self.sinks_for(ui);
        let u = &mut self.units[ui];
        u.status = Status::Starting;
        if let Err(e) = proc::spawn(u.key.clone(), &u.name, &u.path, &u.cmd, sinks, self.tx.clone(), None) {
            u.status = Status::Failed(-1);
            proc::log_line(&u.log, format!("── could not start: {e}"));
            let msg = format!("✗ {}: {e}", u.name);
            self.flash(msg);
        }
    }

    pub fn stop_unit(&mut self, ui: usize) {
        let u = &mut self.units[ui];
        if let Some((_, c)) = u.waiting.take() {
            c.store(true, Ordering::Relaxed);
            u.status = Status::Idle;
            proc::log_line(&u.log, "── cancelled");
            return;
        }
        if let Some(pid) = u.pid {
            if u.status.is_live() {
                u.status = Status::Stopping;
                u.stopping = true;
                std::thread::spawn(move || proc::kill_tree(pid));
            }
        }
    }

    pub fn start_group(&mut self, gi: usize) {
        if self.groups[gi].runner.is_some() {
            return;
        }
        let token = self.next_token;
        self.next_token += 1;
        let cancel = Arc::new(AtomicBool::new(false));

        struct Plan {
            key: String,
            name: String,
            path: PathBuf,
            cmd: String,
            sinks: Vec<Sink>,
            wait_exit: bool,
            wait_port: Option<u16>,
            live: bool,
        }
        // Each step's needs come first; a unit is listed once however many steps need it.
        let mut order: Vec<(usize, bool, Option<u16>)> = Vec::new();
        for s in &self.groups[gi].steps {
            for n in self.collect_needs(s.unit) {
                if !order.iter().any(|(u, ..)| *u == n) {
                    order.push((n, false, self.units[n].cfg_port));
                }
            }
            if let Some(o) = order.iter_mut().find(|(u, ..)| *u == s.unit) {
                o.1 |= s.wait_exit;
                o.2 = s.wait_port.or(o.2);
            } else {
                order.push((s.unit, s.wait_exit, s.wait_port));
            }
        }

        let mut plan = Vec::new();
        let mut pending = Vec::new();
        for (ui, wait_exit, wait_port) in order {
            let u = &self.units[ui];
            let live = u.status.is_live();
            plan.push(Plan {
                key: u.key.clone(),
                name: u.name.clone(),
                path: u.path.clone(),
                cmd: u.cmd.clone(),
                sinks: self.sinks_for(ui),
                wait_exit,
                wait_port,
                live,
            });
            if !live {
                pending.push(ui);
            }
        }
        // Marked as starting now, so nothing else starts them a second time meanwhile.
        for &ui in &pending {
            self.units[ui].status = Status::Starting;
        }

        let g = &mut self.groups[gi];
        g.runner = Some((token, cancel.clone()));
        g.pending = pending;
        proc::log_line(&g.log, format!("── starting {}", g.name));
        let (group, tx) = (g.id.clone(), self.tx.clone());

        std::thread::spawn(move || {
            let mut error = None;
            for p in plan {
                if cancel.load(Ordering::Relaxed) {
                    error = Some("cancelled".into());
                    break;
                }
                if !p.live {
                    if !p.path.is_dir() {
                        error = Some(format!("{}: folder not found", p.name));
                        break;
                    }
                    let (dtx, drx) = mpsc::channel();
                    let done = p.wait_exit.then_some(dtx);
                    if let Err(e) = proc::spawn(p.key, &p.name, &p.path, &p.cmd, p.sinks, tx.clone(), done) {
                        error = Some(format!("{}: {e}", p.name));
                        break;
                    }
                    if p.wait_exit {
                        match drx.recv() {
                            Ok(0) => {}
                            Ok(code) => {
                                error = Some(format!("{} exited with code {code}", p.name));
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                }
                if let Some(port) = p.wait_port {
                    if !proc::wait_port(port, &cancel, Duration::from_secs(180)) {
                        error = Some(if cancel.load(Ordering::Relaxed) {
                            "cancelled".into()
                        } else {
                            format!("{}: port {port} never opened", p.name)
                        });
                        break;
                    }
                }
            }
            let _ = tx.send(Msg::GroupDone { group, token, error });
        });
    }

    pub fn stop_group(&mut self, gi: usize) {
        if let Some((_, c)) = self.groups[gi].runner.take() {
            c.store(true, Ordering::Relaxed);
        }
        self.reset_pending(gi);
        let steps: Vec<usize> = self.groups[gi].steps.iter().map(|s| s.unit).collect();
        for ui in steps {
            self.stop_unit(ui);
            let u = &self.units[ui];
            if let Some(stop) = u.stop_cmd.clone() {
                let key = format!("stop:{}", u.key);
                let sinks = self.sinks_for(ui);
                let _ = proc::spawn(key, &format!("{}-stop", u.name), &u.path, &stop, sinks, self.tx.clone(), None);
            }
        }
        let name = self.groups[gi].name.clone();
        proc::log_line(&self.groups[gi].log, format!("── stopping {name}"));
    }

    fn toggle(&mut self) {
        match self.current().cloned() {
            Some(Row::Group(gi)) => {
                if self.group_active(gi) {
                    self.stop_group(gi)
                } else {
                    self.start_group(gi)
                }
            }
            Some(row) => {
                if let Some(ui) = row.unit() {
                    if self.units[ui].status.is_live() {
                        self.stop_unit(ui)
                    } else {
                        self.start_with_needs(ui)
                    }
                }
            }
            None => {}
        }
        self.scroll = 0;
    }

    fn restart(&mut self) {
        match self.current().cloned() {
            Some(Row::Group(gi)) => {
                if self.group_active(gi) {
                    self.restart_groups.insert(self.groups[gi].id.clone());
                    self.stop_group(gi);
                } else {
                    self.start_group(gi);
                }
            }
            Some(row) => {
                if let Some(ui) = row.unit() {
                    let u = &self.units[ui];
                    if u.pid.is_some() && u.status.is_live() {
                        self.restart_units.insert(u.key.clone());
                        self.stop_unit(ui);
                    } else if !u.status.is_live() {
                        self.start_with_needs(ui);
                    }
                }
            }
            None => {}
        }
        self.scroll = 0;
    }

    fn stop_selected(&mut self) {
        match self.current().cloned() {
            Some(Row::Group(gi)) => self.stop_group(gi),
            Some(row) => {
                if let Some(ui) = row.unit() {
                    self.stop_unit(ui)
                }
            }
            None => {}
        }
    }

    fn stop_all(&mut self) {
        for gi in 0..self.groups.len() {
            if self.group_active(gi) {
                self.stop_group(gi);
            }
        }
        for ui in 0..self.units.len() {
            self.stop_unit(ui);
        }
        for o in std::mem::take(&mut self.orphans) {
            std::thread::spawn(move || proc::kill_tree(o.pid));
        }
        self.save_registry();
        self.flash("Stopping everything");
    }

    // ---------------------------------------------------------------- open

    /// The folder and port the selected row stands for.
    fn target(&self) -> Option<(PathBuf, Option<u16>)> {
        match self.current()? {
            Row::Unit(ui) | Row::Step(_, ui) => {
                let u = &self.units[*ui];
                Some((u.path.clone(), u.port))
            }
            Row::Group(gi) => {
                let steps = &self.groups[*gi].steps;
                let path = self.units[steps.first()?.unit].path.clone();
                let port = steps.iter().rev().find_map(|s| self.units[s.unit].port);
                Some((path, port))
            }
            Row::Header(_) => None,
        }
    }

    fn open_browser(&mut self) {
        match self.target() {
            Some((_, Some(port))) => proc::open_url(&format!("http://localhost:{port}")),
            Some(_) => self.flash("No port yet: start it, or set `port` in the config"),
            None => {}
        }
    }

    fn open_with(&mut self, template: &str) {
        let Some((path, _)) = self.target() else { return };
        let quoted = format!("\"{}\"", path.display());
        let cmd = if template.contains("{path}") {
            template.replace("{path}", &quoted)
        } else {
            format!("{template} {quoted}")
        };
        proc::detach(&cmd, Some(&path));
    }

    fn edit_config(&mut self) {
        let cmd = format!("{} \"{}\"", self.editor, self.cfg_path.display());
        proc::detach(&cmd, None);
        self.flash("Opened the config: press R here after saving");
    }

    /// Reads git info for the selected project in the background (cached for 15 s).
    fn load_git(&mut self, force: bool) {
        let Some((path, _)) = self.target() else { return };
        let fresh = self.git.get(&path).is_some_and(|(_, t)| t.elapsed() < Duration::from_secs(15));
        if (fresh && !force) || self.git_loading.contains(&path) {
            return;
        }
        self.git_loading.insert(path.clone());
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let info = proc::git_info(&path).unwrap_or_default();
            let _ = tx.send(Msg::Git { path, info });
        });
    }

    /// The log the right-hand panel shows for the selected row.
    pub fn current_log(&self) -> Option<Log> {
        match self.current()? {
            Row::Group(gi) => Some(self.groups[*gi].log.clone()),
            row => row.unit().map(|ui| self.units[ui].log.clone()),
        }
    }

    /// Moves the find-in-log hit: to the newest match, or the next older / newer one,
    /// and scrolls so it sits mid-panel.
    fn search_jump(&mut self, older: bool, from_newest: bool) {
        let q = self.search.to_lowercase();
        let Some(log) = self.current_log() else { return };
        if q.is_empty() {
            self.search_hit = None;
            return;
        }
        let found = {
            let Ok(buf) = log.lock() else { return };
            let n = buf.lines.len();
            let hit = |i: &usize| buf.lines[*i].to_lowercase().contains(&q);
            let start = if from_newest { n } else { self.search_hit.unwrap_or(n).min(n) };
            let found = if older || from_newest {
                (0..start).rev().find(hit)
            } else {
                (start + 1..n).find(hit)
            };
            found.map(|i| (i, n))
        };
        match found {
            Some((i, n)) => {
                self.search_hit = Some(i);
                self.scroll = n.saturating_sub(i + 1 + self.log_rows / 2);
                self.log_seen.1 = n;
            }
            None if from_newest => {
                self.search_hit = None;
            }
            None => self.flash(if older { "No older match" } else { "No newer match" }),
        }
    }

    /// Writes the selected log to a temp file and opens it in the editor.
    fn open_log(&mut self) {
        let Some(log) = self.current_log() else { return };
        let name = match self.current() {
            Some(Row::Group(gi)) => self.groups[*gi].name.clone(),
            Some(row) => row.unit().map(|ui| self.units[ui].name.clone()).unwrap_or_default(),
            None => return,
        };
        let text = match log.lock() {
            Ok(buf) => buf.lines.iter().cloned().collect::<Vec<_>>().join("\n"),
            Err(_) => return,
        };
        let dir = std::env::temp_dir().join("devhub");
        let safe: String = name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
        let file = dir.join(format!("{safe}.log"));
        if std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&file, text)).is_err() {
            self.flash("✗ Could not write the log file");
            return;
        }
        proc::detach(&format!("{} \"{}\"", self.editor, file.display()), None);
    }

    /// Opens the branch picker for the selected project (not for a whole group).
    fn open_picker(&mut self) {
        let Some(ui) = self.current().and_then(|r| r.unit()) else {
            self.flash("Select a project to switch its branch");
            return;
        };
        let (path, project) = (self.units[ui].path.clone(), self.units[ui].name.clone());
        match proc::branch_list(&path) {
            Ok(items) if items.is_empty() => self.flash("No branches here"),
            Ok(items) => {
                let changed = proc::changed_count(&path);
                let sel = items.iter().position(|b| !b.current).unwrap_or(0);
                let tx = self.tx.clone();
                let fetch_path = path.clone();
                std::thread::spawn(move || {
                    let result = proc::fetch(&fetch_path, Duration::from_secs(30))
                        .and_then(|_| proc::branch_list(&fetch_path));
                    let _ = tx.send(Msg::Fetched { path: fetch_path, result });
                });
                self.picker = Some(Picker {
                    path,
                    project,
                    items,
                    filter: String::new(),
                    sel,
                    changed,
                    busy: false,
                    error: None,
                    fetching: true,
                    fetch_error: None,
                });
                self.mode = Mode::Branches;
                self.load_git(true);
            }
            Err(e) => self.flash(format!("✗ {}", if e.is_empty() { "not a git repository".into() } else { e })),
        }
    }

    fn on_picker_key(&mut self, k: KeyEvent) {
        let Some(p) = self.picker.as_mut() else {
            self.mode = Mode::Normal;
            return;
        };
        if p.busy {
            return;
        }
        let visible = p.visible();
        match k.code {
            KeyCode::Esc => {
                self.picker = None;
                self.mode = Mode::Normal;
            }
            KeyCode::Up => p.sel = p.sel.saturating_sub(1),
            KeyCode::Down => p.sel = (p.sel + 1).min(visible.len().saturating_sub(1)),
            KeyCode::Backspace => {
                p.filter.pop();
                p.sel = 0;
            }
            KeyCode::Char(c) => {
                p.filter.push(c);
                p.sel = 0;
            }
            KeyCode::Enter => {
                let Some(&i) = visible.get(p.sel) else { return };
                let branch = p.items[i].clone();
                if branch.current {
                    self.picker = None;
                    self.mode = Mode::Normal;
                    return;
                }
                p.busy = true;
                p.error = None;
                let (path, tx) = (p.path.clone(), self.tx.clone());
                std::thread::spawn(move || {
                    let result = proc::switch_branch(&path, &branch);
                    let _ = tx.send(Msg::Switched { path, result });
                });
            }
            _ => {}
        }
    }

    fn clear_log(&mut self) {
        let log = match self.current() {
            Some(Row::Unit(ui) | Row::Step(_, ui)) => self.units[*ui].log.clone(),
            Some(Row::Group(gi)) => self.groups[*gi].log.clone(),
            _ => return,
        };
        if let Ok(mut l) = log.lock() {
            l.lines.clear();
        }
        self.scroll = 0;
    }

    // ---------------------------------------------------------------- keys

    fn on_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && k.code == KeyCode::Char('c') {
            self.ask_quit();
            return;
        }
        let before = self.selected_id();
        match self.mode {
            Mode::Help => self.mode = Mode::Normal,
            Mode::ConfirmQuit => match k.code {
                // s stops everything, l leaves it running; any other key cancels.
                KeyCode::Char('s') | KeyCode::Char('S') => self.quit = true,
                KeyCode::Char('l') | KeyCode::Char('L') => {
                    self.leave_running = true;
                    self.quit = true;
                }
                _ => self.mode = Mode::Normal,
            },
            Mode::Filter => self.on_filter_key(k),
            Mode::Search => self.on_search_key(k),
            Mode::Branches => self.on_picker_key(k),
            Mode::Normal if ctrl && k.code == KeyCode::Char('f') => {
                self.tab = Tab::Log;
                self.mode = Mode::Search;
            }
            Mode::Normal => self.on_normal_key(k),
        }
        if self.selected_id() != before {
            // Another project: its own log, so the old search hit means nothing there.
            self.search_hit = None;
            if !self.search.is_empty() {
                self.search_jump(true, true);
            }
        }
    }

    fn on_search_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Esc => {
                self.search.clear();
                self.search_hit = None;
                self.scroll = 0;
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => self.mode = Mode::Normal,
            KeyCode::Backspace => {
                self.search.pop();
                self.search_jump(true, true);
            }
            KeyCode::Up => self.search_jump(true, false),
            KeyCode::Down => self.search_jump(false, false),
            KeyCode::Char(c) => {
                self.search.push(c);
                self.search_jump(true, true);
            }
            _ => {}
        }
    }

    fn ask_quit(&mut self) {
        if self.live_count() + self.orphans.len() == 0 {
            self.quit = true;
        } else {
            self.mode = Mode::ConfirmQuit;
        }
    }

    fn on_filter_key(&mut self, k: KeyEvent) {
        let keep = self.selected_id();
        match k.code {
            KeyCode::Esc => {
                self.filter.clear();
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => self.mode = Mode::Normal,
            KeyCode::Backspace => {
                self.filter.pop();
            }
            KeyCode::Up => return self.move_by(-1),
            KeyCode::Down => return self.move_by(1),
            KeyCode::Char(c) => self.filter.push(c),
            _ => return,
        }
        self.rebuild_rows(keep);
    }

    fn on_normal_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Char('q') => self.ask_quit(),
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::Home | KeyCode::Char('g') => self.move_to_edge(false),
            KeyCode::End | KeyCode::Char('G') => self.move_to_edge(true),
            KeyCode::Enter | KeyCode::Char(' ') if matches!(self.rows.get(self.selected), Some(Row::Header(_))) => self.fold(None),
            KeyCode::Enter | KeyCode::Char(' ') => self.toggle(),
            KeyCode::Left | KeyCode::Char('h') => self.fold(Some(true)),
            KeyCode::Right | KeyCode::Char('l') => self.fold(Some(false)),
            KeyCode::Char('r') => self.restart(),
            KeyCode::Char('s') => self.stop_selected(),
            KeyCode::Char('S') => self.stop_all(),
            KeyCode::Char('o') => self.open_browser(),
            KeyCode::Char('c') => self.clear_log(),
            KeyCode::Char('f') => {
                self.scroll = 0;
                self.search_hit = None;
            }
            KeyCode::Tab => {
                self.tab = if self.tab == Tab::Log { Tab::Git } else { Tab::Log };
                if self.tab == Tab::Git {
                    self.load_git(true);
                }
            }
            KeyCode::Char('n') if !self.search.is_empty() => self.search_jump(true, false),
            KeyCode::Char('N') if !self.search.is_empty() => self.search_jump(false, false),
            KeyCode::Char('L') => self.open_log(),
            KeyCode::Char('b') => self.open_picker(),
            KeyCode::Esc if !self.search.is_empty() => {
                self.search.clear();
                self.search_hit = None;
                self.scroll = 0;
            }
            KeyCode::PageUp => self.scroll += 10,
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::Char('/') => self.mode = Mode::Filter,
            KeyCode::Esc if !self.filter.is_empty() => {
                let keep = self.selected_id();
                self.filter.clear();
                self.rebuild_rows(keep);
            }
            KeyCode::Char('R') => {
                self.git.clear();
                self.reload();
                if self.error.is_none() {
                    self.flash("Config reloaded");
                }
            }
            KeyCode::Char(',') => self.edit_config(),
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char(c) => {
                if let Some((_, tpl)) = self.open.iter().find(|(o, _)| *o == c).cloned() {
                    self.open_with(&tpl);
                }
            }
            _ => {}
        }
    }
}

/// Keys the app uses itself; `open` entries may not take them.
const RESERVED: &str = "qkjgGrsSocf/R,? nNLhlb";

/// The section holding groups that name no section.
pub const LOOSE: &str = "groups";

fn explorer() -> &'static str {
    if cfg!(windows) {
        "explorer {path}"
    } else if cfg!(target_os = "macos") {
        "open {path}"
    } else {
        "xdg-open {path}"
    }
}

fn norm(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let s = s.trim_end_matches('/');
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s.to_string()
    }
}

fn base_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| p.display().to_string())
}

fn new_unit(key: String, name: String, path: PathBuf, cmd: String, category: Option<String>) -> Unit {
    Unit {
        key,
        name,
        path,
        cmd,
        category,
        group: None,
        cfg_port: None,
        stop_cmd: None,
        status: Status::Idle,
        pid: None,
        run: 0,
        stopping: false,
        port: None,
        branch: None,
        log: proc::new_log(),
        needs: Vec::new(),
        needs_names: Vec::new(),
        waiting: None,
        pos: usize::MAX,
        metrics: Metrics::default(),
        started: None,
        up: None,
        proc_started: None,
        log_file: None,
    }
}

/// Turns the config into the project list: discovered folders, hand-added repos, groups.
/// Also returns problems worth telling the person (an unknown name, a step missing a field).
fn build(cfg: &Config) -> (Vec<Unit>, Vec<Group>, Vec<String>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut units: Vec<Unit> = Vec::new();
    let mut cats: Vec<String> = Vec::new();
    let mut hidden: HashSet<String> = HashSet::new();
    let add_cat = |cats: &mut Vec<String>, c: &str| {
        if !cats.iter().any(|x| x == c) {
            cats.push(c.to_string());
        }
    };

    for r in &cfg.repos {
        if r.hidden {
            hidden.insert(norm(&r.path));
        }
    }

    for root in &cfg.roots {
        add_cat(&mut cats, &root.category);
        let Ok(entries) = std::fs::read_dir(&root.path) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if !path.is_dir()
                || name.starts_with('.')
                || root.exclude.iter().any(|x| x.eq_ignore_ascii_case(&name))
                || !path.join(&root.marker).exists()
                || hidden.contains(&norm(&path))
            {
                continue;
            }
            let key = format!("{}|{}", norm(&path), root.cmd);
            units.push(new_unit(key, name, path, root.cmd.clone(), Some(root.category.clone())));
        }
    }

    // A repo entry overrides a project a root discovered; otherwise it adds a new one
    // (so one folder may be listed twice, e.g. with two different commands).
    let discovered = units.len();
    for r in cfg.repos.iter().filter(|r| !r.hidden) {
        let n = norm(&r.path);
        if let Some(u) = units[..discovered].iter_mut().find(|u| norm(&u.path) == n) {
            if let Some(name) = &r.name {
                u.name = name.clone();
            }
            if let Some(cmd) = &r.cmd {
                u.cmd = cmd.clone();
                u.key = format!("{n}|{cmd}");
            }
            if let Some(c) = &r.category {
                add_cat(&mut cats, c);
                u.category = Some(c.clone());
            }
            u.cfg_port = r.port;
            u.port = r.port;
            u.needs_names = r.needs.clone();
            if !r.listed {
                u.category = None;
            }
        } else {
            let cmd = r.cmd.clone().unwrap_or_else(|| "npm run dev".into());
            let cat = r.category.clone().unwrap_or_else(|| "Other".into());
            add_cat(&mut cats, &cat);
            let name = r.name.clone().unwrap_or_else(|| base_name(&r.path));
            let mut u = new_unit(format!("{n}|{cmd}"), name, r.path.clone(), cmd, Some(cat));
            u.cfg_port = r.port;
            u.port = r.port;
            u.needs_names = r.needs.clone();
            if !r.listed {
                u.category = None;
            }
            units.push(u);
        }
    }

    // Product sections: move the named projects in, in the order given. Sections come
    // first; categories that still hold projects follow.
    let group_names: HashSet<String> = cfg.groups.iter().map(|g| g.name.to_lowercase()).collect();
    let mut group_place: HashMap<String, (String, usize)> = HashMap::new();
    let mut ordered: Vec<String> = Vec::new();
    for s in &cfg.sections {
        add_cat(&mut ordered, &s.name);
        for (i, item) in s.items.iter().enumerate() {
            let item = item.trim();
            if let Some(u) = units.iter_mut().find(|u| u.name.eq_ignore_ascii_case(item)) {
                u.category = Some(s.name.clone());
                u.pos = i + 1;
            } else if group_names.contains(&item.to_lowercase()) {
                group_place.insert(item.to_lowercase(), (s.name.clone(), i + 1));
            } else {
                warnings.push(format!("section {}: no project or group named {item}", s.name));
            }
        }
    }
    for c in &cats {
        add_cat(&mut ordered, c);
    }
    for g in &cfg.groups {
        if let Some(s) = &g.section {
            add_cat(&mut ordered, s);
        }
    }
    let cats = ordered;

    units.sort_by_key(|u| {
        let ci = u
            .category
            .as_ref()
            .and_then(|c| cats.iter().position(|x| x == c))
            .unwrap_or(usize::MAX);
        (ci, u.pos, u.name.to_lowercase())
    });

    // Projects in the list, by lowercase name (shared steps and `needs` point at these).
    let listed = units.len();
    let find = |units: &[Unit], name: &str| {
        units[..listed].iter().position(|u| u.name.eq_ignore_ascii_case(name.trim()))
    };

    let mut groups = Vec::new();
    for g in &cfg.groups {
        // Placed by `section`, or by being named in a section's items (which also sets its spot).
        let (section, pos) = match (group_place.get(&g.name.to_lowercase()), &g.section) {
            (Some((s, p)), None) => (Some(s.clone()), *p),
            (Some((s, p)), Some(own)) if s == own => (Some(s.clone()), *p),
            (_, own) => (own.clone(), 0),
        };
        let id = match &section {
            Some(s) => format!("{s}/{}", g.name),
            None => g.name.clone(),
        };
        if groups.iter().any(|x: &Group| x.id == id) {
            warnings.push(format!("two groups are called {id}"));
            continue;
        }
        let mut steps = Vec::new();
        for s in &g.steps {
            if let Some(repo) = &s.repo {
                match find(&units, repo) {
                    Some(ui) => steps.push(Step {
                        unit: ui,
                        wait_exit: s.wait_exit,
                        wait_port: s.wait_port.or(units[ui].cfg_port),
                    }),
                    None => warnings.push(format!("group {}: no project named {repo}", g.name)),
                }
                continue;
            }
            let (Some(path), Some(cmd)) = (&s.path, &s.cmd) else {
                warnings.push(format!("group {}: a step needs `repo`, or `path` and `cmd`", g.name));
                continue;
            };
            let name = s.name.clone().unwrap_or_else(|| base_name(path));
            let key = format!("{id}|{}|{}", norm(path), cmd);
            let mut u = new_unit(key, name, path.clone(), cmd.clone(), None);
            u.group = Some(groups.len());
            u.cfg_port = s.port;
            u.port = s.port;
            u.stop_cmd = s.stop.clone();
            steps.push(Step { unit: units.len(), wait_exit: s.wait_exit, wait_port: s.wait_port });
            units.push(u);
        }
        groups.push(Group {
            id,
            name: g.name.clone(),
            section,
            pos,
            steps,
            log: proc::new_log(),
            runner: None,
            pending: Vec::new(),
        });
    }

    for i in 0..units.len() {
        let names = std::mem::take(&mut units[i].needs_names);
        let mut needs = Vec::new();
        for n in names {
            match find(&units, &n) {
                Some(j) if j != i => needs.push(j),
                Some(_) => {}
                None => warnings.push(format!("{}: needs unknown project {n}", units[i].name)),
            }
        }
        units[i].needs = needs;
    }
    (units, groups, warnings, cats)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests that start servers share one running.toml: they take turns.
    fn registry_turn() -> std::sync::MutexGuard<'static, ()> {
        static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
        TURN.lock().unwrap_or_else(|e| e.into_inner())
    }

    const LONG: &str = if cfg!(windows) { "ping -n 30 127.0.0.1" } else { "sleep 30" };

    fn app_with(cfg: &str) -> App {
        let dir = std::env::temp_dir().join(format!("devhub-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("cfg-{}.toml", rand_suffix()));
        std::fs::write(&file, cfg.replace("{dir}", &dir.display().to_string())).unwrap();
        App::new(file)
    }

    fn rand_suffix() -> u128 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    }

    fn idx(app: &App, name: &str) -> usize {
        app.units.iter().position(|u| u.name == name && u.category.is_some()).unwrap()
    }

    /// Processes messages until `done` holds or 10 s pass.
    fn settle(app: &mut App, done: impl Fn(&App) -> bool) {
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end {
            app.drain();
            app.tick();
            if done(app) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("timed out");
    }

    fn config(api_port: &str) -> String {
        format!(
            r#"
[[repos]]
name = "api"
path = '{{dir}}'
cmd = "{LONG}"
category = "Backend"
{api_port}

[[repos]]
name = "web1"
path = '{{dir}}'
cmd = "{LONG} "
category = "Frontend"
needs = ["api"]

[[repos]]
name = "web2"
path = '{{dir}}'
cmd = "{LONG}  "
category = "Frontend"
needs = ["api"]

[[groups]]
name = "all"
steps = [{{ repo = "web1" }}, {{ repo = "web2" }}]
"#
        )
    }

    #[test]
    fn shared_backend_starts_once() {
        let _turn = registry_turn();
        let mut app = app_with(&config(""));
        assert!(app.error.is_none(), "{:?}", app.error);
        let (api, w1, w2) = (idx(&app, "api"), idx(&app, "web1"), idx(&app, "web2"));

        app.start_with_needs(w1);
        settle(&mut app, |a| a.units[api].status == Status::Running && a.units[w1].status == Status::Running);
        let api_run = app.units[api].run;

        app.start_with_needs(w2);
        settle(&mut app, |a| a.units[w2].status == Status::Running);
        assert_eq!(app.units[api].run, api_run, "api was started a second time");

        // Stopping a frontend leaves the shared backend running.
        app.stop_unit(w1);
        settle(&mut app, |a| a.units[w1].status == Status::Idle);
        assert_eq!(app.units[api].status, Status::Running);

        // The group reuses what runs: only web1 starts again.
        let w2_run = app.units[w2].run;
        let g = app.groups.iter().position(|g| g.name == "all").unwrap();
        app.start_group(g);
        settle(&mut app, |a| a.groups[g].runner.is_none() && a.units[w1].status == Status::Running);
        assert_eq!(app.units[api].run, api_run);
        assert_eq!(app.units[w2].run, w2_run);

        app.shutdown();
    }

    #[test]
    fn waits_for_a_needed_port() {
        let _turn = registry_turn();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut app = app_with(&config(&format!("port = {port}")));
        let (api, w1) = (idx(&app, "api"), idx(&app, "web1"));

        app.start_with_needs(w1);
        assert!(app.units[w1].waiting.is_some(), "should wait for the api port");
        settle(&mut app, |a| a.units[w1].status == Status::Running);
        assert_eq!(app.units[api].status, Status::Running);
        app.shutdown();
    }

    #[test]
    fn unlisted_projects_stay_usable() {
        let app = app_with(
            r#"
[[repos]]
name = "api"
path = '{dir}'
listed = false

[[repos]]
name = "web"
path = '{dir}'
cmd = "x"
needs = ["api"]

[[groups]]
name = "g"
steps = [{ repo = "api" }]
"#,
        );
        assert!(app.toast.is_none(), "{:?}", app.toast);
        let api = app.units.iter().position(|u| u.name == "api").unwrap();
        assert!(!app.rows.contains(&Row::Unit(api)), "api should not be in a section");
        assert!(app.rows.contains(&Row::Step(0, api)), "api should be in its group");
        let web = app.units.iter().position(|u| u.name == "web").unwrap();
        assert_eq!(app.units[web].needs, vec![api]);
    }

    #[test]
    fn unknown_names_are_reported() {
        let app = app_with(
            r#"
[[repos]]
name = "web"
path = '{dir}'
needs = ["nope"]

[[groups]]
name = "g"
steps = [{ repo = "missing" }]
"#,
        );
        let (msg, _) = app.toast.as_ref().expect("a warning");
        assert!(msg.contains("no project named missing") || msg.contains("nope"), "{msg}");
    }

    #[test]
    fn finds_in_the_log() {
        let mut app = app_with(
            r#"
[[repos]]
name = "web"
path = '{dir}'
"#,
        );
        let log = app.current_log().unwrap();
        for i in 0..50 {
            proc::log_line(&log, if i % 10 == 3 { format!("line {i} ERROR boom") } else { format!("line {i}") });
        }
        app.log_rows = 10;
        app.search = "error".into();
        app.search_jump(true, true);
        assert_eq!(app.search_hit, Some(43), "newest match first");
        app.search_jump(true, false);
        assert_eq!(app.search_hit, Some(33), "n goes older");
        app.search_jump(false, false);
        assert_eq!(app.search_hit, Some(43), "N goes newer");
        assert_eq!(app.scroll, 50 - 43 - 1 - 5, "hit sits mid-panel");
    }

    #[test]
    fn sections_order_items_and_fold() {
        let mut app = app_with(
            r#"
[[repos]]
name = "a"
path = '{dir}'
cmd = "a"
category = "Other"

[[repos]]
name = "b"
path = '{dir}'
cmd = "b"
category = "Other"

[[repos]]
name = "c"
path = '{dir}'
cmd = "c"
category = "Other"

[[sections]]
name = "P"
items = ["c", "stack", "a"]

[[sections]]
name = "Q"

[[groups]]
name = "stack"
section = "P"
steps = [{ name = "s1", path = '{dir}', cmd = "x" }]

[[groups]]
name = "stack"
section = "Q"
steps = [{ name = "s2", path = '{dir}', cmd = "y" }]
"#,
        );
        assert!(app.toast.is_none(), "{:?}", app.toast);
        let label = |app: &App| -> Vec<String> {
            app.rows
                .iter()
                .map(|r| match r {
                    Row::Header(h) => format!("#{h}"),
                    Row::Group(g) => format!("g:{}", app.groups[*g].id),
                    Row::Step(_, u) | Row::Unit(u) => app.units[*u].name.clone(),
                })
                .collect()
        };
        assert_eq!(
            label(&app),
            ["#P", "c", "g:P/stack", "s1", "a", "#Q", "g:Q/stack", "s2", "#Other", "b"]
        );

        app.selected = 1; // "c" in P
        app.fold(Some(true));
        assert_eq!(label(&app)[..3], ["#P", "#Q", "g:Q/stack"]);
        assert_eq!(app.selected, 0, "selection moves to the folded header");
        app.fold(Some(false));
        assert_eq!(label(&app).len(), 10);
        let _ = std::fs::remove_file(app.state_path());
    }

    /// Presses keys like a person would and draws after each one, at several terminal
    /// sizes, so a state that panics while drawing (an empty list, a tiny window) fails here.
    #[test]
    fn every_screen_state_draws_without_panicking() {
        let _turn = registry_turn();
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let key = |c: KeyCode| KeyEvent::new(c, KeyModifiers::NONE);
        let ctrl = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);

        let cfg = r#"
[[sections]]
name = "P"
items = ["stack", "web"]

[[repos]]
name = "web"
path = '{dir}'
port = 3000

[[repos]]
name = "api"
path = '{dir}'
cmd = "a"
listed = false

[[groups]]
name = "stack"
section = "P"
steps = [{ repo = "api" }, { name = "s", path = '{dir}', cmd = "x" }]
"#;
        let presses: Vec<Vec<KeyEvent>> = vec![
            vec![],
            vec![key(KeyCode::Char('/')), key(KeyCode::Char('/'))], // filter matching nothing
            vec![key(KeyCode::Char('/')), key(KeyCode::Char('z')), key(KeyCode::Down), key(KeyCode::Enter), key(KeyCode::Left), key(KeyCode::Right), key(KeyCode::Char('o')), key(KeyCode::Char('c'))],
            vec![key(KeyCode::Char('?'))],
            vec![key(KeyCode::Tab)],
            vec![key(KeyCode::Up), key(KeyCode::Up), key(KeyCode::Enter)], // fold from the header
            vec![key(KeyCode::Left), key(KeyCode::Left), key(KeyCode::Down), key(KeyCode::Down)],
            vec![ctrl('f'), key(KeyCode::Char('x')), key(KeyCode::Up), key(KeyCode::Down), key(KeyCode::Esc)],
            vec![key(KeyCode::Char('n')), key(KeyCode::Char('N')), key(KeyCode::PageUp), key(KeyCode::Char('f'))],
            vec![key(KeyCode::Char('G')), key(KeyCode::Char('g')), key(KeyCode::End), key(KeyCode::Home)],
        ];
        for (w, h) in [(120, 40), (40, 10), (12, 4), (1, 1)] {
            for seq in &presses {
                let mut app = app_with(cfg);
                let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
                term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
                for k in seq {
                    app.on_key(*k);
                    app.drain();
                    app.tick();
                    term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
                }
                // An empty list after a filter that matches nothing.
                app.filter = "no-such-project".into();
                app.rebuild_rows(None);
                for k in [key(KeyCode::Enter), key(KeyCode::Left), key(KeyCode::Char('r')), key(KeyCode::Char('L')), key(KeyCode::Tab)] {
                    app.on_key(k);
                    term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
                }
                app.mode = Mode::ConfirmQuit;
                term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
                let _ = std::fs::remove_file(app.state_path());
            }
        }
    }

    #[test]
    fn branch_picker_draws_and_filters() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut app = app_with("[[repos]]
name = \"web\"
path = '{dir}'
");
        let b = |name: &str, remote: Option<&str>, current: bool| proc::BranchItem {
            name: name.into(),
            remote: remote.map(Into::into),
            when: "2 days ago".into(),
            subject: "a commit with a fairly long subject line to clip".into(),
            current,
        };
        app.picker = Some(Picker {
            path: PathBuf::from("."),
            project: "web".into(),
            items: vec![b("main", None, true), b("feat/very-long-branch-name-that-needs-clipping", None, false), b("hotfix", Some("origin"), false)],
            filter: String::new(),
            sel: 1,
            changed: 2,
            busy: false,
            error: Some("error: your local changes would be overwritten".into()),
            fetching: true,
            fetch_error: Some("Could not resolve host: github.com".into()),
        });
        app.mode = Mode::Branches;
        for (w, h) in [(120, 40), (40, 12), (10, 4), (1, 1)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        }
        for c in "fix".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(app.picker.as_ref().unwrap().visible(), vec![2], "typing filters");
        app.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        assert!(app.picker.as_ref().unwrap().visible().is_empty());
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)); // nothing to pick: no panic
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.picker.is_none() && app.mode == Mode::Normal);
    }


    /// Renders the dashboard with sample data and prints it (run with --nocapture to look).
    #[test]
    fn dashboard_renders_with_data() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut app = app_with(
            r#"
[[sections]]
name = "zimpra-core"
items = ["backends", "tracking-web", "order-web"]

[[repos]]
name = "api-core"
path = '{dir}'
cmd = "nest start"
port = 3000
listed = false

[[repos]]
name = "tracking-web"
path = '{dir}'
cmd = "next dev -p 3002"
port = 3002
needs = ["api-core"]

[[repos]]
name = "order-web"
path = '{dir}'
cmd = "next dev -p 3004"

[[groups]]
name = "backends"
section = "zimpra-core"
steps = [{ repo = "api-core" }]
"#,
        );
        for name in ["api-core", "tracking-web"] {
            let i = app.units.iter().position(|u| u.name == name).unwrap();
            let u = &mut app.units[i];
            u.status = Status::Running;
            u.pid = Some(4000 + i as u32);
            u.started = Some(Instant::now() - Duration::from_secs(2537));
            u.up = Some(true);
            u.branch = Some("feat/trip-eta".into());
            for k in 0..120 {
                let x = k as f32;
                u.metrics.cpu.push_back(3.0 + (x / 6.0).sin().abs() * 9.0);
                u.metrics.mem.push_back(300_000_000 + (k as u64) * 2_500_000);
                u.metrics.rt.push_back(if k % 3 == 0 { None } else { Some((35.0 + (x / 4.0).cos() * 8.0 + if k == 100 { 380.0 } else { 0.0 }) as f64) });
                u.metrics.rate.push_back(((x / 3.0).sin().abs() * 20.0).round());
            }
            u.metrics.rt_peak = 420.0;
            u.metrics.errors = [0, 1, 0, 0, 3, 0, 0, 1].into_iter().collect();
            proc::log_line(&u.log, "✓ Compiled /trips in 1.2s (812 modules)");
            proc::log_line(&u.log, "GET /api/trips?status=live 200 in 38ms");
            proc::log_line(&u.log, "⚠ Fast Refresh had to perform a full reload");
            proc::log_line(&u.log, "Error: ETA service timeout after 400ms");
        }
        let web = app.rows.iter().position(|r| matches!(r, Row::Unit(u) if app.units[*u].name == "tracking-web")).unwrap();
        app.selected = web;
        let mut term = Terminal::new(TestBackend::new(130, 36)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer();
        let mut screen = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                screen.push_str(buf[(x, y)].symbol());
            }
            screen.push('\n');
        }
        println!("{screen}");
        for want in ["PROJECTS", "TRACKING-WEB", "CPU", "MEMORY", "RESPONSE TIME", "START-UP", "LOGS", "localhost:3002", "ready", "ERRORS (1H)"] {
            assert!(screen.contains(want), "missing {want}");
        }
        let _ = std::fs::remove_file(app.state_path());
    }

    /// devhub quits with "leave running"; the next devhub shows the server as running,
    /// and "stop all and quit" then really stops it.
    #[test]
    fn a_new_devhub_finds_servers_left_running() {
        let _turn = registry_turn();
        let long = if cfg!(windows) { "ping -n 60 127.0.0.1" } else { "sleep 60" };
        let cfg = format!("[[repos]]\nname = \"keeper\"\npath = '{{dir}}'\ncmd = \"{long}\"\n");
        let mut first = app_with(&cfg);
        let ui = first.units.iter().position(|u| u.name == "keeper").unwrap();
        first.start_unit(ui);
        settle(&mut first, |a| a.units[ui].status == Status::Running);
        let pid = first.units[ui].pid.unwrap();
        assert!(registry_path().exists(), "running.toml lists the server");

        // Quit with l: the server keeps running.
        first.mode = Mode::ConfirmQuit;
        first.on_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
        assert!(first.quit);
        first.shutdown();
        drop(first);
        std::thread::sleep(Duration::from_millis(500));
        assert!(proc::process_started(pid).is_some(), "left running");

        // A new devhub finds it.
        let mut second = app_with(&cfg);
        let ui = second.units.iter().position(|u| u.name == "keeper").unwrap();
        assert_eq!(second.units[ui].status, Status::Running);
        assert_eq!(second.units[ui].pid, Some(pid));
        assert!(second.toast.as_ref().is_some_and(|(t, _)| t.contains("Reattached 1")));

        // Quit with s: stopped for real, and forgotten.
        second.mode = Mode::ConfirmQuit;
        second.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        second.shutdown();
        let deadline = Instant::now() + Duration::from_secs(10);
        while proc::process_started(pid).is_some() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(200));
        }
        assert!(proc::process_started(pid).is_none(), "stopped");
        assert!(!registry_path().exists());
        let _ = std::fs::remove_file(second.state_path());
    }
}
