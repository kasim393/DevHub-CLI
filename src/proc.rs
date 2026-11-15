//! Child processes: spawning hidden, streaming output into logs, killing trees.

use regex::Regex;
use std::collections::VecDeque;
use std::io::Read;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const LOG_CAP: usize = 5000;

#[derive(Default)]
pub struct LogBuf {
    pub lines: VecDeque<String>,
    /// Running totals for the charts, counted on the project's own log only.
    pub total: u64,
    pub errors: u64,
    /// HTTP requests seen in the output (`GET /x 200 in 38ms`): count, summed and
    /// slowest milliseconds since the last sample, and server errors (5xx).
    pub requests: u64,
    pub req_ms: f64,
    pub req_max: f64,
    pub server_errors: u64,
}

impl LogBuf {
    pub fn push(&mut self, line: String) {
        if self.lines.len() >= LOG_CAP {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    /// Counts a line of the project's own output for the charts.
    pub fn note(&mut self, line: &str) {
        self.total += 1;
        if is_error_line(line) {
            self.errors += 1;
        }
        if let Some((status, ms)) = parse_request(line) {
            self.requests += 1;
            self.req_ms += ms;
            self.req_max = self.req_max.max(ms);
            if status >= 500 {
                self.server_errors += 1;
            }
        }
    }

    /// Takes the request figures gathered since the last call: (count, average ms, slowest ms).
    pub fn take_requests(&mut self) -> Option<(u64, f64, f64)> {
        let n = self.requests;
        let out = (n > 0).then(|| (n, self.req_ms / n as f64, self.req_max));
        self.requests = 0;
        self.req_ms = 0.0;
        self.req_max = 0.0;
        out
    }
}

/// An error line: says error / exception / ERR!, but not "0 errors" or "no errors".
pub fn is_error_line(line: &str) -> bool {
    let l = line.to_lowercase();
    if l.starts_with("$ ") || l.starts_with("──") {
        return false;
    }
    let says = l.contains("error") || l.contains("exception") || l.contains("err!") || l.starts_with('✗');
    says && !l.contains(" 0 error") && !l.starts_with("0 error") && !l.contains("no error") && !l.contains("without error")
}

/// `GET /api/trips 200 in 38ms` (Next.js), `GET / 200 12.3 ms - 52` (morgan) and the like:
/// the status code and the milliseconds.
pub fn parse_request(line: &str) -> Option<(u16, f64)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"\b(?:GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS)\s+\S+.*?\b([1-5]\d\d)\b.*?(\d+(?:\.\d+)?)\s?ms\b").unwrap()
    });
    let c = re.captures(line)?;
    Some((c.get(1)?.as_str().parse().ok()?, c.get(2)?.as_str().parse().ok()?))
}

pub type Log = Arc<Mutex<LogBuf>>;

pub fn new_log() -> Log {
    Arc::new(Mutex::new(LogBuf::default()))
}

pub fn log_line(log: &Log, line: impl Into<String>) {
    if let Ok(mut l) = log.lock() {
        l.push(line.into());
    }
}

/// Where a process's output goes: its own log, and optionally its group's log with a prefix.
pub struct Sink {
    pub log: Log,
    pub prefix: Option<String>,
}

