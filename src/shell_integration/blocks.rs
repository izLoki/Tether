//! Command blocks built from shell-integration marks.
//!
//! The shell scripts in this module emit the FinalTerm / VS Code marks
//! (`OSC 133 ; A` prompt start, `B` prompt end, `C` command start, `D ; <exit>` command
//! end), the standard `OSC 7` working directory, and two extras under our own number:
//! `OSC 6973 ; cmd ; <escaped text>` (the command line) and `OSC 6973 ; branch ; <name>`.
//! [`ShellState`] turns them into a ring of [`Block`]s positioned on *absolute* lines.
//!
//! ## Absolute lines
//! vt100 keeps `filled` rows of scrollback (up to its cap) above the visible screen, and
//! drops the oldest one for every new one once the cap is reached. A line's absolute
//! number is `evicted + index`, where `index` counts from the oldest stored scrollback
//! row and `evicted` is how many rows have fallen off the top so far. vt100 does not
//! count evictions, so [`LineTracker`] infers them: scrollback rows never change once
//! stored, so it remembers a hash of the newest few, and on the next sync looks for that
//! run again; how far it moved towards the front is how many rows were evicted. If the
//! run is gone (more output than the whole scrollback in one go) or the scrollback
//! shrank (a terminal reset), the blocks can no longer be placed and are dropped.

use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Most blocks kept per terminal; the oldest are dropped first.
pub const MAX_BLOCKS: usize = 2000;
/// Our private OSC number (`OSC 6973 ; key ; value`).
pub const OSC_EXT: &[u8] = b"6973";
/// Scrollback rows hashed to find the eviction offset (more = fewer false matches).
const ANCHOR_ROWS: usize = 4;
/// Lazy marks: reads in a row with the cursor above the previous prompt before that
/// counts as a cleared screen rather than a repaint split across reads.
const CLEAR_VOTES: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// The prompt is showing (between `A` and `C`).
    Prompt,
    /// The command runs (`C` seen, no `D` yet).
    Running,
    /// `D` arrived.
    Done,
}

/// One prompt + command + output.
#[derive(Debug, Clone)]
pub struct Block {
    pub id: u64,
    pub phase: Phase,
    /// Absolute line of the prompt's first row (`A`).
    pub prompt_line: u64,
    /// Absolute line where the typed command begins (`B`).
    pub input_line: Option<u64>,
    /// Absolute line where the output begins (`C`).
    pub output_line: Option<u64>,
    /// Exclusive end line of the output (`D`).
    pub end_line: Option<u64>,
    pub exit: Option<i32>,
    pub command: Option<Box<str>>,
    pub cwd: Option<Arc<str>>,
    pub branch: Option<Arc<str>>,
    pub started: Option<Instant>,
    pub ended: Option<Instant>,
    /// False while `prompt_line` may still move down (see `ShellState::prompt_start`).
    settled: bool,
    /// Rows the prompt takes (`OSC 6973 ; prompt_rows ; N`), for placing lazy marks.
    prompt_rows: u8,
}

impl Block {
    fn new(id: u64, prompt_line: u64, cwd: Option<Arc<str>>, branch: Option<Arc<str>>) -> Self {
        Self {
            id,
            phase: Phase::Prompt,
            prompt_line,
            input_line: None,
            output_line: None,
            end_line: None,
            exit: None,
            command: None,
            cwd,
            branch,
            started: None,
            ended: None,
            settled: true,
            prompt_rows: 1,
        }
    }

    pub fn duration(&self) -> Option<Duration> {
        let start = self.started?;
        Some(self.ended.unwrap_or_else(Instant::now).saturating_duration_since(start))
    }

    pub fn failed(&self) -> bool {
        self.exit.is_some_and(|c| c != 0)
    }

    /// Lines of the output, `[start, end)`; open-ended while running.
    pub fn output_range(&self) -> Option<(u64, Option<u64>)> {
        let start = self.output_line?;
        Some((start, self.end_line.map(|e| e.max(start))))
    }

    /// Lines holding the typed command: from the input (or prompt) line to the output.
    pub fn command_range(&self) -> (u64, u64) {
        let from = self.input_line.unwrap_or(self.prompt_line);
        let to = self.output_line.unwrap_or(from + 1).max(from + 1);
        (from, to)
    }

    /// Last line belonging to this block (inclusive), as far as known.
    pub fn last_line(&self) -> u64 {
        self.end_line.map(|e| e.saturating_sub(1)).or(self.output_line).or(self.input_line).unwrap_or(self.prompt_line).max(self.prompt_line)
    }
}

/// A command that just finished, for "long command done" notifications.
#[derive(Debug, Clone)]
pub struct Finished {
    pub command: Option<Box<str>>,
    pub exit: Option<i32>,
    pub duration: Duration,
}

