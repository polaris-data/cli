use std::path::{Path, PathBuf};

use chrono::{Datelike, Duration as ChronoDuration};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::block::Title;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::layout::Layout as AppLayout;

use super::app::RemoteListTui;
use super::coverage::{
    access_color, compact_count, render_completion_bar, spinner_frame, sync_adjusted_day_totals,
};
use super::model::{
    AccountIdentity, AccountLoginSession, ActiveDaySync, ApiKeyPromptState, DatasetView,
    DayCoverage, DayState, ViewMode,
};

pub(crate) fn render(frame: &mut ratatui::Frame<'_>, app: &RemoteListTui) {
    match &app.mode {
        ViewMode::Splash => render_splash(frame, app.spinner_tick),
        ViewMode::Browser => render_browser(frame, app),
        ViewMode::Account => render_account_view(frame, app),
        ViewMode::Dataset(view) => render_dataset_view(
            frame,
            view,
            app.is_bookmarked(view.dataset.dataset.as_str()),
            app.status_message.as_deref(),
            app.active_sync.as_ref(),
            app.spinner_tick,
        ),
    }

    if let Some(prompt) = &app.api_key_prompt {
        render_api_key_prompt(frame, prompt);
    }
}

fn render_splash(frame: &mut ratatui::Frame<'_>, spinner_tick: usize) {
    let area = frame.area();
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(3),
            Constraint::Length(6),
            Constraint::Length(2),
            Constraint::Fill(1),
        ])
        .split(area);

    let sky_area = centered_rect(92, sections[0].height, sections[0]);
    let copy_area = centered_rect(84, sections[1].height, sections[1]);
    let footer_area = centered_rect(64, sections[2].height, sections[2]);

    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(splash_motif_lines(sky_area, spinner_tick)).alignment(Alignment::Center),
        sky_area,
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![Span::styled(
                "Polaris",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )]),
            Line::from(""),
            Line::from(vec![Span::styled(
                "High-fidelity market data from Hyperliquid, Lighter, and more.",
                Style::default().fg(Color::White),
            )]),
            Line::from(vec![Span::styled(
                "Browse datasets. Track daily coverage. Pull the missing pieces.",
                Style::default().fg(Color::DarkGray),
            )]),
        ])
        .alignment(Alignment::Center),
        copy_area,
    );
    frame.render_widget(
        Paragraph::new(vec![Line::from(vec![Span::styled(
            " Space to open catalog ",
            Style::default().fg(Color::Black).bg(Color::White),
        )])])
        .alignment(Alignment::Center),
        footer_area,
    );
}

fn splash_motif_lines(area: Rect, spinner_tick: usize) -> Vec<Line<'static>> {
    let height = usize::from(area.height).max(8);
    let width = usize::from(area.width).max(24);
    let mut lines = Vec::with_capacity(height);

    for row in 0..height {
        lines.push(splash_motif_line(width, height, row, spinner_tick));
    }

    lines
}

fn splash_motif_line(
    width: usize,
    height: usize,
    row: usize,
    spinner_tick: usize,
) -> Line<'static> {
    if row + 1 >= height {
        return Line::from(" ".repeat(width));
    }

    let mut spans = Vec::with_capacity(width);

    for col in 0..width {
        spans.push(splash_motif_cell(width, height, col, row, spinner_tick));
    }

    Line::from(spans)
}

