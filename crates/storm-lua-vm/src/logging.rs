//! VMごとのロギング。printは上限付きの開発用拡張、debug.logは実ゲームの契約（引数1個の文字列・数値のみ記録し、上限・エラーなし）に従います。ログの配信およびテキストデコードはホスト側の責務です。
use crate::runner::{lock, ErrorKind, VmError};
use mlua::{Lua, Table, Value, Variadic};
use std::sync::{Arc, Mutex, TryLockError};
/// ログレコードを生成したLuaエントリポイント。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogSource {
    /// 明示的な開発用print拡張。
    Print,
    /// 実ゲームのdebug.log（Luaの標準debugライブラリではありません）。
    Debug,
}
/// 呼び出し時点のLuaチャンク名と実行行。最適化前ソースへの対応はホストが管理します。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLocation {
    /// 実行時の完全なチャンク名。ファイルを自動的に読み込むパスではありません。
    pub chunk: String,
    /// Luaが報告した1始まりの実行行。列位置や消失したフレームは推測しません。
    pub line: u32,
}
/// 所有権を持つログデータ。非UTF-8バイト列も有効であり、暗黙に修復されることはありません。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecord {
    /// 発生元のLua関数。
    pub source: LogSource,
    /// タブ区切りの値列（改行は付加されません）。
    pub bytes: Vec<u8>,
    /// ログ生成時の最も近いLua呼び出し元。位置を取得できない場合はNone。
    pub location: Option<LogLocation>,
}
#[derive(Default)]
pub(crate) struct LogBuffer {
    records: Vec<LogRecord>,
    print_records: usize,
    print_bytes: usize,
}
fn limit(message: &str) -> mlua::Error {
    mlua::Error::external(VmError::new(ErrorKind::Limit, message))
}
// Inspect only while the original call stack exists. No line hooks or script-visible
// debug functions are needed. Skip C frames, but never invent a lost tail frame.
fn caller_location(lua: &Lua) -> Option<LogLocation> {
    for level in 1..=16 {
        let frame = lua.inspect_stack(level)?;
        let source = frame.source();
        if source.what == "C" {
            continue;
        }
        let line = u32::try_from(frame.curr_line())
            .ok()
            .filter(|line| *line > 0)?;
        let chunk = source.source?.into_owned();
        return Some(LogLocation { chunk, line });
    }
    None
}
fn print_function(lua: &Lua, buffer: Arc<Mutex<LogBuffer>>) -> mlua::Result<mlua::Function> {
    lua.create_function(move |lua, values: Variadic<Value>| {
        let mut line = Vec::new();
        for (i, value) in values.into_iter().enumerate() {
            if i > 0 {
                line.push(b'\t');
            }
            let bytes = match value {
                Value::Nil => b"nil".to_vec(),
                Value::Boolean(true) => b"true".to_vec(),
                Value::Boolean(false) => b"false".to_vec(),
                value => {
                    let kind = value.type_name();
                    if let Some(text) = lua.coerce_string(value)? {
                        text.as_bytes().to_vec()
                    } else {
                        kind.as_bytes().to_vec()
                    }
                }
            };
            if line.len() + bytes.len() > 16384 {
                return Err(limit("log line exceeds 16 KiB"));
            }
            line.extend_from_slice(&bytes);
        }
        let location = caller_location(lua);
        let record_bytes =
            line.len() + location.as_ref().map_or(0, |location| location.chunk.len());
        // A re-entrant log during an active drain keeps the historical Busy
        // error instead of blocking the Lua thread on a host-held lock.
        let mut buffer = match buffer.try_lock() {
            Ok(buffer) => buffer,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                return Err(mlua::Error::external(VmError::new(
                    ErrorKind::Busy,
                    "log buffer is borrowed",
                )))
            }
        };
        if buffer.print_records >= 128 || buffer.print_bytes + record_bytes > 65536 {
            return Err(limit(
                "log buffer limit exceeded; drain logs between callbacks",
            ));
        }
        buffer.print_records += 1;
        buffer.print_bytes += record_bytes;
        buffer.records.push(LogRecord {
            source: LogSource::Print,
            bytes: line,
            location,
        });
        Ok(())
    })
}
fn debug_log_function(lua: &Lua, buffer: Arc<Mutex<LogBuffer>>) -> mlua::Result<mlua::Function> {
    lua.create_function(move |lua, values: Variadic<Value>| {
        // 実ゲームの契約: 引数が1個で文字列のときだけ記録します。数値は文字列として通ります。
        // それ以外（0個・複数・nil・真偽値・テーブル等）は何も出力せず、エラーにもなりません。
        // 件数・byte数の上限も存在しないため、上限到達でスクリプトを止めることはありません。
        if values.len() == 1 {
            let bytes = match &values[0] {
                Value::String(text) => Some(text.as_bytes().to_vec()),
                Value::Number(_) | Value::Integer(_) => lua
                    .coerce_string(values[0].clone())
                    .ok()
                    .flatten()
                    .map(|text| text.as_bytes().to_vec()),
                _ => None,
            };
            if let Some(bytes) = bytes {
                let location = caller_location(lua);
                match buffer.try_lock() {
                    Ok(mut buffer) => {
                        buffer.records.push(LogRecord {
                            source: LogSource::Debug,
                            bytes,
                            location,
                        });
                    }
                    Err(TryLockError::Poisoned(poisoned)) => {
                        poisoned.into_inner().records.push(LogRecord {
                            source: LogSource::Debug,
                            bytes,
                            location,
                        });
                    }
                    Err(TryLockError::WouldBlock) => {}
                }
            }
        }
        Ok(())
    })
}
pub(crate) fn install(
    lua: &Lua,
    env: &Table,
    buffer: Arc<Mutex<LogBuffer>>,
    print: bool,
) -> mlua::Result<()> {
    if print {
        env.raw_set("print", print_function(lua, Arc::clone(&buffer))?)?;
    }
    let debug = lua.create_table()?;
    debug.raw_set("log", debug_log_function(lua, buffer)?)?;
    env.raw_set("debug", debug)?;
    Ok(())
}
/// Reader usable during synchronous host callbacks without borrowing the VM.
#[cfg(feature = "backend-mlua")]
#[derive(Clone)]
pub struct LogReader(Arc<Mutex<LogBuffer>>);
#[cfg(feature = "backend-mlua")]
impl LogReader {
    /// Whether records still await host delivery. Continuation callers must not discard them.
    pub fn has_pending(&self) -> bool {
        !lock(&self.0).records.is_empty()
    }