/// Infers absolute line numbers from vt100's capped scrollback (see the module docs).
#[derive(Debug, Default)]
struct LineTracker {
    /// vt100's scrollback cap; 0 = unknown (evictions are always searched for).
    capacity: usize,
    evicted: u64,
    filled: usize,
    /// Stored index and hash of the `ANCHOR_ROWS` rows above the cursor at the last sync.
    anchor: Option<(usize, u64)>,
}

/// Result of a [`LineTracker::sync`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sync {
    Ok,
    /// Line numbers before this point can no longer be placed.
    Lost,
}

impl LineTracker {
    /// `guess`: rows probably evicted since the last sync (search starts there).
    fn sync(&mut self, screen: &mut vt100::Screen, guess: usize) -> Sync {
        let saved = screen.scrollback();
        screen.set_scrollback(usize::MAX);
        let filled = screen.scrollback();
        let mut result = Sync::Ok;
        if filled < self.filled {
            // Only a reset (RIS) empties vt100's scrollback.
            self.evicted = 0;
            result = Sync::Lost;
        } else if (self.capacity == 0 || filled >= self.capacity)
            && let Some((start, hash)) = self.anchor
        {
            // Rows pushed = growth + evictions, so evictions ≈ newlines - growth.
            let guess = guess.saturating_sub(filled - self.filled);
            match find_anchor(screen, filled, start, hash, guess) {
                Some(shift) => self.evicted += shift as u64,
                None => {
                    self.evicted += self.filled as u64;
                    result = Sync::Lost;
                }
            }
        }
        self.filled = filled;
        // The rows just above the cursor are done changing (the cursor row may not be).
        let end = filled + usize::from(screen.cursor_position().0);
        self.anchor = (end >= ANCHOR_ROWS).then(|| (end - ANCHOR_ROWS, run_hash(screen, filled, end - ANCHOR_ROWS)));
        screen.set_scrollback(saved);
        result
    }

    /// Absolute line of visible row 0 when the view is scrolled back `scroll` rows.
    fn top_line(&self, scroll: usize) -> u64 {
        self.evicted + self.filled.saturating_sub(scroll) as u64
    }
}

/// How many rows the anchor run (stored index `start` at the last sync) moved towards
/// the front, i.e. how many rows were evicted since. The search starts at `guess` and
/// widens from there.
fn find_anchor(screen: &mut vt100::Screen, filled: usize, start: usize, hash: u64, guess: usize) -> Option<usize> {
    let rows = usize::from(screen.size().0);
    let guess = guess.min(start);
    let fits = |shift: usize| start - shift + ANCHOR_ROWS <= filled + rows;
    for d in 0..=start {
        let below = guess.checked_sub(d).filter(|_| d > 0);
        let above = Some(guess + d).filter(|s| *s <= start);
        if below.is_none() && above.is_none() {
            break;
        }
        for shift in [above, below].into_iter().flatten() {
            if fits(shift) && run_hash(screen, filled, start - shift) == hash {
                return Some(shift);
            }
        }
    }
    None
}

/// Hash of `ANCHOR_ROWS` stored rows from index `start` (0 = oldest scrollback row;
/// indexes from `filled` on are screen rows).
fn run_hash(screen: &mut vt100::Screen, filled: usize, start: usize) -> u64 {
    let cols = screen.size().1;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for i in start..start + ANCHOR_ROWS {
        let row = if i < filled {
            screen.set_scrollback(filled - i);
            0
        } else {
            screen.set_scrollback(0);
            (i - filled) as u16
        };
        for c in 0..cols {
            match screen.cell(row, c) {
                Some(cell) => cell.contents().hash(&mut h),
                None => 0u8.hash(&mut h),
            }
        }
        screen.row_wrapped(row).hash(&mut h);
    }
    h.finish()
}

/// Per-terminal shell-integration state, owned by the vt100 callbacks.
#[derive(Debug, Default)]
pub struct ShellState {
    lines: LineTracker,
    blocks: VecDeque<Block>,
    next_id: u64,
    pending_command: Option<Box<str>>,
    /// Newlines fed since the last sync.
    newlines: usize,
    /// Marks may arrive ahead of their text (ConPTY); see [`Self::prompt_start`].
    lazy_marks: bool,
    /// Reads in a row that left the cursor above the previous prompt (see `settle`).
    clear_votes: u8,
    /// Latest prompt height the shell reported.
    prompt_rows: u8,
    cwd: Option<Arc<str>>,
    branch: Option<Arc<str>>,
    /// Bumped on every change to the blocks (views redraw marks when it moves).
    pub version: u64,
    /// Commands finished so far, and the latest one.
    pub finished_count: u64,
    pub last_finished: Option<Finished>,
    /// `finished_count` the app has already looked at (see `take_finished`).
    finished_seen: u64,
}