fn splash_motif_cell(
    width: usize,
    height: usize,
    col: usize,
    row: usize,
    _spinner_tick: usize,
) -> Span<'static> {
    let width_f = width as f32;
    let height_f = height as f32;

    let mut shade = None;

    let layers = [
        (
            0isize,
            0isize,
            width,
            (height_f * 0.34).ceil() as usize,
            '█',
            Color::DarkGray,
        ),
        (
            (width_f * 0.18).floor() as isize,
            0isize,
            (width_f * 0.82).ceil() as usize,
            (height_f * 0.52).ceil() as usize,
            '█',
            Color::Gray,
        ),
        (
            (width_f * 0.46).floor() as isize,
            0isize,
            (width_f * 0.54).ceil() as usize,
            (height_f * 0.68).ceil() as usize,
            '█',
            Color::DarkGray,
        ),
        (
            (width_f * 0.94).floor() as isize,
            0isize,
            (width_f * 0.06).max(1.0).ceil() as usize,
            (height_f * 0.86).ceil() as usize,
            '█',
            Color::Gray,
        ),
    ];

    for (left, top, rect_width, rect_height, symbol, color) in layers {
        let within_x = (col as isize) >= left && (col as isize) < left + rect_width as isize;
        let within_y = (row as isize) >= top && (row as isize) < top + rect_height as isize;

        if within_x && within_y {
            shade = Some((symbol, color, left, top, rect_width, rect_height));
        }
    }

    let Some((symbol, color, left, top, rect_width, rect_height)) = shade else {
        return Span::raw(" ");
    };

    let edge = (col as isize) == left
        || (col + 1) == (left + rect_width as isize).max(0) as usize
        || (row as isize) == top
        || (row + 1) == (top + rect_height as isize).max(0) as usize;
    let style = if edge {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(color)
    };

    Span::styled(symbol.to_string(), style)
}

fn render_browser(frame: &mut ratatui::Frame<'_>, app: &RemoteListTui) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(frame.area());

    let version_title = Title {
        content: Line::from(concat!("Polaris v", env!("CARGO_PKG_VERSION")))
            .alignment(Alignment::Right),
        alignment: Some(Alignment::Right),
        position: None,
    };
    let search = Paragraph::new(app.search.clone()).block(
        Block::default()
            .title("Search dataset or access")
            .title(version_title)
            .borders(Borders::ALL),
    );
    frame.render_widget(search, areas[0]);

    let items = if app.filtered_indices.is_empty() {
        vec![ListItem::new("No datasets match the current search")]
    } else {
        app.filtered_indices
            .iter()
            .map(|index| {
                let dataset = &app.datasets[*index];
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{:<12} ", dataset.access_badge()),
                        Style::default().fg(access_color(dataset.access.as_ref())),
                    ),
                    Span::styled(
                        if app.is_bookmarked(dataset.dataset.as_str()) {
                            "* "
                        } else {
                            "  "
                        },
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::styled(
                        dataset.dataset.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                ]))
            })
            .collect()
    };

    let mut state = ListState::default()
        .with_selected((!app.filtered_indices.is_empty()).then_some(app.selected));
    let list = List::new(items)
        .block(
            Block::default()
                .title("Datasets")
                .title(category_carousel_title(app))
                .borders(Borders::ALL),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol(">> ");
    frame.render_stateful_widget(list, areas[1], &mut state);

    render_footer(
        frame,
        areas[2],
        " Type to search  │  . account  │  ←/→ category  │  ↑/↓ navigate  │  Tab bookmark  │  Enter inspect dataset  │  Ctrl+C quit ",
    );
}

