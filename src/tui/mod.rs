use std::io::{self, IsTerminal, Stdout};
use std::path::PathBuf;
use std::time::Duration;

use chrono::Utc;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::api::PolarisClient;
use crate::config::Config;
use crate::error::{Result, TickError};
use crate::layout::LocalSnapshotEntry;

mod app;
mod coverage;
mod model;
mod render;
mod storage;
#[cfg(test)]
mod tests;

use render::render;

pub(crate) use app::RemoteListTui;
pub(crate) use model::ViewMode;
pub use model::{RemoteDatasetEntry, RemoteTuiSeed};

#[cfg(test)]
pub(crate) use coverage::{
    api_key_requirement_for_download, build_day_coverages, diff_missing_snapshot_keys,
};
#[cfg(test)]
pub(crate) use model::{
    AccountIdentity, ActiveDaySync, ApiKeyRequirement, BrowserCategory, DatasetView, DaySyncUpdate,
    FileManagerTarget,
};
#[cfg(test)]
pub(crate) use render::format_snapshot_location;
#[cfg(test)]
pub(crate) use storage::{
    load_account_identity, load_bookmarks, save_account_identity, save_bookmarks,
    snapshot_reveal_target,
};

pub fn can_render_tui() -> bool {
    io::stdout().is_terminal() && io::stdin().is_terminal()
}

pub async fn run_remote_list_tui(
    client: PolarisClient,
    datasets: Vec<RemoteDatasetEntry>,
    local_snapshots: Vec<LocalSnapshotEntry>,
    root: PathBuf,
    concurrency: usize,
    seed: RemoteTuiSeed,
    config: &Config,
) -> Result<()> {
    let mut terminal = setup_terminal()?;
    let mut app = RemoteListTui::new(datasets, local_snapshots, root, concurrency, seed);
    app.apply_runtime_config(config);
    if let Err(err) = app.hydrate_account_identity(&client).await {
        if app.account_view.identity.is_none() {
            app.status_message = Some(format!("warning: failed to refresh account details: {err}"));
        }
    }
    let result = run_event_loop(&mut terminal, client, app).await;
    restore_terminal(&mut terminal)?;
    result
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().map_err(|err| TickError::Other(err.into()))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(|err| TickError::Other(err.into()))?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend).map_err(|err| TickError::Other(err.into()))
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode().map_err(|err| TickError::Other(err.into()))?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)
        .map_err(|err| TickError::Other(err.into()))?;
    terminal
        .show_cursor()
        .map_err(|err| TickError::Other(err.into()))?;
    Ok(())
}

