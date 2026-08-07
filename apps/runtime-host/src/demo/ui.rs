//! Ratatui 绘制层；只消费 [`DemoApp`] 的本地展示投影。

use agent_types::{AssistantPart, ConversationMessage, ToolResultContent, UserPart};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Wrap,
    },
};

use super::app::{ConnectionStatus, DemoApp, Focus};

const MIN_WIDTH: u16 = 48;
const MIN_HEIGHT: u16 = 12;

pub(crate) fn render(frame: &mut Frame<'_>, app: &DemoApp) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        frame.render_widget(
            Paragraph::new("Terminal too small. Resize to at least 48x12.")
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),
            Constraint::Length(3),
            Constraint::Length(2),
        ])
        .split(area);
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(28), Constraint::Min(20)])
        .split(rows[0]);

    render_sessions(frame, app, main[0]);
    render_conversation(frame, app, main[1]);
    render_input(frame, app, rows[1]);
    render_status(frame, app, rows[2]);

    if app.shutdown_confirmation {
        render_shutdown_confirmation(frame, area);
    } else if app.model_validation_confirmation {
        render_model_validation_confirmation(frame, app, area);
    }
}

fn render_sessions(frame: &mut Frame<'_>, app: &DemoApp, area: Rect) {
    let focused = app.focus == Focus::Sessions;
    let border_style = focused.then_some(Style::default().fg(Color::Cyan));
    let items = app.sessions.iter().map(|session| {
        let activity = session.active_run_id.as_ref().map_or("", |_| " ●");
        ListItem::new(Line::from(vec![
            Span::raw(format!("{} [{}]", session.title, session.model_key)),
            Span::styled(activity, Style::default().fg(Color::Yellow)),
        ]))
    });
    let list = List::new(items)
        .block(
            Block::default()
                .title(" Sessions ")
                .borders(Borders::ALL)
                .border_style(border_style.unwrap_or_default()),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");
    let mut state =
        ListState::default().with_selected(app.selected_session_id.as_ref().and_then(|selected| {
            app.sessions
                .iter()
                .position(|session| &session.session_id == selected)
        }));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_conversation(frame: &mut Frame<'_>, app: &DemoApp, area: Rect) {
    let lines = conversation_lines(app);
    let viewport_height = area.height.saturating_sub(2) as usize;
    let paragraph = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
    // `Line` 是逻辑行，终端中的长文本还会继续折成多个视觉行。滚动范围必须按
    // Ratatui 实际使用的折行规则计算，否则一段很长的 reasoning 仍会被误判为一行。
    let visual_line_count = paragraph.line_count(area.width.saturating_sub(2));
    let max_scroll = visual_line_count.saturating_sub(viewport_height);
    let scroll = max_scroll.saturating_sub(app.scroll_from_bottom as usize);
    let paragraph = paragraph
        .block(
            Block::default()
                .title(" Conversation / Run ")
                .borders(Borders::ALL),
        )
        .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0));
    frame.render_widget(paragraph, area);

    if max_scroll > 0 {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None);
        // Scrollbar 的 position 表示可滚动偏移，因此范围应是 0..=max_scroll，
        // 而不是正文总行数；这样正文处于底部时滑块也会准确贴到底部。
        let mut state = ScrollbarState::new(max_scroll.saturating_add(1))
            .position(scroll)
            .viewport_content_length(viewport_height);
        frame.render_stateful_widget(
            scrollbar,
            area.inner(Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut state,
        );
    }
}