    /// Take pending records, leaving the buffer empty.
    pub fn drain(&self) -> Vec<LogRecord> {
        let mut buffer = lock(&self.0);
        buffer.print_records = 0;
        buffer.print_bytes = 0;
        std::mem::take(&mut buffer.records)
    }
}

impl crate::runner::Vm {
    /// 明示的な開発用拡張として print を組み込みます。debug.log は対象プロファイルへ別途組み込まれます。
    pub fn enable_logs(&mut self) -> Result<(), VmError> {
        self.ensure_idle()?;
        if self.environment_profile != storm_lua_spec::environment::EnvironmentProfile::Extended {
            return Err(VmError::new(
                ErrorKind::InvalidArgument,
                "print requires the extended environment; debug.log is already available",
            ));
        }
        // Extended construction already installed print; keep explicit host overrides intact.
        Ok(())
    }
    /// debug.log を標準APIとして提供するプロファイル向けに、debug.log のみを組み込みます。
    pub fn enable_debug_log(&mut self) -> Result<(), VmError> {
        self.ensure_idle()?;
        install(&self.lua, &self.environment, Arc::clone(&self.logs), false)?;
        self.note_host_path(vec!["debug".into()]);
        self.note_host_path(vec!["debug".into(), "log".into()]);
        Ok(())
    }
    /// Obtain a reader independent of execution borrows.
    #[cfg(feature = "backend-mlua")]
    pub fn log_reader(&self) -> LogReader {
        LogReader(Arc::clone(&self.logs))
    }
    /// 一時停止やエラーの前に出力されたログを含め、所有権を持つログレコードを取り出します。
    pub fn drain_log_records(&mut self) -> Vec<LogRecord> {
        let mut buffer = lock(&self.logs);
        buffer.print_records = 0;
        buffer.print_bytes = 0;
        std::mem::take(&mut buffer.records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_host_call_has_no_invented_lua_location() -> mlua::Result<()> {
        let lua = Lua::new();
        let env = lua.create_table()?;
        let buffer = Arc::new(Mutex::new(LogBuffer::default()));
        install(&lua, &env, Arc::clone(&buffer), true)?;
        let print: mlua::Function = env.raw_get("print")?;
        print.call::<()>("host")?;
        let records = &lock(&buffer).records;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].bytes, b"host");
        assert_eq!(records[0].location, None);
        Ok(())
    }

