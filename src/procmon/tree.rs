//! Process-tree aggregation over one sampled process table. Pure, so it is tested with
//! a fake table.

use std::collections::{HashMap, HashSet};

use crate::agents::AgentKind;

/// How many processes a tooltip lists.
pub const TOP_N: usize = 5;
/// Trees deeper than this are cut (pid reuse can fake absurd chains).
const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub struct ProcRow {
    pub pid: u32,
    pub parent: Option<u32>,
    pub name: String,
    /// Percent of the whole machine (all cores = 100).
    pub cpu: f32,
    /// Resident memory, bytes.
    pub mem: u64,
    /// Seconds since the epoch; a "child" older than its parent is a reused pid.
    pub start: u64,
    pub agent: Option<AgentKind>,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProcUsage {
    pub pid: u32,
    pub name: String,
    pub cpu: f32,
    pub mem: u64,
}

/// One terminal's process tree, summed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Usage {
    pub cpu: f32,
    pub mem: u64,
    pub procs: usize,
    /// The busiest processes (CPU, then memory), at most [`TOP_N`].
    pub top: Vec<ProcUsage>,
    /// The agent closest to the shell, if one runs in the tree.
    pub agent: Option<AgentKind>,
    /// True when the agent process has active child processes (subagents, shells, scripts,
    /// tools) or significant CPU activity in its subtree.
    pub subprocs_busy: bool,
    /// Current working directory of the shell or foreground child process.
    pub cwd: Option<String>,
}

impl Usage {
    /// What the chip shows, so a repaint is needed only when this changes.
    pub fn visible_key(&self) -> (i32, u64, Option<AgentKind>) {
        (self.cpu.round() as i32, self.mem >> 20, self.agent)
    }
}

pub struct Table {
    rows: Vec<ProcRow>,
    index: HashMap<u32, usize>,
    children: HashMap<u32, Vec<usize>>,
}

impl Table {
    pub fn new(rows: Vec<ProcRow>) -> Self {
        let index = rows.iter().enumerate().map(|(i, r)| (r.pid, i)).collect();
        let mut children: HashMap<u32, Vec<usize>> = HashMap::new();
        for (i, r) in rows.iter().enumerate() {
            if let Some(p) = r.parent
                && p != r.pid
            {
                children.entry(p).or_default().push(i);
            }
        }
        Self { rows, index, children }
    }

    pub fn row(&self, pid: u32) -> Option<&ProcRow> {
        self.index.get(&pid).map(|&i| &self.rows[i])
    }

    pub fn row_mut(&mut self, pid: u32) -> Option<&mut ProcRow> {
        self.index.get(&pid).map(|&i| &mut self.rows[i])
    }