impl ShellState {
    /// `scrollback` is the parser's scrollback cap.
    pub fn new(scrollback: usize) -> Self {
        Self { lines: LineTracker { capacity: scrollback, ..Default::default() }, ..Default::default() }
    }

    /// Positions of marks are only approximate when they arrive (Windows ConPTY renders
    /// text a frame later than it passes escape sequences through).
    pub fn set_lazy_marks(&mut self, lazy: bool) {
        self.lazy_marks = lazy;
    }

    /// True once the shell has sent any integration mark.
    pub fn active(&self) -> bool {
        self.next_id > 0
    }

    pub fn blocks(&self) -> &VecDeque<Block> {
        &self.blocks
    }

    /// Drop decorations for content actually erased by ED/RIS, preserving blocks
    /// wholly in scrollback. Called at the escape sequence, before later marks.
    pub fn erase_display(&mut self, screen: &mut vt100::Screen, history: bool) {
        self.sync_inner(screen, false);
        let top = self.lines.top_line(0);
        self.blocks.retain(|b| !history && b.last_line() < top);
        self.pending_command = None;
        self.clear_votes = 0;
        // Erased screen rows are no longer valid eviction anchors.
        self.lines.anchor = None;
        self.version += 1;
    }

    pub fn branch(&self) -> Option<&str> {
        self.branch.as_deref()
    }

    /// Absolute line of visible row 0 for a view scrolled back `scroll` rows (as of the
    /// latest [`Self::sync`] or mark).
    pub fn top_line(&self, scroll: usize) -> u64 {
        self.lines.top_line(scroll)
    }

    /// Absolute line of the cursor (call [`Self::sync`] first).
    fn cursor_line(&self, screen: &vt100::Screen) -> u64 {
        self.lines.top_line(0) + u64::from(screen.cursor_position().0)
    }

    /// Most output newlines to feed between two [`Self::sync`] calls so evictions can
    /// still be counted (`None` while the shell has sent no marks).
    pub fn sync_every(&self) -> Option<usize> {
        (self.active() && self.lines.capacity > 0).then(|| (self.lines.capacity / 2).max(1))
    }

    /// `n` newlines were fed since the last sync (a hint for the eviction search).
    pub fn note_newlines(&mut self, n: usize) {
        self.newlines += n;
    }

    /// Re-reads the scrollback length and eviction count; blocks that scrolled out of
    /// the history are dropped. A few row hashes; the search for evictions starts at the
    /// noted newline count, so it is short too.
    pub fn sync(&mut self, screen: &mut vt100::Screen) {
        self.sync_inner(screen, true);
    }

    /// `at_read_end`: the parser is between two reads (lazy marks only settle then).
    fn sync_inner(&mut self, screen: &mut vt100::Screen, at_read_end: bool) {
        if !self.active() {
            return;
        }
        let guess = std::mem::take(&mut self.newlines);
        if self.lines.sync(screen, guess) == Sync::Lost {
            self.blocks.clear();
            self.version += 1;
        }
        if at_read_end || !self.lazy_marks {
            let saved = screen.scrollback();
            screen.set_scrollback(0);
            self.settle(screen);
            screen.set_scrollback(saved);
        }
        let first = self.lines.evicted;
        let before = self.blocks.len();
        while self.blocks.front().is_some_and(|b| b.phase == Phase::Done && b.last_line() < first) {
            self.blocks.pop_front();
        }
        if self.blocks.len() != before {
            self.version += 1;
        }
    }

    /// Handles `OSC 133 ; …` and `OSC 6973 ; …`. Returns false for any other OSC.
    /// `cwd` is the latest OSC 7 directory.
    pub fn on_osc(&mut self, screen: &mut vt100::Screen, params: &[&[u8]], cwd: Option<&str>) -> bool {
        match params {
            [b"133", kind, rest @ ..] => {
                self.on_mark(screen, kind, rest, cwd);
                true
            }
            [ext, key, value @ ..] if *ext == OSC_EXT => {
                let text = unescape(&value.join(&b';'));
                match *key {
                    b"cmd" => self.pending_command = Some(text.into_boxed_str()).filter(|c| !c.trim().is_empty()),
                    b"prompt_rows" => {
                        // bash/zsh send it just before the prompt, PowerShell inside it.
                        if let Ok(rows) = text.trim().parse::<u8>() {
                            self.prompt_rows = rows.clamp(1, 16);
                            if let Some(b) = self.blocks.back_mut().filter(|b| b.phase == Phase::Prompt) {
                                b.prompt_rows = self.prompt_rows;
                            }
                        }
                    }
                    b"branch" => {
                        let text = text.trim();
                        if self.branch.as_deref() != Some(text) {
                            self.branch = (!text.is_empty()).then(|| Arc::from(text));
                        }
                    }
                    _ => {}
                }
                true
            }
            _ => false,
        }
    }

