use crate::app::{App, Mode, Row, Status, Tab};
use crate::proc::Log;
use crate::theme::th;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Padding, Paragraph, Wrap};


pub fn draw(f: &mut Frame, app: &mut App) {
    crate::theme::set(app.theme);
    paint(f, f.area());
    if crate::dash::fits(f.area()) && app.error.is_none() {
        crate::dash::draw(f, app);
        draw_overlay(f, app);
        return;
    }
    let [top, body, bottom] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(f.area());

    app.log_rows = body.height.saturating_sub(3) as usize;
    draw_top(f, app, top);
    let lw = list_width(app, body.width);
    let [left, right] =
        Layout::horizontal([Constraint::Length(lw), Constraint::Min(1)]).areas(body);
    draw_list(f, app, left);
    draw_detail(f, app, right);
    draw_bottom(f, app, bottom);
    draw_overlay(f, app);
}

/// Help, quit confirmation and the branch picker, over either layout.
fn draw_overlay(f: &mut Frame, app: &App) {
    match app.mode {
        Mode::Help => draw_help(f, app),
        Mode::ConfirmQuit => draw_confirm(f, app),
        Mode::Branches => draw_picker(f, app),
        _ => {}
    }
}

fn draw_picker(f: &mut Frame, app: &App) {
    let Some(p) = &app.picker else { return };
    let visible = p.visible();
    let area = centered(f.area(), 72, 22);
    let inner_w = area.width.saturating_sub(4) as usize;

    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(" › ", Style::new().fg(th().accent)),
        Span::raw(p.filter.clone()),
        Span::styled("█", Style::new().fg(th().accent)),
        Span::styled(format!("   {} of {}", visible.len(), p.items.len()), Style::new().fg(th().dim)),
        if p.fetching {
            Span::styled("   ↻ fetching…", Style::new().fg(th().busy))
        } else if p.fetch_error.is_some() {
            Span::styled("   offline list", Style::new().fg(th().bad))
        } else {
            Span::styled("   ✓ up to date", Style::new().fg(th().ok))
        },
    ]));
    if let Some(e) = &p.fetch_error {
        lines.push(Line::styled(format!(" fetch failed: {e}"), Style::new().fg(th().dim)));
    }
    if p.changed > 0 {
        lines.push(Line::styled(
            format!(" ⚠ {} uncommitted change{}: git keeps them, or refuses if they clash", p.changed, if p.changed == 1 { "" } else { "s" }),
            Style::new().fg(th().busy),
        ));
    }
    if let Some(e) = &p.error {
        lines.push(Line::styled(format!(" ✗ {e}"), Style::new().fg(th().bad)));
    }
    if p.busy {
        lines.push(Line::styled(" switching…", Style::new().fg(th().busy)));
    }
    lines.push(Line::raw(""));

    let room = (area.height as usize).saturating_sub(lines.len() + 3);
    let start = p.sel.saturating_sub(room.saturating_sub(1));
    let name_w = visible.iter().map(|&i| p.items[i].name.chars().count() + p.items[i].remote.as_ref().map_or(0, |r| r.len() + 1)).max().unwrap_or(0).min(34);
    for (row, &i) in visible.iter().enumerate().skip(start).take(room) {
        let b = &p.items[i];
        let selected = row == p.sel;
        let shown = match &b.remote {
            Some(r) => format!("{r}/{}", b.name),
            None => b.name.clone(),
        };
        let shown: String = if shown.chars().count() > name_w {
            shown.chars().take(name_w.saturating_sub(1)).collect::<String>() + "…"
        } else {
            shown
        };
        let when = format!("{:<15}", b.when);
        let rest = inner_w.saturating_sub(3 + name_w + 2 + when.len());
        let subject: String = b.subject.chars().take(rest).collect();
        let base = if selected { selected_style() } else { Style::new() };
        let name_style = if b.current {
            base.fg(th().ok).bold()
        } else if b.remote.is_some() {
            base.fg(th().dim)
        } else {
            base
        };
        lines.push(Line::from(vec![
            Span::styled(if b.current { " ● " } else { "   " }, base.fg(th().ok)),
            Span::styled(format!("{shown:<name_w$}  "), name_style),
            Span::styled(when, base.fg(th().dim)),
            Span::styled(format!("{subject:<rest$}"), base.fg(th().dim)),
        ]));
    }
    if visible.is_empty() {
        lines.push(Line::styled("   no branch matches", Style::new().fg(th().dim)));
    }

    f.render_widget(Clear, area);
    paint(f, area);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_style(Style::new().fg(th().dim))
                .title(Span::styled(format!(" switch branch · {} ", p.project), Style::new().fg(th().accent)))
                .title_bottom(Span::styled(" ↑↓ pick · enter switch · esc close ", Style::new().fg(th().dim))),
        ),
        area,
    );
}

