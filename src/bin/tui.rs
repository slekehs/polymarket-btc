mod tui_app;

use std::io;
use std::time::Duration;

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState},
    Frame, Terminal,
};
use tui_app::{format_class, format_duration, format_spread, format_time_ns, truncate, AppState, ConnectionStatus};

/// Which pane has focus for keyboard input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Markets,
    Windows,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> io::Result<()> {
    let base_url = std::env::var("API_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("failed to build HTTP client");

    let mut app = AppState::new(base_url);

    // Initial fetch before rendering
    app.refresh(&client).await;

    // Terminal setup
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut market_table_state = TableState::default();
    market_table_state.select(None);
    let mut window_table_state = TableState::default();
    window_table_state.select(None);
    let mut focus = Focus::Markets;

    let result = run_loop(
        &mut terminal,
        &mut app,
        &client,
        &mut market_table_state,
        &mut window_table_state,
        &mut focus,
    )
    .await;

    // Restore terminal regardless of result
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}

// ---------------------------------------------------------------------------
// Main event loop
// ---------------------------------------------------------------------------

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut AppState,
    client: &reqwest::Client,
    market_state: &mut TableState,
    window_state: &mut TableState,
    focus: &mut Focus,
) -> io::Result<()> {
    let refresh_interval = Duration::from_secs(2);
    let mut last_tick = std::time::Instant::now();

    loop {
        terminal.draw(|f| render(f, app, market_state, window_state, *focus))?;

        let timeout = refresh_interval
            .checked_sub(last_tick.elapsed())
            .unwrap_or(Duration::ZERO);

        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Char('Q') => return Ok(()),
                        KeyCode::Char('r') | KeyCode::Char('R') => {
                            app.refresh(client).await;
                            last_tick = std::time::Instant::now();
                        }
                        KeyCode::Tab | KeyCode::BackTab => {
                            *focus = match *focus {
                                Focus::Markets => Focus::Windows,
                                Focus::Windows => Focus::Markets,
                            };
                        }
                        KeyCode::Esc => {
                            if app.showing_market_windows() {
                                app.clear_market_windows();
                                *focus = Focus::Markets;
                                window_state.select(None);
                            }
                        }
                        KeyCode::Enter => {
                            if *focus == Focus::Markets {
                                if let Some(i) = market_state.selected() {
                                    if let Some(m) = app.markets.get(i) {
                                        let id = m.id.clone();
                                        app.fetch_market_windows(client, &id).await;
                                        *focus = Focus::Windows;
                                        window_state.select(Some(0));
                                    }
                                }
                            }
                        }
                        KeyCode::Down | KeyCode::Char('j') => match *focus {
                            Focus::Markets => {
                                let max = app.markets.len().saturating_sub(1);
                                let next = market_state.selected().map_or(0, |i| (i + 1).min(max));
                                market_state.select(Some(next));
                            }
                            Focus::Windows => {
                                let windows = app.displayed_windows();
                                let max = windows.len().saturating_sub(1);
                                let next = window_state.selected().map_or(0, |i| (i + 1).min(max));
                                window_state.select(Some(next));
                            }
                        },
                        KeyCode::Up | KeyCode::Char('k') => match *focus {
                            Focus::Markets => {
                                let prev = market_state
                                    .selected()
                                    .map_or(0, |i| i.saturating_sub(1));
                                market_state.select(Some(prev));
                            }
                            Focus::Windows => {
                                let prev = window_state
                                    .selected()
                                    .map_or(0, |i| i.saturating_sub(1));
                                window_state.select(Some(prev));
                            }
                        },
                        _ => {}
                    }
                }
            }
        }

        if last_tick.elapsed() >= refresh_interval {
            app.refresh(client).await;
            last_tick = std::time::Instant::now();
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render(
    f: &mut Frame,
    app: &AppState,
    market_state: &mut TableState,
    window_state: &mut TableState,
    focus: Focus,
) {
    let area = f.area();

    // Outer vertical split: header | body | footer
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Min(0),    // body
            Constraint::Length(1), // footer
        ])
        .split(area);

    render_header(f, app, chunks[0]);
    render_body(f, app, market_state, window_state, focus, chunks[1]);
    render_footer(f, chunks[2], focus);
}