pub enum Msg {
    /// `started`: the process's own start time (seconds since 1970), to tell it
    /// apart from a later process that reuses the pid. `log`: its output file.
    Started { key: String, pid: u32, run: u64, started: u64, log: PathBuf },
    Exited { key: String, run: u64, code: i32 },
    Port { key: String, run: u64, port: u16 },
    Branch { path: PathBuf, branch: String },
    GroupDone { group: String, token: u64, error: Option<String> },
    /// A unit's needs are up (or failed to come up): it may start now.
    Ready { key: String, token: u64, error: Option<String> },
    /// CPU (% of the whole machine) and memory (bytes) per root pid, its whole tree summed.
    Stats(Vec<(u32, f32, u64)>),
    Git { path: PathBuf, info: GitInfo },
    /// A branch switch finished: the new branch, or git's own error text.
    Switched { path: PathBuf, result: Result<String, String> },
    /// Whether each running server's port answers, by root pid.
    Health(Vec<(u32, bool)>),
    /// A background fetch finished: the fresh branch list, or why it failed.
    Fetched { path: PathBuf, result: Result<Vec<BranchItem>, String> },
    /// A `git pull` finished: its summary line or git's error.
    Pulled { path: PathBuf, project: String, result: Result<String, String> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct BranchItem {
    /// Local name (`feat/x`); for a remote-only branch, the name without its remote.
    pub name: String,
    /// `Some("origin")` when the branch exists only on that remote.
    pub remote: Option<String>,
    pub when: String,
    pub subject: String,
    pub current: bool,
}

/// Local branches plus remote ones with no local copy, most recent commit first.
pub fn branch_list(path: &Path) -> Result<Vec<BranchItem>, String> {
    let out = hidden("git")
        .arg("-C")
        .arg(path)
        .args([
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname)%09%(committerdate:relative)%09%(HEAD)%09%(contents:subject)",
            "refs/heads",
            "refs/remotes",
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    // One date order across local and remote refs; a remote branch is kept only when
    // no local branch (and no earlier remote) has its name.
    let text = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<Vec<&str>> = text
        .lines()
        .map(|l| l.splitn(4, '\t').collect::<Vec<_>>())
        .filter(|f| f.len() == 4)
        .collect();
    let locals: std::collections::HashSet<&str> =
        rows.iter().filter_map(|f| f[0].strip_prefix("refs/heads/")).collect();
    let mut all: Vec<BranchItem> = Vec::new();
    for f in &rows {
        let (refname, when, head, subject) = (f[0], f[1], f[2], f[3]);
        let item = if let Some(name) = refname.strip_prefix("refs/heads/") {
            BranchItem { name: name.into(), remote: None, when: when.into(), subject: subject.into(), current: head == "*" }
        } else if let Some((r, name)) = refname.strip_prefix("refs/remotes/").and_then(|x| x.split_once('/')) {
            if name == "HEAD" || locals.contains(name) || all.iter().any(|a| a.name == name) {
                continue;
            }
            BranchItem { name: name.into(), remote: Some(r.into()), when: when.into(), subject: subject.into(), current: false }
        } else {
            continue;
        };
        all.push(item);
    }
    Ok(all)
}

/// `git fetch --all --prune`, never asking for a password on a terminal (there is none).
/// Gives up after `timeout`. Returns git's error text on failure.
pub fn fetch(path: &Path, timeout: Duration) -> Result<(), String> {
    git_timed(path, &["fetch", "--all", "--prune", "--quiet"], timeout).map(|_| ())
}

/// `git pull --ff-only`: never merges or rebases on its own, so it can't leave the
/// project mid-conflict. Returns a one-line summary ("3 files changed, …" or
/// "Already up to date.") or git's reason for refusing.
pub fn pull(path: &Path, timeout: Duration) -> Result<String, String> {
    let out = git_timed(path, &["pull", "--ff-only"], timeout)?;
    let lines: Vec<&str> = out.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    Ok(lines
        .iter()
        .rev()
        .find(|l| l.contains("changed") || l.contains("up to date"))
        .or(lines.last())
        .map(|l| l.to_string())
        .unwrap_or_else(|| "pulled".into()))
}

/// Runs git in `path` with no terminal to prompt on, giving up after `timeout`.
/// Returns its output, or the first line of its error.
fn git_timed(path: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut child = hidden("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    // Read both pipes on the side, so a chatty git can't fill one and stall.
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_string(&mut s);
            }
            s
        })
    };
    let out = read(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = read(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let start = Instant::now();
    let what = args.first().copied().unwrap_or("git");
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
                if status.success() {
                    return Ok(out);
                }
                let first = err.lines().find(|l| !l.trim().is_empty()).map(str::trim);
                return Err(first.unwrap_or(&format!("{what} failed")).to_string());
            }
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                return Err(format!("{what} timed out"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// How many files have uncommitted changes.
pub fn changed_count(path: &Path) -> usize {
    hidden("git")
        .arg("-C")
        .arg(path)
        .args(["status", "--porcelain"])
        .stdin(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().count())
        .unwrap_or(0)
}

/// Switches to `b` (a remote-only branch becomes a local one tracking it).
/// Returns the branch now checked out, or git's error.
pub fn switch_branch(path: &Path, b: &BranchItem) -> Result<String, String> {
    let mut c = hidden("git");
    c.arg("-C").arg(path).arg("switch");
    match &b.remote {
        Some(r) => c.args(["--track", &format!("{r}/{}", b.name)]),
        None => c.arg(&b.name),
    };
    let out = c.stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(b.name.clone())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        // git's first lines say why; the rest is advice.
        Err(err.lines().filter(|l| !l.trim().is_empty()).take(3).collect::<Vec<_>>().join(" "))
    }
}

#[derive(Clone, Default, Debug)]
pub struct GitInfo {
    pub branch: String,
    pub changed: usize,
    /// Local branches, most recent commit first: (name, when, subject).
    pub branches: Vec<(String, String, String)>,
    /// Latest commits: (hash, when, author, subject).
    pub commits: Vec<(String, String, String, String)>,
}

/// Reads a repo's branch, changed-file count, branches by commit date and recent commits.
pub fn git_info(path: &Path) -> Option<GitInfo> {
    let run = |args: &[&str]| -> Option<String> {
        let o = hidden("git").arg("-C").arg(path).args(args).stdin(Stdio::null()).output().ok()?;
        o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
    };
    let branch = run(&["branch", "--show-current"])?.trim().to_string();
    let changed = run(&["status", "--porcelain"]).map(|s| s.lines().count()).unwrap_or(0);
    let split = |s: String, n: usize| -> Vec<Vec<String>> {
        s.lines()
            .map(|l| l.splitn(n, '\t').map(str::to_string).collect::<Vec<_>>())
            .filter(|v| v.len() == n)
            .collect()
    };
    let branches = run(&[
        "for-each-ref",
        "--sort=-committerdate",
        "--count=15",
        "--format=%(refname:short)%09%(committerdate:relative)%09%(contents:subject)",
        "refs/heads",
    ])
    .map(|s| split(s, 3))
    .unwrap_or_default()
    .into_iter()
    .map(|v| (v[0].clone(), v[1].clone(), v[2].clone()))
    .collect();
    let commits = run(&["log", "-20", "--format=%h%x09%ar%x09%an%x09%s"])
        .map(|s| split(s, 4))
        .unwrap_or_default()
        .into_iter()
        .map(|v| (v[0].clone(), v[1].clone(), v[2].clone(), v[3].clone()))
        .collect();
    Some(GitInfo {
        branch: if branch.is_empty() { "detached".into() } else { branch },
        changed,
        branches,
        commits,
    })
}

/// Every 2 s, checks whether each (pid, port) target's port accepts a connection on
/// localhost and sends `Health`. Idle while `targets` is empty.
pub fn spawn_health(targets: Arc<Mutex<Vec<(u32, u16)>>>, tx: Sender<Msg>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(2));
        let list = targets.lock().map(|t| t.clone()).unwrap_or_default();
        if list.is_empty() {
            continue;
        }
        let out = list
            .into_iter()
            .map(|(pid, port)| {
                let addrs: [SocketAddr; 2] =
                    [([127, 0, 0, 1], port).into(), (std::net::Ipv6Addr::LOCALHOST, port).into()];
                let up = addrs
                    .iter()
                    .any(|a| TcpStream::connect_timeout(a, Duration::from_millis(300)).is_ok());
                (pid, up)
            })
            .collect();
        if tx.send(Msg::Health(out)).is_err() {
            break;
        }
    });
}