/// Fills `area` with the theme's background and text colour (nothing for a
/// transparent theme, so the terminal's own background or blur shows).
fn paint(f: &mut Frame, area: Rect) {
    let t = th();
    let mut style = Style::new().fg(t.text);
    if let Some(bg) = t.background {
        style = style.bg(bg);
    }
    f.render_widget(Block::new().style(style), area);
}

/// The selected row: the theme's fill, or bold accent text when it has none.
pub(crate) fn selected_style() -> Style {
    match th().selected {
        Some(c) => Style::new().bg(c),
        None => Style::new().fg(th().accent).add_modifier(Modifier::BOLD),
    }
}

pub(crate) fn icon(s: Status) -> Span<'static> {
    match s {
        Status::Idle => Span::styled("○", Style::new().fg(th().dim)),
        Status::Starting => Span::styled("◐", Style::new().fg(th().busy)),
        Status::Running => Span::styled("●", Style::new().fg(th().ok)),
        Status::Stopping => Span::styled("◌", Style::new().fg(th().busy)),
        Status::Done => Span::styled("✓", Style::new().fg(th().dim)),
        Status::Failed(_) => Span::styled("✗", Style::new().fg(th().bad)),
    }
}

pub(crate) fn status_word(s: Status) -> Span<'static> {
    match s {
        Status::Idle => Span::styled("stopped", Style::new().fg(th().dim)),
        Status::Starting => Span::styled("starting", Style::new().fg(th().busy)),
        Status::Running => Span::styled("running", Style::new().fg(th().ok)),
        Status::Stopping => Span::styled("stopping", Style::new().fg(th().busy)),
        Status::Done => Span::styled("finished", Style::new().fg(th().dim)),
        Status::Failed(c) => Span::styled(format!("exited {c}"), Style::new().fg(th().bad)),
    }
}

fn draw_top(f: &mut Frame, app: &App, area: Rect) {
    let left = Line::from(vec![
        Span::styled(" devhub", Style::new().fg(th().accent).bold()),
        Span::styled(format!("  {} projects", app.units.iter().filter(|u| u.category.is_some()).count()), Style::new().fg(th().dim)),
    ]);
    let live = app.live_count();
    let right = if live > 0 {
        let (cpu, mem) = app
            .units
            .iter()
            .filter_map(|u| u.pid.and_then(|p| app.stats.get(&p)))
            .fold((0.0f32, 0u64), |(c, m), (uc, um)| (c + uc, m + um));
        let mut spans = vec![
            Span::styled("● ", Style::new().fg(th().ok)),
            Span::raw(format!("{live} running")),
        ];
        if mem > 0 {
            spans.push(Span::styled(format!("  {}  {}", fmt_cpu(cpu), fmt_mem(mem)), Style::new().fg(th().dim)));
        }
        spans.push(Span::raw(" "));
        Line::from(spans)
    } else {
        Line::styled("idle ", Style::new().fg(th().dim))
    };
    f.render_widget(Paragraph::new(left), area);
    f.render_widget(Paragraph::new(right).alignment(Alignment::Right), area);
}

