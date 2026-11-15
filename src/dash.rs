//! The neon dashboard: projects on the left; for the selection an info card, four
//! charts and the log. Used when the window is large enough (see `fits`).

use crate::app::{App, Row, Status, Tab, Unit};
use crate::proc::Log;
use crate::theme::th;
use crate::ui::{
    draw_bottom, draw_git, draw_list, draw_log, draw_section, fmt_cpu, fmt_mem, status_word,
};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Paragraph};
use std::time::Duration;

pub const MIN_WIDTH: u16 = 110;
pub const MIN_HEIGHT: u16 = 30;

pub fn fits(area: Rect) -> bool {
    area.width >= MIN_WIDTH && area.height >= MIN_HEIGHT
}

// ------------------------------------------------------------------ pieces

fn dim(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), Style::new().fg(th().dim))
}

fn neon(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), Style::new().fg(th().accent))
}

fn text(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), Style::new().fg(th().text))
}

/// A bordered panel: an uppercase title on the left, `right` on the right.
fn panel(title: &str, right: Line<'static>) -> Block<'static> {
    Block::bordered()
        .border_style(Style::new().fg(th().border))
        .title(Line::from(Span::styled(format!(" {} ", title.to_uppercase()), Style::new().fg(th().label))))
        .title_top(right.right_aligned())
}

fn status_chip(s: Status) -> Line<'static> {
    let w = status_word(s);
    let style = w.style;
    Line::from(vec![Span::styled(format!("● {} ", w.content.to_uppercase()), style)])
}

fn fmt_uptime(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

/// How long `samples` 2-second samples span, as "-40s" / "-5m".
fn span_label(samples: usize) -> String {
    let secs = samples * 2;
    if secs >= 120 {
        format!("-{}m", secs / 60)
    } else {
        format!("-{secs}s")
    }
}

/// A row of dim text with `left` at the start and `right` at the end of `width`.
fn axis(left: &str, mid: &str, right: &str, width: usize) -> Line<'static> {
    let gap = width.saturating_sub(left.chars().count() + mid.chars().count() + right.chars().count());
    let a = gap / 2;
    let b = gap - a;
    Line::from(dim(format!("{left}{}{mid}{}{right}", " ".repeat(a), " ".repeat(b))))
}

/// Plots `series` (newest last, one value per dot column) in braille: `w`×`h` cells,
/// 2×4 dots each, scaled to `lo..hi`. None leaves a gap. `fill` shades below the line.
pub fn braille(series: &[Option<f64>], w: usize, h: usize, lo: f64, hi: f64, fill: bool) -> Vec<String> {
    let (cols, rows) = (w * 2, h * 4);
    if cols == 0 || rows == 0 {
        return Vec::new();
    }
    let mut dots = vec![vec![false; cols]; rows];
    let shown = &series[series.len().saturating_sub(cols)..];
    let x0 = cols - shown.len();
    let span = if hi > lo { hi - lo } else { 1.0 };
    let to_row = |v: f64| {
        let t = ((v - lo) / span).clamp(0.0, 1.0);
        ((rows - 1) as f64 - t * (rows - 1) as f64).round() as usize
    };
    let mut prev: Option<usize> = None;
    for (i, v) in shown.iter().enumerate() {
        let x = x0 + i;
        match v {
            Some(v) => {
                let y = to_row(*v);
                let (a, b) = match prev {
                    Some(p) => (p.min(y), p.max(y)),
                    None => (y, y),
                };
                for row in dots.iter_mut().take(b + 1).skip(a) {
                    row[x] = true;
                }
                if fill {
                    for row in dots.iter_mut().skip(y) {
                        row[x] = true;
                    }
                }
                prev = Some(y);
            }
            None => prev = None,
        }
    }
    const BIT: [[u32; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];
    (0..h)
        .map(|r| {
            (0..w)
                .map(|c| {
                    let mut v = 0;
                    for (dy, bits) in BIT.iter().enumerate() {
                        for (dx, bit) in bits.iter().enumerate() {
                            if dots[r * 4 + dy][c * 2 + dx] {
                                v |= bit;
                            }
                        }
                    }
                    char::from_u32(0x2800 + v).unwrap_or(' ')
                })
                .collect()
        })
        .collect()
}

/// ▁▂▃▅▇ bars for the last `width` values, scaled to their own maximum.
fn bars(values: &[f32], width: usize) -> String {
    const B: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let shown = &values[values.len().saturating_sub(width)..];
    let max = shown.iter().cloned().fold(0.0f32, f32::max);
    let pad = width.saturating_sub(shown.len());
    let mut s = " ".repeat(pad);
    for v in shown {
        let i = if max > 0.0 { ((v / max) * 7.0).round() as usize } else { 0 };
        s.push(B[i.min(7)]);
    }
    s
}

/// A sample value that can be charted.
trait Sample: Copy {
    fn f(self) -> f64;
}
impl Sample for f32 {
    fn f(self) -> f64 {
        self as f64
    }
}
impl Sample for u64 {
    fn f(self) -> f64 {
        self as f64
    }
}
impl Sample for u32 {
    fn f(self) -> f64 {
        self as f64
    }
}

/// Sums several projects' series, aligned at the newest sample.
fn sum_aligned<T: Sample>(series: &[&std::collections::VecDeque<T>]) -> Vec<f64> {
    let n = series.iter().map(|s| s.len()).max().unwrap_or(0);
    (0..n)
        .map(|i| {
            series
                .iter()
                .filter_map(|s| {
                    let off = n - s.len();
                    (i >= off).then(|| s[i - off].f())
                })
                .sum()
        })
        .collect()
}

// ------------------------------------------------------------------ the selection

/// What the right-hand side shows: one project, or a group's projects together.
struct Target<'a> {
    name: String,
    status: Status,
    units: Vec<&'a Unit>,
    /// Set for a single project.
    main: Option<&'a Unit>,
    /// The start-up chain, in order: needs then the project, or a group's steps.
    chain: Vec<&'a Unit>,
    log: Log,
}