    fn on_mark(&mut self, screen: &mut vt100::Screen, kind: &[u8], rest: &[&[u8]], cwd: Option<&str>) {
        if screen.alternate_screen() {
            return;
        }
        if self.next_id == 0 {
            self.next_id = 1;
        }
        self.sync_inner(screen, false);
        // Row reads below must see the live screen, not where the user scrolled to.
        let saved = screen.scrollback();
        screen.set_scrollback(0);
        self.apply_mark(screen, kind, rest, cwd);
        screen.set_scrollback(saved);
    }

    fn apply_mark(&mut self, screen: &vt100::Screen, kind: &[u8], rest: &[&[u8]], cwd: Option<&str>) {
        let line = self.cursor_line(screen);
        let col = screen.cursor_position().1;
        let now = Instant::now();
        match kind {
            b"A" => self.prompt_start(line, col, cwd),
            b"B" => {
                if let Some(b) = self.blocks.back_mut().filter(|b| b.phase == Phase::Prompt) {
                    b.input_line = Some(line);
                }
            }
            b"C" => {
                let command = self.pending_command.take();
                if !self.lazy_marks {
                    self.settle(screen);
                }
                let lazy = self.lazy_marks;
                let top = self.lines.top_line(0);
                if let Some(b) = self.blocks.back_mut().filter(|b| b.phase == Phase::Prompt) {
                    // Lazy: placed for real once its command line shows (find_command_line).
                    b.settled = !lazy;
                    b.input_line = b.input_line.map(|l| l.max(b.prompt_line));
                    b.phase = Phase::Running;
                    b.output_line = Some(if lazy {
                        // The cursor may be anywhere mid-repaint: output starts below the
                        // prompt line and the rows it wraps onto.
                        wrapped_run_end(screen, top, b.prompt_line) + 1
                    } else {
                        // Still on the command line when the echoed Enter has not moved
                        // the cursor yet; output never shares the prompt's line.
                        if col > 0 { line + 1 } else { line }.max(b.prompt_line + 1)
                    });
                    b.started = Some(now);
                    b.command = command;
                }
            }
            b"D" => {
                let exit = rest.first().and_then(|c| std::str::from_utf8(c).ok()).and_then(|c| c.trim().parse::<i32>().ok());
                let lazy = self.lazy_marks;
                if let Some(b) = self.blocks.back_mut().filter(|b| b.phase == Phase::Running) {
                    // Lazy marks: the end is fixed up when the next prompt settles.
                    let end = if lazy {
                        0
                    } else if col > 0 {
                        line + 1
                    } else {
                        line
                    };
                    b.phase = Phase::Done;
                    b.exit = exit;
                    b.end_line = Some(end.max(b.output_line.unwrap_or(end)));
                    b.ended = Some(now);
                    self.finished_count += 1;
                    self.last_finished = Some(Finished { command: b.command.clone(), exit, duration: b.duration().unwrap_or_default() });
                }
            }
            _ => return,
        }
        self.version += 1;
    }

    /// `A` at `line`. At column 0 it is exact, unless marks are *lazy*: ConPTY passes
    /// marks through at once but renders text on its next frame, so a mark can overtake
    /// the text (and even the newline) before it. Such a block stays *unsettled* and
    /// follows the cursor down while the prompt shows (see [`Self::settle`]).
    fn prompt_start(&mut self, mut line: u64, col: u16, cwd: Option<&str>) {
        if self.cwd.as_deref() != cwd {
            self.cwd = cwd.map(Arc::from);
        }
        let settled = col == 0 && !self.lazy_marks;
        if settled {
            // Anything at or below the new prompt was overwritten (`clear`, a redrawn prompt).
            while self.blocks.back().is_some_and(|b| b.prompt_line >= line) {
                self.blocks.pop_back();
            }
        } else if let Some(prev) = self.blocks.back_mut().filter(|b| b.phase != Phase::Prompt) {
            line = line.max(prev.output_line.unwrap_or(prev.prompt_line + 1));
            if self.lazy_marks
                && let Some(end) = prev.end_line.as_mut()
            {
                // Provisional; `settle` moves both when the prompt lands elsewhere.
                *end = line;
            }
        }
        match self.blocks.back_mut() {
            // A prompt that ran nothing (empty Enter, Ctrl+C) is replaced by the new one.
            Some(b) if b.phase == Phase::Prompt => {
                b.prompt_line = line;
                b.input_line = None;
                b.settled = settled;
                b.cwd.clone_from(&self.cwd);
                b.branch.clone_from(&self.branch);
                return;
            }
            // The `D` never came (the shell died, an integration that skips it): close it.
            Some(b) if b.phase == Phase::Running => {
                b.end_line = Some(line.max(b.output_line.unwrap_or(line)));
                b.ended = Some(Instant::now());
                b.phase = Phase::Done;
            }
            _ => {}
        }
        if self.blocks.len() >= MAX_BLOCKS {
            self.blocks.pop_front();
        }
        let id = self.next_id;
        self.next_id += 1;
        let mut block = Block::new(id, line, self.cwd.clone(), self.branch.clone());
        block.settled = settled;
        block.prompt_rows = self.prompt_rows.max(1);
        self.blocks.push_back(block);
    }