/// Every 2 s, sums CPU and memory over each root pid's process tree (cmd → npm → node …)
/// and sends `Stats`. Idle while `pids` is empty.
pub fn spawn_stats(pids: Arc<Mutex<Vec<u32>>>, tx: Sender<Msg>) {
    use std::collections::{HashMap, HashSet};
    use sysinfo::{Pid, ProcessesToUpdate, System};
    std::thread::spawn(move || {
        let mut sys = System::new();
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f32;
        loop {
            std::thread::sleep(Duration::from_secs(2));
            let roots = pids.lock().map(|p| p.clone()).unwrap_or_default();
            if roots.is_empty() {
                continue;
            }
            sys.refresh_processes(ProcessesToUpdate::All, true);
            let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
            for (pid, p) in sys.processes() {
                if let Some(parent) = p.parent() {
                    children.entry(parent.as_u32()).or_default().push(pid.as_u32());
                }
            }
            let mut out = Vec::new();
            for root in roots {
                let (mut cpu, mut mem) = (0.0f32, 0u64);
                let mut stack = vec![root];
                let mut seen = HashSet::new();
                while let Some(p) = stack.pop() {
                    if !seen.insert(p) {
                        continue;
                    }
                    if let Some(proc_) = sys.process(Pid::from_u32(p)) {
                        cpu += proc_.cpu_usage();
                        mem += proc_.memory();
                    }
                    if let Some(c) = children.get(&p) {
                        stack.extend(c);
                    }
                }
                out.push((root, cpu / cores, mem));
            }
            if tx.send(Msg::Stats(out)).is_err() {
                break;
            }
        }
    });
}

static RUN: AtomicU64 = AtomicU64::new(1);

/// Exit code reported when a server ended outside devhub (no exit code is known).
pub const UNKNOWN_EXIT: i32 = i32::MIN;

/// devhub's own data: server logs and the list of what it started.
pub fn data_dir() -> PathBuf {
    if cfg!(test) {
        return std::env::temp_dir().join(format!("devhub-test-data-{}", std::process::id()));
    }
    dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("devhub")
}

