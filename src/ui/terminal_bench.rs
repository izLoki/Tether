//! Headless terminal rendering benchmark and pixel snapshot (both `#[ignore]`).
//!
//! `cargo test --release --lib bench:: -- --ignored --nocapture --test-threads=1` prints
//! per-frame timings for ten terminals running a Claude-Code-like redraw loop, and writes
//! `target/ui-shots/terminal_render.png` for before/after pixel comparison.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use eframe::egui::{self, Pos2, Rect, Vec2};

use super::{Cell, TerminalView};
use crate::profile::Profile;
use crate::ssh::terminal::{LinkState, TerminalHandle};

const TERMINALS: usize = 10;
const WARMUP_FRAMES: usize = 30;
const FRAMES: usize = 200;
/// Pause between chunks of each simulated reader thread.
const READER_PAUSE: Duration = Duration::from_millis(2);

/// A pane layout: the whole window and how many terminals per row.
#[derive(Clone, Copy)]
struct Scenario {
    name: &'static str,
    screen: Vec2,
    per_row: usize,
}

const SCENARIOS: [Scenario; 2] = [
    // Ten splits of a 1080p window (small panes).
    Scenario { name: "1080p window, 5x2 panes", screen: Vec2::new(1920.0, 1080.0), per_row: 5 },
    // Ten large panes (several monitors / 4K): ~150x45 cells each.
    Scenario { name: "large panes, 5x2", screen: Vec2::new(5400.0, 1520.0), per_row: 5 },
];

/// One "frame" of a TUI like Claude Code: a few streamed lines scroll into history, then
/// the bottom status block (spinner, truecolor gradient, 256-colour input box) is redrawn
/// in place with relative cursor moves.
pub(super) fn claude_chunk(i: usize, cols: usize) -> Vec<u8> {
    const SPINNER: [&str; 6] = ["·", "✢", "✳", "∗", "✻", "✽"];
    const BLOCK: usize = 12;
    let mut out = String::with_capacity(8 * 1024);
    out.push_str("\x1b[?2026h");
    // Jump to the top of the redraw block and clear it.
    out.push_str(&format!("\x1b[{BLOCK}A\r\x1b[J"));
    for k in 0..3 {
        let n = i * 3 + k;
        out.push_str(&format!(
            "\x1b[38;2;{};{};{}m⏺\x1b[0m \x1b[1mRead\x1b[0m(\x1b[38;5;{}msrc/ui/file_{n}.rs\x1b[0m) \x1b[2m⎿ {} lines\x1b[0m \x1b[3mstreamed output token run {n}\x1b[0m\r\n",
            (n * 7) % 256,
            (n * 13) % 256,
            200,
            16 + n % 216,
            n % 997
        ));
    }
    out.push_str(&format!(
        "\x1b[38;2;215;119;87m{} Thinking…\x1b[0m \x1b[2m({}s · ↑ {} tokens · esc to interrupt)\x1b[0m\r\n",
        SPINNER[i % SPINNER.len()],
        i / 10,
        i * 37
    ));
    out.push_str("\r\n");
    let inner = cols.saturating_sub(2);
    out.push_str(&format!("\x1b[38;5;244m╭{}╮\x1b[0m\r\n", "─".repeat(inner)));
    let prompt = format!("> refactor the renderer so that ten panes stay smooth (draft {i})");
    let pad = inner.saturating_sub(prompt.chars().count());
    out.push_str(&format!("\x1b[38;5;244m│\x1b[0m\x1b[48;5;236m\x1b[38;2;230;230;230m{prompt}{}\x1b[0m\x1b[38;5;244m│\x1b[0m\r\n", " ".repeat(pad)));
    out.push_str(&format!("\x1b[38;5;244m╰{}╯\x1b[0m\r\n", "─".repeat(inner)));
    // A truecolor gradient bar (one SGR per cell, the worst case for style runs).
    for c in 0..cols.min(120) {
        let v = (c * 255 / cols.max(1)) as u8;
        out.push_str(&format!("\x1b[48;2;{v};{};{}m ", 255 - v, (i * 5 % 256) as u8));
    }
    out.push_str("\x1b[0m\r\n");
    out.push_str(&format!("  \x1b[38;5;{}m⏵⏵ auto-accept edits on\x1b[0m \x1b[2m(shift+tab to cycle)\x1b[0m\r\n", 70 + i % 10));
    for _ in 0..(BLOCK - 9) {
        out.push_str("\x1b[2K\r\n");
    }
    out.push_str("\x1b[?2026l");
    out.into_bytes()
}