async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    client: PolarisClient,
    mut app: RemoteListTui,
) -> Result<()> {
    let mut client = client;
    loop {
        app.pump_cli_login_updates(&mut client).await?;
        app.pump_account_refresh_updates()?;
        app.pump_sync_updates(&client).await?;
        terminal
            .draw(|frame| render(frame, &app))
            .map_err(|err| TickError::Other(err.into()))?;

        if event::poll(Duration::from_millis(250)).map_err(|err| TickError::Other(err.into()))? {
            let event = event::read().map_err(|err| TickError::Other(err.into()))?;
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    return Ok(());
                }
                if app.api_key_prompt.is_some() {
                    match key.code {
                        KeyCode::Esc => app.close_api_key_prompt(),
                        KeyCode::Enter => {
                            if let Err(err) = app.submit_api_key_prompt(&mut client).await {
                                app.status_message = Some(format!("error: {err}"));
                            }
                        }
                        KeyCode::Backspace => app.pop_api_key_prompt_char(),
                        KeyCode::Char(c) if !c.is_control() => app.push_api_key_prompt_char(c),
                        _ => {}
                    }
                    continue;
                }
                match key.code {
                    KeyCode::Esc => match app.mode {
                        ViewMode::Splash => return Ok(()),
                        ViewMode::Browser => return Ok(()),
                        ViewMode::Dataset(_) | ViewMode::Account => app.mode = ViewMode::Browser,
                    },
                    KeyCode::Char(' ') if matches!(app.mode, ViewMode::Splash) => {
                        app.mode = ViewMode::Browser;
                    }
                    KeyCode::F(2) if !matches!(app.mode, ViewMode::Splash) => {
                        app.open_account_view();
                    }
                    KeyCode::Char('.') if !matches!(app.mode, ViewMode::Splash) => {
                        if let Err(err) = app.handle_account_shortcut(&client).await {
                            app.status_message = Some(format!("error: {err}"));
                        }
                    }
                    KeyCode::F(3) if !matches!(app.mode, ViewMode::Splash) => {
                        if let Err(err) = app.handle_account_shortcut(&client).await {
                            app.status_message = Some(format!("error: {err}"));
                        }
                    }
                    KeyCode::Char('p') | KeyCode::Char('P')
                        if matches!(app.mode, ViewMode::Account) =>
                    {
                        if let Err(err) = app.open_pricing_page() {
                            app.status_message = Some(format!("error: {err}"));
                        }
                    }
                    KeyCode::Char('r') | KeyCode::Char('R')
                        if matches!(app.mode, ViewMode::Account) =>
                    {
                        if let Err(err) = app.refresh_account_details(&client) {
                            app.status_message = Some(format!("error: {err}"));
                        }
                    }
                    KeyCode::Enter => {
                        if matches!(app.mode, ViewMode::Browser) {
                            if let Err(err) = app.open_selected_dataset(&client).await {
                                app.status_message = Some(format!("error: {err}"));
                            }
                        } else if let Some(requirement) = app.api_key_requirement_for_selected_day(
                            Utc::now().date_naive(),
                            client.has_api_key(),
                        ) {
                            app.open_api_key_prompt(requirement);
                        } else if let Err(err) = app.sync_selected_day(&client).await {
                            app.status_message = Some(format!("error: {err}"));
                        }
                    }
                    KeyCode::Up => {
                        if let ViewMode::Dataset(view) = &mut app.mode {
                            view.move_selection(-7);
                        } else {
                            app.move_up();
                        }
                    }
                    KeyCode::Down => {
                        if let ViewMode::Dataset(view) = &mut app.mode {
                            view.move_selection(7);
                        } else {
                            app.move_down();
                        }
                    }
                    KeyCode::Left => {
                        if let ViewMode::Dataset(view) = &mut app.mode {
                            view.move_selection(-1);
                        } else if matches!(app.mode, ViewMode::Browser) {
                            app.cycle_category(-1);
                        }
                    }
                    KeyCode::Right => {
                        if let ViewMode::Dataset(view) = &mut app.mode {
                            view.move_selection(1);
                        } else if matches!(app.mode, ViewMode::Browser) {
                            app.cycle_category(1);
                        }
                    }
                    KeyCode::Tab => match app.mode {
                        ViewMode::Browser => {
                            if let Err(err) = app.toggle_current_bookmark() {
                                app.status_message = Some(format!("error: {err}"));
                            }
                        }
                        ViewMode::Dataset(_) => {
                            if let Err(err) = app.reveal_selected_day_snapshot() {
                                app.status_message = Some(format!("error: {err}"));
                            }
                        }
                        ViewMode::Splash | ViewMode::Account => {}
                    },
                    KeyCode::Backspace => {
                        if matches!(app.mode, ViewMode::Browser) {
                            app.search.pop();
                            app.recompute_filter();
                        }
                    }
                    KeyCode::Char(c)
                        if matches!(app.mode, ViewMode::Browser) && is_search_input_key(&key) =>
                    {
                        app.search.push(c);
                        app.recompute_filter();
                    }
                    _ => {}
                }
            }
        }
    }
}

fn is_search_input_key(key: &crossterm::event::KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };

    let allowed_modifiers = KeyModifiers::NONE | KeyModifiers::SHIFT;
    if !(key.modifiers - allowed_modifiers).is_empty() {
        return false;
    }

    c == ' ' || c.is_ascii_graphic()
}