/// The output file of a project: readable name plus a short hash of its key
/// (two groups may each have a step called "api").
pub fn log_path(key: &str, name: &str) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut h);
    let safe: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    data_dir().join("logs").join(format!("{safe}-{:08x}.log", h.finish() as u32))
}

/// When a process started (seconds since 1970), or None when no such process runs.
pub fn process_started(pid: u32) -> Option<u64> {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let mut sys = System::new();
    let p = Pid::from_u32(pid);
    sys.refresh_processes(ProcessesToUpdate::Some(&[p]), true);
    sys.process(p).map(|x| x.start_time())
}

/// Whether `pid` is still the process that started at `started` (not a reused pid).
pub fn is_alive(pid: u32, started: u64) -> bool {
    process_started(pid).is_some_and(|s| s.abs_diff(started) <= 2)
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// How a follower learns that its server ended.
enum Watch {
    /// devhub started it: wait on it, and get the exit code.
    Child(std::process::Child),
    /// Found running from an earlier session: poll that it's still the same process.
    Pid(u32, u64),
}

/// Starts `cmd` in `cwd` with no window, its output going to its own log file (so it
/// keeps running when devhub closes). Sends `Started` first, then `Port` when the output
/// names a local port, then `Exited`. `done` also gets the exit code.
pub fn spawn(
    key: String,
    name: &str,
    cwd: &Path,
    cmd: &str,
    sinks: Vec<Sink>,
    tx: Sender<Msg>,
    done: Option<Sender<i32>>,
) -> std::io::Result<()> {
    let run = RUN.fetch_add(1, Ordering::Relaxed);
    let log = log_path(&key, name);
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let out = std::fs::File::create(&log)?;
    let err = out.try_clone()?;
    let mut c = shell(cmd);
    c.current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    let child = c.spawn()?;
    let pid = child.id();
    let started = process_started(pid).unwrap_or_else(now_secs);
    let _ = tx.send(Msg::Started { key: key.clone(), pid, run, started, log: log.clone() });
    let sinks = Arc::new(sinks);
    push_all(&sinks, format!("$ {cmd}"));
    follow(log, 0, sinks, tx, key, run, Watch::Child(child), done);
    Ok(())
}

/// Follows a server started in an earlier session: shows the end of its log, then new
/// output, and reports `Exited` (with `UNKNOWN_EXIT`) when it ends. Returns its run id.
pub fn attach(key: String, pid: u32, started: u64, log: PathBuf, sinks: Vec<Sink>, tx: Sender<Msg>) -> u64 {
    let run = RUN.fetch_add(1, Ordering::Relaxed);
    let sinks = Arc::new(sinks);
    push_all(&sinks, "── still running from an earlier session: reattached".into());
    let len = std::fs::metadata(&log).map(|m| m.len()).unwrap_or(0);
    let from = len.saturating_sub(64 * 1024); // recent history
    follow(log, from, sinks, tx, key, run, Watch::Pid(pid, started), None);
    run
}

/// Reads `log` from `from` as it grows until the server ends, then reports the exit.
/// Lines that were already in the file are shown but not counted for the charts.
#[allow(clippy::too_many_arguments)]
fn follow(
    log: PathBuf,
    from: u64,
    sinks: Arc<Vec<Sink>>,
    tx: Sender<Msg>,
    key: String,
    run: u64,
    watch: Watch,
    done: Option<Sender<i32>>,
) {
    use std::io::{Seek, SeekFrom};
    let ended = Arc::new(AtomicBool::new(false));
    let code: Arc<Mutex<Option<i32>>> = Arc::new(Mutex::new(None));
    {
        let (ended, code) = (ended.clone(), code.clone());
        std::thread::spawn(move || {
            match watch {
                Watch::Child(mut child) => {
                    let c = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
                    *code.lock().unwrap() = Some(c);
                }
                Watch::Pid(pid, started) => {
                    while is_alive(pid, started) {
                        std::thread::sleep(Duration::from_secs(1));
                    }
                }
            }
            ended.store(true, Ordering::Relaxed);
        });
    }
    std::thread::spawn(move || {
        let backlog_end = std::fs::metadata(&log).map(|m| m.len()).unwrap_or(0);
        let mut pos = from;
        let mut pending: Vec<u8> = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut file = std::fs::File::open(&log).ok();
        if let Some(f) = file.as_mut() {
            let _ = f.seek(SeekFrom::Start(from));
            // Starting mid-file: skip to the next whole line.
            if from > 0 {
                let mut one = [0u8; 1];
                while let Ok(1) = f.read(&mut one) {
                    pos += 1;
                    if one[0] == b'\n' {
                        break;
                    }
                }
            }
        }
        let mut seen_port = false;
        let emit = |raw: &[u8], counted: bool, seen_port: &mut bool| {
            let text = String::from_utf8_lossy(raw);
            let text = text.trim_end_matches(['\n', '\r']);
            let clean = strip_ansi(text.rsplit('\r').next().unwrap_or(""));
            if !*seen_port {
                if let Some(port) = detect_port(&clean) {
                    *seen_port = true;
                    let _ = tx.send(Msg::Port { key: key.clone(), run, port });
                }
            }
            if counted {
                push_output(&sinks, clean);
            } else {
                push_all(&sinks, clean);
            }
        };
        loop {
            let finished = ended.load(Ordering::Relaxed);
            let n = file.as_mut().and_then(|f| f.read(&mut buf).ok()).unwrap_or(0);
            if n > 0 {
                pending.extend_from_slice(&buf[..n]);
                while let Some(i) = pending.iter().position(|b| *b == b'\n') {
                    let line: Vec<u8> = pending.drain(..=i).collect();
                    let counted = pos >= backlog_end;
                    pos += line.len() as u64;
                    emit(&line, counted, &mut seen_port);
                }
                continue;
            }
            if finished {
                if !pending.is_empty() {
                    let rest = std::mem::take(&mut pending);
                    emit(&rest, true, &mut seen_port);
                }
                break;
            }
            if file.is_none() {
                file = std::fs::File::open(&log).ok();
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        let c = code.lock().unwrap().unwrap_or(UNKNOWN_EXIT);
        push_all(
            &sinks,
            if c == UNKNOWN_EXIT { "── ended outside devhub".into() } else { format!("── exited with code {c}") },
        );
        let _ = tx.send(Msg::Exited { key, run, code: c });
        if let Some(d) = done {
            let _ = d.send(c);
        }
    });
}

fn push_all(sinks: &[Sink], line: String) {
    for s in sinks {
        let text = match &s.prefix {
            Some(p) => format!("{p}{line}"),
            None => line.clone(),
        };
        log_line(&s.log, text);
    }
}

/// Like `push_all`, and counts the line for the charts on the project's own log.
fn push_output(sinks: &[Sink], line: String) {
    if let Some(own) = sinks.iter().find(|s| s.prefix.is_none()) {
        if let Ok(mut l) = own.log.lock() {
            l.note(&line);
        }
    }
    push_all(sinks, line);
}

pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            match it.peek() {
                Some('[') => {
                    it.next();
                    for c in it.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    it.next();
                    while let Some(c) = it.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' {
                            it.next();
                            break;
                        }
                    }
                }
                _ => {
                    it.next();
                }
            }
        } else if c == '\t' {
            out.push_str("    ");
        } else if !c.is_control() {
            out.push(c);
        }
    }
    out
}

