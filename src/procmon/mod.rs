//! Per-terminal CPU / memory, sampled by one background thread for the whole app.
//!
//! The app registers the shell pid of every local terminal ([`Monitor::set_watch`]);
//! each tick refreshes the process table once (CPU and memory only, command lines only
//! for new launcher processes such as `node`), sums each shell's process tree, and
//! publishes an immutable [`Snapshot`] the UI reads without waiting. A repaint is asked
//! for only when something the UI shows changed.

pub mod tree;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

pub use tree::{ProcRow, ProcUsage, Table, Usage};

use crate::agents::AgentKind;

pub const DEFAULT_INTERVAL: Duration = Duration::from_millis(1500);
/// Sampling slows to this while the window is in the background.
pub const BACKGROUND_INTERVAL: Duration = Duration::from_secs(5);

/// Results of one sample.
#[derive(Debug, Default)]
pub struct Snapshot {
    /// Keyed by the watch key (tab uid). Shared so readers don't copy the top lists.
    pub per_tab: HashMap<u64, Arc<Usage>>,
    /// tether itself plus everything it started.
    pub total: Usage,
    pub seq: u64,
    /// When the sample was taken.
    pub at: Option<Instant>,
}

/// What the UI shows of a usage (see [`Usage::visible_key`]).
type VisibleKey = (i32, u64, Option<AgentKind>);
type VisibleTab = (u64, VisibleKey, bool, Option<String>);

impl Snapshot {
    fn visible_keys(&self) -> (Vec<VisibleTab>, VisibleKey) {
        let mut v: Vec<_> = self.per_tab.iter().map(|(k, u)| (*k, u.visible_key(), u.subprocs_busy, u.cwd.clone())).collect();
        v.sort_unstable_by_key(|(k, ..)| *k);
        (v, self.total.visible_key())
    }
}

static LATEST: Mutex<Option<Arc<Snapshot>>> = Mutex::new(None);

/// The last published sample (for widgets deep in the tree, like tab titles).
pub fn latest() -> Option<Arc<Snapshot>> {
    LATEST.lock().ok().and_then(|s| s.clone())
}

/// Usage of one watched terminal, from the last sample.
pub fn usage_for(key: u64) -> Option<Arc<Usage>> {
    latest().and_then(|s| s.per_tab.get(&key).cloned())
}

struct Shared {
    watch: Mutex<HashMap<u64, u32>>,
    focused: AtomicBool,
    stop: AtomicBool,
    interval_ms: AtomicU64,
    wake: (Mutex<bool>, Condvar),
}

pub struct Monitor {
    shared: Arc<Shared>,
}

impl Monitor {
    /// Starts the sampling thread. `ctx` is woken when visible numbers change.
    pub fn start(ctx: &egui::Context) -> Self {
        let shared = Arc::new(Shared {
            watch: Mutex::new(HashMap::new()),
            focused: AtomicBool::new(true),
            stop: AtomicBool::new(false),
            interval_ms: AtomicU64::new(DEFAULT_INTERVAL.as_millis() as u64),
            wake: (Mutex::new(false), Condvar::new()),
        });
        let (s, ctx) = (shared.clone(), ctx.clone());
        let _ = std::thread::Builder::new().name("tether-procmon".into()).spawn(move || run(&s, &ctx));
        Self { shared }
    }

    /// Replaces the watched `key -> shell pid` set. New pids are sampled at once.
    pub fn set_watch(&self, watch: HashMap<u64, u32>) {
        let Ok(mut cur) = self.shared.watch.lock() else { return };
        if *cur == watch {
            return;
        }
        let grew = watch.iter().any(|(k, p)| cur.get(k) != Some(p));
        *cur = watch;
        drop(cur);
        if grew {
            self.wake();
        }
    }

    pub fn set_focused(&self, focused: bool) {
        if self.shared.focused.swap(focused, Ordering::Relaxed) != focused && focused {
            self.wake();
        }
    }

    pub fn set_interval(&self, interval: Duration) {
        self.shared.interval_ms.store(interval.as_millis().clamp(250, 60_000) as u64, Ordering::Relaxed);
    }

    fn wake(&self) {
        let (lock, cv) = &self.shared.wake;
        if let Ok(mut w) = lock.lock() {
            *w = true;
            cv.notify_one();
        }
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.wake();
    }
}

fn run(shared: &Shared, ctx: &egui::Context) {
    let mut sampler = Sampler::new();
    let mut seq = 0u64;
    let mut last_keys = None;
    loop {
        let base = Duration::from_millis(shared.interval_ms.load(Ordering::Relaxed));
        let interval = if shared.focused.load(Ordering::Relaxed) { base } else { base.max(BACKGROUND_INTERVAL) };
        {
            let (lock, cv) = &shared.wake;
            let Ok(guard) = lock.lock() else { return };
            let (mut woken, _) = cv.wait_timeout_while(guard, interval, |w| !*w).unwrap_or_else(|e| e.into_inner());
            *woken = false;
        }
        if shared.stop.load(Ordering::Relaxed) {
            return;
        }
        let watch = shared.watch.lock().map(|w| w.clone()).unwrap_or_default();
        if watch.is_empty() {
            if last_keys.take().is_some() {
                publish(Snapshot::default());
                ctx.request_repaint();
            }
            continue;
        }
        seq += 1;
        let snap = sampler.sample(&watch, seq);
        let keys = snap.visible_keys();
        let changed = last_keys.as_ref() != Some(&keys);
        last_keys = Some(keys);
        publish(snap);
        if changed {
            ctx.request_repaint();
        }
    }
}

