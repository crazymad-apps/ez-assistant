//! 输入验证与有限文本组装。序号、按下集合和文本只属于一次控制，不重放副作用。
use assistant_protocol::DesktopInput;
use std::collections::BTreeSet;
use tokio::time::{Duration, Instant};

#[derive(Default, Clone)]
pub(super) struct Pressed {
    pub keys: BTreeSet<String>,
    pub buttons: BTreeSet<u8>,
}
pub(super) struct Input {
    pub sequence: u64,
    pub pressed: Pressed,
    text: Option<(String, Instant)>,
}
impl Input {
    pub fn new() -> Self {
        Self {
            sequence: 0,
            pressed: Pressed::default(),
            text: None,
        }
    }
    pub fn prepare(
        &mut self,
        seq: u64,
        input: &DesktopInput,
    ) -> Result<Option<String>, &'static str> {
        if seq != self.sequence + 1 || seq > 9_007_199_254_740_991 {
            return Err("invalid_input");
        }
        if self
            .text
            .as_ref()
            .is_some_and(|(_, start)| start.elapsed() > Duration::from_secs(5))
        {
            return Err("text_expired");
        }
        if self.text.is_some()
            && !matches!(
                input,
                DesktopInput::Heartbeat
                    | DesktopInput::Release
                    | DesktopInput::TextChunk { .. }
                    | DesktopInput::TextEnd
            )
        {
            return Err("invalid_input");
        }
        self.sequence = seq;
        let command = match input {
            DesktopInput::Heartbeat => format!("heartbeat {seq}"),
            DesktopInput::Release => {
                self.text = None;
                self.pressed = Pressed::default();
                format!("release {seq}")
            }
            DesktopInput::Move { x, y } if unit(*x) && unit(*y) => format!("move {seq} {x} {y}"),
            DesktopInput::Button {
                button,
                down,
                clicks,
            } if *button <= 2 && (1..=2).contains(clicks) => {
                if *down {
                    if !self.pressed.buttons.insert(*button) {
                        return Err("invalid_input");
                    }
                } else if !self.pressed.buttons.remove(button) {
                    return Ok(None);
                }
                format!("button {seq} {button} {} {clicks}", u8::from(*down))
            }
            DesktopInput::Key { code, down, repeat } if valid_key(code) => {
                if *down {
                    let held = self.pressed.keys.contains(code);
                    if held != *repeat {
                        return Err("invalid_input");
                    }
                    self.pressed.keys.insert(code.clone());
                } else if !self.pressed.keys.remove(code) {
                    return Ok(None);
                }
                format!("key {seq} {code} {} {}", u8::from(*down), u8::from(*repeat))
            }
            DesktopInput::Scroll { x, y }
                if x.unsigned_abs() <= 1200 && y.unsigned_abs() <= 1200 =>
            {
                format!("scroll {seq} {x} {y}")
            }
            DesktopInput::TextStart
                if self.text.is_none()
                    && self.pressed.keys.is_empty()
                    && self.pressed.buttons.is_empty() =>
            {
                self.text = Some((String::new(), Instant::now()));
                return Ok(None);
            }
            DesktopInput::TextChunk { text } => {
                let (buffer, _) = self.text.as_mut().ok_or("invalid_input")?;
                if buffer.len() + text.len() > 16_384 || text.contains('\0') {
                    return Err("text_too_large");
                }
                buffer.push_str(text);
                return Ok(None);
            }
            DesktopInput::TextEnd => {
                let (text, _) = self.text.take().ok_or("invalid_input")?;
                if text.is_empty() {
                    return Ok(None);
                }
                let hex: String = text.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
                format!("text {seq} {hex}")
            }
            _ => return Err("invalid_input"),
        };
        Ok(Some(command))
    }
}
fn unit(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}
pub(super) fn valid_key(code: &str) -> bool {
    (code.len() == 4 && code.starts_with("Key") && code.as_bytes()[3].is_ascii_uppercase())
        || (code.len() == 6 && code.starts_with("Digit") && code.as_bytes()[5].is_ascii_digit())
        || matches!(
            code,
            "Backquote"
                | "Minus"
                | "Equal"
                | "BracketLeft"
                | "BracketRight"
                | "Backslash"
                | "Semicolon"
                | "Quote"
                | "Comma"
                | "Period"
                | "Slash"
                | "Space"
                | "Enter"
                | "Tab"
                | "Escape"
                | "Backspace"
                | "Delete"
                | "Insert"
                | "Home"
                | "End"
                | "PageUp"
                | "PageDown"
                | "ArrowLeft"
                | "ArrowRight"
                | "ArrowUp"
                | "ArrowDown"
                | "ShiftLeft"
                | "ShiftRight"
                | "ControlLeft"
                | "ControlRight"
                | "AltLeft"
                | "AltRight"
                | "MetaLeft"
                | "MetaRight"
                | "CapsLock"
                | "F1"
                | "F2"
                | "F3"
                | "F4"
                | "F5"
                | "F6"
                | "F7"
                | "F8"
                | "F9"
                | "F10"
                | "F11"
                | "F12"
        )
}
