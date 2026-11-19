//! ホスト呼び出し、イベント、セーブデータスナップショット向けの上限付き所有値。バックエンドのハンドルは外部に漏洩しません。
use crate::runner::{ErrorKind, VmError};
use mlua::{Lua, MultiValue, Table, Value};
use std::collections::HashSet;

/// 損失のない非循環Luaデータ。テーブルはスカラーキー、欠番（holes）、異なるキー型の混在をそのまま保持します。
#[derive(Debug, Clone, PartialEq)]
pub enum LuaValue {
    /// nilの結果または引数（末尾のnilを含む）。
    Nil,
    /// ブール値。
    Bool(bool),
    /// 符号付き64ビット整数（f64を仲介して精度が失われることはありません）。
    Integer(i64),
    /// 符号付きゼロや非有限値を含むbinary64値。
    Number(f64),
    /// 任意のLua文字列バイト列。
    Bytes(Vec<u8>),
    /// 生のエントリ群。キーはnil以外のスカラー、値はnil以外である必要があり、重複キーは拒否されます。
    Table(Vec<(LuaValue, LuaValue)>),
}
impl LuaValue {
    /// 基礎となるバイト文字列契約を変更することなく、UTF-8 Lua文字列を構築します。
    pub fn text(text: impl AsRef<str>) -> Self {
        Self::Bytes(text.as_ref().as_bytes().to_vec())
    }
    /// 文字列キーのレコードを構築します。
    pub fn record(entries: impl IntoIterator<Item = (impl AsRef<str>, Self)>) -> Self {
        Self::Table(
            entries
                .into_iter()
                .map(|(key, value)| (Self::text(key), value))
                .collect(),
        )
    }
}
/// テーブルや引数ごとではなく、転送全体で共有される単一の集約バジェット。
#[derive(Default)]
struct Budget {
    nodes: usize,
    bytes: usize,
}
impl Budget {
    fn charge(&mut self, depth: usize, bytes: usize) -> Result<(), VmError> {
        self.nodes += 1;
        self.bytes = self.bytes.checked_add(bytes).ok_or_else(limit)?;
        if depth > 32 || self.nodes > 65536 || self.bytes > 1024 * 1024 {
            return Err(limit());
        }
        Ok(())
    }
}
fn limit() -> VmError {
    VmError::new(
        ErrorKind::Limit,
        "host value exceeds depth, node or byte budget",
    )
}
fn invalid(message: &str) -> VmError {
    VmError::new(ErrorKind::InvalidArgument, message)
}
fn key_allowed(value: &LuaValue) -> bool {
    match value {
        LuaValue::Nil | LuaValue::Table(_) => false,
        LuaValue::Number(n) => !n.is_nan(),
        _ => true,
    }
}
fn to_lua(
    lua: &Lua,
    value: &LuaValue,
    budget: &mut Budget,
    depth: usize,
) -> Result<Value, VmError> {
    budget.charge(
        depth,
        if let LuaValue::Bytes(v) = value {
            v.len()
        } else {
            0
        },
    )?;
    Ok(match value {
        LuaValue::Nil => Value::Nil,
        LuaValue::Bool(v) => Value::Boolean(*v),
        LuaValue::Integer(v) => Value::Integer(*v),
        LuaValue::Number(v) => Value::Number(*v),
        LuaValue::Bytes(v) => Value::String(lua.create_string(v)?),
        LuaValue::Table(entries) => {
            if entries.len() > 32768 {
                return Err(limit());
            }
            let table = lua.create_table()?;
            for (key, value) in entries {
                if !key_allowed(key) || matches!(value, LuaValue::Nil) {
                    return Err(invalid("invalid table key or nil table value"));
                }
                let key = to_lua(lua, key, budget, depth + 1)?;
                if !table.raw_get::<Value>(key.clone())?.is_nil() {
                    return Err(invalid("duplicate table key"));
                }
                table.raw_set(key, to_lua(lua, value, budget, depth + 1)?)?;
            }
            Value::Table(table)
        }
    })
}
fn from_lua(
    value: Value,
    budget: &mut Budget,
    depth: usize,
    active: &mut HashSet<usize>,
) -> Result<LuaValue, VmError> {
    budget.charge(
        depth,
        if let Value::String(v) = &value {
            v.as_bytes().len()
        } else {
            0
        },
    )?;
    Ok(match value {
        Value::Nil => LuaValue::Nil,
        Value::Boolean(v) => LuaValue::Bool(v),
        Value::Integer(v) => LuaValue::Integer(v),
        Value::Number(v) => LuaValue::Number(v),
        Value::String(v) => LuaValue::Bytes(v.as_bytes().to_vec()),
        Value::Table(table) => {
            let pointer = table.to_pointer() as usize;
            if !active.insert(pointer) {
                return Err(invalid(
                    "cyclic tables cannot cross the owned-value boundary",
                ));
            }
            if table.metatable().is_some() {
                return Err(invalid(
                    "tables with metatables cannot cross the owned-value boundary",
                ));
            }
            let mut entries = Vec::new();
            for pair in table.pairs::<Value, Value>() {
                let (key, value) = pair?;
                let key = from_lua(key, budget, depth + 1, active)?;
                if !key_allowed(&key) {
                    return Err(invalid("table keys must be scalar"));
                }
                entries.push((key, from_lua(value, budget, depth + 1, active)?));
            }
            active.remove(&pointer);
            LuaValue::Table(entries)
        }
        _ => {
            return Err(invalid(
                "functions, threads and userdata cannot cross the owned-value boundary",
            ))
        }
    })
}
/// 共有リソースバジェットを用いて引数／結果リスト全体を変換します。
pub(crate) fn encode(lua: &Lua, values: &[LuaValue]) -> Result<MultiValue, VmError> {
    let mut budget = Budget::default();
    values
        .iter()
        .map(|v| to_lua(lua, v, &mut budget, 0))
        .collect()
}
/// メタメソッドを実行せずに生の値をコピーします。共有された部分木は値としてコピーされます。
pub(crate) fn decode(values: MultiValue) -> Result<Vec<LuaValue>, VmError> {
    let mut budget = Budget::default();
    let mut active = HashSet::new();
    values
        .into_iter()
        .map(|v| from_lua(v, &mut budget, 0, &mut active))
        .collect()
}
/// 同期ホスト関数。Lua実行が継続する前にサーバーへの問い合わせが完了して返る必要があります。
pub type HostFunction =
    std::sync::Arc<dyn Fn(&[LuaValue]) -> Result<Vec<LuaValue>, VmError> + Send + Sync>;