fn connected_terminal(rows: u16, cols: u16) -> TerminalView {
    let (handle, _rx) = TerminalHandle::new(rows, cols);
    if let Ok(mut s) = handle.shared.lock() {
        s.state = LinkState::Connected;
        // Fill the scrollback so history-related costs are realistic.
        for i in 0..12_000 {
            s.parser.process(format!("\x1b[3{}mhistory line {i} lorem ipsum dolor sit amet\x1b[0m\r\n", i % 8).as_bytes());
        }
    }
    TerminalView::new(handle)
}

fn grid_rects(sc: Scenario) -> Vec<Rect> {
    let rows = TERMINALS.div_ceil(sc.per_row);
    let cell = Vec2::new(sc.screen.x / sc.per_row as f32, sc.screen.y / rows as f32);
    (0..TERMINALS).map(|i| Rect::from_min_size(Pos2::new((i % sc.per_row) as f32 * cell.x, (i / sc.per_row) as f32 * cell.y), cell).shrink(3.0)).collect()
}

fn frame(ctx: &egui::Context, views: &mut [TerminalView], rects: &[Rect], screen: Vec2, t: f64, profile: &Profile) -> Duration {
    let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, screen)), time: Some(t), ..Default::default() };
    let start = Instant::now();
    let out = ctx.run_ui(input, |ui| {
        for (i, (view, rect)) in views.iter_mut().zip(rects).enumerate() {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(*rect).id_salt(("bench_term", i)));
            view.ui(&mut child, profile);
        }
    });
    let _ = ctx.tessellate(out.shapes, out.pixels_per_point);
    start.elapsed()
}

fn stats(label: &str, samples: &mut [Duration]) {
    samples.sort();
    let total: Duration = samples.iter().sum();
    let mean = total.as_secs_f64() * 1000.0 / samples.len() as f64;
    let p50 = samples[samples.len() / 2].as_secs_f64() * 1000.0;
    let p95 = samples[samples.len() * 95 / 100].as_secs_f64() * 1000.0;
    println!("[bench] {label:<34} mean {mean:7.3} ms   p50 {p50:7.3} ms   p95 {p95:7.3} ms");
}

fn feed(views: &[TerminalView], i: usize, cols: usize) -> Duration {
    let start = Instant::now();
    for v in views {
        let chunk = claude_chunk(i, cols);
        if let Ok(mut s) = v.handle.shared.lock() {
            s.parser.process(&chunk);
            s.generation = s.generation.wrapping_add(1);
        }
    }
    start.elapsed()
}

/// One reader thread per terminal streaming continuously, like the SSH reader tasks.
fn spawn_readers(views: &[TerminalView], cols: usize, stop: &Arc<AtomicBool>) -> Vec<std::thread::JoinHandle<usize>> {
    views
        .iter()
        .map(|v| {
            let (shared, stop) = (v.handle.shared.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut n = 0usize;
                while !stop.load(Ordering::Relaxed) {
                    let chunk = claude_chunk(n, cols);
                    if let Ok(mut s) = shared.lock() {
                        s.parser.process(&chunk);
                        s.generation = s.generation.wrapping_add(1);
                    }
                    n += 1;
                    std::thread::sleep(READER_PAUSE);
                }
                n
            })
        })
        .collect()
}