fn conversation_lines(app: &DemoApp) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for message in &app.conversation.messages {
        match message {
            ConversationMessage::System(message) => {
                push_section(&mut lines, "SYSTEM", Color::Magenta, &message.text);
            }
            ConversationMessage::ContextSummary(message) => {
                push_section(&mut lines, "SUMMARY", Color::Magenta, &message.text);
            }
            ConversationMessage::User(message) => {
                let text = message
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        UserPart::Text(part) => Some(part.text.as_str()),
                        UserPart::Injected(_) => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                push_section(&mut lines, "YOU", Color::Cyan, &text);
            }
            ConversationMessage::Assistant(message) => {
                lines.push(Line::styled(
                    "ASSISTANT",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ));
                for part in &message.parts {
                    match part {
                        AssistantPart::Reasoning(part) => lines.push(Line::styled(
                            format!("thinking: {}", part.text),
                            Style::default().fg(Color::DarkGray),
                        )),
                        AssistantPart::Text(part) => push_text_lines(&mut lines, &part.text),
                        AssistantPart::ToolCall(call) => lines.push(Line::styled(
                            format!("tool call: {}", call.name),
                            Style::default().fg(Color::Yellow),
                        )),
                        AssistantPart::ProviderState(_) => {}
                    }
                }
                lines.push(Line::default());
            }
            ConversationMessage::Tool(message) => {
                let content = match &message.result.content {
                    ToolResultContent::Text(text) => text.clone(),
                    ToolResultContent::Json(value) => value.to_string(),
                };
                push_section(&mut lines, "TOOL", Color::Yellow, &content);
            }
        }
    }

    if let Some(run) = &app.current_run {
        lines.push(Line::styled(
            format!("RUN {} · {:?}", run.run_id, run.status),
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        ));
        if !run.reasoning.is_empty() {
            lines.push(Line::styled(
                format!("thinking: {}", run.reasoning),
                Style::default().fg(Color::DarkGray),
            ));
        }
        if !run.text.is_empty() {
            push_text_lines(&mut lines, &run.text);
        }
        for tool in &run.tools {
            lines.push(Line::styled(
                format!("tool: {} · {:?}", tool.tool_name, tool.status),
                Style::default().fg(Color::Yellow),
            ));
            if !tool.stdout.is_empty() {
                push_text_lines(&mut lines, &tool.stdout);
            }
            if !tool.stderr.is_empty() {
                lines.push(Line::styled(
                    tool.stderr.clone(),
                    Style::default().fg(Color::Red),
                ));
            }
        }
        if let Some(error) = &run.error {
            lines.push(Line::styled(
                format!("error: {}", error.message),
                Style::default().fg(Color::Red),
            ));
        }
    }

    if lines.is_empty() {
        lines.push(Line::styled(
            "No conversation yet. Type a message below and press Enter.",
            Style::default().fg(Color::DarkGray),
        ));
    }
    lines
}

fn push_section(lines: &mut Vec<Line<'static>>, label: &str, color: Color, text: &str) {
    lines.push(Line::styled(
        label.to_owned(),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ));
    push_text_lines(lines, text);
    lines.push(Line::default());
}

fn push_text_lines(lines: &mut Vec<Line<'static>>, text: &str) {
    lines.extend(text.lines().map(|line| Line::raw(line.to_owned())));
}

fn render_input(frame: &mut Frame<'_>, app: &DemoApp, area: Rect) {
    let focused = app.focus == Focus::Input;
    let width = area.width.saturating_sub(2) as usize;
    let scroll = app.input.visual_scroll(width.max(1));
    let paragraph = Paragraph::new(app.input.value())
        .scroll((0, u16::try_from(scroll).unwrap_or(u16::MAX)))
        .block(
            Block::default()
                .title(" Message ")
                .borders(Borders::ALL)
                .border_style(if focused {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default()
                }),
        );
    frame.render_widget(paragraph, area);
    if focused {
        let cursor = app.input.visual_cursor().saturating_sub(scroll);
        frame.set_cursor_position((
            area.x
                .saturating_add(1)
                .saturating_add(u16::try_from(cursor).unwrap_or(u16::MAX)),
            area.y.saturating_add(1),
        ));
    }
}