    /// An unsettled prompt moves to the line the cursor is on (the start of its wrapped
    /// run) once the text it overtook has arrived; the previous block's output then ends
    /// there. Exact marks only ever move down; lazy ones go wherever the cursor rests at
    /// the end of a read, as long as that is below the previous block's command (above
    /// its prompt means the screen was cleared).
    fn settle(&mut self, screen: &vt100::Screen) {
        if self.lazy_marks {
            self.find_command_line(screen);
        }
        let Some(last) = self.blocks.back().filter(|b| b.phase == Phase::Prompt && !b.settled) else { return };
        if screen.alternate_screen() {
            return;
        }
        let mut row = screen.cursor_position().0;
        while row > 0 && screen.row_wrapped(row - 1) {
            row -= 1;
        }
        let mut line = self.lines.top_line(0) + u64::from(row);
        if self.lazy_marks {
            // The cursor rests on the prompt's last row.
            line = line.saturating_sub(u64::from(last.prompt_rows.saturating_sub(1)));
        }
        if line == last.prompt_line || (!self.lazy_marks && line < last.prompt_line) {
            return;
        }
        let n = self.blocks.len();
        if self.lazy_marks && n >= 2 {
            let prev = &self.blocks[n - 2];
            if line < prev.output_line.unwrap_or(prev.prompt_line + 1) {
                // Inside or above the previous block: a repaint cut in half by a read, or
                // a cleared screen (`cls`) if the cursor stays up there.
                self.clear_votes = if line < prev.prompt_line { self.clear_votes + 1 } else { 0 };
                if self.clear_votes < CLEAR_VOTES {
                    return;
                }
                self.clear_votes = 0;
                let last = self.blocks.pop_back();
                while self.blocks.back().is_some_and(|b| b.prompt_line >= line) {
                    self.blocks.pop_back();
                }
                self.blocks.extend(last);
            }
        }
        self.clear_votes = 0;
        let n = self.blocks.len();
        self.blocks[n - 1].prompt_line = line;
        if n >= 2 {
            let lazy = self.lazy_marks;
            let prev = &mut self.blocks[n - 2];
            let floor = prev.output_line.unwrap_or(line);
            if let Some(end) = prev.end_line.as_mut() {
                *end = if lazy { line.max(floor) } else { (*end).max(line) };
            }
        }
        self.version += 1;
    }

    /// Lazy marks: a command typed (or pasted) faster than ConPTY's frames leaves its
    /// prompt mark wherever the cursor happened to be. Once the command line is on
    /// screen, find it (first row below the previous block that shows the command) and
    /// put the prompt and output start there.
    fn find_command_line(&mut self, screen: &vt100::Screen) {
        let n = self.blocks.len();
        // The newest block that ran something (a new prompt may already follow it).
        let Some(i) = (n.saturating_sub(2)..n).rev().find(|&i| self.blocks[i].phase != Phase::Prompt) else { return };
        if self.blocks[i].settled {
            return;
        }
        let Some(needle) = self.blocks[i].command.as_deref().and_then(|c| c.lines().next()).map(str::trim).filter(|c| !c.is_empty()) else {
            self.blocks[i].settled = true;
            return;
        };
        let (rows, cols) = screen.size();
        // A command that fits on its row ends that row; a longer one starts somewhere on it.
        let whole = needle.chars().count() + 4 < usize::from(cols);
        let needle: String = if whole { needle.to_string() } else { needle.chars().take(24).collect() };
        let top = self.lines.top_line(0);
        let floor = match i.checked_sub(1).map(|p| &self.blocks[p]) {
            Some(prev) => prev.output_line.unwrap_or(prev.prompt_line + 1),
            None => 0,
        };
        let first_row = floor.saturating_sub(top).min(u64::from(rows)) as u16;
        let cursor_row = screen.cursor_position().0;
        let shows_command = |r: u16| {
            let text = screen.contents_between(r, 0, r, cols);
            let text = text.trim_end();
            let at = if whole { text.strip_suffix(needle.as_str()).map(str::len) } else { text.find(needle.as_str()) };
            // After the prompt: the start of the row, a space or a prompt character.
            at.is_some_and(|at| text[..at].chars().next_back().is_none_or(|c| c.is_whitespace() || ">$#%❯»:".contains(c)))
        };
        let Some(row) = (first_row..=cursor_row.min(rows - 1)).find(|r| shows_command(*r)) else { return };
        let mut start = row;
        while start > first_row && screen.row_wrapped(start - 1) {
            start -= 1;
        }
        // A multi-line prompt ends on the command's row.
        let prompt_line = (top + u64::from(start)).saturating_sub(u64::from(self.blocks[i].prompt_rows.saturating_sub(1))).max(floor);
        let output_line = wrapped_run_end(screen, top, top + u64::from(row)) + 1;
        let b = &mut self.blocks[i];
        b.settled = true;
        b.prompt_line = prompt_line;
        b.output_line = Some(output_line);
        if let Some(end) = b.end_line.as_mut() {
            *end = (*end).max(output_line);
        }
        if i >= 1
            && let Some(end) = self.blocks[i - 1].end_line.as_mut()
        {
            *end = prompt_line.max(floor);
        }
        // A prompt already shown after it cannot sit above its output.
        if let Some(next) = self.blocks.get_mut(i + 1)
            && next.prompt_line < output_line
        {
            next.prompt_line = output_line;
        }
        self.version += 1;
    }