fn render_account_view(frame: &mut ratatui::Frame<'_>, app: &RemoteListTui) {
    let account = &app.account_view;
    let area = centered_rect(90, 24, frame.area());

    let status_style = if account.api_key_present {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    };
    let status_label = if account.api_key_present {
        "configured"
    } else {
        "not configured"
    };
    let intro = if account.active_login.is_some() {
        "Finish login in your browser. Polaris will save the returned API key automatically."
    } else {
        "Press . again to sign in in your browser and add an API key."
    };

    let mut lines = vec![
        Line::from(""),
        Line::from(vec![Span::styled(
            "Polaris account",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from((!account.api_key_present).then_some(intro).unwrap_or("")),
        Line::from(""),
    ];
    if let Some(identity) = &account.identity {
        append_identity_rows(&mut lines, identity);
        lines.push(Line::from(""));
    }
    lines.push(account_divider());
    lines.push(account_row("base", &account.base_url));
    lines.push(account_row("root", &format_account_root(&account.root)));
    if let Some(active_login) = &account.active_login {
        lines.push(Line::from(""));
        append_active_login_rows(&mut lines, active_login);
    }

    let status_title = Title {
        content: Line::from(Span::styled(status_label, status_style)).alignment(Alignment::Right),
        alignment: Some(Alignment::Right),
        position: None,
    };

    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }).block(
            Block::default()
                .title("Account")
                .title(status_title)
                .borders(Borders::ALL),
        ),
        area,
    );

    let footer_area = Rect {
        x: frame.area().x,
        y: frame.area().bottom().saturating_sub(1),
        width: frame.area().width,
        height: 1,
    };
    let footer = if account.api_key_present {
        " . open account  │  P pricing  │  R refresh  │  Esc back  │  Ctrl+C quit "
    } else if account.active_login.is_some() {
        " . reopen browser  │  P pricing  │  R refresh  │  Esc back  │  Ctrl+C quit "
    } else {
        " . sign in  │  P pricing  │  R refresh  │  Esc back  │  Ctrl+C quit "
    };
    render_footer(frame, footer_area, footer);
}

fn append_active_login_rows(lines: &mut Vec<Line<'static>>, active_login: &AccountLoginSession) {
    lines.push(account_row("code", &active_login.user_code));
    lines.push(account_row(
        "expires",
        &active_login
            .expires_at
            .format("%Y-%m-%d %H:%M:%SZ")
            .to_string(),
    ));
    lines.push(account_row("browser", &active_login.login_url));
}

fn append_identity_rows(lines: &mut Vec<Line<'static>>, identity: &AccountIdentity) {
    lines.push(account_row(
        "name",
        identity.display_name.as_deref().unwrap_or("--"),
    ));
    lines.push(account_row(
        "email",
        identity.email.as_deref().unwrap_or("--"),
    ));
    lines.push(account_row(
        "plan",
        identity.plan.as_deref().unwrap_or("--"),
    ));
    if let Some(wallet_address) = &identity.wallet_address {
        lines.push(account_row("wallet", wallet_address));
    }
    if let Some(avatar_url) = &identity.avatar_url {
        lines.push(account_row("avatar", avatar_url));
    }
}

fn account_row(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<7}"), Style::default().fg(Color::DarkGray)),
        Span::raw(value.to_string()),
    ])
}

fn account_divider() -> Line<'static> {
    Line::from(Span::styled(
        "────────────────────────────────────────────────────────────────────",
        Style::default().fg(Color::DarkGray),
    ))
}

fn format_account_root(root: &Path) -> String {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .and_then(|home| {
            root.strip_prefix(&home).ok().map(|relative| {
                if relative.as_os_str().is_empty() {
                    "~".to_string()
                } else {
                    format!("~/{}", relative.display())
                }
            })
        })
        .unwrap_or_else(|| root.display().to_string())
}

fn category_carousel_title(app: &RemoteListTui) -> Title<'static> {
    let labels = app.category_display_labels();
    let mut spans = Vec::new();

    for (index, label) in labels.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" | ", Style::default().fg(Color::DarkGray)));
        }

        let is_active = index == 0;
        let text = if is_active {
            format!("*{label}*")
        } else {
            label.clone()
        };
        let style = if is_active {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        spans.push(Span::styled(text, style));
    }

    Title {
        content: Line::from(spans).alignment(Alignment::Center),
        alignment: Some(Alignment::Center),
        position: None,
    }
}

fn render_footer(frame: &mut ratatui::Frame<'_>, area: Rect, text: &str) {
    let footer = Paragraph::new(text).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(footer, area);
}