/// Projects in a section (groups' own steps included) and how many of them run.
pub(crate) fn section_counts(app: &App, section: &str) -> (usize, usize) {
    let mut units: Vec<usize> = Vec::new();
    for (i, u) in app.units.iter().enumerate() {
        if u.category.as_deref() == Some(section) {
            units.push(i);
        }
    }
    for g in &app.groups {
        let here = match &g.section {
            Some(s) => s == section,
            None => section == crate::app::LOOSE,
        };
        if here {
            units.extend(g.steps.iter().map(|s| s.unit));
        }
    }
    units.sort_unstable();
    units.dedup();
    let live = units.iter().filter(|&&i| app.units[i].status.is_live()).count();
    (units.len(), live)
}

pub(crate) fn fmt_mem(bytes: u64) -> String {
    let mb = bytes as f64 / 1_048_576.0;
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else {
        format!("{mb:.0} MB")
    }
}

pub(crate) fn fmt_cpu(pct: f32) -> String {
    if pct < 10.0 {
        format!("{pct:.1}% cpu")
    } else {
        format!("{pct:.0}% cpu")
    }
}

fn list_width(app: &App, total: u16) -> u16 {
    let longest = app
        .units
        .iter()
        .map(|u| u.name.chars().count() + if u.group.is_some() { 4 } else { 0 })
        .chain(app.groups.iter().map(|g| g.name.chars().count()))
        .max()
        .unwrap_or(10);
    let want = (longest + 22) as u16;
    want.clamp(30, (total * 45 / 100).max(30)).min(total)
}

fn row_line(app: &App, row: &Row, width: usize) -> Line<'static> {
    let (indent, status, name, right): (&str, Status, String, Vec<Span<'static>>) = match row {
        Row::Group(gi) => {
            let g = &app.groups[*gi];
            let live = g.steps.iter().filter(|s| app.units[s.unit].status.is_live()).count();
            let right = if live > 0 {
                vec![Span::styled(format!("{live}/{}", g.steps.len()), Style::new().fg(th().dim))]
            } else {
                vec![Span::styled(format!("{} steps", g.steps.len()), Style::new().fg(th().dim))]
            };
            ("", app.group_status(*gi), g.name.clone(), right)
        }
        Row::Unit(ui) | Row::Step(_, ui) => {
            let in_group = matches!(row, Row::Step(..));
            let u = &app.units[*ui];
            let mut right = Vec::new();
            if u.status.is_live() {
                if let Some(p) = u.port {
                    right.push(Span::styled(format!(":{p}"), Style::new().fg(th().accent)));
                }
            }
            if !in_group {
                if let Some(b) = &u.branch {
                    if !right.is_empty() {
                        right.push(Span::raw(" "));
                    }
                    let b: String = if b.chars().count() > 14 {
                        b.chars().take(13).collect::<String>() + "…"
                    } else {
                        b.clone()
                    };
                    right.push(Span::styled(b, Style::new().fg(th().dim)));
                }
            }
            let indent = if in_group { "  └ " } else { "" };
            (indent, u.status, u.name.clone(), right)
        }
        Row::Header(_) => unreachable!(),
    };

    let lead = 3 + indent.chars().count(); // " ● "
    // In a narrow column the name wins: drop the branch (the last, dim part) first.
    let mut right = right;
    let fits = |r: &[Span]| lead + name.chars().count() + r.iter().map(|s| s.width()).sum::<usize>() + 2 <= width;
    if !fits(&right) && right.len() > 1 {
        right.truncate(1);
    } else if !fits(&right) && right.len() == 1 && right[0].style.fg == Some(th().dim) {
        right.clear();
    }
    let right_w: usize = right.iter().map(|s| s.width()).sum();
    let room = width.saturating_sub(lead + right_w + 2);
    let name = if name.chars().count() > room {
        name.chars().take(room.saturating_sub(1)).collect::<String>() + "…"
    } else {
        name
    };
    let pad = width.saturating_sub(lead + name.chars().count() + right_w + 1);
    let name_style = if status.is_live() { Style::new().bold() } else { Style::new() };

    let mut spans = vec![
        Span::raw(" "),
        Span::styled(indent.to_string(), Style::new().fg(th().dim)),
        icon(status),
        Span::raw(" "),
        Span::styled(name, name_style),
        Span::raw(" ".repeat(pad)),
    ];
    spans.extend(right);
    Line::from(spans)
}

