//! LuaやWASMバックエンドから独立したバイナリ描画バッチプロトコル。
//! 各レコードはリトルエンディアンの u16 opcode、u16 reserved(0)、u32 ペイロード長、その後にペイロードが続きます。
use crate::{
    draw::{DrawCommand, ScreenError},
    screen::Rgba8,
};
/// 共通デコーダが受け付ける最大エンコードバッチサイズ。
pub const MAX_BATCH_BYTES: usize = 8 * 1024 * 1024;
/// コマンドを適用する前にバッチ全体をデコードします。ポインタキャストやJSONは使用しません。
pub fn decode(bytes: &[u8]) -> Result<Vec<DrawCommand>, ScreenError> {
    if bytes.len() > MAX_BATCH_BYTES {
        return Err(ScreenError::LimitExceeded);
    }
    let mut remaining = bytes;
    let mut commands = Vec::new();
    commands
        .try_reserve((bytes.len() / 32).min(4096))
        .map_err(|_| ScreenError::LimitExceeded)?;
    let mut text_bytes = 0_usize;
    while !remaining.is_empty() {
        let header = remaining.get(..8).ok_or(ScreenError::InvalidCommand)?;
        let opcode = u16::from_le_bytes([header[0], header[1]]);
        if header[2] != 0 || header[3] != 0 {
            return Err(ScreenError::InvalidCommand);
        }
        let length = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        let end = 8_usize
            .checked_add(length)
            .ok_or(ScreenError::InvalidCommand)?;
        let payload = remaining.get(8..end).ok_or(ScreenError::InvalidCommand)?;
        remaining = remaining.get(end..).ok_or(ScreenError::InvalidCommand)?;
        let numbers = |count: usize| -> Result<[f64; 6], ScreenError> {
            let raw = payload
                .get(..count * 8)
                .ok_or(ScreenError::InvalidCommand)?;
            let mut values = [0.0; 6];
            for (out, chunk) in values.iter_mut().zip(raw.chunks_exact(8)) {
                *out =
                    f64::from_le_bytes(chunk.try_into().map_err(|_| ScreenError::InvalidCommand)?);
            }
            Ok(values)
        };
        let command = match opcode {
            1 if length == 4 => DrawCommand::SetColor(Rgba8(
                payload
                    .try_into()
                    .map_err(|_| ScreenError::InvalidCommand)?,
            )),
            2 if length == 0 => DrawCommand::Clear,
            3 if length == 32 => {
                let n = numbers(4)?;
                DrawCommand::Line([[n[0], n[1]], [n[2], n[3]]])
            }
            4 | 5 if length == 32 => {
                let n = numbers(4)?;
                DrawCommand::Rect([n[0], n[1], n[2], n[3]], opcode == 5)
            }
            6 | 7 if length == 24 => {
                let n = numbers(3)?;
                DrawCommand::Circle([n[0], n[1], n[2]], opcode == 7)
            }
            8 | 9 if length == 48 => {
                let n = numbers(6)?;
                DrawCommand::Triangle([[n[0], n[1]], [n[2], n[3]], [n[4], n[5]]], opcode == 9)
            }
            10 if length >= 16 => {
                let n = numbers(2)?;
                DrawCommand::Text([n[0], n[1]], payload[16..].to_vec())
            }
            11 if length >= 48 => {
                let n = numbers(6)?;
                DrawCommand::TextBox([n[0], n[1], n[2], n[3], n[4], n[5]], payload[48..].to_vec())
            }
            12 if length == 24 => {
                let n = numbers(3)?;
                DrawCommand::Map([n[0], n[1], n[2]])
            }
            13 if length == 5 => DrawCommand::MapColor(
                crate::map::MapColorKind::try_from(payload[0])?,
                Rgba8([payload[1], payload[2], payload[3], payload[4]]),
            ),
            _ => return Err(ScreenError::InvalidCommand),
        };
        text_bytes = text_bytes
            .checked_add(command.text_bytes())
            .ok_or(ScreenError::LimitExceeded)?;
        if commands.len() >= 65536 || text_bytes > 1024 * 1024 {
            return Err(ScreenError::LimitExceeded);
        }
        commands
            .try_reserve(1)
            .map_err(|_| ScreenError::LimitExceeded)?;
        commands.push(command);
    }
    Ok(commands)
}