    /// Pids in the tree under `root` (breadth first, root first).
    pub fn descendants(&self, root: u32) -> Vec<u32> {
        let Some(&start) = self.index.get(&root) else { return Vec::new() };
        let mut seen = HashSet::from([root]);
        let mut order = vec![start];
        let mut level = vec![start];
        for _ in 0..MAX_DEPTH {
            let mut next = Vec::new();
            for &i in &level {
                let parent = &self.rows[i];
                for &c in self.children.get(&parent.pid).map(Vec::as_slice).unwrap_or_default() {
                    let child = &self.rows[c];
                    if child.start >= parent.start && seen.insert(child.pid) {
                        next.push(c);
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            order.extend_from_slice(&next);
            level = next;
        }
        order.into_iter().map(|i| self.rows[i].pid).collect()
    }

    pub fn aggregate(&self, root: u32) -> Option<Usage> {
        self.index.get(&root)?;
        let pids = self.descendants(root);
        let rows: Vec<&ProcRow> = pids.iter().filter_map(|p| self.row(*p)).collect();
        let mut top: Vec<ProcUsage> = rows.iter().map(|r| ProcUsage { pid: r.pid, name: r.name.clone(), cpu: r.cpu, mem: r.mem }).collect();
        top.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.mem.cmp(&a.mem)));
        top.truncate(TOP_N);
        let agent_row = rows.iter().copied().find(|r| r.agent.is_some());
        let subprocs_busy = match agent_row {
            Some(ap) => {
                let desc_pids = self.descendants(ap.pid);
                let has_active_child = desc_pids.iter().copied().filter(|&p| p != ap.pid).filter_map(|p| self.row(p)).any(|r| {
                    !is_passive_helper(&r.name) && (r.agent.is_some() || is_tool_or_shell_proc(&r.name) || r.start > ap.start.saturating_add(2) || r.cpu >= 0.8)
                });
                has_active_child || ap.cpu >= 2.0
            }
            None => false,
        };
        let cwd = agent_row.and_then(|r| r.cwd.clone()).or_else(|| rows.first().and_then(|r| r.cwd.clone()));
        Some(Usage {
            cpu: rows.iter().map(|r| r.cpu).sum(),
            mem: rows.iter().map(|r| r.mem).sum(),
            procs: rows.len(),
            top,
            // Breadth-first order: the shallowest agent is the one the user started.
            agent: agent_row.and_then(|r| r.agent),
            subprocs_busy,
            cwd,
        })
    }
}

fn is_passive_helper(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let stem = n.strip_suffix(".exe").unwrap_or(&n);
    matches!(stem, "conhost" | "openconsole" | "tether")
}

fn is_tool_or_shell_proc(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let stem = n.strip_suffix(".exe").unwrap_or(&n);
    matches!(
        stem,
        "bash"
            | "sh"
            | "zsh"
            | "fish"
            | "pwsh"
            | "powershell"
            | "cmd"
            | "cargo"
            | "rustc"
            | "rustup"
            | "npm"
            | "npx"
            | "pnpm"
            | "yarn"
            | "bun"
            | "deno"
            | "python"
            | "python3"
            | "py"
            | "pytest"
            | "uv"
            | "git"
            | "rg"
            | "grep"
            | "find"
            | "fd"
            | "make"
            | "cmake"
            | "ninja"
            | "go"
            | "dotnet"
            | "msbuild"
            | "cl"
            | "link"
            | "jest"
            | "vitest"
            | "tsc"
            | "eslint"
            | "prettier"
            | "curl"
            | "wget"
            | "ssh"
            | "scp"
            | "tar"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: u32, parent: Option<u32>, name: &str, cpu: f32, mem_mb: u64) -> ProcRow {
        ProcRow { pid, parent, name: name.into(), cpu, mem: mem_mb << 20, start: 100 + pid as u64, agent: AgentKind::detect(name, &[]), cwd: None }
    }

    fn table() -> Table {
        Table::new(vec![
            row(1, None, "tether.exe", 1.0, 120),
            row(10, Some(1), "pwsh.exe", 0.5, 80),
            row(11, Some(10), "claude.exe", 6.0, 300),
            row(12, Some(11), "node.exe", 2.0, 90),
            row(13, Some(11), "rg.exe", 9.0, 10),
            row(20, Some(1), "cmd.exe", 0.0, 5),
            row(30, None, "explorer.exe", 3.0, 200),
        ])
    }

    #[test]
    fn sums_the_whole_tree_under_a_shell() {
        let u = table().aggregate(10).unwrap();
        assert_eq!(u.procs, 4);
        assert!((u.cpu - 17.5).abs() < 1e-4);
        assert_eq!(u.mem, (80 + 300 + 90 + 10) << 20);
        assert_eq!(u.agent, Some(AgentKind::Claude));
        assert_eq!(u.top.iter().map(|p| p.pid).collect::<Vec<_>>(), vec![13, 11, 12, 10]);
        let shell_only = table().aggregate(20).unwrap();
        assert_eq!((shell_only.procs, shell_only.agent), (1, None));
        assert!(table().aggregate(999).is_none());
        // The app total: tether and everything it started, not unrelated processes.
        assert_eq!(table().aggregate(1).unwrap().procs, 6);
    }

    #[test]
    fn top_list_is_capped_and_orders_by_cpu_then_memory() {
        let mut rows = vec![row(1, None, "sh", 0.0, 1)];
        for pid in 2..12 {
            rows.push(row(pid, Some(1), "worker", 1.0, pid as u64));
        }
        let u = Table::new(rows).aggregate(1).unwrap();
        assert_eq!(u.top.len(), TOP_N);
        assert_eq!(u.top[0].pid, 11, "equal CPU: the bigger one first");
    }

    #[test]
    fn cycles_and_reused_pids_do_not_loop_or_count() {
        let mut a = row(5, Some(6), "a", 1.0, 1);
        let mut b = row(6, Some(5), "b", 1.0, 1);
        a.start = 10;
        b.start = 10;
        let u = Table::new(vec![a, b]).aggregate(5).unwrap();
        assert_eq!(u.procs, 2, "a cycle is walked once");
        // A process that claims a parent younger than itself is a reused pid.
        let parent = row(7, None, "new-shell", 0.0, 1);
        let mut stale = row(8, Some(7), "old", 50.0, 1);
        stale.start = 1;
        assert_eq!(Table::new(vec![parent, stale]).aggregate(7).unwrap().procs, 1);
    }

    #[test]
    fn shallowest_agent_wins() {
        let rows = vec![row(1, None, "pwsh", 0.0, 1), row(2, Some(1), "codex.exe", 0.0, 1), row(3, Some(2), "claude.exe", 0.0, 1)];
        assert_eq!(Table::new(rows).aggregate(1).unwrap().agent, Some(AgentKind::Codex));
    }

    #[test]
    fn visible_key_rounds() {
        let u = Usage { cpu: 12.4, mem: (340 << 20) + 5, ..Default::default() };
        assert_eq!(u.visible_key(), (12, 340, None));
    }
}