pub(crate) fn draw_list(f: &mut Frame, app: &mut App, area: Rect) {
    if app.rows.is_empty() {
        let msg = if app.filter.is_empty() {
            " No projects yet.\n Press , to edit the config."
        } else {
            " Nothing matches the filter."
        };
        f.render_widget(Paragraph::new(msg).style(Style::new().fg(th().dim)), area);
        return;
    }
    let width = area.width as usize;
    let items: Vec<ListItem> = app
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| match row {
            Row::Header(t) => {
                let mut lines = Vec::new();
                if i > 0 {
                    lines.push(Line::raw(""));
                }
                let shut = app.collapsed.contains(t) && app.filter.is_empty();
                let (total, live) = section_counts(app, t);
                let mut spans = vec![
                    Span::styled(if shut { " ▸ " } else { " ▾ " }, Style::new().fg(th().dim)),
                    Span::styled(t.to_lowercase(), Style::new().fg(if shut { th().text } else { th().dim })),
                ];
                let right = if live > 0 {
                    Span::styled(format!("● {live}"), Style::new().fg(th().ok))
                } else if shut {
                    Span::styled(format!("{total}"), Style::new().fg(th().dim))
                } else {
                    Span::raw("")
                };
                let used: usize = spans.iter().map(|s| s.width()).sum::<usize>() + right.width();
                spans.push(Span::raw(" ".repeat(width.saturating_sub(used + 1))));
                spans.push(right);
                lines.push(Line::from(spans));
                ListItem::new(lines)
            }
            _ => ListItem::new(row_line(app, row, width)),
        })
        .collect();

    // Never leave blank rows below the list while rows above are scrolled away
    // (after a reload, a filter, or a taller window).
    let heights: Vec<usize> = items.iter().map(|i| i.height()).collect();
    let mut fill = 0;
    let mut max_offset = heights.len();
    for (i, h) in heights.iter().enumerate().rev() {
        fill += h;
        if fill > area.height as usize {
            break;
        }
        max_offset = i;
    }
    if app.list_state.offset() > max_offset {
        *app.list_state.offset_mut() = max_offset;
    }

    let list = List::new(items).highlight_style(selected_style());
    app.list_state.select(Some(app.selected));
    f.render_stateful_widget(list, area, &mut app.list_state);
}