    #[test]
    fn locations_do_not_require_the_debugger_feature_or_line_hooks() -> mlua::Result<()> {
        let lua = Lua::new();
        let env = lua.create_table()?;
        let buffer = Arc::new(Mutex::new(LogBuffer::default()));
        install(&lua, &env, Arc::clone(&buffer), true)?;
        lua.load("local alias=print\n  alias('line two')")
            .set_name("@plain.lua")
            .set_environment(env)
            .exec()?;
        assert_eq!(
            lock(&buffer).records[0].location,
            Some(LogLocation {
                chunk: "@plain.lua".into(),
                line: 2,
            })
        );
        Ok(())
    }

    #[test]
    fn debug_log_records_only_a_single_string_or_number() -> mlua::Result<()> {
        let lua = Lua::new();
        let env = lua.create_table()?;
        let buffer = Arc::new(Mutex::new(LogBuffer::default()));
        install(&lua, &env, Arc::clone(&buffer), false)?;
        lua.load(
            "debug.log('one')\ndebug.log(42)\ndebug.log(1.5)\ndebug.log()\ndebug.log('a', 'b')\ndebug.log(nil)\ndebug.log(true)\ndebug.log({})\ndebug.log(function() end)",
        )
        .set_environment(env)
        .exec()?;
        let records = &lock(&buffer).records;
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].bytes, b"one");
        assert_eq!(records[1].bytes, b"42");
        assert_eq!(records[2].bytes, b"1.5");
        assert!(records
            .iter()
            .all(|record| record.source == LogSource::Debug));
        Ok(())
    }

    #[test]
    fn debug_log_has_no_line_or_buffer_limits() -> mlua::Result<()> {
        let lua = Lua::new();
        let env = lua.create_table()?;
        let buffer = Arc::new(Mutex::new(LogBuffer::default()));
        install(&lua, &env, Arc::clone(&buffer), false)?;
        env.raw_set("payload", lua.create_string("x".repeat(100000))?)?;
        lua.load("debug.log(payload)\nfor _ = 1, 200 do debug.log('row') end")
            .set_environment(env)
            .exec()?;
        let records = &lock(&buffer).records;
        assert_eq!(records.len(), 201);
        assert_eq!(records[0].bytes.len(), 100000);
        Ok(())
    }

    #[test]
    fn one_execution_logs_well_past_the_removed_caps() -> mlua::Result<()> {
        let lua = Lua::new();
        let env = lua.create_table()?;
        let buffer = Arc::new(Mutex::new(LogBuffer::default()));
        install(&lua, &env, Arc::clone(&buffer), true)?;
        lua.load(
            "for i = 1, 1000 do debug.log('line ' .. i) end\n\
             local long = 'x'\n\
             for _ = 1, 15 do long = long .. long end\n\
             debug.log(long)\n\
             debug.log('after')",
        )
        .set_name("@loglimit.lua")
        .set_environment(env)
        .exec()?;
        let records = &lock(&buffer).records;
        assert_eq!(records.len(), 1002);
        assert_eq!(records[0].bytes, b"line 1");
        assert_eq!(records[999].bytes, b"line 1000");
        assert_eq!(records[1000].bytes.len(), 32768);
        assert_eq!(records[1001].bytes, b"after");
        Ok(())
    }
}
