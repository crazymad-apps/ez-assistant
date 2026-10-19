//! 中心与 Host 共用的固定 wire 形状；业务头仅在 Host 解析。
use super::StreamError;

pub(super) const BLOCK: usize = 32 * 1024;
pub(super) const WINDOW: usize = 64 * 1024;
pub(super) const HEADER: usize = 30;
#[derive(Clone)]
pub(crate) struct Frame {
    pub kind: u8,
    pub payload: Vec<u8>,
}
impl Frame {
    pub(super) fn cost(&self) -> usize {
        (HEADER + self.payload.len()).max(256)
    }
    pub(super) fn validate(&self) -> Result<(), StreamError> {
        if !(1..=10).contains(&self.kind)
            || self.payload.len() > BLOCK
            || ((self.kind == 1 || self.kind == 2) && self.payload.len() > 16384)
            || (matches!(self.kind, 4 | 10) && !self.payload.is_empty())
            || (matches!(self.kind, 5 | 6) && self.payload.len() > 16384)
            || (matches!(self.kind, 8 | 9) && self.payload.len() > 125)
        {
            return Err(StreamError::Invalid);
        }
        Ok(())
    }
}
pub(super) fn encode(access: &str, id: u64, frame: &Frame) -> Result<Vec<u8>, StreamError> {
    frame.validate()?;
    let hex: String = access.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || !hex.is_ascii() || id == 0 {
        return Err(StreamError::Invalid);
    }
    let mut bytes = Vec::with_capacity(HEADER + frame.payload.len());
    bytes.extend([2, frame.kind]);
    for i in (0..32).step_by(2) {
        bytes.push(u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| StreamError::Invalid)?);
    }
    bytes.extend(id.to_be_bytes());
    bytes.extend((frame.payload.len() as u32).to_be_bytes());
    bytes.extend(&frame.payload);
    Ok(bytes)
}
pub(super) fn decode(bytes: &[u8]) -> Result<(String, u64, Frame), StreamError> {
    if bytes.len() < HEADER || bytes[0] != 2 {
        return Err(StreamError::Invalid);
    }
    let id = u64::from_be_bytes(bytes[18..26].try_into().map_err(|_| StreamError::Invalid)?);
    let len =
        u32::from_be_bytes(bytes[26..30].try_into().map_err(|_| StreamError::Invalid)?) as usize;
    if id == 0 || len > BLOCK || bytes.len() != HEADER + len {
        return Err(StreamError::Invalid);
    }
    let mut access = String::with_capacity(36);
    use std::fmt::Write;
    for (i, byte) in bytes[2..18].iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            access.push('-');
        }
        write!(access, "{byte:02x}").map_err(|_| StreamError::Invalid)?;
    }
    let frame = Frame {
        kind: bytes[1],
        payload: bytes[HEADER..].to_vec(),
    };
    frame.validate()?;
    Ok((access, id, frame))
}