fn draw_detail(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::new()
        .borders(Borders::LEFT)
        .border_style(Style::new().fg(th().dim))
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if let Some(err) = &app.error {
        let text = vec![
            Line::styled("Config error", Style::new().fg(th().bad).bold()),
            Line::raw(""),
            Line::raw(err.clone()),
            Line::raw(""),
            Line::styled("Press , to edit the config, then R to reload.", Style::new().fg(th().dim)),
        ];
        f.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), inner);
        return;
    }

    let [head, sub, _, body] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(inner);

    let sep = || Span::styled("  ·  ", Style::new().fg(th().dim));
    let (title, details, log): (Line, Line, Log) = match app.current() {
        Some(Row::Unit(ui) | Row::Step(_, ui)) => {
            let u = &app.units[*ui];
            let mut t = vec![Span::styled(u.name.clone(), Style::new().bold()), sep(), status_word(u.status)];
            if let (true, Some(p)) = (u.status.is_live(), u.port) {
                t.push(sep());
                t.push(Span::styled(format!("localhost:{p}"), Style::new().fg(th().accent)));
            }
            if let Some((cpu, mem)) = u.pid.and_then(|p| app.stats.get(&p)) {
                t.push(sep());
                t.push(Span::styled(format!("{}  {}", fmt_cpu(*cpu), fmt_mem(*mem)), Style::new().fg(th().dim)));
            }
            let mut d = vec![Span::styled(u.cmd.clone(), Style::new().fg(th().dim))];
            d.push(sep());
            d.push(Span::styled(u.path.display().to_string(), Style::new().fg(th().dim)));
            if !u.needs.is_empty() {
                d.push(sep());
                d.push(Span::styled("needs ", Style::new().fg(th().dim)));
                for (i, &n) in u.needs.iter().enumerate() {
                    if i > 0 {
                        d.push(Span::styled(", ", Style::new().fg(th().dim)));
                    }
                    let nu = &app.units[n];
                    let color = if nu.status == Status::Running { th().ok } else { th().dim };
                    d.push(Span::styled(nu.name.clone(), Style::new().fg(color)));
                }
            }
            (Line::from(t), Line::from(d), u.log.clone())
        }
        Some(Row::Group(gi)) => {
            let g = &app.groups[*gi];
            let t = vec![
                Span::styled(g.name.clone(), Style::new().bold()),
                sep(),
                status_word(app.group_status(*gi)),
            ];
            let names: Vec<String> = g.steps.iter().map(|s| app.units[s.unit].name.clone()).collect();
            let d = vec![Span::styled(names.join(" → "), Style::new().fg(th().dim))];
            (Line::from(t), Line::from(d), g.log.clone())
        }
        _ => {
            draw_section(f, app, inner);
            return;
        }
    };

    let mut title = title;
    if app.scroll > 0 && app.tab == Tab::Log {
        title.spans.push(sep());
        title.spans.push(Span::styled(format!("↑ {} lines  f follow", app.scroll), Style::new().fg(th().busy)));
    }
    f.render_widget(Paragraph::new(title), head);
    f.render_widget(Paragraph::new(details), sub);

    // log | git switch, top right.
    let tab = |name: &'static str, on: bool| {
        if on {
            Span::styled(name, Style::new().fg(th().accent).bold())
        } else {
            Span::styled(name, Style::new().fg(th().dim))
        }
    };
    let tabs = Line::from(vec![
        tab("log", app.tab == Tab::Log),
        Span::styled(" │ ", Style::new().fg(th().dim)),
        tab("git", app.tab == Tab::Git),
    ]);
    f.render_widget(Paragraph::new(tabs).alignment(Alignment::Right), head);

    match app.tab {
        Tab::Log => draw_log(f, body, &log, app.scroll, &app.search, app.search_hit),
        Tab::Git => draw_git(f, app, body),
    }
}