impl crate::runner::Vm {
    /// 明示的に提供されたホスト関数を登録します。これは信頼されたホストによる拡張です。
    pub fn register_function(
        &mut self,
        namespace: &str,
        name: &str,
        handler: HostFunction,
    ) -> Result<(), VmError> {
        self.ensure_idle()?;
        let table: Table = self.environment.raw_get(namespace)?;
        table.raw_set(
            name,
            self.lua.create_function(move |lua, args: MultiValue| {
                let args = decode(args).map_err(mlua::Error::external)?;
                let values = handler(&args).map_err(mlua::Error::external)?;
                encode(lua, &values).map_err(mlua::Error::external)
            })?,
        )?;
        self.note_host_path(vec![namespace.to_string()]);
        self.note_host_path(vec![namespace.to_string(), name.to_string()]);
        Ok(())
    }
    /// アイドル境界でグローバル変数の生データをコピーします（セーブスナップショット用）。
    pub fn global(&self, name: &str) -> Result<LuaValue, VmError> {
        self.ensure_idle()?;
        let value = self.environment.raw_get::<Value>(name)?;
        from_lua(value, &mut Budget::default(), 0, &mut HashSet::new())
    }
    /// 完全な所有値を検証した後に、1つのグローバル変数をアトミックに置換します。
    pub fn set_global(&mut self, name: &str, value: &LuaValue) -> Result<(), VmError> {
        self.ensure_idle()?;
        let value = to_lua(&self.lua, value, &mut Budget::default(), 0)?;
        self.environment.raw_set(name, value)?;
        Ok(())
    }
}