fn render_header(f: &mut Frame, app: &AppState, area: Rect) {
    let (status_text, status_color) = match &app.status {
        ConnectionStatus::Connected => ("● connected".to_string(), Color::Green),
        ConnectionStatus::Connecting => ("◌ connecting".to_string(), Color::Yellow),
        ConnectionStatus::Error(e) => (format!("✗ {}", truncate(e, 40)), Color::Red),
    };

    let avg_str = app
        .summary
        .avg_duration_ms_today
        .map_or("—".to_string(), |v| format!("{:.0}ms avg", v));

    let ws_str = app
        .health
        .ws_connected
        .map(|v| if v { "WS ✓" } else { "WS ✗" })
        .unwrap_or("WS —")
        .to_string();
    let ws_color = app
        .health
        .ws_connected
        .map(|v| if v { Color::Green } else { Color::Red })
        .unwrap_or(Color::DarkGray);

    let hydrated = app
        .health
        .hydrated_markets
        .zip(app.health.total_markets)
        .map_or("—/—".to_string(), |(h, t)| format!("{h}/{t} hydrated"));

    let p99_str = app
        .latency
        .p99_ms
        .map_or("—".to_string(), |v| format!("p99 {:.2}ms", v));
    let p99_color = app.latency.p99_ms.map_or(Color::DarkGray, |v| {
        if v < 5.0 {
            Color::Green
        } else if v < 10.0 {
            Color::Yellow
        } else {
            Color::Red
        }
    });

    let queue_str = app
        .health
        .write_queue_pending
        .map_or("—".to_string(), |q| format!("queue {q}"));

    let title_spans = vec![
        Span::styled(
            " Polymarket Scanner  ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(status_text, Style::default().fg(status_color)),
        Span::raw("  │  "),
        Span::styled(ws_str, Style::default().fg(ws_color)),
        Span::raw("  │  "),
        Span::styled(hydrated, Style::default().fg(Color::White)),
        Span::raw("  │  "),
        Span::styled(
            format!("{} windows today", app.summary.windows_today),
            Style::default().fg(Color::White),
        ),
        Span::raw("  │  "),
        Span::styled(avg_str, Style::default().fg(Color::White)),
        Span::raw("  │  "),
        Span::styled(p99_str, Style::default().fg(p99_color)),
        Span::raw("  │  "),
        Span::styled(queue_str, Style::default().fg(Color::DarkGray)),
        Span::raw("  │  "),
        Span::styled(
            format!("{} markets", app.summary.total_markets),
            Style::default().fg(Color::White),
        ),
    ];

    let header_line = Line::from(title_spans);
    let paragraph = Paragraph::new(header_line)
        .block(Block::default().borders(Borders::ALL).border_style(
            Style::default().fg(Color::DarkGray),
        ));

    f.render_widget(paragraph, area);
}

fn render_body(
    f: &mut Frame,
    app: &AppState,
    market_state: &mut TableState,
    window_state: &mut TableState,
    focus: Focus,
    area: Rect,
) {
    // Horizontal split: markets (40%) | right side (60%)
    let halves = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);

    let markets_focused = focus == Focus::Markets;
    render_markets_table(f, app, market_state, halves[0], markets_focused);

    let right_area = halves[1];
    if app.open_windows.is_empty() {
        render_windows_table(f, app, window_state, right_area, focus == Focus::Windows);
    } else {
        let vert = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length((app.open_windows.len() as u16 + 2).min(8)),
                Constraint::Min(5),
            ])
            .split(right_area);
        render_open_windows(f, app, vert[0]);
        render_windows_table(
            f,
            app,
            window_state,
            vert[1],
            focus == Focus::Windows,
        );
    }
}