/// A section header is selected: what it holds, and what runs there.
pub(crate) fn draw_section(f: &mut Frame, app: &App, area: Rect) {
    let Some((_, name)) = app.section_of_selected() else {
        f.render_widget(Paragraph::new("Select a project.").style(Style::new().fg(th().dim)), area);
        return;
    };
    let (total, live) = section_counts(app, &name);
    let shut = app.collapsed.contains(&name);
    let mut lines = vec![
        Line::from(vec![
            Span::styled(name.to_lowercase(), Style::new().bold()),
            Span::styled("  ·  ", Style::new().fg(th().dim)),
            Span::styled(format!("{total} projects"), Style::new().fg(th().dim)),
            Span::styled("  ·  ", Style::new().fg(th().dim)),
            if live > 0 {
                Span::styled(format!("{live} running"), Style::new().fg(th().ok))
            } else {
                Span::styled("nothing running", Style::new().fg(th().dim))
            },
        ]),
        Line::styled(
            if shut { "enter or → to unfold" } else { "enter or ← to fold" },
            Style::new().fg(th().dim),
        ),
        Line::raw(""),
    ];
    let running: Vec<&crate::app::Unit> = app
        .units
        .iter()
        .filter(|u| u.status.is_live())
        .filter(|u| {
            u.category.as_deref() == Some(name.as_str())
                || app.groups.iter().any(|g| {
                    g.section.as_deref() == Some(name.as_str())
                        && g.steps.iter().any(|s| std::ptr::eq(&app.units[s.unit], *u))
                })
        })
        .collect();
    for u in running {
        let mut spans = vec![icon(u.status), Span::raw(format!(" {}", u.name))];
        if let Some(p) = u.port {
            spans.push(Span::styled(format!("  :{p}"), Style::new().fg(th().accent)));
        }
        if let Some((cpu, mem)) = u.pid.and_then(|p| app.stats.get(&p)) {
            spans.push(Span::styled(format!("  {}  {}", fmt_cpu(*cpu), fmt_mem(*mem)), Style::new().fg(th().dim)));
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), area);
}

pub(crate) fn draw_git(f: &mut Frame, app: &App, area: Rect) {
    let path = match app.current() {
        Some(Row::Group(gi)) => app.groups[*gi].steps.first().map(|s| app.units[s.unit].path.clone()),
        Some(row) => row.unit().map(|ui| app.units[ui].path.clone()),
        None => None,
    };
    let Some(path) = path else { return };
    let Some((info, _)) = app.git.get(&path) else {
        f.render_widget(Paragraph::new("Reading git…").style(Style::new().fg(th().dim)), area);
        return;
    };
    if info.branch.is_empty() {
        f.render_widget(Paragraph::new("Not a git repository.").style(Style::new().fg(th().dim)), area);
        return;
    }

    let w = area.width as usize;
    let clip = |s: &str, n: usize| -> String {
        if s.chars().count() > n {
            s.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
        } else {
            s.to_string()
        }
    };
    let mut lines = vec![Line::from(vec![
        Span::styled("on ", Style::new().fg(th().dim)),
        Span::styled(info.branch.clone(), Style::new().fg(th().accent).bold()),
        Span::styled("  ·  ", Style::new().fg(th().dim)),
        if info.changed > 0 {
            Span::styled(format!("{} changed", info.changed), Style::new().fg(th().busy))
        } else {
            Span::styled("clean", Style::new().fg(th().ok))
        },
    ])];

    lines.push(Line::raw(""));
    lines.push(Line::styled("branches · newest first · b to switch", Style::new().fg(th().dim)));
    let name_w = info.branches.iter().map(|b| b.0.chars().count()).max().unwrap_or(0).min(28);
    for (name, when, subject) in info.branches.iter().take(8) {
        let current = *name == info.branch;
        let when = format!("{when:<14}");
        let room = w.saturating_sub(name_w + 2 + when.len() + 2);
        lines.push(Line::from(vec![
            Span::styled(if current { "● " } else { "  " }, Style::new().fg(th().ok)),
            Span::styled(
                format!("{:<name_w$}  ", clip(name, name_w)),
                if current { Style::new().bold() } else { Style::new() },
            ),
            Span::styled(when, Style::new().fg(th().dim)),
            Span::styled(clip(subject, room), Style::new().fg(th().dim)),
        ]));
    }

    lines.push(Line::raw(""));
    lines.push(Line::styled("commits", Style::new().fg(th().dim)));
    for (hash, when, author, subject) in &info.commits {
        let meta = format!("{when} · {author}");
        let room = w.saturating_sub(hash.len() + 1 + meta.chars().count() + 2);
        lines.push(Line::from(vec![
            Span::styled(format!("{hash} "), Style::new().fg(th().busy)),
            Span::raw(format!("{}  ", clip(subject, room))),
            Span::styled(meta, Style::new().fg(th().dim)),
        ]));
    }
    f.render_widget(Paragraph::new(lines), area);
}