fn render_status(frame: &mut Frame<'_>, app: &DemoApp, area: Rect) {
    let connection = match &app.connection {
        ConnectionStatus::Connecting => "CONNECTING".to_owned(),
        ConnectionStatus::Connected { runtime_version } => {
            format!("CONNECTED {runtime_version}")
        }
        ConnectionStatus::Disconnected { .. } => "DISCONNECTED".to_owned(),
    };
    let selected = app
        .selected_session()
        .map_or_else(|| "no session".to_owned(), |session| session.title.clone());
    let run = app.current_run.as_ref().map_or_else(
        || "idle".to_owned(),
        |run| format!("{} {:?}", run.run_id, run.status),
    );
    let model = app
        .selected_model_key
        .as_ref()
        .map_or_else(|| "no model".to_owned(), ToString::to_string);
    let help = match app.focus {
        Focus::Sessions => {
            "↑/↓ session  M model  V validate  N new  F5 reload  R reconnect  Tab input  Q quit  Ctrl+Q stop"
        }
        Focus::Input => {
            "Enter send  F5 reload config  Tab sessions  PgUp/PgDn scroll  Ctrl+C cancel  Ctrl+Q stop Runtime"
        }
    };
    let text = Text::from(vec![
        Line::from(vec![
            Span::styled(connection, Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!(
                " · model {model} · {selected} · {run} · {}",
                app.status_message
            )),
        ]),
        Line::styled(help, Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(text), area);
}

fn render_shutdown_confirmation(frame: &mut Frame<'_>, area: Rect) {
    let popup = centered_rect(60, 5, area);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(Text::from(vec![
            Line::styled(
                "Stop the Runtime process?",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Line::raw("Enter confirms · Esc cancels"),
        ]))
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .title(" Confirm shutdown ")
                .borders(Borders::ALL),
        ),
        popup,
    );
}

fn render_model_validation_confirmation(frame: &mut Frame<'_>, app: &DemoApp, area: Rect) {
    let popup = centered_rect(70, 6, area);
    let model = app
        .selected_model_key
        .as_ref()
        .map_or_else(|| "unknown".to_owned(), ToString::to_string);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(Text::from(vec![
            Line::styled(
                format!("Validate model {model}?"),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::raw("This sends a minimal real request and may incur a small charge."),
            Line::raw("Enter confirms · Esc cancels"),
        ]))
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .title(" Confirm model validation ")
                .borders(Borders::ALL),
        ),
        popup,
    );
}

fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let width = area.width.saturating_mul(percent_x).saturating_div(100);
    Rect::new(
        area.x.saturating_add(area.width.saturating_sub(width) / 2),
        area.y
            .saturating_add(area.height.saturating_sub(height) / 2),
        width,
        height.min(area.height),
    )
}

#[cfg(test)]
mod tests {
    use assistant_protocol::{RunId, RunSnapshot, RunStatus, SessionId};
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    #[test]
    fn normal_screen_contains_the_primary_regions() {
        let backend = TestBackend::new(100, 28);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| render(frame, &DemoApp::default()))
            .expect("draw");
        let screen = terminal.backend().to_string();
        assert!(screen.contains("Sessions"));
        assert!(screen.contains("Conversation / Run"));
        assert!(screen.contains("Message"));
    }

    #[test]
    fn tiny_terminal_renders_a_resize_message_without_panicking() {
        let backend = TestBackend::new(30, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| render(frame, &DemoApp::default()))
            .expect("draw");
        assert!(
            terminal
                .backend()
                .to_string()
                .contains("Terminal too small")
        );
    }

    #[test]
    fn model_validation_confirmation_discloses_real_request_and_cost() {
        let backend = TestBackend::new(100, 28);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let app = DemoApp {
            selected_model_key: Some(assistant_protocol::ModelKey::new("model-1").expect("model")),
            model_validation_confirmation: true,
            ..DemoApp::default()
        };
        terminal.draw(|frame| render(frame, &app)).expect("draw");
        let screen = terminal.backend().to_string();
        assert!(screen.contains("Validate model model-1?"));
        assert!(screen.contains("minimal real request"));
        assert!(screen.contains("small charge"));
    }

    #[test]
    fn wrapped_run_content_follows_the_visual_bottom() {
        let backend = TestBackend::new(80, 16);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let app = app_with_long_wrapped_run(0);

        terminal.draw(|frame| render(frame, &app)).expect("draw");

        let screen = terminal.backend().to_string();
        assert!(screen.contains("BOTTOM"));
        assert!(!screen.contains("TOP"));
    }

    #[test]
    fn wrapped_run_content_can_scroll_back_to_the_visual_top() {
        let backend = TestBackend::new(80, 16);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let app = app_with_long_wrapped_run(u16::MAX);

        terminal.draw(|frame| render(frame, &app)).expect("draw");

        let screen = terminal.backend().to_string();
        assert!(screen.contains("TOP"));
        assert!(!screen.contains("BOTTOM"));
    }

    fn app_with_long_wrapped_run(scroll_from_bottom: u16) -> DemoApp {
        DemoApp {
            current_run: Some(RunSnapshot {
                run_id: RunId::new("run-scroll").expect("run id"),
                session_id: SessionId::new("session-scroll").expect("session id"),
                status: RunStatus::Completed,
                cancel_requested: false,
                reasoning: String::new(),
                text: format!("TOP {} BOTTOM", "wrapped-content ".repeat(80)),
                tools: Vec::new(),
                error: None,
            }),
            scroll_from_bottom,
            ..DemoApp::default()
        }
    }
}