    /// The block with `id`, if it is still kept.
    pub fn block(&self, id: u64) -> Option<&Block> {
        // Ids grow with position: binary search instead of a scan.
        let i = self.blocks.partition_point(|b| b.id < id);
        self.blocks.get(i).filter(|b| b.id == id)
    }

    /// The finished command the app has not looked at yet, if any.
    pub fn take_finished(&mut self) -> Option<Finished> {
        if self.finished_seen == self.finished_count {
            return None;
        }
        self.finished_seen = self.finished_count;
        self.last_finished.clone()
    }

    /// Where absolute lines are right now, to read text with [`LineMap::text`] (the
    /// parser lends out its screen and its callbacks separately).
    pub fn line_map(&self) -> LineMap {
        LineMap { evicted: self.lines.evicted, filled: self.lines.filled as u64 }
    }
}

/// A snapshot of [`LineTracker`]'s numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineMap {
    pub evicted: u64,
    pub filled: u64,
}

impl LineMap {
    /// Text of absolute lines `[from, to)` (wrapped rows joined), trailing blanks trimmed.
    pub fn text(self, screen: &mut vt100::Screen, from: u64, to: u64) -> String {
        let saved = screen.scrollback();
        let (rows, cols) = screen.size();
        let first = self.evicted;
        let filled = self.filled;
        let last = (first + filled + u64::from(rows)).min(to);
        let mut out = String::new();
        for line in from.max(first)..last {
            let index = line - first;
            // Scrollback lines are read from the top row with the view scrolled to them.
            let (offset, row) = if index < filled { ((filled - index) as usize, 0u16) } else { (0, (index - filled) as u16) };
            if screen.scrollback() != offset {
                screen.set_scrollback(offset);
            }
            let text = screen.contents_between(row, 0, row, cols);
            if screen.row_wrapped(row) {
                out.push_str(&text);
            } else {
                out.push_str(text.trim_end());
                out.push('\n');
            }
        }
        screen.set_scrollback(saved);
        out.trim_end().to_string()
    }
}

/// Last absolute line of the run of wrapped rows starting at `line` (rows off the
/// visible screen are taken as single rows). `top` is the line of screen row 0.
fn wrapped_run_end(screen: &vt100::Screen, top: u64, line: u64) -> u64 {
    let rows = u64::from(screen.size().0);
    let Some(mut row) = line.checked_sub(top).filter(|r| *r < rows) else { return line };
    while row + 1 < rows && screen.row_wrapped(row as u16) {
        row += 1;
    }
    top + row
}