fn run_scenario(sc: Scenario) {
    let ctx = egui::Context::default();
    let profile = Profile::new();
    let rects = grid_rects(sc);
    let mut views: Vec<TerminalView> = (0..TERMINALS).map(|_| connected_terminal(24, 80)).collect();
    let mut t = 0.0;
    let mut step = |views: &mut [TerminalView]| {
        let d = frame(&ctx, views, &rects, sc.screen, t, &profile);
        t += 1.0 / 60.0;
        d
    };
    // Settle sizes (the first frame resizes every parser to its pane).
    for _ in 0..3 {
        step(&mut views);
    }
    let (rows, cols) = views[0].size;
    for i in 0..WARMUP_FRAMES {
        feed(&views, i, cols as usize);
        step(&mut views);
    }
    let (mut busy, mut parse) = (Vec::new(), Vec::new());
    for i in 0..FRAMES {
        parse.push(feed(&views, WARMUP_FRAMES + i, cols as usize));
        busy.push(step(&mut views));
    }
    let mut idle = Vec::new();
    for _ in 0..FRAMES {
        idle.push(step(&mut views));
    }
    let stop = Arc::new(AtomicBool::new(false));
    let readers = spawn_readers(&views, cols as usize, &stop);
    let mut live = Vec::new();
    let started = Instant::now();
    for _ in 0..FRAMES {
        live.push(step(&mut views));
    }
    let elapsed = started.elapsed().as_secs_f64();
    stop.store(true, Ordering::Relaxed);
    let chunks: usize = readers.into_iter().map(|r| r.join().unwrap_or(0)).sum();
    println!("[bench] == {}: {TERMINALS} terminals, {rows}x{cols} cells each, 12k lines of history ==", sc.name);
    stats("busy frame (all streaming)", &mut busy);
    stats("idle frame (no output)", &mut idle);
    stats("frame with live reader threads", &mut live);
    println!("[bench] {:<34} {:7.0} chunks/s", "reader throughput meanwhile", chunks as f64 / elapsed);
    stats("vt100 parse (10 chunks, reference)", &mut parse);
}

#[test]
#[ignore = "benchmark; run with --release -- --ignored --nocapture"]
fn bench_ten_heavy_terminals() {
    for sc in SCENARIOS {
        run_scenario(sc);
    }
}

/// Deterministic styled screen for before/after pixel comparison.
#[test]
#[ignore = "writes target/ui-shots/terminal_render.png"]
fn render_snapshot() {
    let (handle, _rx) = TerminalHandle::new(24, 80);
    if let Ok(mut s) = handle.shared.lock() {
        s.state = LinkState::Connected;
        for i in 0..40 {
            s.parser.process(
                format!("\x1b[3{}mline {i}\x1b[0m \x1b[1;3{}mbold\x1b[0m \x1b[2mdim\x1b[0m \x1b[3mitalic\x1b[0m \x1b[4munder\x1b[0m\r\n", i % 8, (i + 3) % 8)
                    .as_bytes(),
            );
        }
        s.parser.process(
            "\x1b[7m inverse \x1b[0m 漢字 wide \x1b[48;5;24m\x1b[38;5;220m 256 \x1b[0m \x1b[38;2;250;80;120m\x1b[48;2;20;60;40m truecolor \x1b[0m\r\n"
                .as_bytes(),
        );
        for i in 0..6 {
            s.parser.process(&claude_chunk(i, 96));
        }
    }
    let mut view = TerminalView::new(handle);
    let profile = Profile::new();
    let mut frames = 0;
    let mut harness = egui_kittest::Harness::builder().with_size(Vec2::new(900.0, 560.0)).wgpu().build_ui(move |ui| {
        frames += 1;
        if frames == 3 {
            view.selection = Some((Cell { row: 2, col: 4 }, Cell { row: 4, col: 20 }));
        }
        view.ui(ui, &profile);
    });
    harness.run_steps(8);
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/ui-shots");
    std::fs::create_dir_all(&dir).unwrap();
    harness.render().expect("render").save(dir.join("terminal_render.png")).unwrap();
}