fn target(app: &App) -> Option<Target<'_>> {
    match app.current()? {
        Row::Group(gi) => {
            let g = &app.groups[*gi];
            let mut chain: Vec<usize> = Vec::new();
            for s in &g.steps {
                for n in app.collect_needs(s.unit).into_iter().chain([s.unit]) {
                    if !chain.contains(&n) {
                        chain.push(n);
                    }
                }
            }
            Some(Target {
                name: g.name.clone(),
                status: app.group_status(*gi),
                units: g.steps.iter().map(|s| &app.units[s.unit]).collect(),
                main: None,
                chain: chain.into_iter().map(|i| &app.units[i]).collect(),
                log: g.log.clone(),
            })
        }
        row => {
            let ui = row.unit()?;
            let u = &app.units[ui];
            let mut chain: Vec<&Unit> = app.collect_needs(ui).into_iter().map(|i| &app.units[i]).collect();
            chain.push(u);
            Some(Target {
                name: u.name.clone(),
                status: u.status,
                units: vec![u],
                main: Some(u),
                chain,
                log: u.log.clone(),
            })
        }
    }
}

// ------------------------------------------------------------------ layout

pub fn draw(f: &mut Frame, app: &mut App) {
    let [top, body, bottom] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(1)]).areas(f.area());
    let [left, right] = Layout::horizontal([Constraint::Length(34), Constraint::Min(1)])
        .spacing(1)
        .areas(body);

    // Rows for the log, before anything borrows the app for drawing.
    let chart_h = ((right.height.saturating_sub(5 + 7)) / 2).clamp(6, 10);
    let logs_h = right.height.saturating_sub(5 + chart_h * 2);
    app.log_rows = logs_h.saturating_sub(2) as usize;

    draw_top(f, app, top);
    draw_projects(f, app, left);
    match target(app) {
        Some(t) => draw_target(f, app, &t, right, chart_h),
        None => {
            let block = panel("section", Line::raw(""));
            let inner = block.inner(right);
            f.render_widget(block, right);
            draw_section(f, app, inner.inner(Margin::new(1, 0)));
        }
    }
    draw_bottom(f, app, bottom);
}