fn publish(snap: Snapshot) {
    if let Ok(mut l) = LATEST.lock() {
        *l = Some(Arc::new(snap));
    }
}

/// Launchers whose command line tells which agent they run.
fn needs_cmd(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let stem = n.strip_suffix(".exe").unwrap_or(&n);
    matches!(stem, "node" | "bun" | "deno" | "python" | "python3" | "py" | "pythonw" | "uv" | "uvx")
}

struct Sampler {
    sys: System,
    cpus: f32,
    /// `pid -> (start time, agent)`, so command lines are read once per process.
    agents: HashMap<u32, (u64, Option<AgentKind>)>,
    self_pid: u32,
}

impl Sampler {
    fn new() -> Self {
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get()) as f32;
        Self { sys: System::new(), cpus, agents: HashMap::new(), self_pid: std::process::id() }
    }

    fn sample(&mut self, watch: &HashMap<u64, u32>, seq: u64) -> Snapshot {
        self.sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing().with_cpu().with_memory());
        self.summarize(watch, seq)
    }

    /// Builds the snapshot from the refreshed process list: one table of the whole
    /// system, names and agents filled in only for our own trees.
    fn summarize(&mut self, watch: &HashMap<u64, u32>, seq: u64) -> Snapshot {
        let mut table = self.table();
        let roots = watch.values().copied().chain(std::iter::once(self.self_pid));
        let mut ours: Vec<u32> = roots.flat_map(|root| table.descendants(root)).collect();
        ours.sort_unstable();
        ours.dedup();
        // Refresh CWD for root watched shells so `Ctrl+Shift+T` and session restore always
        // know the live working directory even when OSC 7 is off.
        let watched_pids: Vec<Pid> = watch.values().copied().map(Pid::from_u32).collect();
        if !watched_pids.is_empty() {
            self.sys.refresh_processes_specifics(ProcessesToUpdate::Some(&watched_pids), false, ProcessRefreshKind::nothing().with_cwd(UpdateKind::Always));
        }
        // Command lines only for launchers in our trees we have not classified yet.
        let mut unknown = Vec::new();
        for &pid in &ours {
            let Some(p) = self.sys.process(Pid::from_u32(pid)) else { continue };
            let name = p.name().to_string_lossy().into_owned();
            if needs_cmd(&name) && self.agents.get(&pid).is_none_or(|(s, _)| *s != p.start_time()) {
                unknown.push(Pid::from_u32(pid));
            }
            if let Some(row) = table.row_mut(pid) {
                row.name = name;
                row.cwd = p.cwd().and_then(|c| c.to_str()).map(str::to_string).filter(|s| !s.is_empty());
            }
        }
        if !unknown.is_empty() {
            self.sys.refresh_processes_specifics(ProcessesToUpdate::Some(&unknown), false, ProcessRefreshKind::nothing().with_cmd(UpdateKind::OnlyIfNotSet));
            for pid in &unknown {
                if let Some(p) = self.sys.process(*pid) {
                    let cmd: Vec<String> = p.cmd().iter().map(|a| a.to_string_lossy().into_owned()).collect();
                    let name = p.name().to_string_lossy();
                    self.agents.insert(pid.as_u32(), (p.start_time(), AgentKind::detect(&name, &cmd)));
                }
            }
        }
        for &pid in &ours {
            if let Some(row) = table.row_mut(pid) {
                row.agent = match self.agents.get(&pid) {
                    Some((s, a)) if *s == row.start => *a,
                    _ => AgentKind::detect(&row.name, &[]),
                };
            }
        }
        let sys = &self.sys;
        self.agents.retain(|pid, _| sys.process(Pid::from_u32(*pid)).is_some());
        let per_tab = watch.iter().filter_map(|(k, pid)| table.aggregate(*pid).map(|u| (*k, Arc::new(u)))).collect();
        let total = table.aggregate(self.self_pid).unwrap_or_default();
        Snapshot { per_tab, total, seq, at: Some(Instant::now()) }
    }

    /// Every process, without names or agents (filled in for our trees by `summarize`).
    fn table(&self) -> Table {
        let rows = self
            .sys
            .processes()
            .values()
            .map(|p| ProcRow {
                pid: p.pid().as_u32(),
                parent: p.parent().map(|pp| pp.as_u32()),
                name: String::new(),
                cpu: p.cpu_usage() / self.cpus,
                mem: p.memory(),
                start: p.start_time(),
                agent: None,
                cwd: None,
            })
            .collect();
        Table::new(rows)
    }
}

/// `340 MB`, `1.2 GB`.
pub fn human_mem(bytes: u64) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb >= 1024.0 { format!("{:.1} GB", mb / 1024.0) } else { format!("{mb:.0} MB") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_memory() {
        assert_eq!(human_mem(340 << 20), "340 MB");
        assert_eq!(human_mem(1536 << 20), "1.5 GB");
    }

    /// Real sampler against this test process: it must find itself and stay cheap.
    #[test]
    fn samples_own_process_tree() {
        let mut s = Sampler::new();
        let me = std::process::id();
        let watch = HashMap::from([(7u64, me)]);
        let first = s.sample(&watch, 1);
        let u = first.per_tab.get(&7).expect("own process is visible");
        assert!(u.mem > 0 && u.procs >= 1);
        // The fastest of a few ticks: one slow tick under a loaded parallel test run is
        // scheduling noise, not a slow sampler.
        let fastest = (2..5)
            .map(|seq| {
                let t = Instant::now();
                let _ = s.sample(&watch, seq);
                t.elapsed()
            })
            .min()
            .unwrap();
        assert!(fastest < Duration::from_millis(500), "the fastest tick took {fastest:?}");
    }
}