fn render_dataset_view(
    frame: &mut ratatui::Frame<'_>,
    view: &DatasetView,
    is_bookmarked: bool,
    status_message: Option<&str>,
    active_sync: Option<&ActiveDaySync>,
    spinner_tick: usize,
) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(8),
            Constraint::Length(7),
            Constraint::Length(1),
        ])
        .split(frame.area());

    frame.render_widget(
        render_day_grid(view, is_bookmarked, active_sync, spinner_tick),
        areas[0],
    );
    frame.render_widget(
        render_selected_day_summary(view, active_sync, status_message),
        areas[1],
    );

    render_footer(
        frame,
        areas[2],
        " Enter sync day  │  Tab Show in Finder  │  ←/→ move day  │  ↑/↓ move week  │  Esc back  │  Ctrl+C quit ",
    );
}

fn render_day_grid(
    view: &DatasetView,
    is_bookmarked: bool,
    active_sync: Option<&ActiveDaySync>,
    spinner_tick: usize,
) -> Paragraph<'static> {
    let selected_date = view.selected_coverage().date;
    let mut lines = Vec::new();
    let mut month_start = 0usize;

    while month_start < view.days.len() {
        let month = view.days[month_start].date.month();
        let year = view.days[month_start].date.year();
        let mut month_end = month_start;
        while month_end < view.days.len()
            && view.days[month_end].date.month() == month
            && view.days[month_end].date.year() == year
        {
            month_end += 1;
        }

        if !lines.is_empty() {
            lines.push(Line::from(""));
        }

        lines.push(Line::from(vec![Span::styled(
            view.days[month_start].date.format("%B %Y").to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        )]));
        lines.push(weekday_header_line());

        let month_days = &view.days[month_start..month_end];
        let month_first = month_days
            .first()
            .map(|day| day.date)
            .expect("month section requires at least one day");
        let month_last = month_days
            .last()
            .map(|day| day.date)
            .expect("month section requires at least one day");
        let grid_start =
            month_first - ChronoDuration::days(month_first.weekday().num_days_from_monday() as i64);
        let grid_end = month_last
            + ChronoDuration::days((6 - month_last.weekday().num_days_from_monday()) as i64);

        let mut cursor = grid_start;
        while cursor <= grid_end {
            let mut spans = Vec::with_capacity(7);
            for _ in 0..7 {
                if let Some(day) = month_days.iter().find(|day| day.date == cursor) {
                    spans.push(render_day_cell(
                        day,
                        cursor == selected_date,
                        active_sync,
                        &view.dataset.dataset,
                        spinner_tick,
                    ));
                } else {
                    spans.push(Span::styled(
                        "        ".to_string(),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                cursor += ChronoDuration::days(1);
            }
            lines.push(Line::from(spans));
        }

        month_start = month_end;
    }

    let mut dataset_title_spans = vec![Span::styled(
        view.dataset.access_badge(),
        Style::default().fg(access_color(view.dataset.access.as_ref())),
    )];
    if is_bookmarked {
        dataset_title_spans.push(Span::styled(" *", Style::default().fg(Color::Yellow)));
    }
    dataset_title_spans.push(Span::raw(" "));
    dataset_title_spans.push(Span::styled(
        view.dataset.dataset.clone(),
        Style::default().add_modifier(Modifier::BOLD),
    ));

    Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .title("Daily Coverage")
            .title(Title {
                content: Line::from(dataset_title_spans).alignment(Alignment::Right),
                alignment: Some(Alignment::Right),
                position: None,
            })
            .borders(Borders::ALL),
    )
}

fn weekday_header_line() -> Line<'static> {
    Line::from(vec![
        Span::raw(" Mon    "),
        Span::raw(" Tue    "),
        Span::raw(" Wed    "),
        Span::raw(" Thu    "),
        Span::raw(" Fri    "),
        Span::raw(" Sat    "),
        Span::raw(" Sun"),
    ])
}

