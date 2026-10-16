//! ホスト専用デバッガ、生の内部状態検査、および副作用を明示的に伴うウォッチ式評価。

/// 1つのVMおよび1つの中断エポックにスコープされた一時停止オブジェクト参照。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DebugHandle {
    /// VMの識別子（生のポインタではありません）。
    pub vm_id: u64,
    /// 再開、リセット、または破棄時に無効化されます。
    pub pause_epoch: u64,
    /// 停止ごとのレジストリスロット。
    pub slot: u32,
}

/// 損失のないデバッガペイロード。文字列を暗黙にUTF-8としてデコードしてはなりません。
#[derive(Debug, Clone, PartialEq)]
pub enum DebugValue {
    /// Luaのnil。
    Nil,
    /// LuaのBoolean。
    Bool(bool),
    /// Luaの符号付き整数。JSアダプタ側ではbigintまたはタグ付き10進文字列を使用する必要があります。
    Integer(i64),
    /// 非有限値や負のゼロを含むLua浮動小数点数。
    Number(f64),
    /// Lua文字列のバイト列。
    Bytes(Vec<u8>),
    /// 遅延展開されるテーブル（Luaヒープを再帰的にシリアライズしません）。
    Table(DebugHandle),
    /// 関数、スレッド、またはuserdataの検査不可能なスカラー表現。
    Opaque(&'static str),
}

use crate::runner::{ErrorKind, Vm, VmError};
use mlua::{ChunkMode, Function, Lua, Table, Thread, Value};
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT_VM: AtomicU64 = AtomicU64::new(1);
pub(crate) struct Debugger {
    pub(crate) library: Table,
    vm_id: u64,
    epoch: u64,
    tables: Vec<Table>,
    pub(crate) hook_dirty: bool,
}
impl Debugger {
    pub(crate) fn new(lua: &Lua) -> mlua::Result<Self> {
        let vm_id = NEXT_VM
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| mlua::Error::RuntimeError("debug VM identifiers exhausted".into()))?;
        Ok(Self {
            library: lua.globals().get("debug")?,
            vm_id,
            epoch: 0,
            tables: Vec::new(),
            hook_dirty: false,
        })
    }
    pub(crate) fn invalidate(&mut self) -> Result<(), VmError> {
        self.epoch = self
            .epoch
            .checked_add(1)
            .ok_or_else(|| VmError::new(ErrorKind::Limit, "debug epochs exhausted"))?;
        self.tables.clear();
        Ok(())
    }
    fn value(&mut self, value: Value) -> Result<DebugValue, VmError> {
        Ok(match value {
            Value::Nil => DebugValue::Nil,
            Value::Boolean(b) => DebugValue::Bool(b),
            Value::Integer(i) => DebugValue::Integer(i),
            Value::Number(n) => DebugValue::Number(n),
            Value::String(s) => DebugValue::Bytes(s.as_bytes().to_vec()),
            Value::Table(table) => {
                if self.tables.len() >= 4096 {
                    return Err(VmError::new(
                        ErrorKind::Limit,
                        "too many debug object handles",
                    ));
                }
                let slot = self.tables.len() as u32;
                self.tables.push(table);
                DebugValue::Table(DebugHandle {
                    vm_id: self.vm_id,
                    pause_epoch: self.epoch,
                    slot,
                })
            }
            other => DebugValue::Opaque(other.type_name()),
        })
    }
}
/// 1つのソーススタックフレーム。現在中断しているフレームを0として番号付けされます。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackFrame {
    /// スタック深度インデックス。
    pub level: u32,
    /// 元のチャンク識別子。
    pub source: String,
    /// 現在のソース行番号（Cフレームの場合は -1）。
    pub line: i32,
    /// 取得可能な関数名。
    pub function_name: Option<String>,
}
/// 1つの生のローカル変数またはupvalue。内部名もそのまま観測可能です。
#[derive(Debug, Clone, PartialEq)]
pub struct Variable {
    /// Luaデバッグ情報から提供される識別子バイト列。
    pub name: Vec<u8>,
    /// 損失のないスカラーまたは遅延展開オブジェクト。
    pub value: DebugValue,
}
/// 生のテーブルエントリ。キーは値そのものであり、メタメソッドによって文字列化されません。
#[derive(Debug, Clone, PartialEq)]
pub struct TableEntry {
    /// 元のキー。
    pub key: DebugValue,
    /// 元の値。
    pub value: DebugValue,
}
impl Vm {
    fn paused_thread(&self) -> Result<Thread, VmError> {
        self.pending.clone().ok_or_else(|| {
            VmError::new(
                ErrorKind::InvalidArgument,
                "debug inspection requires a paused callback",
            )
        })
    }
    fn frame_function(&self, thread: &Thread, level: u32) -> Result<Function, VmError> {
        let get: Function = self.debugger.library.raw_get("getinfo")?;
        let info: Option<Table> = get.call((thread.clone(), level, "f"))?;
        Ok(info
            .ok_or_else(|| VmError::new(ErrorKind::InvalidArgument, "stack frame does not exist"))?
            .raw_get("func")?)
    }
    /// ユーザーのメタメソッドを呼び出すことなく、最大256件の生のスタックフレームを読み取ります。
    pub fn stack(&self) -> Result<Vec<StackFrame>, VmError> {
        let thread = self.paused_thread()?;
        let get: Function = self.debugger.library.raw_get("getinfo")?;
        let mut frames = Vec::new();
        for level in 0..256_u32 {
            let info: Option<Table> = get.call((thread.clone(), level, "nSl"))?;
            let Some(info) = info else {
                break;
            };
            frames.push(StackFrame {
                level,
                source: info.raw_get("source")?,
                line: if level == 0 {
                    self.control
                        .borrow()
                        .last_line
                        .as_ref()
                        .map_or(info.raw_get::<i32>("currentline")?, |(_, line)| *line)
                } else {
                    info.raw_get("currentline")?
                },
                function_name: info.raw_get("name")?,
            });
        }
        Ok(frames)
    }
    /// VM内部名を含むローカル変数を読み取ります。どの名前を表示するかは利用側が決定します。
    pub fn locals(&mut self, level: u32) -> Result<Vec<Variable>, VmError> {
        let thread = self.paused_thread()?;
        self.frame_function(&thread, level)?;
        let get: Function = self.debugger.library.raw_get("getlocal")?;
        let mut result = Vec::new();
        let captured = if level == 0 {
            self.control.borrow().local_names.clone()
        } else {
            Vec::new()
        };
        for index in 1..=1024 {
            let (name, value): (Option<mlua::String>, Value) =
                get.call((thread.clone(), level, index))?;
            let override_name = captured
                .iter()
                .find(|(i, _)| *i == index)
                .map(|(_, name)| name.clone());
            let name = match (override_name, name) {
                (Some(name), _) => name,
                (None, Some(name)) => name.as_bytes().to_vec(),
                (None, None) => break,
            };
            result.push(Variable {
                name,
                value: self.debugger.value(value)?,
            });
        }
        Ok(result)
    }
    /// 中断中の関数のupvalueを検査します。信頼できるホストに対して _ENV は隠蔽されません。
    pub fn upvalues(&mut self, level: u32) -> Result<Vec<Variable>, VmError> {
        let thread = self.paused_thread()?;
        let function = self.frame_function(&thread, level)?;
        let get: Function = self.debugger.library.raw_get("getupvalue")?;
        let mut result = Vec::new();
        for index in 1..=256 {
            let (name, value): (Option<mlua::String>, Value) =
                get.call((function.clone(), index))?;
            let Some(name) = name else {
                break;
            };
            result.push(Variable {
                name: name.as_bytes().to_vec(),
                value: self.debugger.value(value)?,
            });
        }
        Ok(result)
    }
    /// __pairs、__index、__tostring などの呼び出しを行わずに、生のテーブルページを列挙します。
    pub fn expand_table(
        &mut self,
        handle: DebugHandle,
        start: usize,
        limit: usize,
    ) -> Result<Vec<TableEntry>, VmError> {
        self.paused_thread()?;
        if handle.vm_id != self.debugger.vm_id || handle.pause_epoch != self.debugger.epoch {
            return Err(VmError::new(
                ErrorKind::InvalidArgument,
                "stale debug handle",
            ));
        }
        if limit == 0 || limit > 256 || start > 1_000_000 {
            return Err(VmError::new(
                ErrorKind::InvalidArgument,
                "invalid debug page",
            ));
        }
        let table = self
            .debugger
            .tables
            .get(handle.slot as usize)
            .cloned()
            .ok_or_else(|| VmError::new(ErrorKind::InvalidArgument, "unknown debug handle"))?;
        let mut values = Vec::new();
        for pair in table.pairs::<Value, Value>().skip(start).take(limit) {
            let (key, value) = pair?;
            values.push(TableEntry {
                key: self.debugger.value(key)?,
                value: self.debugger.value(value)?,
            });
        }
        Ok(values)
    }
    /// 中断中のフレーム内で式を評価します。共有テーブルを変更したり関数を呼び出したりする可能性があります。
    /// 既存のデバッグハンドルは無効化されます。これは明示的に読み取り専用操作ではありません。
    pub fn evaluate_watch(&mut self, level: u32, expression: &str) -> Result<DebugValue, VmError> {
        if expression.len() > 16384 {
            return Err(VmError::new(
                ErrorKind::Limit,
                "watch expression is too large",
            ));
        }
        let thread = self.paused_thread()?;
        let function = self.frame_function(&thread, level)?;
        let scope = self.lua.create_table()?;
        let mut bound_names = std::collections::HashSet::new();
        let mut environment = self.environment.clone();
        let getup: Function = self.debugger.library.raw_get("getupvalue")?;
        for i in 1..=256 {
            let (name, value): (Option<mlua::String>, Value) = getup.call((function.clone(), i))?;
            let Some(name) = name else {
                break;
            };
            if name.as_bytes().as_ref() == b"_ENV" {
                if let Value::Table(t) = &value {
                    environment = t.clone();
                }
            }
            bound_names.insert(name.as_bytes().to_vec());
            scope.raw_set(name, value)?;
        }
        let getlocal: Function = self.debugger.library.raw_get("getlocal")?;
        let captured = if level == 0 {
            self.control.borrow().local_names.clone()
        } else {
            Vec::new()
        };
        for i in 1..=1024 {
            let (name, value): (Option<mlua::String>, Value) =
                getlocal.call((thread.clone(), level, i))?;
            let name = match (captured.iter().find(|(index, _)| *index == i), name) {
                (Some((_, name)), _) => name.clone(),
                (None, Some(name)) => name.as_bytes().to_vec(),
                (None, None) => break,
            };
            if !name.starts_with(b"(") {
                bound_names.insert(name.clone());
                scope.raw_set(self.lua.create_string(name)?, value)?;
            }
        }
        let meta = self.lua.create_table()?;
        meta.raw_set(
            "__index",
            self.lua
                .create_function(move |_, (_scope, key): (Table, Value)| {
                    if let Value::String(name) = &key {
                        if bound_names.contains(name.as_bytes().as_ref()) {
                            return Ok(Value::Nil);
                        }
                    }
                    environment.raw_get::<Value>(key)
                })?,
        )?;
        scope.set_metatable(Some(meta));
        self.debugger.invalidate()?;
        let function = self
            .lua
            .load(format!("return ({expression})"))
            .set_name("=watch")
            .set_mode(ChunkMode::Text)
            .set_environment(scope)
            .into_function()?;
        let evaluator = self.lua.create_thread(function)?;
        self.debugger.hook_dirty = true;
        let value = self.evaluate_watch_thread(&evaluator)?;
        self.debugger.value(value)
    }
}