pub(crate) fn line_style(l: &str) -> Style {
    let lower = l.to_lowercase();
    if l.starts_with("$ ") || l.contains("] $ ") {
        Style::new().fg(th().dim).italic()
    } else if l.starts_with("──") || l.contains("] ──") {
        Style::new().fg(th().dim)
    } else if lower.contains("error") || lower.contains("err!") || lower.contains("failed") || lower.contains("exception") {
        Style::new().fg(th().bad)
    } else if lower.contains("warn") {
        Style::new().fg(th().busy)
    } else {
        Style::new()
    }
}

/// Draws the end of the log, wrapped by characters so the newest line is always visible.
pub(crate) fn draw_log(f: &mut Frame, area: Rect, log: &Log, scroll: usize, search: &str, hit: Option<usize>) {
    let q = search.to_lowercase();
    let Ok(buf) = log.lock() else { return };
    if buf.lines.is_empty() {
        f.render_widget(
            Paragraph::new("No output yet. Press enter to start.").style(Style::new().fg(th().dim)),
            area,
        );
        return;
    }
    let w = (area.width as usize).max(1);
    let h = area.height as usize;
    let end = buf.lines.len().saturating_sub(scroll).max(1.min(buf.lines.len()));

    let mut rows: Vec<Line> = Vec::new();
    let mut i = end;
    while i > 0 && rows.len() < h {
        i -= 1;
        let line = &buf.lines[i];
        let mut style = line_style(line);
        if Some(i) == hit {
            style = Style::new().fg(Color::Black).bg(th().busy);
        } else if !q.is_empty() && line.to_lowercase().contains(&q) {
            style = match th().matched {
                Some(c) => style.bg(c),
                None => style.add_modifier(Modifier::UNDERLINED),
            };
        }
        let chars: Vec<char> = line.chars().collect();
        let mut chunks: Vec<Line> = if chars.is_empty() {
            vec![Line::raw("")]
        } else {
            chars.chunks(w).map(|c| Line::styled(c.iter().collect::<String>(), style)).collect()
        };
        chunks.extend(rows);
        rows = chunks;
    }
    let skip = rows.len().saturating_sub(h);
    let visible: Vec<Line> = rows.into_iter().skip(skip).collect();
    f.render_widget(Paragraph::new(visible), area);
}