fn detect_port(line: &str) -> Option<u16> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?i)(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1?\]):(\d{2,5})").unwrap()
    });
    re.captures(line)?.get(1)?.as_str().parse().ok()
}

/// Polls until `port` accepts a connection on localhost, `cancel` is set, or `timeout` passes.
pub fn wait_port(port: u16, cancel: &AtomicBool, timeout: Duration) -> bool {
    let start = Instant::now();
    let addrs: [SocketAddr; 2] = [
        ([127, 0, 0, 1], port).into(),
        (std::net::Ipv6Addr::LOCALHOST, port).into(),
    ];
    while start.elapsed() < timeout {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        if addrs
            .iter()
            .any(|a| TcpStream::connect_timeout(a, Duration::from_millis(300)).is_ok())
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    false
}

/// Kills a process and every child it started.
pub fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        let _ = hidden("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill")
            .args(["-TERM", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Runs a command without waiting and without a console window (editors, explorer, browser).
pub fn detach(cmd: &str, cwd: Option<&Path>) {
    let mut c = shell(cmd);
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let _ = c.spawn();
}

pub fn open_url(url: &str) {
    #[cfg(windows)]
    detach(&format!("start \"\" \"{url}\""), None);
    #[cfg(target_os = "macos")]
    detach(&format!("open '{url}'"), None);
    #[cfg(all(unix, not(target_os = "macos")))]
    detach(&format!("xdg-open '{url}'"), None);
}

pub fn git_branch(path: &Path) -> Option<String> {
    let out = hidden("git")
        .arg("-C")
        .arg(path)
        .args(["branch", "--show-current"])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let b = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Some(if b.is_empty() { "detached".into() } else { b })
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn hidden(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

#[cfg(windows)]
fn shell(cmd: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let mut c = hidden("cmd");
    c.args(["/D", "/S", "/C"]).raw_arg(format!("\"{cmd}\""));
    c
}

#[cfg(not(windows))]
fn shell(cmd: &str) -> Command {
    use std::os::unix::process::CommandExt;
    let mut c = Command::new("sh");
    c.arg("-c").arg(cmd).process_group(0);
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_colors_and_finds_ports() {
        let line = "\x1b[32m  ➜  Local:   \x1b[1mhttp://localhost:5173/\x1b[0m";
        let clean = strip_ansi(line);
        assert_eq!(clean, "  ➜  Local:   http://localhost:5173/");
        assert_eq!(detect_port(&clean), Some(5173));
        assert_eq!(detect_port("Listening on 0.0.0.0:3000"), Some(3000));
        assert_eq!(detect_port("no port here"), None);
    }

    fn drain_until(rx: &std::sync::mpsc::Receiver<Msg>, want_exit: bool) -> Vec<Msg> {
        let mut got = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if let Ok(m) = rx.recv_timeout(Duration::from_millis(200)) {
                let done = matches!(m, Msg::Exited { .. });
                got.push(m);
                if done && want_exit {
                    break;
                }
            }
        }
        got
    }

    #[test]
    fn runs_logs_and_reports_port_and_exit() {
        let (tx, rx) = std::sync::mpsc::channel();
        let log = new_log();
        let cmd = "echo Local: http://localhost:4321/ && echo second";
        spawn("test-1".into(), "test", Path::new("."), cmd, vec![Sink { log: log.clone(), prefix: None }], tx, None)
            .unwrap();
        let msgs = drain_until(&rx, true);
        assert!(matches!(msgs.first(), Some(Msg::Started { .. })));
        assert!(msgs.iter().any(|m| matches!(m, Msg::Port { port: 4321, .. })));
        assert!(msgs.iter().any(|m| matches!(m, Msg::Exited { code: 0, .. })));
        let lines: Vec<String> = log.lock().unwrap().lines.iter().cloned().collect();
        assert!(lines.iter().any(|l| l.contains("second")), "{lines:?}");
    }

    #[test]
    fn kill_tree_stops_a_long_command() {
        let (tx, rx) = std::sync::mpsc::channel();
        let cmd = if cfg!(windows) { "ping -n 60 127.0.0.1" } else { "sleep 60" };
        spawn("test-2".into(), "test", Path::new("."), cmd, vec![Sink { log: new_log(), prefix: None }], tx, None)
            .unwrap();
        let Ok(Msg::Started { pid, .. }) = rx.recv_timeout(Duration::from_secs(5)) else {
            panic!("no Started")
        };
        let t = Instant::now();
        kill_tree(pid);
        let msgs = drain_until(&rx, true);
        assert!(msgs.iter().any(|m| matches!(m, Msg::Exited { .. })));
        assert!(t.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn samples_memory_of_a_process_tree() {
        let (tx, rx) = std::sync::mpsc::channel();
        let cmd = if cfg!(windows) { "ping -n 30 127.0.0.1" } else { "sleep 30" };
        spawn("test-3".into(), "test", Path::new("."), cmd, vec![Sink { log: new_log(), prefix: None }], tx.clone(), None)
            .unwrap();
        let Ok(Msg::Started { pid, .. }) = rx.recv_timeout(Duration::from_secs(5)) else { panic!() };
        spawn_stats(Arc::new(Mutex::new(vec![pid])), tx);
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut mem = 0;
        while Instant::now() < deadline && mem == 0 {
            if let Ok(Msg::Stats(list)) = rx.recv_timeout(Duration::from_millis(500)) {
                mem = list.iter().find(|(p, ..)| *p == pid).map(|s| s.2).unwrap_or(0);
            }
        }
        kill_tree(pid);
        assert!(mem > 1_000_000, "memory should be measured, got {mem}");
    }

    #[test]
    fn reads_git_info() {
        let info = git_info(Path::new(env!("CARGO_MANIFEST_DIR"))).expect("devhub is a git repo");
        assert!(!info.branch.is_empty());
        assert!(git_info(&std::env::temp_dir().join("surely-not-a-repo-devhub")).is_none());
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "init.defaultBranch=main"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn lists_and_switches_branches() {
        let root = std::env::temp_dir().join(format!("devhub-git-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let (origin, work) = (root.join("origin"), root.join("work"));
        std::fs::create_dir_all(&origin).unwrap();
        git(&origin, &["init"]);
        std::fs::write(origin.join("a.txt"), "one").unwrap();
        git(&origin, &["add", "."]);
        git(&origin, &["commit", "-m", "first"]);
        git(&origin, &["checkout", "-b", "remote-only"]);
        std::fs::write(origin.join("b.txt"), "remote").unwrap();
        git(&origin, &["add", "."]);
        git(&origin, &["commit", "-m", "on the remote"]);
        git(&origin, &["checkout", "main"]);
        git(&root, &["clone", "-q", origin.to_str().unwrap(), "work"]);
        git(&work, &["checkout", "-b", "feat"]);
        std::fs::write(work.join("a.txt"), "two").unwrap();
        git(&work, &["commit", "-am", "feat change"]);

        let list = branch_list(&work).unwrap();
        let names: Vec<(String, Option<String>)> = list.iter().map(|b| (b.name.clone(), b.remote.clone())).collect();
        assert!(names.contains(&("feat".into(), None)), "{names:?}");
        assert!(names.contains(&("main".into(), None)), "{names:?}");
        assert!(names.contains(&("remote-only".into(), Some("origin".into()))), "{names:?}");
        assert_eq!(names.iter().filter(|n| n.0 == "main").count(), 1, "origin/main is folded into main");
        assert!(!names.iter().any(|n| n.0 == "HEAD"));
        assert!(list.iter().find(|b| b.name == "feat").unwrap().current);

        let main = list.iter().find(|b| b.name == "main").unwrap().clone();
        assert_eq!(switch_branch(&work, &main), Ok("main".into()));
        assert_eq!(git_branch(&work).as_deref(), Some("main"));

        let remote = list.iter().find(|b| b.name == "remote-only").unwrap().clone();
        assert_eq!(switch_branch(&work, &remote), Ok("remote-only".into()));
        assert!(work.join("b.txt").exists(), "the remote branch's files are checked out");

        // An uncommitted change that the switch would overwrite: git refuses and says why.
        git(&work, &["checkout", "-q", "main"]);
        std::fs::write(work.join("a.txt"), "dirty").unwrap();
        assert_eq!(changed_count(&work), 1);
        let feat = list.iter().find(|b| b.name == "feat").unwrap().clone();
        let err = switch_branch(&work, &feat).unwrap_err();
        assert!(!err.is_empty());
        assert_eq!(git_branch(&work).as_deref(), Some("main"), "stayed put");

        // Pull: brings in new commits, then says it's up to date.
        git(&work, &["checkout", "-q", "--", "a.txt"]);
        std::fs::write(origin.join("c.txt"), "new").unwrap();
        git(&origin, &["add", "."]);
        git(&origin, &["commit", "-m", "new on main"]);
        let pulled = pull(&work, Duration::from_secs(30)).unwrap();
        assert!(pulled.contains("changed"), "{pulled}");
        assert!(work.join("c.txt").exists());
        assert!(pull(&work, Duration::from_secs(30)).unwrap().contains("up to date"));
        // Diverged history: --ff-only refuses instead of merging.
        std::fs::write(work.join("a.txt"), "local").unwrap();
        git(&work, &["commit", "-qam", "local only"]);
        std::fs::write(origin.join("a.txt"), "remote").unwrap();
        git(&origin, &["commit", "-qam", "remote only"]);
        assert!(pull(&work, Duration::from_secs(30)).is_err());
        assert_eq!(changed_count(&work), 0, "nothing left half-merged");

        // A branch pushed to the remote after the clone appears only after a fetch.
        git(&origin, &["branch", "brand-new"]);
        assert!(!branch_list(&work).unwrap().iter().any(|b| b.name == "brand-new"));
        fetch(&work, Duration::from_secs(30)).unwrap();
        let fresh = branch_list(&work).unwrap();
        assert!(fresh.iter().any(|b| b.name == "brand-new" && b.remote.as_deref() == Some("origin")));
        // A deleted remote branch disappears (--prune).
        git(&origin, &["branch", "-D", "remote-only"]);
        fetch(&work, Duration::from_secs(30)).unwrap();
        assert!(!branch_list(&work).unwrap().iter().any(|b| b.name == "remote-only" && b.remote.is_some()));
        // No remote reachable: an error, not a hang.
        assert!(fetch(&root.join("no-such-repo"), Duration::from_secs(10)).is_err());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn spots_errors_and_request_times() {
        assert!(is_error_line("Error: ETA service timeout"));
        assert!(is_error_line("npm ERR! missing script"));
        assert!(!is_error_line("Compiled with 0 errors"));
        assert!(!is_error_line("Found 0 errors. Watching for file changes."));
        assert!(!is_error_line("$ npm run lint-errors"));
        assert_eq!(parse_request("GET /api/trips?status=live 200 in 38ms"), Some((200, 38.0)));
        assert_eq!(parse_request(" POST /login 500 12.5 ms - 52"), Some((500, 12.5)));
        assert_eq!(parse_request("Compiled /trips in 1.2s"), None);
        let mut b = LogBuf::default();
        b.note("GET / 200 in 10ms");
        b.note("GET / 503 in 30ms");
        b.note("Error: boom");
        assert_eq!((b.total, b.errors, b.server_errors), (3, 1, 1));
        assert_eq!(b.take_requests(), Some((2, 20.0, 30.0)));
        assert_eq!(b.take_requests(), None);
    }

    /// The helper half of `servers_survive_devhub_being_killed`: run as its own process,
    /// it starts a long "server" the way devhub does, prints that server's pid, and waits.
    #[test]
    fn job_helper_process() {
        if std::env::var("DEVHUB_SERVER_HELPER").is_err() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let cmd = if cfg!(windows) { "ping -n 120 127.0.0.1" } else { "sleep 120" };
        spawn("test-4".into(), "test", Path::new("."), cmd, vec![Sink { log: new_log(), prefix: None }], tx, None).unwrap();
        let Ok(Msg::Started { pid, .. }) = rx.recv_timeout(Duration::from_secs(5)) else { panic!() };
        println!("SERVER_PID={pid}");
        std::thread::sleep(Duration::from_secs(60));
    }

    /// Kills a devhub-like process abruptly (as the window's ✕ or a crash would) and
    /// checks the server it started keeps running: devhub closing must not cost the servers.
    #[cfg(windows)]
    #[test]
    fn servers_survive_devhub_being_killed() {
        use std::io::BufRead;
        let exe = std::env::current_exe().unwrap();
        let mut helper = Command::new(&exe)
            .args(["proc::tests::job_helper_process", "--exact", "--nocapture", "--test-threads=1"])
            .env("DEVHUB_SERVER_HELPER", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let out = std::io::BufReader::new(helper.stdout.take().unwrap());
        let pid: u32 = out
            .lines()
            .map_while(Result::ok)
            .find_map(|l| {
                let at = l.find("SERVER_PID=")? + "SERVER_PID=".len();
                l[at..].chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok()
            })
            .expect("helper printed the server pid");
        let alive = |pid: u32| {
            let out = hidden("tasklist").args(["/FI", &format!("PID eq {pid}"), "/NH"]).output().unwrap();
            String::from_utf8_lossy(&out.stdout).contains(&pid.to_string())
        };
        assert!(alive(pid), "the server runs while devhub does");
        helper.kill().unwrap(); // abrupt: no cleanup code runs
        let _ = helper.wait();
        std::thread::sleep(Duration::from_secs(2));
        assert!(alive(pid), "the server must outlive devhub");
        kill_tree(pid);
    }

    /// A later devhub finds a running server again: it follows new output from the log
    /// file, and notices when the server ends.
    #[test]
    fn attaches_to_a_running_server() {
        let (tx, rx) = std::sync::mpsc::channel();
        let cmd = if cfg!(windows) {
            "for /l %i in (1,1,40) do @(echo tick %i & ping -n 2 127.0.0.1 >nul)"
        } else {
            "for i in $(seq 1 40); do echo tick $i; sleep 1; done"
        };
        spawn("attach-test".into(), "attach", Path::new("."), cmd, vec![Sink { log: new_log(), prefix: None }], tx, None)
            .unwrap();
        let Ok(Msg::Started { pid, started, log, .. }) = rx.recv_timeout(Duration::from_secs(5)) else { panic!() };
        assert!(is_alive(pid, started));
        assert!(!is_alive(pid, started + 3600), "a different start time is a different process");

        // "Another devhub" follows the same server.
        let (tx2, rx2) = std::sync::mpsc::channel();
        let later = new_log();
        attach("attach-test".into(), pid, started, log.clone(), vec![Sink { log: later.clone(), prefix: None }], tx2);
        let deadline = Instant::now() + Duration::from_secs(8);
        let count_ticks = || later.lock().unwrap().lines.iter().filter(|l| l.starts_with("tick")).count();
        let first = loop {
            let n = count_ticks();
            if n > 0 || Instant::now() > deadline {
                break n;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        assert!(first > 0, "history from the log file is shown");
        std::thread::sleep(Duration::from_millis(2500));
        assert!(count_ticks() > first, "new output keeps arriving");

        kill_tree(pid);
        let ended = (0..40).any(|_| matches!(rx2.recv_timeout(Duration::from_millis(500)), Ok(Msg::Exited { code: UNKNOWN_EXIT, .. })));
        assert!(ended, "the follower notices the server ended");
        let _ = std::fs::remove_file(log);
    }
}
