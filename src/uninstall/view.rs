use super::ui::{Page, Screen};
use crate::model::{safe_text, size};
use ratatui::{
    prelude::*,
    widgets::{Cell, Paragraph, Row, Table, TableState, Wrap},
};
const INK: Color = Color::Rgb(221, 224, 216);
const MUTED: Color = Color::Rgb(127, 137, 133);
const DANGER: Color = Color::Rgb(218, 149, 139);
const ACTIVE: Color = Color::Rgb(46, 34, 33);

pub(super) fn draw(f: &mut Frame, s: &mut Screen) {
    let area = f.area().inner(Margin::new(3, 1));
    if area.width < 58 || area.height < 16 {
        f.render_widget(Paragraph::new("cleanix\nMinimum 64 × 18. q to quit."), area);
        return;
    }
    let layout = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(6),
        Constraint::Length(4),
        Constraint::Length(2),
    ])
    .split(area);
    let selected_bytes: u64 = s
        .inventory
        .as_ref()
        .map(|i| s.selected.iter().filter_map(|&n| i.apps[n].bytes).sum())
        .unwrap_or(0);
    let total = s
        .inventory
        .as_ref()
        .map(|i| {
            let bytes: u64 = i.apps.iter().filter_map(|app| app.bytes).sum();
            let suffix = if i.apps.iter().any(|app| app.bytes.is_none()) {
                "+"
            } else {
                ""
            };
            format!("{} apps · {}{} total", i.apps.len(), size(bytes), suffix)
        })
        .unwrap_or_default();
    let title = if s.config.dry {
        "cleanix   DRY"
    } else {
        "cleanix"
    };
    f.render_widget(
        Paragraph::new(vec![
            Line::styled(title, Style::default().fg(DANGER).bold()),
            Line::styled(total, Style::default().fg(MUTED)),
            Line::styled(
                format!("{} selected · {}", s.selected.len(), size(selected_bytes)),
                Style::default().fg(MUTED),
            ),
        ]),
        layout[0],
    );
    if s.page == Page::Confirm {
        let bytes: u64 = s
            .plans
            .iter()
            .flat_map(|p| &p.entries)
            .map(|e| e.bytes)
            .sum();
        let mut lines = vec![
            Line::styled(
                format!("Remove {} applications · {}", s.plans.len(), size(bytes)),
                Style::default().fg(DANGER).bold(),
            ),
            Line::default(),
        ];
        lines.extend([
            Line::default(),
            Line::from(if s.config.dry {
                "Dry check only. Apps will not quit or be removed."
            } else {
                "Permanently deletes apps and matched data, including settings and documents."
            }),
            Line::from("Apps are asked to quit; their login helpers are stopped."),
        ]);
        if s.plans.iter().any(|p| p.app.brew.is_some()) {
            lines.push(Line::from(
                "Homebrew runs its cask uninstall hooks; zap is not used.",
            ));
        }
        if s.plans.iter().any(|p| p.app.system_extensions) {
            lines.push(Line::styled("System extensions may remain active. Deactivate them in the app before uninstalling.", Style::default().fg(DANGER)));
        }
        let skipped = s.plans.iter().any(|p| {
            p.notes.iter().any(|n| {
                n.starts_with("Skipped")
                    || n.contains("preserved:")
                    || n.contains("check unavailable")
            })
        });
        if skipped {
            lines.push(Line::from(
                "Some related data is preserved because it could not be safely matched or read.",
            ));
        }
        lines.push(Line::default());
        for plan in &s.plans {
            lines.push(Line::from(format!(
                "{}{}{}",
                safe_text(&plan.app.name),
                if plan.app.running {
                    " · quit first"
                } else {
                    ""
                },
                if plan.app.brew.is_some() {
                    " · Homebrew"
                } else {
                    ""
                }
            )));
        }
        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((s.confirm_scroll, 0)),
            layout[1],
        );
    } else {
        let visible = s.visible();
        let rows: Vec<_> = s
            .inventory
            .as_ref()
            .map(|inventory| {
                visible
                    .iter()
                    .map(|&i| {
                        let app = &inventory.apps[i];
                        Row::new(vec![
                            Cell::from(if app.blocked.is_some() {
                                " - "
                            } else if s.selected.contains(&i) {
                                "[x]"
                            } else {
                                "[ ]"
                            }),
                            Cell::from(safe_text(&app.name)),
                            Cell::from(app.bytes.map(size).unwrap_or_else(|| "unknown".into())),
                            Cell::from(app.last_used.clone().unwrap_or_else(|| "unknown".into())),
                        ])
                        .style(Style::default().fg(if app.blocked.is_some() { MUTED } else { INK }))
                    })
                    .collect()
            })
            .unwrap_or_default();
        s.cursor = s.cursor.min(rows.len().saturating_sub(1));
        let mut state = TableState::default()
            .with_offset(s.state.offset())
            .with_selected(if rows.is_empty() {
                None
            } else {
                Some(s.cursor)
            });
        let table = Table::new(
            rows,
            [
                Constraint::Length(3),
                Constraint::Fill(1),
                Constraint::Length(10),
                Constraint::Length(12),
            ],
        )
        .header(
            Row::new(["", "APPLICATION", "SIZE", "LAST USED"])
                .style(Style::default().fg(MUTED))
                .bottom_margin(1),
        )
        .row_highlight_style(Style::default().bg(ACTIVE).fg(INK));
        f.render_stateful_widget(table, layout[1], &mut state);
        *s.state.offset_mut() = state.offset();
    }
    let mut status = if s.page == Page::Confirm {
        format!(
            "Type {} to {}:\n> {}\n{}",
            safe_text(&s.confirmation_phrase()),
            if s.config.dry {
                "validate only"
            } else {
                "confirm permanent removal"
            },
            safe_text(&s.confirmation),
            s.status
        )
    } else {
        s.status.clone()
    };
    if s.page == Page::Apps
        && let Some(inventory) = &s.inventory
    {
        if let Some(app) = s.visible().get(s.cursor).map(|&i| &inventory.apps[i])
            && let Some(reason) = &app.blocked
        {
            status.push_str(&format!("\n{reason}"));
        }
        if let Some(note) = inventory.notes.first() {
            status.push_str(&format!("\n{note}"));
        }
    }
    f.render_widget(
        Paragraph::new(status.lines().map(safe_text).collect::<Vec<_>>().join("\n"))
            .style(Style::default().fg(MUTED))
            .wrap(Wrap { trim: false }),
        layout[2],
    );
    let footer = if s.busy {
        "working…".into()
    } else if s.searching {
        format!("/ {}", safe_text(&s.query))
    } else if s.page == Page::Confirm {
        "enter confirm   esc cancel   pgup/pgdn scroll".into()
    } else {
        "↑↓ move   space select   d uninstall…   / search   r rescan   q quit".into()
    };
    f.render_widget(
        Paragraph::new(footer).style(Style::default().fg(MUTED)),
        layout[3],
    );
}