/// Our escape for free text in an OSC: `\\` for a backslash, `\xNN` for `;` and control
/// bytes. Everything else (UTF-8 included) is sent as is.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\x3b"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Inverse of [`escape`]; malformed escapes are kept literally.
pub fn unescape(raw: &[u8]) -> String {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'\\' {
            if raw.get(i + 1) == Some(&b'\\') {
                out.push(b'\\');
                i += 2;
                continue;
            }
            if raw.get(i + 1) == Some(&b'x')
                && let Some(b) = raw.get(i + 2..i + 4).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(b);
                i += 4;
                continue;
            }
        }
        out.push(raw[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh::terminal::{Events, feed};

    fn parser(rows: u16, scrollback: usize) -> vt100::Parser<Events> {
        vt100::Parser::new_with_callbacks(rows, 40, scrollback, Events { shell: ShellState::new(scrollback), ..Default::default() })
    }

    /// One prompt + command + output, the way the bash script emits it.
    fn command(cmd: &str, output_lines: usize, exit: i32) -> Vec<u8> {
        let mut s = format!("\x1b]133;A\x07$ \x1b]133;B\x07{cmd}\r\n\x1b]6973;cmd;{}\x07\x1b]133;C\x07", escape(cmd));
        for i in 0..output_lines {
            s.push_str(&format!("{cmd} line {i}\r\n"));
        }
        s.push_str(&format!("\x1b]133;D;{exit}\x07"));
        s.into_bytes()
    }

    fn shell(p: &vt100::Parser<Events>) -> &ShellState {
        &p.callbacks().shell
    }

    #[test]
    fn marks_build_blocks_even_when_split_across_reads() {
        let mut stream = command("make", 2, 2);
        stream.extend(command("echo hi", 1, 0));
        stream.extend(b"\x1b]6973;branch;main\x07\x1b]133;A\x07$ ");
        for cut in 0..=stream.len() {
            let mut p = parser(24, 100);
            feed(&mut p, &stream[..cut]);
            feed(&mut p, &stream[cut..]);
            let blocks = shell(&p).blocks();
            assert_eq!(blocks.len(), 3, "cut at {cut}");
            let (a, b) = (&blocks[0], &blocks[1]);
            assert_eq!((a.exit, a.command.as_deref(), a.phase), (Some(2), Some("make"), Phase::Done), "cut at {cut}");
            assert_eq!((a.prompt_line, a.output_line, a.end_line), (0, Some(1), Some(3)), "cut at {cut}");
            assert_eq!((b.exit, b.command.as_deref(), b.prompt_line, b.end_line), (Some(0), Some("echo hi"), 3, Some(5)));
            assert_eq!(blocks[2].phase, Phase::Prompt);
            assert_eq!(blocks[2].branch.as_deref(), Some("main"));
            assert_eq!(shell(&p).finished_count, 2);
        }
    }

    /// ConPTY passes marks through as soon as they are written but renders text on its
    /// next frame, so D and A overtake the output and the newline before them.
    #[test]
    fn marks_that_overtake_their_text_settle_on_the_right_lines() {
        let mut p = parser(24, 100);
        p.callbacks_mut().shell.set_lazy_marks(true);
        feed(&mut p, b"\x1b]133;A\x07PS> \x1b]133;B\x07cmd /c exit 3");
        feed(&mut p, b"\x1b]6973;cmd;cmd /c exit 3\x07\x1b]133;C\x07");
        feed(&mut p, b"\x1b]133;D;3\x07\x1b]133;A\x07\x1b]133;B\x07");
        feed(&mut p, b"\r\nPS> ");
        feed(&mut p, b"echo hi\x1b]133;C\x07");
        feed(&mut p, b"\x1b]133;D;0\x07\x1b]133;A\x07");
        feed(&mut p, b"\r\nhi\r\nPS> ");
        let b = shell(&p).blocks();
        assert_eq!(b.len(), 3, "{b:#?}");
        assert_eq!((b[0].prompt_line, b[0].output_line, b[0].end_line, b[0].exit), (0, Some(1), Some(1), Some(3)));
        assert_eq!((b[1].prompt_line, b[1].output_line, b[1].end_line, b[1].exit), (1, Some(2), Some(3), Some(0)));
        assert_eq!(b[2].prompt_line, 3);
        let map = shell(&p).line_map();
        assert_eq!(map.text(p.screen_mut(), 2, 3), "hi");
    }

    #[test]
    fn byte_by_byte_and_unknown_exit() {
        let mut p = parser(24, 100);
        for b in b"\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\x\r\n\x1b]133;C\x1b\\\x1b]133;D\x1b\\" {
            feed(&mut p, std::slice::from_ref(b));
        }
        let b = &shell(&p).blocks()[0];
        assert_eq!((b.phase, b.exit, b.command.as_deref()), (Phase::Done, None, None));
    }

    #[test]
    fn absolute_lines_survive_scrollback_eviction() {
        // 5 visible rows, 20 rows of scrollback: 60 output lines evict 40+ rows.
        let mut p = parser(5, 20);
        feed(&mut p, &command("first", 3, 1));
        let first_prompt = shell(&p).blocks()[0].prompt_line;
        assert_eq!(first_prompt, 0);
        feed(&mut p, &command("big", 60, 0));
        feed(&mut p, &command("last", 2, 0));
        feed(&mut p, b"\x1b]133;A\x07$ ");
        let s = shell(&p);
        // "first" (lines 0..5) fell out of the 25 kept rows; "big" started at line 4.
        let ids: Vec<&str> = s.blocks().iter().filter_map(|b| b.command.as_deref()).collect();
        assert_eq!(ids, ["big", "last"]);
        let last = s.blocks().iter().find(|b| b.command.as_deref() == Some("last")).unwrap();
        assert_eq!(last.prompt_line, 4 + 1 + 60);
        assert_eq!(last.output_line, Some(4 + 1 + 60 + 1));
        assert_eq!(last.end_line, Some(4 + 1 + 60 + 1 + 2));
        // The screen agrees: the last block's output is where the numbers say.
        let (from, to) = (last.output_line.unwrap(), last.end_line.unwrap());
        let map = s.line_map();
        let text = map.text(p.screen_mut(), from, to);
        assert_eq!(text, "last line 0\nlast line 1");
    }

    #[test]
    fn evicted_count_is_exact_through_many_small_commands() {
        let mut p = parser(4, 10);
        for i in 0..50 {
            feed(&mut p, &command(&format!("c{i}"), 1, i % 3));
        }
        feed(&mut p, b"\x1b]133;A\x07");
        let s = shell(&p);
        // Each command is 3 lines (prompt, output, nothing more): prompt i at 2*i.
        let last = s.blocks().back().unwrap();
        assert_eq!(last.prompt_line, 100);
        let c49 = &s.blocks()[s.blocks().len() - 2];
        assert_eq!((c49.command.as_deref(), c49.prompt_line, c49.exit), (Some("c49"), 98, Some(1)));
        assert!(s.blocks().len() <= 8, "older blocks were evicted with the scrollback");
    }

    #[test]
    fn ring_is_capped() {
        let mut p = parser(24, 100_000);
        for _ in 0..MAX_BLOCKS + 50 {
            feed(&mut p, b"\x1b]133;A\x07$ \x1b]133;C\x07\x1b]133;D;0\x07\r\n");
        }
        let s = shell(&p);
        assert_eq!(s.blocks().len(), MAX_BLOCKS);
        assert_eq!(s.blocks().front().unwrap().id, 51);
        assert_eq!(s.block(51).map(|b| b.id), Some(51));
        assert!(s.block(50).is_none());
    }

    #[test]
    fn clear_drops_overwritten_blocks_and_reset_drops_all() {
        let mut p = parser(10, 100);
        feed(&mut p, &command("a", 1, 0));
        feed(&mut p, &command("b", 1, 0));
        // `clear`: home + erase, then a new prompt on row 0.
        feed(&mut p, b"\x1b[H\x1b[2J\x1b]133;A\x07$ ");
        assert_eq!(shell(&p).blocks().len(), 1);
        feed(&mut p, &command("c", 30, 0));
        feed(&mut p, b"\x1bc\x1b]133;A\x07$ ");
        let s = shell(&p);
        assert_eq!(s.blocks().len(), 1, "a reset forgets the history");
        assert_eq!(s.blocks()[0].prompt_line, 0);
    }

    #[test]
    fn empty_prompts_replace_each_other() {
        let mut p = parser(10, 100);
        feed(&mut p, b"\x1b]133;A\x07$ \x1b]133;B\x07\r\n\x1b]133;A\x07$ \x1b]133;B\x07");
        let s = shell(&p);
        assert_eq!(s.blocks().len(), 1);
        assert_eq!((s.blocks()[0].phase, s.blocks()[0].prompt_line), (Phase::Prompt, 1));
    }

    #[test]
    fn escape_round_trips() {
        for text in ["plain", "a;b", r"C:\path\to", "multi\nline\ttab", "ünï ✓ 日本", "\x1b]0;evil\x07", "\\x41 literal"] {
            let e = escape(text);
            assert!(!e.bytes().any(|b| b < 0x20 || b == b';' || b == 0x7f), "{e:?}");
            assert_eq!(unescape(e.as_bytes()), text);
        }
        assert_eq!(unescape(br"bad \xZZ \x4"), r"bad \xZZ \x4");
    }

    #[test]
    fn command_text_with_semicolons_and_escapes_reaches_the_block() {
        let mut p = parser(10, 100);
        let cmd = "echo a; echo 'b\\c'";
        feed(&mut p, format!("\x1b]133;A\x07$ \x1b]6973;cmd;{}\x07\x1b]133;C\x07\x1b]133;D;0\x07", escape(cmd)).as_bytes());
        assert_eq!(shell(&p).blocks()[0].command.as_deref(), Some(cmd));
        // Raw ';' (a shell that did not escape) is joined back too.
        feed(&mut p, b"\r\n\x1b]133;A\x07\x1b]6973;cmd;x;y\x07\x1b]133;C\x07");
        assert_eq!(shell(&p).blocks()[1].command.as_deref(), Some("x;y"));
    }
}