fn render_markets_table(
    f: &mut Frame,
    app: &AppState,
    state: &mut TableState,
    area: Rect,
    focused: bool,
) {
    let header_cells = ["#", "Market", "Score", "W/24h", "P1", "P2"]
        .iter()
        .map(|h| Cell::from(*h).style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)));
    let header = Row::new(header_cells).height(1);

    let border_color = if focused { Color::Cyan } else { Color::DarkGray };

    let rows: Vec<Row> = app
        .markets
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let score = m
                .opportunity_score
                .map_or("—".to_string(), |s| format!("{:.2}", s));
            let w24 = m
                .windows_24h
                .map_or("—".to_string(), |w| w.to_string());
            let p1 = m.p1_windows_24h.map_or("—".to_string(), |n| n.to_string());
            let p2 = m.p2_windows_24h.map_or("—".to_string(), |n| n.to_string());

            let score_color = m.opportunity_score.map_or(Color::DarkGray, |s| {
                if s >= 0.7 {
                    Color::Green
                } else if s >= 0.4 {
                    Color::Yellow
                } else {
                    Color::Red
                }
            });

            Row::new(vec![
                Cell::from(format!("{}", i + 1)).style(Style::default().fg(Color::DarkGray)),
                Cell::from(truncate(&m.question, 24)),
                Cell::from(score).style(Style::default().fg(score_color)),
                Cell::from(w24).style(Style::default().fg(Color::Cyan)),
                Cell::from(p1).style(Style::default().fg(Color::Green)),
                Cell::from(p2).style(Style::default().fg(Color::LightGreen)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(5),
            Constraint::Length(5),
            Constraint::Length(3),
            Constraint::Length(3),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(Span::styled(
                " TOP MARKETS ",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )),
    )
    .row_highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    f.render_stateful_widget(table, area, state);
}

fn render_open_windows(f: &mut Frame, app: &AppState, area: Rect) {
    let market_lookup: std::collections::HashMap<&str, &str> = app
        .markets
        .iter()
        .map(|m| (m.id.as_str(), m.question.as_str()))
        .collect();

    let rows: Vec<Row> = app
        .open_windows
        .iter()
        .map(|w| {
            let time = format_time_ns(w.opened_at);
            let market_label = market_lookup
                .get(w.market_id.as_str())
                .map(|q| truncate(q, 20))
                .unwrap_or_else(|| truncate(&w.market_id, 20));
            let spread = format_spread(w.spread_size);
            Row::new(vec![
                Cell::from(time).style(Style::default().fg(Color::DarkGray)),
                Cell::from(market_label),
                Cell::from(spread).style(Style::default().fg(Color::Green)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Min(8),
            Constraint::Length(7),
        ],
    )
    .header(
        Row::new(vec![
            Cell::from("Time").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Cell::from("Market").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Cell::from("Spread").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        ])
        .height(1),
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(Span::styled(
                format!(" OPEN NOW ({}) ", app.open_windows.len()),
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            )),
    );

    f.render_widget(table, area);
}

fn render_windows_table(
    f: &mut Frame,
    app: &AppState,
    state: &mut TableState,
    area: Rect,
    focused: bool,
) {
    let header_cells = ["Time", "Market", "Spread", "Dur", "Class", "Reason", "μs"]
        .iter()
        .map(|h| Cell::from(*h).style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)));
    let header = Row::new(header_cells).height(1);

    let border_color = if focused { Color::Cyan } else { Color::DarkGray };

    // Build a market question lookup from the markets list
    let market_lookup: std::collections::HashMap<&str, &str> = app
        .markets
        .iter()
        .map(|m| (m.id.as_str(), m.question.as_str()))
        .collect();

    let displayed = app.displayed_windows();
    let title = if app.showing_market_windows() {
        let q = app
            .market_windows
            .market_question
            .as_deref()
            .map(|s| truncate(s, 25))
            .unwrap_or_else(|| "Unknown".to_string());
        format!(" {} ({}) ", q, displayed.len())
    } else {
        format!(" RECENT WINDOWS ({}) ", displayed.len())
    };

    let rows: Vec<Row> = displayed
        .iter()
        .map(|w| {
            let time = format_time_ns(w.opened_at);
            let market_label = market_lookup
                .get(w.market_id.as_str())
                .map(|q| truncate(q, 22))
                .unwrap_or_else(|| truncate(&w.market_id, 22));
            let spread = format_spread(w.spread_size);
            let duration = format_duration(w.duration_ms);
            let class = format_class(
                w.opportunity_class,
                w.open_duration_class.as_deref(),
            );
            let reason = w
                .close_reason
                .as_deref()
                .map(|r| shorten_reason(r))
                .unwrap_or("—");

            let class_color = match class.as_str() {
                "P1" => Color::Green,
                "P2" => Color::LightGreen,
                "P3" => Color::Yellow,
                "P4" => Color::Red,
                "noise" => Color::DarkGray,
                _ => Color::White,
            };

            let spread_color = if w.spread_size >= 0.03 {
                Color::Green
            } else if w.spread_size >= 0.01 {
                Color::Yellow
            } else {
                Color::White
            };

            let latency_str = w
                .detection_latency_us
                .map(|u| format!("{}", u))
                .unwrap_or_else(|| "—".to_string());
            let latency_color = w.detection_latency_us.map_or(Color::DarkGray, |u| {
                if u < 500 {
                    Color::Green
                } else if u < 2000 {
                    Color::Yellow
                } else {
                    Color::Red
                }
            });

            Row::new(vec![
                Cell::from(time).style(Style::default().fg(Color::DarkGray)),
                Cell::from(market_label),
                Cell::from(spread).style(Style::default().fg(spread_color)),
                Cell::from(duration),
                Cell::from(class).style(Style::default().fg(class_color)),
                Cell::from(reason).style(Style::default().fg(Color::DarkGray)),
                Cell::from(latency_str).style(Style::default().fg(latency_color)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Min(8),
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Length(6),
            Constraint::Length(9),
            Constraint::Length(5),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(Span::styled(
                title,
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )),
    )
    .row_highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    f.render_stateful_widget(table, area, state);
}

fn render_footer(f: &mut Frame, area: Rect, focus: Focus) {
    let focus_hint = match focus {
        Focus::Markets => "markets (↑↓/jk)",
        Focus::Windows => "windows (↑↓/jk)",
    };
    let line = Line::from(vec![
        Span::styled(" [q] ", Style::default().fg(Color::Yellow)),
        Span::raw("quit  "),
        Span::styled("[r] ", Style::default().fg(Color::Yellow)),
        Span::raw("refresh  "),
        Span::styled("[Tab] ", Style::default().fg(Color::Yellow)),
        Span::raw("switch pane  "),
        Span::styled("[↑↓/jk] ", Style::default().fg(Color::Yellow)),
        Span::raw(format!("scroll {}  ", focus_hint)),
        Span::styled("[Enter] ", Style::default().fg(Color::Yellow)),
        Span::raw("market windows  "),
        Span::styled("[Esc] ", Style::default().fg(Color::Yellow)),
        Span::raw("back  "),
        Span::styled("auto-refresh: 2s", Style::default().fg(Color::DarkGray)),
    ]);
    let paragraph = Paragraph::new(line).style(Style::default().fg(Color::White));
    f.render_widget(paragraph, area);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn shorten_reason(r: &str) -> &'static str {
    match r {
        "volume_spike_gradual" => "vol-grad",
        "volume_spike_instant" => "vol-inst",
        "price_drift" => "drift",
        "order_vanished" => "vanished",
        _ => "—",
    }
}
