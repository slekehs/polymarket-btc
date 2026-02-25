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

    let result = run_loop(&mut terminal, &mut app, &client, &mut market_table_state).await;

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
) -> io::Result<()> {
    let refresh_interval = Duration::from_secs(2);
    let mut last_tick = std::time::Instant::now();

    loop {
        terminal.draw(|f| render(f, app, market_state))?;

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
                        KeyCode::Down | KeyCode::Char('j') => {
                            let max = app.markets.len().saturating_sub(1);
                            let next = market_state.selected().map_or(0, |i| (i + 1).min(max));
                            market_state.select(Some(next));
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            let prev = market_state
                                .selected()
                                .map_or(0, |i| i.saturating_sub(1));
                            market_state.select(Some(prev));
                        }
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

fn render(f: &mut Frame, app: &AppState, market_state: &mut TableState) {
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
    render_body(f, app, market_state, chunks[1]);
    render_footer(f, chunks[2]);
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

    let title_spans = vec![
        Span::styled(
            " Polymarket Scanner  ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(status_text, Style::default().fg(status_color)),
        Span::raw("  │  "),
        Span::styled(
            format!("{} windows today", app.summary.windows_today),
            Style::default().fg(Color::White),
        ),
        Span::raw("  │  "),
        Span::styled(avg_str, Style::default().fg(Color::White)),
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

fn render_body(f: &mut Frame, app: &AppState, market_state: &mut TableState, area: Rect) {
    // Horizontal split: markets (40%) | windows (60%)
    let halves = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);

    render_markets_table(f, app, market_state, halves[0]);
    render_windows_table(f, app, halves[1]);
}

fn render_markets_table(f: &mut Frame, app: &AppState, state: &mut TableState, area: Rect) {
    let header_cells = ["#", "Market", "Score", "W/24h"]
        .iter()
        .map(|h| Cell::from(*h).style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)));
    let header = Row::new(header_cells).height(1);

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
                Cell::from(truncate(&m.question, 28)),
                Cell::from(score).style(Style::default().fg(score_color)),
                Cell::from(w24).style(Style::default().fg(Color::Cyan)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(5),
            Constraint::Length(5),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
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

fn render_windows_table(f: &mut Frame, app: &AppState, area: Rect) {
    let header_cells = ["Time", "Market", "Spread", "Duration", "Class", "Reason"]
        .iter()
        .map(|h| Cell::from(*h).style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)));
    let header = Row::new(header_cells).height(1);

    // Build a market question lookup from the markets list
    let market_lookup: std::collections::HashMap<&str, &str> = app
        .markets
        .iter()
        .map(|m| (m.id.as_str(), m.question.as_str()))
        .collect();

    let rows: Vec<Row> = app
        .recent_windows
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

            Row::new(vec![
                Cell::from(time).style(Style::default().fg(Color::DarkGray)),
                Cell::from(market_label),
                Cell::from(spread).style(Style::default().fg(spread_color)),
                Cell::from(duration),
                Cell::from(class).style(Style::default().fg(class_color)),
                Cell::from(reason).style(Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Min(10),
            Constraint::Length(7),
            Constraint::Length(8),
            Constraint::Length(6),
            Constraint::Length(10),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(Span::styled(
                " RECENT WINDOWS ",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )),
    );

    f.render_widget(table, area);
}

fn render_footer(f: &mut Frame, area: Rect) {
    let line = Line::from(vec![
        Span::styled(" [q] ", Style::default().fg(Color::Yellow)),
        Span::raw("quit  "),
        Span::styled("[r] ", Style::default().fg(Color::Yellow)),
        Span::raw("refresh  "),
        Span::styled("[↑↓ / j k] ", Style::default().fg(Color::Yellow)),
        Span::raw("scroll markets  "),
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