fn render_day_cell(
    day: &DayCoverage,
    selected: bool,
    active_sync: Option<&ActiveDaySync>,
    dataset: &str,
    spinner_tick: usize,
) -> Span<'static> {
    let status = if active_sync
        .map(|sync| sync.dataset == dataset && sync.date == day.date)
        .unwrap_or(false)
    {
        spinner_frame(spinner_tick).to_string()
    } else {
        match day.state() {
            DayState::Full => "OK".to_string(),
            DayState::Partial => format!("~{}", compact_count(day.missing_keys.len())),
            DayState::Empty => "--".to_string(),
            DayState::NoRemote => "..".to_string(),
        }
    };
    let style = match day.state() {
        DayState::Full => Style::default().fg(Color::Green),
        DayState::Partial => Style::default().fg(Color::Yellow),
        DayState::Empty => Style::default().fg(Color::Red),
        DayState::NoRemote => Style::default().fg(Color::DarkGray),
    };
    let style = if selected {
        style.add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        style
    };
    Span::styled(format!("{:>2} {:<4} ", day.date.day(), status), style)
}

fn render_selected_day_summary(
    view: &DatasetView,
    active_sync: Option<&ActiveDaySync>,
    status_message: Option<&str>,
) -> Paragraph<'static> {
    let day = view.selected_coverage();
    let (remote_total, local_total, missing_total, state) =
        sync_adjusted_day_totals(view, day, active_sync);
    let completion_bar = render_completion_bar(local_total, remote_total, 18);
    let state_style = match state {
        "full" => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        "partial" => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        "none local" => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        "no remote data" => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
        "syncing" => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        _ => Style::default().add_modifier(Modifier::BOLD),
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                day.date.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(state.to_ascii_uppercase(), state_style),
        ]),
        Line::from(format!(
            "coverage: {}   missing: {}",
            completion_bar, missing_total
        )),
        Line::from(format_snapshot_location(view, day)),
    ];
    if let Some(status) = status_message {
        lines.push(Line::from(vec![
            Span::styled("status: ", Style::default().fg(Color::DarkGray)),
            Span::raw(status.to_string()),
        ]));
    }

    Paragraph::new(lines)
        .wrap(Wrap { trim: true })
        .block(Block::default().title("Selected Day").borders(Borders::ALL))
}

pub(crate) fn format_snapshot_location(view: &DatasetView, day: &DayCoverage) -> String {
    let key = day.local_keys.first().or_else(|| day.remote_keys.first());
    let path = key
        .and_then(|key| AppLayout::new(PathBuf::new()).data_path_for_key(key).ok())
        .map(|path| {
            path.parent()
                .map(|parent| parent.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|| "data".into())
        })
        .unwrap_or_else(|| {
            format!(
                "data/<source>/{}/{}/{}",
                view.dataset.venue, view.dataset.symbol, day.date
            )
        });
    if day.local_keys.is_empty() {
        format!("will store under: {path}")
    } else {
        format!("stored under: {path}")
    }
}

fn render_api_key_prompt(frame: &mut ratatui::Frame<'_>, prompt: &ApiKeyPromptState) {
    let area = centered_rect(72, 10, frame.area());
    let masked_input = if prompt.input.is_empty() {
        "<empty>".to_string()
    } else {
        "*".repeat(prompt.input.chars().count())
    };

    let mut lines = vec![
        Line::from(vec![Span::styled(
            prompt.access_message.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )]),
        Line::from("Go to polaris.supply to grab your API key."),
        Line::from(""),
        Line::from(format!("API key: {masked_input}")),
        Line::from("Enter saves the key and continues syncing. Esc cancels."),
    ];
    if let Some(error) = &prompt.error_message {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            error.clone(),
            Style::default().fg(Color::Red),
        )));
    }

    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .title("Polaris API Key")
                    .borders(Borders::ALL),
            ),
        area,
    );
}

fn centered_rect(width_percentage: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(height.min(area.height)),
            Constraint::Fill(1),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Fill(1),
            Constraint::Percentage(width_percentage.min(100)),
            Constraint::Fill(1),
        ])
        .split(vertical[1]);
    horizontal[1]
}