pub(crate) fn draw_bottom(f: &mut Frame, app: &App, area: Rect) {
    let key = |k: &'static str| Span::styled(k, Style::new().fg(th().accent));
    let txt = |t: &'static str| Span::styled(t, Style::new().fg(th().dim));

    let left = if app.mode == Mode::Search {
        let count = app.current_log().and_then(|l| {
            let q = app.search.to_lowercase();
            l.lock().ok().map(|b| if q.is_empty() { 0 } else { b.lines.iter().filter(|x| x.to_lowercase().contains(&q)).count() })
        }).unwrap_or(0);
        Line::from(vec![
            Span::styled(" find ", Style::new().fg(th().accent)),
            Span::raw(app.search.clone()),
            Span::styled("█", Style::new().fg(th().accent)),
            txt("   "),
            Span::styled(format!("{count} matches"), Style::new().fg(if count > 0 { th().ok } else { th().dim })),
            txt("   ↑↓ older/newer · enter keep · esc clear"),
        ])
    } else if app.mode == Mode::Filter {
        Line::from(vec![
            Span::styled(" / ", Style::new().fg(th().accent)),
            Span::raw(app.filter.clone()),
            Span::styled("█", Style::new().fg(th().accent)),
            txt("   enter keep · esc clear"),
        ])
    } else {
        let mut spans = vec![
            Span::raw(" "),
            key("⏎"), txt(" start/stop  "),
            key("r"), txt(" restart  "),
            key("o"), txt(" browser  "),
            key("v"), txt(" code  "),
            key("/"), txt(" filter  "),
            key("?"), txt(" more  "),
            key("q"), txt(" quit"),
        ];
        if !app.filter.is_empty() {
            spans.push(Span::styled(format!("   filter: {}", app.filter), Style::new().fg(th().busy)));
        }
        if !app.search.is_empty() {
            spans.push(Span::styled(format!("   find: {}  n/N", app.search), Style::new().fg(th().busy)));
        }
        Line::from(spans)
    };
    f.render_widget(Paragraph::new(left), area);

    if let Some((msg, _)) = &app.toast {
        let style = if msg.starts_with('✗') { Style::new().fg(th().bad) } else { Style::new().fg(th().ok) };
        let right = Line::styled(format!("{msg} "), style);
        let w = (right.width() as u16).min(area.width);
        let r = Rect { x: area.right() - w, width: w, ..area };
        f.render_widget(Clear, r);
        paint(f, r);
        f.render_widget(Paragraph::new(right), r);
    }
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn draw_help(f: &mut Frame, app: &App) {
    let mut rows: Vec<(String, &str)> = vec![
        ("↑↓ j k".into(), "move"),
        ("g G".into(), "first / last"),
        ("← → h l".into(), "fold / unfold a section"),
        ("enter space".into(), "start or stop"),
        ("r".into(), "restart"),
        ("s".into(), "stop"),
        ("S".into(), "stop everything"),
        ("o".into(), "open localhost:port in the browser"),
        ("tab".into(), "switch log / git"),
        ("pgup pgdn".into(), "scroll the log"),
        ("ctrl+f".into(), "find in the log"),
        ("n N".into(), "older / newer match"),
        ("f".into(), "follow the log again"),
        ("c".into(), "clear the log"),
        ("L".into(), "open the log in the editor"),
        ("b".into(), "switch git branch"),
        ("/".into(), "filter by name"),
        (",".into(), "edit the config"),
        ("R".into(), "reload the config"),
        ("q".into(), "quit: stop servers, or leave them running"),
    ];
    let open: Vec<(String, String)> = app
        .open
        .iter()
        .map(|(k, cmd)| (k.to_string(), format!("open with {}", cmd.split_whitespace().next().unwrap_or(cmd))))
        .collect();

    let mut lines = vec![Line::raw("")];
    for (k, d) in rows.drain(..) {
        lines.push(Line::from(vec![
            Span::styled(format!("  {k:<13}"), Style::new().fg(th().accent)),
            Span::raw(d.to_string()),
        ]));
    }
    for (k, d) in open {
        lines.push(Line::from(vec![
            Span::styled(format!("  {k:<13}"), Style::new().fg(th().accent)),
            Span::raw(d),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(format!("  config: {}", app.cfg_path.display()), Style::new().fg(th().dim)));

    let area = centered(f.area(), 64, lines.len() as u16 + 2);
    f.render_widget(Clear, area);
    paint(f, area);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_style(Style::new().fg(th().dim))
                .title(Span::styled(" keys ", Style::new().fg(th().accent))),
        ),
        area,
    );
}

fn draw_confirm(f: &mut Frame, app: &App) {
    let n = app.live_count() + app.orphans.len();
    let key = |k: &'static str| Span::styled(k, Style::new().fg(th().accent));
    let txt = |t: &'static str| Span::styled(t, Style::new().fg(th().dim));
    let lines = vec![
        Line::raw(""),
        Line::raw(format!("  {n} server{} running", if n == 1 { "" } else { "s" })),
        Line::raw(""),
        Line::from(vec![key("  s"), txt(" stop all and quit")]),
        Line::from(vec![key("  l"), txt(" leave running and quit")]),
        Line::from(vec![key("  esc"), txt(" cancel")]),
    ];
    let area = centered(f.area(), 40, 8);
    f.render_widget(Clear, area);
    paint(f, area);
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().border_style(Style::new().fg(th().dim))),
        area,
    );
}