fn draw_top(f: &mut Frame, app: &App, area: Rect) {
    let section = app.section_of_selected().map(|(_, s)| s.to_uppercase()).unwrap_or_default();
    let left = Line::from(vec![
        Span::styled(" DEVHUB", Style::new().fg(th().accent).bold()),
        dim(" // "),
        text(section),
    ]);
    let live = app.live_count();
    let (cpu, mem) = app
        .units
        .iter()
        .filter_map(|u| u.pid.and_then(|p| app.stats.get(&p)))
        .fold((0.0f32, 0u64), |(c, m), (uc, um)| (c + uc, m + um));
    let clock = chrono::Local::now().format("%H:%M:%S").to_string();
    let mut right = vec![];
    if live > 0 {
        right.push(Span::styled(format!("● {live} RUNNING"), Style::new().fg(th().ok)));
        right.push(dim("  ·  CPU "));
        right.push(neon(format!("{cpu:.1}%")));
        right.push(dim("  ·  MEM "));
        right.push(neon(fmt_mem(mem)));
    } else {
        right.push(dim("IDLE"));
    }
    right.push(dim(format!("  ·  {clock} ")));
    f.render_widget(Paragraph::new(left), area);
    f.render_widget(Paragraph::new(Line::from(right)).alignment(Alignment::Right), area);
}

fn draw_projects(f: &mut Frame, app: &mut App, area: Rect) {
    let count = app.units.iter().filter(|u| u.category.is_some()).count();
    let block = panel("projects", Line::from(dim(format!(" {count} "))));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [list, rate] = Layout::vertical([Constraint::Min(1), Constraint::Length(4)]).areas(inner);
    draw_list(f, app, list);

    // Log rate of the selection, as bars.
    let w = rate.width.saturating_sub(2) as usize;
    let series: Vec<f32> = match target(app) {
        Some(t) => sum_aligned(&t.units.iter().map(|u| &u.metrics.rate).collect::<Vec<_>>())
            .into_iter()
            .map(|v| v as f32)
            .collect(),
        None => Vec::new(),
    };
    let max = series.iter().cloned().fold(0.0f32, f32::max);
    let now = series.last().copied().unwrap_or(0.0);
    let lines = vec![
        Line::raw(""),
        axis(" LOG RATE", "", "lines/s ", w + 2),
        Line::from(vec![Span::raw(" "), neon(bars(&series, w))]),
        Line::from(vec![dim(" 0"), dim(" ".repeat(w.saturating_sub(10))), neon(format!("{now:.0}")), dim(format!("/{max:.0}"))]),
    ];
    f.render_widget(Paragraph::new(lines), rate);
}

fn draw_target(f: &mut Frame, app: &App, t: &Target, area: Rect, chart_h: u16) {
    let [card, rest] = Layout::vertical([Constraint::Length(5), Constraint::Min(1)]).areas(area);
    draw_card(f, app, t, card);

    if app.tab == Tab::Git {
        let block = panel(&format!("git · {}", t.name), Line::from(dim(" TAB LOG  ·  B SWITCH ")));
        let inner = block.inner(rest);
        f.render_widget(block, rest);
        draw_git(f, app, inner.inner(Margin::new(1, 0)));
        return;
    }

    let [row1, row2, logs] = Layout::vertical([
        Constraint::Length(chart_h),
        Constraint::Length(chart_h),
        Constraint::Min(3),
    ])
    .areas(rest);
    let [cpu, mem] = Layout::horizontal([Constraint::Ratio(1, 2); 2]).spacing(1).areas(row1);
    let [rt, boot] = Layout::horizontal([Constraint::Ratio(1, 2); 2]).spacing(1).areas(row2);

    let cpu_s = sum_aligned(&t.units.iter().map(|u| &u.metrics.cpu).collect::<Vec<_>>());
    let mem_s = sum_aligned(&t.units.iter().map(|u| &u.metrics.mem).collect::<Vec<_>>());
    draw_cpu(f, &cpu_s, cpu);
    draw_mem(f, &mem_s, mem);
    draw_rt(f, t, rt);
    draw_boot(f, t, boot);

    let block = panel(&format!("logs · {}", t.name), Line::from(dim(" CTRL+F FIND  ·  L OPEN  ·  TAB GIT ")));
    let inner = block.inner(logs).inner(Margin::new(1, 0));
    f.render_widget(block, logs);
    draw_log(f, inner, &t.log, app.scroll, &app.search, app.search_hit);
}

