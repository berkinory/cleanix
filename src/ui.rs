use crate::{
    cleanup,
    config::Config,
    filesystem,
    model::{Report, safe_text, selection_bytes, size},
    scan::{self, Event as ScanEvent},
};
use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseEventKind,
    },
    execute,
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use std::{
    collections::HashSet,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Receiver,
    },
    time::{Duration, Instant},
};
const INK: Color = Color::Rgb(221, 224, 216);
const MUTED: Color = Color::Rgb(127, 137, 133);
const FAINT: Color = Color::Rgb(65, 76, 73);
const ACCENT: Color = Color::Rgb(165, 196, 169);
const ACTIVE: Color = Color::Rgb(32, 43, 39);
const WARN: Color = Color::Rgb(213, 184, 132);

#[derive(Clone, Debug)]
pub struct Row {
    pub id: String,
    pub title: String,
    pub children: Vec<usize>,
    pub leaf: bool,
    pub depth: u16,
}
#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Browse,
    Search,
    Confirm,
    Help,
    Coverage,
}
pub struct App {
    pub report: Report,
    pub selected: HashSet<String>,
    pub expanded: HashSet<String>,
    pub query: String,
    pub mode: Mode,
    pub cursor: usize,
    pub state: ListState,
    pub scroll: u16,
    pub status: String,
    pub busy: bool,
    pub free: Option<u64>,
    pub capacity: Option<u64>,
    failures: usize,
    pub config: Config,
    rx: Option<Receiver<ScanEvent>>,
    cancel: Option<Arc<AtomicBool>>,
    clean_rx: Option<Receiver<ScanEvent>>,
    pub list_area: Rect,
    started: Instant,
}
impl App {
    pub fn new(config: Config) -> Self {
        let (r, c) = scan::start(config.clone());
        let (rx, cancel) = (Some(r), Some(c));
        let report = Report {
            roots: config.roots.clone(),
            dry_run: config.dry,
            ..Report::default()
        };
        let free = filesystem::mounted_free(&config.home);
        Self {
            report,
            selected: HashSet::new(),
            expanded: HashSet::new(),
            query: String::new(),
            mode: Mode::Browse,
            cursor: 0,
            state: ListState::default(),
            scroll: 0,
            status: String::new(),
            busy: false,
            free,
            capacity: filesystem::mounted_capacity(&config.home),
            failures: 0,
            config,
            rx,
            cancel,
            clean_rx: None,
            list_area: Rect::default(),
            started: Instant::now(),
        }
    }
    pub fn rows(&self) -> Vec<Row> {
        let mut groups: Vec<(String, Vec<usize>)> = vec![];
        for (index, item) in self
            .report
            .items
            .iter()
            .enumerate()
            .filter(|(_, i)| i.matches(&self.query))
        {
            if let Some((_, ids)) = groups.iter_mut().find(|(c, _)| c == &item.category) {
                ids.push(index)
            } else {
                groups.push((item.category.clone(), vec![index]))
            }
        }
        groups.sort_by(|a, b| {
            let bytes = |g: &(String, Vec<usize>)| {
                g.1.iter()
                    .filter_map(|&i| self.report.items[i].bytes)
                    .sum::<u64>()
            };
            bytes(b).cmp(&bytes(a)).then(a.0.cmp(&b.0))
        });
        let mut rows = vec![];
        for (category, mut ids) in groups {
            ids.sort_by(|&a, &b| {
                self.report.items[b]
                    .bytes
                    .cmp(&self.report.items[a].bytes)
                    .then(self.report.items[a].name.cmp(&self.report.items[b].name))
            });
            if category == "App caches" {
                for &i in &ids {
                    let item = &self.report.items[i];
                    rows.push(Row {
                        id: item.id.clone(),
                        title: item.name.clone(),
                        children: vec![i],
                        leaf: true,
                        depth: 0,
                    });
                }
                continue;
            }
            let id = format!("category:{category}");
            let open = self.expanded.contains(&id) || !self.query.is_empty();
            rows.push(Row {
                id: id.clone(),
                title: category,
                children: ids.clone(),
                leaf: false,
                depth: 0,
            });
            if open {
                for i in ids {
                    let item = &self.report.items[i];
                    rows.push(Row {
                        id: item.id.clone(),
                        title: item.name.clone(),
                        children: vec![i],
                        leaf: true,
                        depth: 1,
                    });
                }
            }
        }
        rows
    }
    pub fn toggle(&mut self, row: &Row) {
        let all = row
            .children
            .iter()
            .all(|&i| self.selected.contains(&self.report.items[i].id));
        for &i in &row.children {
            if !self.report.items[i].action.permanent() {
                continue;
            }
            let id = self.report.items[i].id.clone();
            if all {
                self.selected.remove(&id);
            } else {
                self.selected.insert(id);
            }
        }
    }
    pub fn drain(&mut self) -> bool {
        let mut events = vec![];
        if let Some(rx) = &self.rx {
            events.extend(rx.try_iter());
        }
        if let Some(rx) = &self.clean_rx {
            events.extend(rx.try_iter());
        }
        let dirty = !events.is_empty();
        let focus = self.rows().get(self.cursor).map(|r| r.id.clone());
        for e in events {
            match e {
                ScanEvent::Item(i) => self.report.items.push(*i),
                ScanEvent::Note(n) => self.report.notes.push(n),
                ScanEvent::AccessWarning(warning) => {
                    self.report.notes.push(warning.clone());
                    self.report.access_warning = Some(warning);
                }
                ScanEvent::Found(_) => {}
                ScanEvent::Complete(t) => {
                    self.report.complete = true;
                    self.report.elapsed_seconds = t;
                    self.rx = None;
                }
                ScanEvent::Cleaned(id, result) => match result {
                    Ok(()) => {
                        if !self.config.dry {
                            self.selected.remove(&id);
                            self.report.items.retain(|i| i.id != id);
                        }
                    }
                    Err(e) => {
                        self.failures += 1;
                        self.report.notes.push(format!("{id}: {e}"));
                    }
                },
                ScanEvent::CleanupDone => {
                    self.busy = false;
                    self.clean_rx = None;
                    self.free = filesystem::mounted_free(&self.config.home);
                    self.status = if self.config.dry {
                        "Dry run finished. Nothing deleted.".into()
                    } else {
                        "Finished. r to refresh.".into()
                    };
                    if self.failures > 0 {
                        self.status = format!(
                            "{} failed or partially completed. i for details; r to refresh.",
                            self.failures
                        );
                    }
                }
            }
        }
        if let Some(id) = focus
            && let Some(index) = self.rows().iter().position(|r| r.id == id)
        {
            self.cursor = index
        }
        dirty
    }
    pub fn key(&mut self, key: event::KeyEvent) -> bool {
        if self.busy {
            return false;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return true;
        }
        if self.mode == Mode::Search {
            match key.code {
                KeyCode::Esc => {
                    self.query.clear();
                    self.mode = Mode::Browse
                }
                KeyCode::Enter => self.mode = Mode::Browse,
                KeyCode::Backspace => {
                    self.query.pop();
                }
                KeyCode::Char(c) => self.query.push(c),
                _ => {}
            }
            self.cursor = 0;
            return false;
        }
        if self.mode == Mode::Help || self.mode == Mode::Coverage {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') | KeyCode::Char('i') => {
                    self.mode = Mode::Browse
                }
                KeyCode::Down | KeyCode::Char('j') => self.scroll = self.scroll.saturating_add(1),
                KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::PageDown => self.scroll = self.scroll.saturating_add(10),
                KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
                _ => {}
            }
            return false;
        }
        if self.mode == Mode::Confirm {
            match key.code {
                KeyCode::Esc => self.mode = Mode::Browse,
                KeyCode::Enter => {
                    self.mode = Mode::Browse;
                    let items = self
                        .report
                        .items
                        .iter()
                        .filter(|i| self.selected.contains(&i.id))
                        .cloned()
                        .collect();
                    self.clean_rx = Some(cleanup::start(items, self.config.dry));
                    self.busy = true;
                    self.failures = 0;
                    self.status = if self.config.dry {
                        "Checking selection…".into()
                    } else {
                        "Deleting…".into()
                    };
                }
                KeyCode::Down | KeyCode::PageDown => self.scroll = self.scroll.saturating_add(5),
                KeyCode::Up | KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(5),
                _ => {}
            }
            return false;
        }
        let rows = self.rows();
        self.cursor = self.cursor.min(rows.len().saturating_sub(1));
        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor = (self.cursor + 1).min(rows.len().saturating_sub(1))
            }
            KeyCode::Up | KeyCode::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = rows.len().saturating_sub(1),
            KeyCode::PageDown => self.cursor = (self.cursor + 10).min(rows.len().saturating_sub(1)),
            KeyCode::PageUp => self.cursor = self.cursor.saturating_sub(10),
            KeyCode::Char(' ') => {
                if let Some(row) = rows.get(self.cursor) {
                    self.toggle(row)
                }
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                if let Some(row) = rows.get(self.cursor) {
                    if row.leaf {
                        self.toggle(row)
                    } else if !self.expanded.remove(&row.id) {
                        self.expanded.insert(row.id.clone());
                    }
                }
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if let Some(row) = rows.get(self.cursor) {
                    if row.depth == 1 {
                        if let Some(parent) = rows[..self.cursor].iter().rposition(|r| r.depth == 0)
                        {
                            self.expanded.remove(&rows[parent].id);
                            self.cursor = parent;
                        }
                    } else {
                        self.expanded.remove(&row.id);
                    }
                }
            }
            KeyCode::Char('/') => self.mode = Mode::Search,
            KeyCode::Esc => self.query.clear(),
            KeyCode::Char('u') => self.selected.clear(),
            KeyCode::Char('d') | KeyCode::Delete => {
                if !self.report.complete {
                    self.status = "Wait for the scan to finish.".into()
                } else if !self.selected.is_empty() {
                    self.mode = Mode::Confirm;
                    self.scroll = 0;
                }
            }
            KeyCode::Char('?') => {
                self.mode = Mode::Help;
                self.scroll = 0
            }
            KeyCode::Char('i') => {
                self.mode = Mode::Coverage;
                self.scroll = 0
            }
            KeyCode::Char('r') if self.report.complete => {
                if let Some(c) = &self.cancel {
                    c.store(true, Ordering::Relaxed)
                }
                let (rx, cancel) = scan::start(self.config.clone());
                self.rx = Some(rx);
                self.cancel = Some(cancel);
                self.report = Report {
                    dry_run: self.config.dry,
                    roots: self.config.roots.clone(),
                    ..Report::default()
                };
                self.selected.clear();
                self.status.clear();
                self.started = Instant::now();
            }
            _ => {}
        }
        false
    }
}
impl Drop for App {
    fn drop(&mut self) {
        if let Some(c) = &self.cancel {
            c.store(true, Ordering::Relaxed)
        }
    }
}
fn text(s: impl Into<String>, color: Color) -> Span<'static> {
    Span::styled(s.into(), Style::default().fg(color))
}
fn label_bytes(app: &App, row: &Row) -> String {
    let n = row
        .children
        .iter()
        .filter_map(|&i| app.report.items[i].bytes)
        .sum();
    if row
        .children
        .iter()
        .any(|&i| app.report.items[i].bytes.is_none())
    {
        format!("{} + ?", size(n))
    } else {
        size(n)
    }
}
pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    f.render_widget(Block::default().style(Style::default().fg(INK)), area);
    if area.width < 64 || area.height < 18 {
        f.render_widget(
            Paragraph::new("cleanix\n\nA little more room, please.\nMinimum 64 × 18. q to quit.")
                .style(Style::default().fg(ACCENT)),
            area.inner(Margin::new(2, 1)),
        );
        return;
    }
    let page = area.inner(Margin::new(3, 1));
    let sections = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(3),
        Constraint::Length(2),
        Constraint::Min(4),
        Constraint::Length(2),
        Constraint::Length(1),
    ])
    .split(page);
    let header =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(25)]).split(sections[0]);
    f.render_widget(
        Paragraph::new(Span::styled("cleanix", Style::default().fg(ACCENT).bold())),
        header[0],
    );
    let timing = if app.busy {
        "working".into()
    } else if app.report.complete {
        format!("{:.1}s scan", app.report.elapsed_seconds)
    } else {
        "scanning…".into()
    };
    f.render_widget(
        Paragraph::new(timing)
            .alignment(Alignment::Right)
            .style(Style::default().fg(MUTED)),
        header[1],
    );
    if app.report.access_warning.is_some() {
        f.render_widget(
            Paragraph::new("Limited access · Full Disk Access needed · i for details")
                .style(Style::default().fg(WARN)),
            Rect::new(sections[0].x, sections[0].y + 1, sections[0].width, 1),
        );
    }
    let storage = match (app.free, app.capacity) {
        (Some(free), Some(total)) => format!("{} available / {} total", size(free), size(total)),
        _ => "Storage unavailable".into(),
    };
    f.render_widget(
        Paragraph::new(vec![
            Line::from(text(storage, MUTED)),
            Line::from(vec![
                Span::styled(size(app.report.bytes()), Style::default().fg(INK).bold()),
                text(" can be cleaned", MUTED),
            ]),
        ]),
        sections[1],
    );
    let browse_label = if app.mode == Mode::Search {
        format!("/ {}▏", safe_text(&app.query))
    } else if !app.query.is_empty() {
        format!("/ {}", safe_text(&app.query))
    } else {
        if app.config.dry {
            "DRY".into()
        } else {
            String::new()
        }
    };
    f.render_widget(
        Paragraph::new(browse_label)
            .style(Style::default().fg(MUTED))
            .block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(FAINT)),
            ),
        sections[2],
    );
    let wide = area.width >= 104;
    let panes = Layout::horizontal(if wide {
        vec![Constraint::Percentage(61), Constraint::Percentage(39)]
    } else {
        vec![Constraint::Percentage(100)]
    })
    .split(sections[3]);
    let list = if wide {
        Rect {
            width: panes[0].width.saturating_sub(3),
            ..panes[0]
        }
    } else {
        panes[0]
    };
    app.list_area = list;
    let rows = app.rows();
    app.cursor = app.cursor.min(rows.len().saturating_sub(1));
    let entries: Vec<ListItem> = rows
        .iter()
        .map(|row| {
            let count = row
                .children
                .iter()
                .filter(|&&i| app.selected.contains(&app.report.items[i].id))
                .count();
            let mark = if count == row.children.len() {
                "●"
            } else if count > 0 {
                "◐"
            } else {
                "○"
            };
            let arrow = if row.leaf {
                " "
            } else if app.expanded.contains(&row.id) || !app.query.is_empty() {
                "⌄"
            } else {
                "›"
            };
            let title = format!(
                "{}{} {}",
                if row.depth == 1 { "   " } else { "" },
                arrow,
                safe_text(&row.title)
            );
            let bytes = label_bytes(app, row);
            let width = list.width.saturating_sub(7 + bytes.len() as u16) as usize;
            let title = truncate(&title, width);
            let gap = width.saturating_sub(unicode_width(&title));
            ListItem::new(Line::from(vec![
                text(format!(" {mark} "), if count > 0 { ACCENT } else { FAINT }),
                text(title, if row.depth == 0 { INK } else { MUTED }),
                text(" ".repeat(gap + 1), INK),
                text(bytes, if count > 0 { ACCENT } else { MUTED }),
            ]))
        })
        .collect();
    app.state.select(if rows.is_empty() {
        None
    } else {
        Some(app.cursor)
    });
    f.render_stateful_widget(
        List::new(entries)
            .highlight_style(Style::default().bg(ACTIVE).fg(INK))
            .highlight_symbol(""),
        list,
        &mut app.state,
    );
    if rows.is_empty() {
        f.render_widget(
            Paragraph::new(if app.report.complete {
                "Nothing here.\nTry another search or inspect scan coverage with i."
            } else {
                "Finding your tools and projects…\nYou can browse results as they arrive."
            })
            .style(Style::default().fg(MUTED)),
            list.inner(Margin::new(1, 2)),
        );
    }
    if wide {
        let panel = panes[1];
        f.render_widget(
            Block::default()
                .borders(Borders::LEFT)
                .border_style(Style::default().fg(FAINT)),
            panel,
        );
        let detail = panel.inner(Margin::new(3, 1));
        let mut lines = vec![];
        if let Some(row) = rows.get(app.cursor) {
            lines.push(Line::from(text(safe_text(&row.title), ACCENT)));
            lines.push(Line::default());
            if row.leaf {
                let item = &app.report.items[row.children[0]];
                lines.push(Line::from(text(safe_text(&item.detail), INK)));
                lines.push(Line::default());
                let paths = item.action.paths();
                if !paths.is_empty() {
                    lines.push(Line::from(text(
                        if paths.len() == 1 {
                            "LOCATION".into()
                        } else {
                            format!("{} LOCATIONS", paths.len())
                        },
                        MUTED,
                    )));
                    for path in paths.iter().take(3) {
                        lines.push(Line::from(text(
                            path.to_string_lossy().replace(
                                app.config
                                    .home
                                    .to_str()
                                    .filter(|s| !s.is_empty())
                                    .unwrap_or("/nonexistent-home"),
                                "~",
                            ),
                            MUTED,
                        )));
                    }
                    if paths.len() > 3 {
                        lines.push(Line::from(text("More locations in this group.", MUTED)));
                    }
                }
            } else {
                lines.push(Line::from(text(category_description(&row.title), INK)));
            }
        }
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), detail);
    }
    let hidden = app
        .report
        .items
        .iter()
        .filter(|i| app.selected.contains(&i.id) && !i.matches(&app.query))
        .count();
    let footer = if app.status.is_empty() && app.selected.is_empty() {
        String::new()
    } else if app.status.is_empty() {
        format!(
            "{} selected · {}{}",
            size(selection_bytes(&app.report.items, &app.selected)),
            app.selected.len(),
            if hidden > 0 {
                format!(" · {hidden} outside this filter")
            } else {
                String::new()
            }
        )
    } else {
        safe_text(&app.status)
    };
    f.render_widget(
        Paragraph::new(footer).style(Style::default().fg(if app.busy { ACCENT } else { MUTED })),
        sections[4],
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![
            text("space", ACCENT),
            text(" select   ", MUTED),
            text("↵", ACCENT),
            text(" expand   ", MUTED),
            text("/", ACCENT),
            text(" find   ", MUTED),
            text("d", ACCENT),
            text(
                if app.config.dry {
                    " check   "
                } else {
                    " delete  "
                },
                MUTED,
            ),
            text("?", ACCENT),
            text(" help", MUTED),
        ])),
        sections[5],
    );
    if matches!(app.mode, Mode::Confirm | Mode::Help | Mode::Coverage) {
        overlay(f, app)
    }
}
fn unicode_width(s: &str) -> usize {
    Line::from(s).width()
}
fn truncate(s: &str, width: usize) -> String {
    if unicode_width(s) <= width {
        return s.into();
    }
    let mut out = String::new();
    for c in s.chars() {
        if unicode_width(&out) + unicode_width(&c.to_string()) >= width {
            break;
        }
        out.push(c)
    }
    out.push('…');
    out
}
fn overlay(f: &mut Frame, app: &mut App) {
    let area = f.area().inner(Margin::new(5, 2));
    f.render_widget(Clear, area);
    let title = match app.mode {
        Mode::Confirm => {
            if app.config.dry {
                " dry run "
            } else {
                " delete selected "
            }
        }
        Mode::Coverage => " scan coverage ",
        _ => " keys ",
    };
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(FAINT))
            .title(Span::styled(title, Style::default().fg(ACCENT))),
        area,
    );
    let inner = area.inner(Margin::new(2, 1));
    let parts = Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).split(inner);
    let mut lines = vec![];
    match app.mode {
        Mode::Confirm => {
            let items: Vec<_> = app
                .report
                .items
                .iter()
                .filter(|i| app.selected.contains(&i.id))
                .collect();
            lines.push(Line::from(text(
                format!(
                    "{} selected",
                    size(selection_bytes(&app.report.items, &app.selected))
                ),
                INK,
            )));
            lines.push(Line::from(text(
                if app.config.dry {
                    "No data will be changed."
                } else {
                    "Selected data will be permanently deleted."
                },
                WARN,
            )));
            lines.push(Line::default());
            for item in items {
                lines.push(Line::from(text(safe_text(&item.name), ACCENT)));
            }
            f.render_widget(
                Paragraph::new(if app.config.dry {
                    "enter check   esc cancel"
                } else {
                    "enter delete   esc cancel"
                })
                .style(Style::default().fg(ACCENT)),
                parts[1],
            );
        }

        Mode::Coverage => {
            lines.push(Line::from(text("PROJECT ROOTS", ACCENT)));
            for p in &app.report.roots {
                lines.push(Line::from(text(p.display().to_string(), INK)))
            }
            lines.push(Line::default());
            lines.push(Line::from(text(
                format!(
                    "Depth limit {}. Config: {}",
                    app.config.max_depth,
                    app.config.config_path.display()
                ),
                MUTED,
            )));
            lines.push(Line::from(text("Home discovery finds dependencies and Python environments by structure, not project folder names. Hidden app state, Library and filesystem boundaries are excluded from that walk. Known tool stores, local Docker and backups are queried separately.",MUTED)));
            lines.push(Line::from(text("Protected data and unreadable paths are excluded from the list and total. App caches are one combined action.",MUTED)));
            lines.push(Line::default());
            lines.push(Line::from(text(
                format!(
                    "Actions below {} are omitted. Custom build outputs: {}.",
                    size(app.config.min_bytes),
                    app.config.build_outputs.len()
                ),
                MUTED,
            )));
            lines.push(Line::default());
            lines.push(Line::from(text("NOTES & FAILURES", ACCENT)));
            if app.report.notes.is_empty() {
                lines.push(Line::from(text("No scan warnings.", MUTED)))
            }
            for n in &app.report.notes {
                lines.push(Line::from(text(safe_text(n), WARN)));
                lines.push(Line::default());
            }
            f.render_widget(
                Paragraph::new("↑/↓ scroll   esc close").style(Style::default().fg(MUTED)),
                parts[1],
            );
        }
        _ => {
            for (key, desc) in [
                ("j / k / arrows", "Move between rows"),
                ("enter / right", "Expand a category"),
                ("left", "Collapse or return to parent"),
                ("space", "Select action or category"),
                ("/", "Search names, paths and descriptions"),
                ("d / delete", "Delete selected data (confirmation required)"),
                ("u", "Clear selection"),
                ("r", "Rescan and clear selection"),
                ("i", "Coverage, skipped paths and failures"),
                ("q / ctrl-c", "Quit (wait for deletion to finish)"),
                ("mouse", "Click a name to expand; checkbox to select"),
            ] {
                lines.push(Line::from(vec![
                    text(format!("{key:20}"), ACCENT),
                    text(desc, INK),
                ]));
                lines.push(Line::default());
            }
            f.render_widget(
                Paragraph::new("esc close").style(Style::default().fg(MUTED)),
                parts[1],
            );
        }
    }
    let max = lines
        .iter()
        .map(|l| (l.width().max(1) as u16).div_ceil(parts[0].width.max(1)))
        .sum::<u16>()
        .saturating_sub(parts[0].height);
    app.scroll = app.scroll.min(max);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.scroll, 0)),
        parts[0],
    );
}
pub fn run(config: Config) -> Result<()> {
    use std::io::IsTerminal;
    anyhow::ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "Use --scan or --json outside an interactive terminal"
    );
    let terminated = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, terminated.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, terminated.clone())?;
    let mut terminal = ratatui::init();
    execute!(io::stdout(), EnableMouseCapture)?;
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = execute!(io::stdout(), DisableMouseCapture);
            ratatui::restore();
        }
    }
    let _restore = Restore;
    let mut app = App::new(config);
    let mut dirty = true;
    loop {
        dirty |= app.drain();
        if terminated.load(Ordering::Relaxed) && !app.busy {
            break;
        }
        if dirty {
            terminal.draw(|f| draw(f, &mut app))?;
            dirty = false;
        }
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if app.key(key) {
                    break;
                }
                dirty = true;
            }
            Event::Resize(..) => dirty = true,
            Event::Mouse(m) if app.mode == Mode::Browse && !app.busy => match m.kind {
                MouseEventKind::ScrollDown => {
                    app.cursor = (app.cursor + 1).min(app.rows().len().saturating_sub(1));
                    dirty = true
                }
                MouseEventKind::ScrollUp => {
                    app.cursor = app.cursor.saturating_sub(1);
                    dirty = true
                }
                MouseEventKind::Down(event::MouseButton::Left)
                    if app.list_area.contains(Position::new(m.column, m.row)) =>
                {
                    let i = (m.row - app.list_area.y) as usize + app.state.offset();
                    let rows = app.rows();
                    if let Some(row) = rows.get(i) {
                        app.cursor = i;
                        if m.column < app.list_area.x + 4 {
                            app.toggle(row)
                        } else if !row.leaf && !app.expanded.remove(&row.id) {
                            app.expanded.insert(row.id.clone());
                        }
                        dirty = true
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
    Ok(())
}
fn category_description(name: &str) -> &str {
    match name {
        "Projects" => {
            "Build outputs, installed dependencies and Python environments discovered in your projects."
        }
        "Developer caches" => {
            "Downloaded packages, build indexes and generated development data. Rebuilding may need time and network access."
        }
        "Simulators" => {
            "Simulator caches, device data and installed Apple runtimes. Device resets remove installed apps and their data."
        }
        "System cleanup" => "Temporary files, diagnostic logs, Trash and local backup snapshots.",
        "Models & virtual machines" => {
            "Downloaded models, virtual machines and unused local Docker resources. Docker cleanup keeps resources newer than 72 hours."
        }
        "Archives & backups" => {
            "Release archives, device backups and downloaded installers. Keep anything you may need to restore or distribute."
        }
        _ => "Choose an item to see its description and location.",
    }
}