fn kv(key: &str, value: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let mut out = vec![dim(format!("{key:<7} : "))];
    let used: usize = value.iter().map(|s| s.width()).sum::<usize>() + 10;
    out.extend(value);
    out.push(Span::raw(" ".repeat(width.saturating_sub(used))));
    out
}

fn draw_card(f: &mut Frame, app: &App, t: &Target, area: Rect) {
    let block = panel(&t.name, status_chip(t.status));
    let inner = block.inner(area).inner(Margin::new(1, 0));
    f.render_widget(block, area);
    let col = (inner.width as usize / 2).max(30);

    let lines: Vec<Line> = match t.main {
        Some(u) => {
            let port = match (u.port, u.status.is_live()) {
                (Some(p), true) => vec![neon(format!("localhost:{p}"))],
                (Some(p), false) => vec![text(format!(":{p}"))],
                (None, _) => vec![dim("—")],
            };
            let mut branch = vec![neon(u.branch.clone().unwrap_or_else(|| "—".into()))];
            if let Some((g, _)) = app.git.get(&u.path) {
                if g.changed > 0 {
                    branch.push(Span::styled(format!("  {} changed", g.changed), Style::new().fg(th().busy)));
                }
            }
            let uptime = u.started.map(|s| fmt_uptime(s.elapsed())).unwrap_or_else(|| "—".into());
            let pid = u.pid.map(|p| p.to_string()).unwrap_or_else(|| "—".into());
            let health = match u.up {
                Some(true) => Span::styled("UP", Style::new().fg(th().ok)),
                Some(false) => Span::styled("DOWN", Style::new().fg(th().bad)),
                None => dim("—"),
            };
            let mut needs: Vec<Span> = Vec::new();
            for (i, &n) in u.needs.iter().enumerate() {
                if i > 0 {
                    needs.push(text("  "));
                }
                let nu = &app.units[n];
                needs.push(text(format!("{} ", nu.name)));
                needs.push(Span::styled(
                    "●",
                    Style::new().fg(if nu.status == Status::Running { th().ok } else { th().dim }),
                ));
            }
            if needs.is_empty() {
                needs.push(dim("—"));
            }
            let cmd: String = u.cmd.chars().take(col.saturating_sub(12)).collect();
            vec![
                Line::from([kv("PORT", port, col), kv("BRANCH", branch, col)].concat()),
                Line::from([kv("UPTIME", vec![text(uptime)], col), kv("PID", vec![text(pid), dim("   "), health], col)].concat()),
                Line::from([kv("CMD", vec![text(cmd)], col), kv("NEEDS", needs, col)].concat()),
            ]
        }
        None => {
            let live = t.units.iter().filter(|u| u.status.is_live()).count();
            let steps: Vec<String> = t.units.iter().map(|u| u.name.clone()).collect();
            let ports: Vec<Span> = t
                .units
                .iter()
                .filter(|u| u.status.is_live())
                .filter_map(|u| u.port)
                .map(|p| neon(format!(":{p} ")))
                .collect();
            vec![
                Line::from(kv("STEPS", vec![text(steps.join(" → "))], col * 2)),
                Line::from([kv("RUNNING", vec![neon(format!("{live}/{}", t.units.len()))], col), kv("PORTS", if ports.is_empty() { vec![dim("—")] } else { ports }, col)].concat()),
                Line::from(kv("ENTER", vec![dim("start / stop every step   R restart")], col * 2)),
            ]
        }
    };
    f.render_widget(Paragraph::new(lines), inner);
}

/// The area inside a chart panel: the plot, and one row of axis labels under it.
fn chart_areas(f: &mut Frame, area: Rect, title: &str, value: Line<'static>) -> (Rect, Rect) {
    let block = panel(title, value);
    let inner = block.inner(area).inner(Margin::new(1, 0));
    f.render_widget(block, area);
    let [plot, axis_row] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    (plot, axis_row)
}

fn no_data(f: &mut Frame, area: Rect, msg: &str) {
    let y = area.y + area.height / 2;
    let r = Rect { y, height: 1, ..area };
    f.render_widget(Paragraph::new(dim(msg.to_string())).alignment(Alignment::Center), r);
}

fn plot(f: &mut Frame, area: Rect, series: &[Option<f64>], lo: f64, hi: f64, fill: bool) {
    let lines: Vec<Line> = braille(series, area.width as usize, area.height as usize, lo, hi, fill)
        .into_iter()
        .map(|l| Line::from(neon(l)))
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_cpu(f: &mut Frame, series: &[f64], area: Rect) {
    let now = series.last().map(|v| fmt_cpu(*v as f32)).unwrap_or_else(|| "—".into());
    let (p, a) = chart_areas(f, area, "cpu", Line::from(vec![neon(now.replace(" cpu", "")), dim(" ")]));
    if series.is_empty() {
        return no_data(f, p, "starts when the server runs");
    }
    let max = series.iter().cloned().fold(0.0, f64::max).max(5.0) * 1.2;
    let shown = series.len().min(p.width as usize * 2);
    plot(f, p, &series.iter().map(|v| Some(*v)).collect::<Vec<_>>(), 0.0, max, false);
    f.render_widget(Paragraph::new(axis(&span_label(shown), &format!("max {:.0}%", max / 1.2), "now", a.width as usize)), a);
}

fn draw_mem(f: &mut Frame, series: &[f64], area: Rect) {
    let now = series.last().map(|v| fmt_mem(*v as u64)).unwrap_or_else(|| "—".into());
    let (p, a) = chart_areas(f, area, "memory", Line::from(vec![neon(now), dim(" ")]));
    if series.is_empty() {
        return no_data(f, p, "starts when the server runs");
    }
    let top = series.iter().cloned().fold(0.0, f64::max).max(1.0) * 1.25;
    let shown = series.len().min(p.width as usize * 2);
    plot(f, p, &series.iter().map(|v| Some(*v)).collect::<Vec<_>>(), 0.0, top, true);
    f.render_widget(Paragraph::new(axis(&span_label(shown), "", &fmt_mem(top as u64), a.width as usize)), a);
}

fn draw_rt(f: &mut Frame, t: &Target, area: Rect) {
    // The project (or group step) that has seen requests most recently.
    let src = t
        .units
        .iter()
        .filter(|u| u.metrics.rt.iter().any(|v| v.is_some()))
        .max_by_key(|u| u.metrics.rt.iter().rposition(|v| v.is_some()).map(|p| (p, u.metrics.rt.len())));
    let last = src.and_then(|u| u.metrics.rt.iter().rev().flatten().next().copied());
    let value = match last {
        Some(ms) => Line::from(vec![neon(format!("{ms:.0} ms")), dim(" ")]),
        None => Line::from(dim("— ")),
    };
    let (p, a) = chart_areas(f, area, "response time", value);
    let Some(u) = src else {
        let port = t.units.iter().find_map(|u| u.up.map(|up| (u.port, up)));
        let msg = match port {
            Some((Some(p), true)) => format!("no requests logged yet  ·  :{p} UP"),
            Some((Some(p), false)) => format!("no requests logged yet  ·  :{p} DOWN"),
            _ => "shows request times printed in the log".into(),
        };
        return no_data(f, p, &msg);
    };
    let rt: Vec<Option<f64>> = u.metrics.rt.iter().copied().collect();
    let vals: Vec<f64> = rt.iter().flatten().copied().collect();
    // Scale to typical requests (95th percentile) so one slow request doesn't flatten
    // the rest; a spike above that just touches the top.
    let mut sorted = vals.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let p95 = sorted.get((sorted.len() * 95 / 100).min(sorted.len().saturating_sub(1))).copied().unwrap_or(10.0);
    let max = p95.max(10.0) * 1.6;
    let avg = vals.iter().sum::<f64>() / vals.len().max(1) as f64;
    plot(f, p, &rt, 0.0, max, false);
    let mut labels = vec![dim(format!("avg {avg:.0} ms   ")), Span::styled(format!("peak {:.0} ms", u.metrics.rt_peak), Style::new().fg(th().busy))];
    if u.metrics.server_errors > 0 {
        labels.push(Span::styled(format!("   5xx {}", u.metrics.server_errors), Style::new().fg(th().bad)));
    }
    f.render_widget(Paragraph::new(Line::from(labels)), a);
}

fn draw_boot(f: &mut Frame, t: &Target, area: Rect) {
    let ready = t.chain.iter().filter(|u| u.status == Status::Running && u.up != Some(false)).count();
    let (p, _) = chart_areas(f, area, "start-up", Line::from(vec![neon(format!("{ready}/{}", t.chain.len())), dim(" ")]));
    let name_w = t.chain.iter().map(|u| u.name.chars().count()).max().unwrap_or(4).min(18);
    // The bar gives way before the words do: "stopped" must fit.
    let seg = (p.width as usize).saturating_sub(name_w + 2 + 2 + 8).clamp(4, 14);
    let mut lines: Vec<Line> = Vec::new();
    let room = (p.height as usize).saturating_sub(2);
    for u in t.chain.iter().rev().take(room).rev() {
        let (filled, word, color) = match u.status {
            Status::Running if u.port.is_none() || u.up == Some(true) => (seg, "ready", th().ok),
            Status::Running => (seg * 3 / 4, "booting", th().busy),
            Status::Starting => {
                let secs = u.started.map(|s| s.elapsed().as_secs_f32()).unwrap_or(0.0);
                ((seg as f32 * (0.15 + secs / 30.0).min(0.6)) as usize, "starting", th().busy)
            }
            Status::Stopping => (seg / 2, "stopping", th().busy),
            Status::Done => (seg, "done", th().dim),
            Status::Failed(_) => (seg, "failed", th().bad),
            Status::Idle => (0, "stopped", th().dim),
        };
        let name: String = u.name.chars().take(name_w).collect();
        lines.push(Line::from(vec![
            text(format!("{name:<name_w$}  ")),
            Span::styled("▮".repeat(filled), Style::new().fg(color)),
            dim("▯".repeat(seg - filled)),
            Span::styled(format!("  {word}"), Style::new().fg(color)),
        ]));
    }
    while lines.len() < room {
        lines.push(Line::raw(""));
    }
    // Errors per minute over the last hour, across the selection.
    let errs = sum_aligned(&t.units.iter().map(|u| &u.metrics.errors).collect::<Vec<_>>());
    // + 0.0: an empty sum is -0.0, which would print as "-0".
    let total: f64 = errs.iter().sum::<f64>() + 0.0;
    let w = (p.width as usize).saturating_sub(18);
    let bar_vals: Vec<f32> = errs.iter().map(|v| *v as f32).collect();
    let color = if total > 0.0 { th().bad } else { th().dim };
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        dim("ERRORS (1H)  "),
        Span::styled(bars(&bar_vals, w), Style::new().fg(color)),
        Span::styled(format!("  {total:.0}"), Style::new().fg(color)),
    ]));
    f.render_widget(Paragraph::new(lines), p);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn braille_draws_lines_gaps_and_fills() {
        // A rising line across 2 cells × 1 row: bottom-left to top-right.
        let up = braille(&[Some(0.0), Some(1.0), Some(2.0), Some(3.0)], 2, 1, 0.0, 3.0, false);
        assert_eq!(up.len(), 1);
        assert_eq!(up[0].chars().count(), 2);
        assert_ne!(up[0], "\u{2800}\u{2800}");
        // Fewer points than columns are drawn at the right edge (newest last).
        let short = braille(&[Some(1.0)], 3, 1, 0.0, 1.0, false);
        assert!(short[0].starts_with("\u{2800}\u{2800}"));
        // A gap draws nothing.
        assert_eq!(braille(&[None, None], 1, 1, 0.0, 1.0, false)[0], "\u{2800}");
        // Filled: everything below a full-height value is set.
        assert_eq!(braille(&[Some(1.0), Some(1.0)], 1, 1, 0.0, 1.0, true)[0], "\u{28FF}");
        // Zero size: nothing, no panic.
        assert!(braille(&[Some(1.0)], 0, 0, 0.0, 1.0, false).is_empty());
    }

    #[test]
    fn bars_and_alignment() {
        assert_eq!(bars(&[0.0, 1.0, 2.0], 3), "▁▅█");
        assert_eq!(bars(&[1.0], 3), "  █");
        assert_eq!(bars(&[], 2), "  ");
        let a: std::collections::VecDeque<f32> = [1.0, 2.0, 3.0].into();
        let b: std::collections::VecDeque<f32> = [10.0].into();
        assert_eq!(sum_aligned(&[&a, &b]), vec![1.0, 2.0, 13.0]);
        assert_eq!(span_label(150), "-5m");
        assert_eq!(span_label(20), "-40s");
    }
}
