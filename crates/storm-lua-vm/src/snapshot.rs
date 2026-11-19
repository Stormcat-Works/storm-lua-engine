//! アイドル境界でのVMヒープ全体のスナップショットと復元。
//!
//! 保存対象はサンドボックス環境テーブルを根とする到達可能グラフ全体です。
//! テーブルとクロージャとアップ値セルは同一性を保持し、循環もそのまま往復します。
//! ホストが種付けした値はシンボリックなパス参照として記録され、復元先VMの
//! 対応する現行インスタンスへ再束縛されます。再実行は一切行いません。
//!
//! スクリプトから到達可能な位置にスレッド・ユーザデータ・メタテーブル付き
//! テーブル・未登録のホスト関数が現れた場合は、黙って初期化したりせず
//! 型付きエラーで失敗します。本番プロファイルではいずれも生成できないため、
//! 出現はバグの兆候として扱います。
#![allow(unsafe_code)]

use crate::runner::{ErrorKind, Vm, VmError};
use mlua::{ffi, Function, Lua, Table, Value};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

/// スナップショットバイナリ形式のバージョン。互換性のない変更で増やします。
pub const FORMAT_VERSION: u32 = 1;

const MAGIC: &[u8; 8] = b"SLVMSNP\0";
/// 壊れた入力に対する防御上限（ヒープ上限8MiBとは独立の健全性レール）。
const MAX_BLOB_BYTES: usize = 256 * 1024 * 1024;
const MAX_NODES: u32 = 4_194_304;
const MAX_STRING_BYTES: u32 = 64 * 1024 * 1024;
/// Aggregate payload bytes copied into GVal::Str during one capture, including
/// keys, values, host tables and upvalue cells. Eight times the default Lua heap
/// budget, but only a quarter of decode's MAX_BLOB_BYTES, leaving room for graph
/// metadata. Reuse decode's per-string cap as the aggregate capture allowance;
/// this bounds string amplification, not all capture/encoding allocations.
const MAX_CAPTURE_STRING_BYTES: usize = MAX_STRING_BYTES as usize;

fn unsupported(message: impl Into<String>) -> VmError {
    VmError::new(ErrorKind::Unsupported, message)
}
fn invalid(message: impl Into<String>) -> VmError {
    VmError::new(ErrorKind::InvalidArgument, message)
}
fn limit(message: impl Into<String>) -> VmError {
    VmError::new(ErrorKind::Limit, message)
}

// ---------------------------------------------------------------------------
// グラフ表現（バイナリ形式と1対1に対応する中間表現）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum GVal {
    Nil,
    Bool(bool),
    Int(i64),
    /// f64のビットパターンをそのまま保持します（NaNペイロード・符号付き零を区別）。
    Num(u64),
    Str(Vec<u8>),
    /// 通常テーブルへの参照（tables[0]は環境ルート）。
    Table(u32),
    /// ホストテーブル定義への参照（host_tablesの添字。定義側がシンボルを持つ）。
    HostTable(u32),
    Closure(u32),
    /// 登録済みホスト関数への参照（symbolsの添字）。
    HostRef(u32),
    /// ホストテーブルの直接の値である未登録C関数。復元先ホストテーブルの
    /// 同一キー現行エントリをそのまま使うことを表します。値位置でのみ有効です。
    HostEntry,
}

#[derive(Debug, Default)]
struct TableDef {
    entries: Vec<(GVal, GVal)>,
}

#[derive(Debug)]
struct ClosureDef {
    proto: u32,
    cells: Vec<u32>,
}

#[derive(Debug)]
struct SnapshotData {
    profile: u8,
    symbols: Vec<Vec<String>>,
    protos: Vec<Vec<u8>>,
    tables: Vec<TableDef>,
    host_tables: Vec<(u32, TableDef)>,
    closures: Vec<ClosureDef>,
    cells: Vec<GVal>,
    root: GVal,
}

// ---------------------------------------------------------------------------
// バイナリ読み書き（全て境界検査付き。パニック経路を持ちません）
// ---------------------------------------------------------------------------

struct Writer {
    buf: Vec<u8>,
}
impl Writer {
    #[cfg(test)]
    fn new() -> Self {
        Self::with_capacity(0)
    }
    fn with_capacity(capacity: usize) -> Self {
        Self {
            buf: Vec::with_capacity(capacity),
        }
    }
    fn u8(&mut self, value: u8) {
        self.buf.push(value);
    }
    fn u16(&mut self, value: u16) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }
    fn u32(&mut self, value: u32) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }
    fn u64(&mut self, value: u64) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }
    fn i64(&mut self, value: i64) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }
    fn bytes(&mut self, value: &[u8]) {
        self.u32(value.len() as u32);
        self.buf.extend_from_slice(value);
    }
    fn gval(&mut self, value: &GVal) {
        match value {
            GVal::Nil => self.u8(0),
            GVal::Bool(v) => {
                self.u8(1);
                self.u8(u8::from(*v));
            }
            GVal::Int(v) => {
                self.u8(2);
                self.i64(*v);
            }
            GVal::Num(bits) => {
                self.u8(3);
                self.u64(*bits);
            }
            GVal::Str(bytes) => {
                self.u8(4);
                self.bytes(bytes);
            }
            GVal::Table(id) => {
                self.u8(5);
                self.u32(*id);
            }
            GVal::HostTable(symbol) => {
                self.u8(6);
                self.u32(*symbol);
            }
            GVal::Closure(id) => {
                self.u8(7);
                self.u32(*id);
            }
            GVal::HostRef(symbol) => {
                self.u8(8);
                self.u32(*symbol);
            }
            GVal::HostEntry => self.u8(9),
        }
    }
    fn table_def(&mut self, def: &TableDef) {
        self.u32(def.entries.len() as u32);
        for (key, value) in &def.entries {
            self.gval(key);
            self.gval(value);
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    nodes: u32,
}
impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            pos: 0,
            nodes: 0,
        }
    }
    fn take(&mut self, count: usize) -> Result<&'a [u8], VmError> {
        let end = self
            .pos
            .checked_add(count)
            .ok_or_else(|| invalid("snapshot blob length overflow"))?;
        let slice = self
            .bytes
            .get(self.pos..end)
            .ok_or_else(|| invalid("snapshot blob is truncated"))?;
        self.pos = end;
        Ok(slice)
    }
    fn u8(&mut self) -> Result<u8, VmError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, VmError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, VmError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Result<u64, VmError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
    fn i64(&mut self) -> Result<i64, VmError> {
        Ok(self.u64()? as i64)
    }
    fn count(&mut self, what: &str) -> Result<u32, VmError> {
        let count = self.u32()?;
        self.nodes = self
            .nodes
            .checked_add(count)
            .filter(|total| *total <= MAX_NODES)
            .ok_or_else(|| limit(format!("snapshot {what} count exceeds the sanity cap")))?;
        Ok(count)
    }
    fn bytes(&mut self) -> Result<Vec<u8>, VmError> {
        let len = self.u32()?;
        if len > MAX_STRING_BYTES {
            return Err(limit("snapshot byte string exceeds the sanity cap"));
        }
        Ok(self.take(len as usize)?.to_vec())
    }
    fn gval(&mut self) -> Result<GVal, VmError> {
        Ok(match self.u8()? {
            0 => GVal::Nil,
            1 => match self.u8()? {
                0 => GVal::Bool(false),
                1 => GVal::Bool(true),
                _ => return Err(invalid("snapshot boolean value is invalid")),
            },
            2 => GVal::Int(self.i64()?),
            3 => GVal::Num(self.u64()?),
            4 => GVal::Str(self.bytes()?),
            5 => GVal::Table(self.u32()?),
            6 => GVal::HostTable(self.u32()?),
            7 => GVal::Closure(self.u32()?),
            8 => GVal::HostRef(self.u32()?),
            9 => GVal::HostEntry,
            tag => return Err(invalid(format!("snapshot value tag {tag} is unknown"))),
        })
    }
    fn table_def(&mut self) -> Result<TableDef, VmError> {
        let count = self.count("table entry")?;
        let mut entries = Vec::with_capacity(count.min(1 << 16) as usize);
        for _ in 0..count {
            let key = self.gval()?;
            let value = self.gval()?;
            entries.push((key, value));
        }
        Ok(TableDef { entries })
    }
}

impl SnapshotData {
    fn gval_encoded_len(value: &GVal) -> usize {
        match value {
            GVal::Nil | GVal::HostEntry => 1,
            GVal::Bool(_) => 2,
            GVal::Int(_) | GVal::Num(_) => 9,
            GVal::Str(bytes) => 5 + bytes.len(),
            GVal::Table(_) | GVal::HostTable(_) | GVal::Closure(_) | GVal::HostRef(_) => 5,
        }
    }

    fn table_def_encoded_len(def: &TableDef) -> usize {
        4 + def
            .entries
            .iter()
            .map(|(k, v)| Self::gval_encoded_len(k) + Self::gval_encoded_len(v))
            .sum::<usize>()
    }

    fn encoded_len(&self) -> usize {
        let mut total = MAGIC.len() + 4 + 1;
        total += 4;
        for path in &self.symbols {
            total += 1;
            for seg in path {
                total += 2 + seg.len();
            }
        }
        total += 4 + self.protos.iter().map(|p| 4 + p.len()).sum::<usize>();
        total += 4 + self
            .tables
            .iter()
            .map(Self::table_def_encoded_len)
            .sum::<usize>();
        total += 4 + self
            .host_tables
            .iter()
            .map(|(_, d)| 4 + Self::table_def_encoded_len(d))
            .sum::<usize>();
        total += 4 + self
            .closures
            .iter()
            .map(|c| 8 + c.cells.len() * 4)
            .sum::<usize>();
        total += 4 + self.cells.iter().map(Self::gval_encoded_len).sum::<usize>();
        total += Self::gval_encoded_len(&self.root);
        total
    }

    fn encode(&self) -> Vec<u8> {
        let mut writer = Writer::with_capacity(self.encoded_len());
        writer.buf.extend_from_slice(MAGIC);
        writer.u32(FORMAT_VERSION);
        writer.u8(self.profile);
        writer.u32(self.symbols.len() as u32);
        for path in &self.symbols {
            writer.u8(path.len() as u8);
            for segment in path {
                writer.u16(segment.len() as u16);
                writer.buf.extend_from_slice(segment.as_bytes());
            }
        }
        writer.u32(self.protos.len() as u32);
        for proto in &self.protos {
            writer.bytes(proto);
        }
        writer.u32(self.tables.len() as u32);
        for def in &self.tables {
            writer.table_def(def);
        }
        writer.u32(self.host_tables.len() as u32);
        for (symbol, def) in &self.host_tables {
            writer.u32(*symbol);
            writer.table_def(def);
        }
        writer.u32(self.closures.len() as u32);
        for def in &self.closures {
            writer.u32(def.proto);
            writer.u32(def.cells.len() as u32);
            for cell in &def.cells {
                writer.u32(*cell);
            }
        }
        writer.u32(self.cells.len() as u32);
        for cell in &self.cells {
            writer.gval(cell);
        }
        writer.gval(&self.root);
        writer.buf
    }

    fn decode(bytes: &[u8]) -> Result<Self, VmError> {
        if bytes.len() > MAX_BLOB_BYTES {
            return Err(limit("snapshot blob exceeds the sanity cap"));
        }
        let mut reader = Reader::new(bytes);
        if reader.take(8)? != MAGIC {
            return Err(invalid("snapshot blob has a bad magic header"));
        }
        let version = reader.u32()?;
        if version != FORMAT_VERSION {
            return Err(unsupported(format!(
                "snapshot format version {version} is not supported (expected {FORMAT_VERSION})"
            )));
        }
        let profile = reader.u8()?;
        if profile > 2 {
            return Err(invalid("snapshot profile byte is out of range"));
        }
        let symbol_count = reader.count("symbol")?;
        let mut symbols = Vec::with_capacity(symbol_count.min(1 << 16) as usize);
        for _ in 0..symbol_count {
            let segments = reader.u8()?;
            if segments == 0 {
                return Err(invalid("snapshot symbol path is empty"));
            }
            let mut path = Vec::with_capacity(segments as usize);
            for _ in 0..segments {
                let len = reader.u16()? as usize;
                if len == 0 {
                    return Err(invalid("snapshot symbol segment is empty"));
                }
                let raw = reader.take(len)?;
                let segment = String::from_utf8(raw.to_vec())
                    .map_err(|_| invalid("snapshot symbol path is not UTF-8"))?;
                path.push(segment);
            }
            symbols.push(path);
        }
        let proto_count = reader.count("prototype")?;
        let mut protos = Vec::with_capacity(proto_count.min(1 << 16) as usize);
        for _ in 0..proto_count {
            protos.push(reader.bytes()?);
        }
        let table_count = reader.count("table")?;
        let mut tables = Vec::with_capacity(table_count.min(1 << 16) as usize);
        for _ in 0..table_count {
            tables.push(reader.table_def()?);
        }
        let host_table_count = reader.count("host table")?;
        let mut host_tables = Vec::with_capacity(host_table_count.min(1 << 16) as usize);
        for _ in 0..host_table_count {
            let symbol = reader.u32()?;
            if symbol as usize >= symbols.len() {
                return Err(invalid("snapshot host table symbol is out of range"));
            }
            host_tables.push((symbol, reader.table_def()?));
        }
        let closure_count = reader.count("closure")?;
        let mut closures = Vec::with_capacity(closure_count.min(1 << 16) as usize);
        for _ in 0..closure_count {
            let proto = reader.u32()?;
            if proto as usize >= protos.len() {
                return Err(invalid("snapshot closure prototype is out of range"));
            }
            let cell_count = reader.count("closure upvalue")?;
            if cell_count > 4096 {
                return Err(limit("function upvalue count exceeds the sanity cap"));
            }
            let mut cells = Vec::with_capacity(cell_count.min(1 << 16) as usize);
            for _ in 0..cell_count {
                let cell = reader.u32()?;
                cells.push(cell);
            }
            closures.push(ClosureDef { proto, cells });
        }
        let cell_count = reader.count("cell")?;
        let mut cells = Vec::with_capacity(cell_count.min(1 << 16) as usize);
        for _ in 0..cell_count {
            cells.push(reader.gval()?);
        }
        for def in &closures {
            for cell in &def.cells {
                if *cell as usize >= cells.len() {
                    return Err(invalid("snapshot closure cell reference is out of range"));
                }
            }
        }
        let root = reader.gval()?;
        if reader.pos != reader.bytes.len() {
            return Err(invalid("snapshot blob has trailing bytes"));
        }
        Ok(Self {
            profile,
            symbols,
            protos,
            tables,
            host_tables,
            closures,
            cells,
            root,
        })
    }

    fn validate_references(&self) -> Result<(), VmError> {
        #[derive(PartialEq, Eq, Hash)]
        enum GKey<'a> {
            Bool(bool),
            Int(i64),
            Num(u64),
            Str(&'a [u8]),
            Table(u32),
            HostTable(u32),
            Closure(u32),
            HostRef(u32),
        }

        let mut seen_symbols: HashSet<&[String]> = HashSet::with_capacity(self.symbols.len());
        for path in &self.symbols {
            if path.is_empty() || path.iter().any(String::is_empty) {
                return Err(invalid("snapshot symbol path has an empty segment"));
            }
            if !seen_symbols.insert(path.as_slice()) {
                return Err(invalid("snapshot symbol table contains duplicate path"));
            }
        }
        for proto in &self.protos {
            validate_lua53_chunk(proto)?;
        }
        for def in &self.closures {
            let expected = usize::from(self.protos[def.proto as usize][33]);
            if def.cells.len() != expected {
                return Err(invalid(format!(
                    "snapshot closure upvalue count {expected} does not match the recorded {}",
                    def.cells.len()
                )));
            }
        }
        let mut seen_host_tables: HashSet<u32> = HashSet::with_capacity(self.host_tables.len());
        for (symbol, _) in &self.host_tables {
            if *symbol as usize >= self.symbols.len() {
                return Err(invalid("snapshot host table symbol is out of range"));
            }
            if !seen_host_tables.insert(*symbol) {
                return Err(invalid("snapshot host table symbol appears multiple times"));
            }
        }

        let check = |value: &GVal, allow_host_entry: bool| -> Result<(), VmError> {
            match value {
                GVal::Table(id) if *id as usize >= self.tables.len() => {
                    Err(invalid("snapshot table reference is out of range"))
                }
                GVal::HostTable(id) if *id as usize >= self.host_tables.len() => {
                    Err(invalid("snapshot host table reference is out of range"))
                }
                GVal::Closure(id) if *id as usize >= self.closures.len() => {
                    Err(invalid("snapshot closure reference is out of range"))
                }
                GVal::HostRef(symbol) if *symbol as usize >= self.symbols.len() => {
                    Err(invalid("snapshot symbol reference is out of range"))
                }
                GVal::HostEntry if !allow_host_entry => Err(invalid(
                    "snapshot host entry reference appeared outside a host table",
                )),
                _ => Ok(()),
            }
        };
        let validate_table = |def: &TableDef, host: bool| -> Result<(), VmError> {
            let mut seen_keys: HashSet<GKey<'_>> = HashSet::with_capacity(def.entries.len());
            for (key, value) in &def.entries {
                check(key, false)?;
                check(value, host)?;
                if matches!(value, GVal::Nil) {
                    return Err(invalid("snapshot table value cannot be nil"));
                }
                let gkey = match key {
                    GVal::Nil => return Err(invalid("snapshot table key cannot be nil")),
                    GVal::Bool(v) => GKey::Bool(*v),
                    GVal::Int(v) => GKey::Int(*v),
                    GVal::Num(bits) => {
                        let n = f64::from_bits(*bits);
                        if n.is_nan() {
                            return Err(invalid("snapshot table key cannot be NaN"));
                        }
                        if (-9223372036854775808.0..9223372036854775808.0).contains(&n)
                            && n.floor() == n
                        {
                            return Err(invalid(
                                "snapshot table float key has an exact integer representation",
                            ));
                        }
                        GKey::Num(*bits)
                    }
                    GVal::Str(bytes) => GKey::Str(bytes.as_slice()),
                    GVal::Table(id) => GKey::Table(*id),
                    GVal::HostTable(id) => GKey::HostTable(*id),
                    GVal::Closure(id) => GKey::Closure(*id),
                    GVal::HostRef(sym) => GKey::HostRef(*sym),
                    GVal::HostEntry => {
                        return Err(invalid(
                            "snapshot host entry reference appeared outside a host table",
                        ))
                    }
                };
                if !seen_keys.insert(gkey) {
                    return Err(invalid("snapshot table contains duplicate key"));
                }
            }
            Ok(())
        };
        for def in &self.tables {
            validate_table(def, false)?;
        }
        for (_, def) in &self.host_tables {
            validate_table(def, true)?;
        }
        for cell in &self.cells {
            check(cell, false)?;
        }
        check(&self.root, false)
    }
}

// ---------------------------------------------------------------------------
// 狭いFFI境界（backend_support.rsと同じ流儀）。
// アップ値の列挙・同一性・結合と、バイナリチャンクのロードだけを行います。
// ---------------------------------------------------------------------------

/// 関数がCクロージャ（ホスト関数）かどうか。
fn is_c_function(lua: &Lua, function: &Function) -> Result<bool, VmError> {
    let mut result = false;
    // SAFETY: ルート化された1つのFunction引数が一時スタック上にあり、読み取り専用の
    // 照会の後に正確にpopします。Luaコードは実行されません。
    unsafe {
        lua.exec_raw::<()>(function.clone(), |state| {
            result = ffi::lua_iscfunction(state, 1) != 0;
            ffi::lua_pop(state, 1);
        })?;
    }
    Ok(result)
}

/// 指定位置のアップ値セルの同一性と現在値を返します。範囲外ならNone。
fn upvalue_at(
    lua: &Lua,
    function: &Function,
    index: i32,
) -> Result<Option<(usize, Value)>, VmError> {
    let mut found = false;
    let mut cell_id = 0_usize;
    // SAFETY: getupvalueが値をpushしたら関数を除去して値を唯一の結果として残し、
    // 範囲外なら関数をpopしてnilを結果とします。スタックは復元されます。
    let value = unsafe {
        lua.exec_raw::<Value>(function.clone(), |state| {
            let name = ffi::lua_getupvalue(state, 1, index);
            if name.is_null() {
                ffi::lua_pop(state, 1);
                ffi::lua_pushnil(state);
            } else {
                found = true;
                cell_id = ffi::lua_upvalueid(state, 1, index) as usize;
                ffi::lua_remove(state, 1);
            }
        })?
    };
    Ok(if found { Some((cell_id, value)) } else { None })
}

/// 関数のアップ値の個数。
fn upvalue_count(lua: &Lua, function: &Function) -> Result<usize, VmError> {
    let mut count = 0_usize;
    let mut ok = false;
    // SAFETY: ルート化されたFunctionがスタック先頭に積まれ、`lua_getinfo(">u")` がそれをpopして
    // nups フィールドだけをO(1)で埋めます。Luaコードは一切実行されません。
    unsafe {
        lua.exec_raw::<()>(function.clone(), |state| {
            static WHAT: &[u8] = b">u\0";
            let mut ar = std::mem::MaybeUninit::<ffi::lua_Debug>::zeroed();
            if ffi::lua_getinfo(
                state,
                WHAT.as_ptr() as *const std::ffi::c_char,
                ar.as_mut_ptr(),
            ) != 0
            {
                count = usize::from((*ar.as_ptr()).nups);
                ok = true;
            }
        })?;
    }
    if !ok {
        return Err(invalid("failed to query closure upvalue count"));
    }
    Ok(count)
}

struct LoadReader<'a> {
    bytes: &'a [u8],
    done: bool,
}

unsafe extern "C-unwind" fn load_reader(
    _state: *mut ffi::lua_State,
    data: *mut std::ffi::c_void,
    size: *mut usize,
) -> *const std::ffi::c_char {
    // SAFETY: dataはload_prototypeが用意したLoadReaderへのポインタで、lua_loadの
    // 呼び出し期間中有効です。全バイトを1回で返し、次回は終端を返します。
    unsafe {
        let reader = &mut *(data as *mut LoadReader);
        if reader.done {
            *size = 0;
            return std::ptr::null();
        }
        reader.done = true;
        *size = reader.bytes.len();
        reader.bytes.as_ptr() as *const std::ffi::c_char
    }
}

/// PUC-Lua 5.3 の `lundump.c` は信頼されないバイナリチャンクの境界検査を
/// 行わない（`LoadDebug` の `f->upvalues` バッファあふれ、定数文字列の NULL
/// 参照、未知の定数タグでの `lua_assert`、深い子プロト再帰による C スタック
/// あふれ、`cl->nupvalues != cl->p->sizeupvalues` 等）。`lua_load` に渡す前に
/// チャンク全体の構造・件数・再帰深さ・末尾余剰バイトを検証します。
fn validate_lua53_chunk(dump: &[u8]) -> Result<(), VmError> {
    const MAX_PROTO_DEPTH: usize = 256;
    let size_t_bytes = std::mem::size_of::<usize>();
    if dump.len() < 34
        || &dump[0..4] != b"\x1bLua"
        || dump[4] != 0x53
        || dump[5] != 0
        || &dump[6..12] != b"\x19\x93\r\n\x1a\n"
        || dump[12] != std::mem::size_of::<std::ffi::c_int>() as u8
        || dump[13] != size_t_bytes as u8
        || dump[14] != 4
        || dump[15] != 8
        || dump[16] != 8
        || dump[17..25] != 0x5678_i64.to_le_bytes()
        || dump[25..33] != 370.5_f64.to_le_bytes()
    {
        return Err(invalid("snapshot prototype header is invalid"));
    }
    let root_upvalues = dump[33] as usize;

    fn take<'a>(bytes: &'a [u8], pos: &mut usize, count: usize) -> Result<&'a [u8], VmError> {
        let end = pos
            .checked_add(count)
            .ok_or_else(|| invalid("snapshot prototype length overflow"))?;
        let slice = bytes
            .get(*pos..end)
            .ok_or_else(|| invalid("snapshot prototype is truncated"))?;
        *pos = end;
        Ok(slice)
    }
    fn read_count(bytes: &[u8], pos: &mut usize, max: u32) -> Result<usize, VmError> {
        let raw = take(bytes, pos, 4)?;
        let n = i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
        if n < 0 {
            return Err(invalid("snapshot prototype has a negative count"));
        }
        let n_u32 = n as u32;
        if n_u32 > max {
            return Err(limit("snapshot prototype count exceeds the sanity cap"));
        }
        Ok(n_u32 as usize)
    }
    fn skip_dump_string(bytes: &[u8], pos: &mut usize, allow_null: bool) -> Result<(), VmError> {
        let tag = take(bytes, pos, 1)?[0];
        let size = if tag == 0xff {
            let raw = take(bytes, pos, std::mem::size_of::<usize>())?;
            let mut buf = [0_u8; std::mem::size_of::<usize>()];
            buf.copy_from_slice(raw);
            usize::from_le_bytes(buf)
        } else {
            tag as usize
        };
        if size == 0 {
            if allow_null {
                return Ok(());
            }
            return Err(invalid("snapshot prototype string constant cannot be NULL"));
        }
        let len = size - 1;
        if len > MAX_STRING_BYTES as usize {
            return Err(limit("snapshot prototype string exceeds the sanity cap"));
        }
        take(bytes, pos, len)?;
        Ok(())
    }
    fn validate_proto(
        bytes: &[u8],
        pos: &mut usize,
        depth: usize,
        parent_frame: Option<(u8, usize)>,
    ) -> Result<usize, VmError> {
        if depth > MAX_PROTO_DEPTH {
            return Err(limit("snapshot prototype nesting exceeds the sanity cap"));
        }
        skip_dump_string(bytes, pos, true)?;
        take(bytes, pos, 8)?; // linedefined, lastlinedefined
        let header = take(bytes, pos, 3)?;
        let numparams = header[0];
        let maxstacksize = header[2];
        if maxstacksize < 2 || maxstacksize < numparams {
            return Err(invalid("snapshot prototype stack size is invalid"));
        }
        let sizecode = read_count(bytes, pos, MAX_NODES)?;
        if sizecode == 0 {
            return Err(invalid("snapshot prototype code cannot be empty"));
        }
        take(
            bytes,
            pos,
            sizecode
                .checked_mul(4)
                .ok_or_else(|| invalid("snapshot prototype code length overflow"))?,
        )?;
        let sizek = read_count(bytes, pos, MAX_NODES)?;
        for _ in 0..sizek {
            match take(bytes, pos, 1)?[0] {
                0 => {}
                1 => {
                    let b = take(bytes, pos, 1)?[0];
                    if b > 1 {
                        return Err(invalid("snapshot prototype boolean constant is invalid"));
                    }
                }
                3 | 19 => {
                    take(bytes, pos, 8)?;
                }
                4 | 20 => {
                    skip_dump_string(bytes, pos, false)?;
                }
                _ => return Err(invalid("snapshot prototype constant tag is invalid")),
            }
        }
        let sizeupvalues = read_count(bytes, pos, 255)?;
        for _ in 0..sizeupvalues {
            let desc = take(bytes, pos, 2)?;
            let instack = desc[0];
            let idx = desc[1];
            if instack > 1 {
                return Err(invalid(
                    "snapshot prototype upvalue instack flag is invalid",
                ));
            }
            if let Some((parent_stack, parent_upvals)) = parent_frame {
                let bound = if instack == 1 {
                    parent_stack as usize
                } else {
                    parent_upvals
                };
                if (idx as usize) >= bound {
                    return Err(invalid("snapshot prototype upvalue index is out of range"));
                }
            }
        }
        let sizep = read_count(bytes, pos, MAX_NODES)?;
        for _ in 0..sizep {
            validate_proto(bytes, pos, depth + 1, Some((maxstacksize, sizeupvalues)))?;
        }
        let sizelineinfo = read_count(bytes, pos, MAX_NODES)?;
        if sizelineinfo > sizecode {
            return Err(invalid(
                "snapshot prototype lineinfo count exceeds code size",
            ));
        }
        take(
            bytes,
            pos,
            sizelineinfo
                .checked_mul(4)
                .ok_or_else(|| invalid("snapshot prototype lineinfo overflow"))?,
        )?;
        let sizelocvars = read_count(bytes, pos, MAX_NODES)?;
        for _ in 0..sizelocvars {
            skip_dump_string(bytes, pos, true)?;
            take(bytes, pos, 8)?; // startpc, endpc
        }
        let sizeupvalnames = read_count(bytes, pos, 255)?;
        if sizeupvalnames > sizeupvalues {
            return Err(invalid(
                "snapshot prototype upvalue debug name count exceeds upvalue count",
            ));
        }
        for _ in 0..sizeupvalnames {
            skip_dump_string(bytes, pos, true)?;
        }
        Ok(sizeupvalues)
    }

    let mut pos = 34usize;
    let actual_upvalues = validate_proto(dump, &mut pos, 0, None)?;
    if actual_upvalues != root_upvalues {
        return Err(invalid(
            "snapshot prototype header upvalue count does not match descriptor count",
        ));
    }
    if pos != dump.len() {
        return Err(invalid("snapshot prototype has trailing bytes"));
    }
    Ok(())
}

/// バイナリチャンク（プロトタイプのダンプ）を新しいクロージャとしてロードします。
fn load_prototype(lua: &Lua, bytes: &[u8]) -> Result<Function, VmError> {
    validate_lua53_chunk(bytes)?;
    let mut status = -1_i32;
    // SAFETY: リーダはこのクロージャ内でのみ生存し、チャンク名とモードは静的文字列です。
    // 成功時は関数、失敗時はエラーメッセージが結果として残ります。
    let value = unsafe {
        lua.exec_raw::<Value>((), |state| {
            static NAME: &[u8] = b"snapshot\0";
            static MODE: &[u8] = b"b\0";
            if ffi::lua_checkstack(state, 1) == 0 {
                ffi::lua_pushnil(state);
                return;
            }
            let mut reader = LoadReader { bytes, done: false };
            status = ffi::lua_load(
                state,
                load_reader,
                &mut reader as *mut LoadReader as *mut std::ffi::c_void,
                NAME.as_ptr() as *const std::ffi::c_char,
                MODE.as_ptr() as *const std::ffi::c_char,
            );
        })?
    };
    if status != 0 {
        return Err(invalid(format!(
            "snapshot prototype failed to load (lua_load status {status})"
        )));
    }
    match value {
        Value::Function(function) => Ok(function),
        _ => Err(invalid(
            "snapshot prototype load did not produce a function",
        )),
    }
}

/// ダンプ済みプロトタイプを子プロトとして内包する工場チャンクを組み立て
/// ます。工場を呼ぶたびに、子プロトを共有する新しいクロージャが1つ
/// 生成されます（OP_CLOSURE）。子プロトのバイト列は無改造で埋め込む
/// （復元したクロージャを再ダンプしても元のバイト列と一致し、捕捉の
/// 正準性が保たれる）。工場本体は、子プロトのアップ値記述子が参照する
/// レジスタ／アップ値を賄えるだけのスタックとアップ値を持つ。
/// 戻り値の2要素目は、呼ぶ前に工場側へ新しいセルを結合し直すべき
/// アップ値添字（1始まり）の列です。子プロトの記述子が全て被覆側
/// アップ値参照（instack=0）の場合、OP_CLOSURE のキャッシュが前回生成
/// したクロージャを返してしまうため、呼び出しごとにアップ値実体を
/// 差し替えてキャッシュを無効化します。
/// 形式が想定（vendored Lua 5.3 のダンプ）と少しでも異なれば None を
/// 返し、呼び出し側は従来のクロージャ単位ロードへフォールバックします。
fn build_proto_factory_chunk(dump: &[u8]) -> Option<(Vec<u8>, Vec<i32>)> {
    let size_t_bytes = std::mem::size_of::<usize>();
    // ヘッダ: 署名4 + 版1 + 形式1 + LUAC_DATA6 + サイズ5 + LUAC_INT8
    // + LUAC_NUM8 = 33バイト、その直後に本体関数のアップ値数が1バイト。
    if dump.len() < 34 || &dump[0..4] != b"\x1bLua" || dump[4] != 0x53 || dump[5] != 0 {
        return None;
    }
    if &dump[6..12] != b"\x19\x93\r\n\x1a\n"
        || dump[12] != std::mem::size_of::<std::ffi::c_int>() as u8
        || dump[13] != size_t_bytes as u8
        || dump[14] != 4
        || dump[15] != 8
        || dump[16] != 8
        || dump[17..25] != 0x5678_i64.to_le_bytes()
        || dump[25..33] != 370.5_f64.to_le_bytes()
    {
        return None;
    }
    let upvalues = dump[33] as usize;
    // 工場フレームのレジスタに連番で割り当てるため、アップ値数が
    // レジスタ番号（バイト）に収まる範囲を超える場合は使わない。
    if upvalues > 253 {
        return None;
    }
    let read_u32 = |bytes: &[u8], pos: usize| -> Option<u32> {
        bytes
            .get(pos..pos + 4)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    };
    // Lua 5.3 の DumpString 形式: NULL は 0 バイト1つ、短い文字列は
    // 「長さ+1」の1バイト＋本体、0xFF の場合は size_t が続く。
    let skip_string = |bytes: &[u8], pos: usize| -> Option<usize> {
        let tag = *bytes.get(pos)?;
        match tag {
            0 => Some(pos + 1),
            0xff => {
                let raw = bytes.get(pos + 1..pos + 1 + size_t_bytes)?;
                let mut buf = [0_u8; std::mem::size_of::<usize>()];
                buf.copy_from_slice(raw);
                let size = usize::from_le_bytes(buf);
                let len = size.checked_sub(1)?;
                pos.checked_add(1 + size_t_bytes)?
                    .checked_add(len)
                    .filter(|&end| end <= bytes.len())
            }
            n => {
                let len = (n - 1) as usize;
                pos.checked_add(1)?
                    .checked_add(len)
                    .filter(|&end| end <= bytes.len())
            }
        }
    };
    // 本体関数ダンプの走査: ソース文字列、行範囲、引数情報、コード、
    // 定数を読み飛ばし、アップ値記述子の位置を特定する。
    let mut pos = 34usize;
    pos = skip_string(dump, pos)?;
    pos = pos.checked_add(4 + 4 + 3)?; // linedefined, lastlinedefined, numparams/is_vararg/maxstack
    let code_len = read_u32(dump, pos)? as usize;
    pos = pos.checked_add(4)?.checked_add(code_len.checked_mul(4)?)?;
    if pos > dump.len() {
        return None;
    }
    let const_len = read_u32(dump, pos)? as usize;
    pos += 4;
    for _ in 0..const_len {
        let tag = *dump.get(pos)?;
        pos += 1;
        pos = match tag {
            0 => pos,                          // nil
            1 => pos.checked_add(1)?,          // boolean
            3 | 19 => pos.checked_add(8)?,     // float / integer（TValueタグ）
            4 | 20 => skip_string(dump, pos)?, // short / long string
            _ => return None,
        };
        if pos > dump.len() {
            return None;
        }
    }
    let desc_count = read_u32(dump, pos)? as usize;
    if desc_count != upvalues || desc_count == 0 {
        // アップ値ゼロの共有プロトは OP_CLOSURE のキャッシュを回避する
        // 手段がないため、従来経路に委ねる。
        return None;
    }
    let desc_start = pos + 4;
    if desc_start + desc_count * 2 > dump.len() {
        return None;
    }
    // 記述子を読み、工場本体に必要なアップ値数・スタック量と、
    // キャッシュ無効化のために結合し直すべき添字を求める。
    let mut wrapper_upvalues = 0usize;
    let mut wrapper_stack = 2usize;
    let mut rebind: Vec<i32> = Vec::new();
    for i in 0..desc_count {
        let at = desc_start + i * 2;
        let instack = *dump.get(at)?;
        let idx = *dump.get(at + 1)? as usize;
        if instack == 0 {
            wrapper_upvalues = wrapper_upvalues.max(idx + 1);
            let slot = idx as i32 + 1;
            if !rebind.contains(&slot) {
                rebind.push(slot);
            }
        } else {
            wrapper_stack = wrapper_stack.max(idx + 1);
        }
    }
    if wrapper_upvalues > 255 || wrapper_stack > 255 {
        return None;
    }
    let child = &dump[34..];
    let mut out = Vec::with_capacity(dump.len() + 64 + wrapper_upvalues * 2);
    out.extend_from_slice(&dump[..33]);
    out.push(wrapper_upvalues as u8); // 工場本体のアップ値数
    out.push(0); // source = NULL（DumpString の NULL 表現）
    out.extend_from_slice(&0_i32.to_le_bytes()); // linedefined
    out.extend_from_slice(&0_i32.to_le_bytes()); // lastlinedefined
    out.push(0); // numparams
    out.push(1); // is_vararg
    out.push(wrapper_stack as u8); // maxstacksize
    out.extend_from_slice(&2_i32.to_le_bytes()); // sizecode
    out.extend_from_slice(&44_u32.to_le_bytes()); // OP_CLOSURE A=0 Bx=0
    out.extend_from_slice(&(38_u32 | (2 << 23)).to_le_bytes()); // OP_RETURN A=0 B=2
    out.extend_from_slice(&0_i32.to_le_bytes()); // constants
    out.extend_from_slice(&(wrapper_upvalues as i32).to_le_bytes()); // upvalues
    for _ in 0..wrapper_upvalues {
        out.push(1); // instack = 1
        out.push(0); // idx = 0
    }
    out.extend_from_slice(&1_i32.to_le_bytes()); // protos
    out.extend_from_slice(child);
    out.extend_from_slice(&0_i32.to_le_bytes()); // debug: lineinfo
    out.extend_from_slice(&0_i32.to_le_bytes()); // debug: locvars
    out.extend_from_slice(&0_i32.to_le_bytes()); // debug: upvalue names
    Some((out, rebind))
}

/// 共有プロトタイプ用の工場関数をロードします。組み立てまたはロードに
/// 失敗した場合は None（呼び出し側はクロージャ単位ロードへ戻ります）。
/// 戻り値の2要素目は build_proto_factory_chunk の結合し直し列です。
fn proto_factory(lua: &Lua, dump: &[u8]) -> Option<(Function, Vec<i32>)> {
    let (chunk, rebind) = build_proto_factory_chunk(dump)?;
    load_prototype(lua, &chunk)
        .ok()
        .map(|factory| (factory, rebind))
}

/// targetのアップ値indexを、donorのアップ値1が保持するセルへ結合します。
fn join_upvalue(lua: &Lua, target: &Function, index: i32, donor: &Function) -> Result<(), VmError> {
    // SAFETY: 2つのルート化されたFunction引数が一時スタック上にあり、結合後に正確に
    // popします。lua_upvaluejoinはLuaコードを実行しません。
    unsafe {
        lua.exec_raw::<()>((target.clone(), donor.clone()), |state| {
            ffi::lua_upvaluejoin(state, 1, index, 2, 1);
            ffi::lua_pop(state, 2);
        })?;
    }
    Ok(())
}

/// セル保持用のゲッタ／セッタ対を生む工場チャンク（LClosureオフセット検証用）。
fn cell_factory(lua: &Lua) -> Result<Function, VmError> {
    Ok(lua
        .load("local v\nreturn function() return v end, function(nv) v = nv end")
        .set_name("snapshot-cell-factory")
        .into_function()?)
}

fn cell_donor_bytes() -> &'static [u8] {
    static DUMP: OnceLock<Vec<u8>> = OnceLock::new();
    DUMP.get_or_init(|| {
        let Ok(lua) = crate::backend_support::create_lua() else {
            return Vec::new();
        };
        lua.load("local v\nreturn function() return v end")
            .set_name("snapshot-cell-donor")
            .into_function()
            .map(|f| f.dump(true))
            .unwrap_or_default()
    })
}

/// 1回の呼び出しごとに新しい1スロットのアップ値セルを保持するドナークロージャを返す工場。
fn cell_donor_factory(lua: &Lua) -> Result<Function, VmError> {
    let bytes = cell_donor_bytes();
    if !bytes.is_empty() {
        if let Ok(func) = load_prototype(lua, bytes) {
            return Ok(func);
        }
    }
    Ok(lua
        .load("local v\nreturn function() return v end")
        .set_name("snapshot-cell-donor")
        .into_function()?)
}

/// targetのアップ値indexへ直接値を設定します（セッタクロージャ呼び出しを省略）。
fn set_upvalue_value(
    lua: &Lua,
    target: &Function,
    index: i32,
    value: Value,
) -> Result<(), VmError> {
    let mut ok = false;
    // SAFETY: target(1)とvalue(2)が一時スタック上にあり、lua_setupvalueが成功時に先頭値をpopして
    // 対象アップ値へ設定します。最後にsettop(0)で空の復帰スタックへ戻します。
    unsafe {
        lua.exec_raw::<()>((target.clone(), value), |state| {
            ok = !ffi::lua_setupvalue(state, 1, index).is_null();
            ffi::lua_settop(state, 0);
        })?;
    }
    if !ok {
        return Err(invalid("snapshot upvalue cell index out of range"));
    }
    Ok(())
}

/// Luaクロージャ実体（LClosure）の中のプロトタイプポインタのオフセットを、
/// セル工場のクロージャ群で実地検証して返す。vendored Lua 5.3 のバイナリレイアウトは
/// プロセス内で不変なため OnceLock で一度だけ検証する。
fn detect_proto_offset(lua: &Lua) -> Option<usize> {
    static OFFSET: OnceLock<Option<usize>> = OnceLock::new();
    *OFFSET.get_or_init(|| {
        let factory = cell_factory(lua).ok()?;
        let (getter1, setter1): (Function, Function) = factory.call(()).ok()?;
        let (getter2, setter2): (Function, Function) = factory.call(()).ok()?;
        let read = |function: &Function, offset: usize| -> usize {
            let base = Value::Function(function.clone()).to_pointer() as usize;
            // SAFETY: 候補オフセットは小さな範囲に限られ、対象クロージャは
            // この関数の間ローカルに保持されて生存している。読み出しは検証
            // 専用で、値の解釈は等値比較にだけ使う。
            unsafe { *((base as *const u8).add(offset) as *const usize) }
        };
        for offset in [16usize, 24, 8, 32, 40] {
            let g = read(&getter1, offset);
            let plausible = g > 0xffff && g % std::mem::size_of::<usize>() == 0;
            if plausible
                && g == read(&getter2, offset)
                && read(&setter1, offset) == read(&setter2, offset)
                && g != read(&setter1, offset)
            {
                return Some(offset);
            }
        }
        None
    })
}

// ---------------------------------------------------------------------------
// 捕捉（ヒープグラフの歩行）
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Pending {
    Plain(u32),
    Host(u32),
}

/// Stable anchors for identity keys. Unknown identities deliberately compare equal:
/// stable sorting retains their first-reach order, never an address-based order.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum IdentityOrder {
    Host(u32),
    Reached(u32),
    Unreached,
}

fn key_type_order(value: &Value) -> u8 {
    match value {
        Value::Boolean(_) => 0,
        Value::Integer(_) | Value::Number(_) => 1,
        Value::String(_) => 2,
        Value::Table(_) => 3,
        Value::Function(_) => 4,
        // These classes are rejected by convert, including when used as keys.
        _ => 5,
    }
}

fn compare_integer_number(integer: i64, number: f64) -> Ordering {
    // Avoid rounding large integers to f64 before comparison. Lua normalizes
    // integral float keys (including -0) to integers, and rejects NaN keys.
    if number < i64::MIN as f64 {
        Ordering::Greater
    } else if number >= -(i64::MIN as f64) {
        Ordering::Less
    } else {
        integer
            .cmp(&(number as i64))
            .then_with(|| (integer as f64).total_cmp(&number))
    }
}

struct Capture<'a> {
    lua: &'a Lua,
    captured_string_bytes: usize,
    /// レジストリ解決結果: オブジェクトポインタ -> (シンボル, テーブルならtrue)。
    registry: HashMap<usize, (u32, bool)>,
    globals_ptr: usize,
    symbols: Vec<Vec<String>>,
    protos: Vec<Vec<u8>>,
    proto_ids: HashMap<Vec<u8>, u32>,
    /// プロトタイプ実体ポインタ -> 保持済みプロトタイプID。同一プロトを
    /// 共有する別クロージャごとの再ダンプを避けるための捕捉内メモ。
    /// 実体ポインタが引けない環境では None のままで、常に従来どおり
    /// ダンプ内容で同定する。
    proto_ptr_ids: HashMap<usize, u32>,
    proto_ptr_offset: Option<Option<usize>>,
    tables: Vec<TableDef>,
    table_ids: HashMap<usize, u32>,
    host_tables: Vec<(u32, TableDef)>,
    host_table_ids: HashMap<usize, u32>,
    closures: Vec<ClosureDef>,
    closure_ids: HashMap<usize, u32>,
    cells: Vec<Option<GVal>>,
    cell_ids: HashMap<usize, u32>,
    pending_tables: Vec<(Pending, Table)>,
    pending_cells: Vec<(u32, Value)>,
    pending_identity_entries: Vec<(Pending, Vec<(Value, Value)>)>,
}

impl<'a> Capture<'a> {
    fn convert(&mut self, value: &Value, in_host_table: bool) -> Result<GVal, VmError> {
        Ok(match value {
            Value::Nil => GVal::Nil,
            Value::Boolean(v) => GVal::Bool(*v),
            Value::Integer(v) => GVal::Int(*v),
            Value::Number(v) => GVal::Num(v.to_bits()),
            Value::String(v) => {
                let bytes = v.as_bytes();
                self.captured_string_bytes = self
                    .captured_string_bytes
                    .checked_add(bytes.len())
                    .filter(|total| *total <= MAX_CAPTURE_STRING_BYTES)
                    .ok_or_else(|| limit("snapshot capture string byte budget exceeded"))?;
                GVal::Str(bytes.to_vec())
            }
            Value::Table(table) => self.convert_table(table)?,
            Value::Function(function) => self.convert_function(function, in_host_table)?,
            Value::Thread(_) => {
                return Err(unsupported(
                    "snapshot graph contains a thread value; coroutine state is not capturable",
                ))
            }
            Value::UserData(_) | Value::LightUserData(_) => {
                return Err(unsupported(
                    "snapshot graph contains userdata; host objects are not capturable",
                ))
            }
            _ => {
                return Err(unsupported(format!(
                    "snapshot graph contains a value of unsupported type {}",
                    value.type_name()
                )))
            }
        })
    }

    fn convert_table(&mut self, table: &Table) -> Result<GVal, VmError> {
        let pointer = Value::Table(table.clone()).to_pointer() as usize;
        if pointer == self.globals_ptr {
            return Err(unsupported(
                "snapshot graph reaches the real globals table; only the sandbox environment is capturable",
            ));
        }
        if table.metatable().is_some() {
            return Err(unsupported(
                "snapshot graph contains a table with a metatable; metatables are not capturable",
            ));
        }
        if let Some(&(symbol, is_table)) = self.registry.get(&pointer) {
            if !is_table {
                return Err(invalid(
                    "registered host function path resolved to a table at snapshot time",
                ));
            }
            if let Some(&id) = self.host_table_ids.get(&pointer) {
                return Ok(GVal::HostTable(id));
            }
            let id = self.host_tables.len() as u32;
            self.host_tables.push((symbol, TableDef::default()));
            self.host_table_ids.insert(pointer, id);
            self.pending_tables.push((Pending::Host(id), table.clone()));
            return Ok(GVal::HostTable(id));
        }
        if let Some(&id) = self.table_ids.get(&pointer) {
            return Ok(GVal::Table(id));
        }
        let id = self.tables.len() as u32;
        self.tables.push(TableDef::default());
        self.table_ids.insert(pointer, id);
        self.pending_tables
            .push((Pending::Plain(id), table.clone()));
        Ok(GVal::Table(id))
    }

    fn convert_function(
        &mut self,
        function: &Function,
        in_host_table: bool,
    ) -> Result<GVal, VmError> {
        let pointer = Value::Function(function.clone()).to_pointer() as usize;
        if let Some(&(symbol, is_table)) = self.registry.get(&pointer) {
            if is_table {
                return Err(invalid(
                    "registered host table path resolved to a function at snapshot time",
                ));
            }
            return Ok(GVal::HostRef(symbol));
        }
        if is_c_function(self.lua, function)? {
            if in_host_table {
                return Ok(GVal::HostEntry);
            }
            return Err(unsupported(
                "snapshot graph contains a host (C) function that is not registered; \
                 register its path with register_host_path before snapshotting",
            ));
        }
        if let Some(&id) = self.closure_ids.get(&pointer) {
            return Ok(GVal::Closure(id));
        }
        let proto = self.proto_id(function)?;
        let mut cells = Vec::new();
        let mut index = 1;
        while let Some((cell_pointer, content)) = upvalue_at(self.lua, function, index)? {
            let cell = match self.cell_ids.get(&cell_pointer) {
                Some(&id) => id,
                None => {
                    let id = self.cells.len() as u32;
                    self.cells.push(None);
                    self.cell_ids.insert(cell_pointer, id);
                    self.pending_cells.push((id, content));
                    id
                }
            };
            cells.push(cell);
            index += 1;
            if index > 4096 {
                return Err(limit("function upvalue count exceeds the sanity cap"));
            }
        }
        let id = self.closures.len() as u32;
        self.closures.push(ClosureDef { proto, cells });
        self.closure_ids.insert(pointer, id);
        Ok(GVal::Closure(id))
    }

    /// クロージャのプロトタイプIDを返す。プロトタイプ実体ポインタで
    /// 同定できる場合はダンプを省略し、保持済みのIDを再利用する。
    /// 引けない場合は従来どおりダンプ内容そのもので同定する（保持形式
    /// はどちらでも同一）。
    fn proto_id(&mut self, function: &Function) -> Result<u32, VmError> {
        let mut proto_ptr = None;
        if let Some(offset) = self.proto_offset() {
            let base = Value::Function(function.clone()).to_pointer() as usize;
            // SAFETY: offset は同一ランタイム上でセル工場クロージャ群を
            // 使って検証済みの LClosure 内プロトタイプポインタ位置であり、
            // function はこの捕捉の間到達可能（＝生存）である。
            let candidate = unsafe { *((base as *const u8).add(offset) as *const usize) };
            if candidate != 0 {
                if let Some(&id) = self.proto_ptr_ids.get(&candidate) {
                    return Ok(id);
                }
                proto_ptr = Some(candidate);
            }
        }
        let dump = function.dump(false);
        let id = match self.proto_ids.get(&dump) {
            Some(&id) => id,
            None => {
                let id = self.protos.len() as u32;
                self.protos.push(dump.clone());
                self.proto_ids.insert(dump, id);
                id
            }
        };
        if let Some(ptr) = proto_ptr {
            self.proto_ptr_ids.insert(ptr, id);
        }
        Ok(id)
    }

    fn proto_offset(&mut self) -> Option<usize> {
        if let Some(cached) = self.proto_ptr_offset {
            return cached;
        }
        let detected = detect_proto_offset(self.lua);
        self.proto_ptr_offset = Some(detected);
        detected
    }

    fn identity_order(&self, key: &Value) -> IdentityOrder {
        let pointer = key.to_pointer() as usize;
        if let Some(&(symbol, _)) = self.registry.get(&pointer) {
            return IdentityOrder::Host(symbol);
        }
        let reached = match key {
            Value::Table(_) => self.table_ids.get(&pointer),
            Value::Function(_) => self.closure_ids.get(&pointer),
            _ => None,
        };
        reached.map_or(IdentityOrder::Unreached, |&id| IdentityOrder::Reached(id))
    }

    fn compare_keys_cached(
        &self,
        left: &Value,
        left_str: Option<&[u8]>,
        right: &Value,
        right_str: Option<&[u8]>,
    ) -> Ordering {
        key_type_order(left)
            .cmp(&key_type_order(right))
            .then_with(|| match (left, right) {
                (Value::Boolean(a), Value::Boolean(b)) => a.cmp(b),
                (Value::Integer(a), Value::Integer(b)) => a.cmp(b),
                (Value::Number(a), Value::Number(b)) => a.total_cmp(b),
                (Value::Integer(a), Value::Number(b)) => compare_integer_number(*a, *b),
                (Value::Number(a), Value::Integer(b)) => compare_integer_number(*b, *a).reverse(),
                (Value::String(_), Value::String(_)) => left_str.cmp(&right_str),
                (Value::Table(_), Value::Table(_)) | (Value::Function(_), Value::Function(_)) => {
                    self.identity_order(left).cmp(&self.identity_order(right))
                }
                _ => Ordering::Equal,
            })
    }

    fn fill_entries(&mut self, kind: &Pending, pairs: Vec<(Value, Value)>) -> Result<(), VmError> {
        let n = pairs.len();
        let mut key_bytes: Vec<Option<Vec<u8>>> = Vec::with_capacity(n);
        for (k, _) in &pairs {
            if let Value::String(s) = k {
                let bytes = s.as_bytes();
                if bytes.len() > MAX_CAPTURE_STRING_BYTES {
                    return Err(limit("snapshot capture string byte budget exceeded"));
                }
                key_bytes.push(Some(bytes.to_vec()));
            } else {
                key_bytes.push(None);
            }
        }
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&ia, &ib| {
            self.compare_keys_cached(
                &pairs[ia].0,
                key_bytes[ia].as_deref(),
                &pairs[ib].0,
                key_bytes[ib].as_deref(),
            )
        });
        let host = matches!(kind, Pending::Host(_));
        let mut entries = Vec::with_capacity(n);
        let mut pairs_slots: Vec<Option<(Value, Value)>> = pairs.into_iter().map(Some).collect();
        for idx in order {
            let Some((key, value)) = pairs_slots[idx].take() else {
                continue;
            };
            // キー位置のホスト関数は復元規則を持たないため、値位置のHostEntry規則は
            // 適用しません（convert側はin_host_table=falseで常にエラーにします）。
            let key = if let Some(bytes) = key_bytes[idx].take() {
                self.captured_string_bytes = self
                    .captured_string_bytes
                    .checked_add(bytes.len())
                    .filter(|total| *total <= MAX_CAPTURE_STRING_BYTES)
                    .ok_or_else(|| limit("snapshot capture string byte budget exceeded"))?;
                GVal::Str(bytes)
            } else {
                self.convert(&key, false)?
            };
            let value = self.convert(&value, host)?;
            entries.push((key, value));
        }
        match kind {
            Pending::Plain(id) => self.tables[*id as usize].entries.extend(entries),
            Pending::Host(id) => self.host_tables[*id as usize].1.entries.extend(entries),
        }
        Ok(())
    }

    fn fill_table(&mut self, kind: &Pending, table: &Table) -> Result<(), VmError> {
        let mut scalar = Vec::new();
        let mut identity = Vec::new();
        for pair in table.pairs::<Value, Value>() {
            let (key, value) = pair?;
            if matches!(key, Value::Table(_) | Value::Function(_)) {
                identity.push((key, value));
            } else {
                scalar.push((key, value));
            }
        }
        self.fill_entries(kind, scalar)?;
        if !identity.is_empty() {
            self.pending_identity_entries.push((*kind, identity));
        }
        Ok(())
    }

    fn run(&mut self) -> Result<(), VmError> {
        let mut table_index = 0;
        let mut cell_index = 0;
        let mut identity_index = 0;
        loop {
            while table_index < self.pending_tables.len() {
                let (kind, table) = self.pending_tables[table_index].clone();
                table_index += 1;
                self.fill_table(&kind, &table)?;
            }
            while cell_index < self.pending_cells.len() {
                let (id, content) = self.pending_cells[cell_index].clone();
                cell_index += 1;
                let converted = self.convert(&content, false)?;
                self.cells[id as usize] = Some(converted);
            }
            if table_index < self.pending_tables.len() {
                continue;
            }
            // Exhaust scalar paths and upvalue edges before assigning identity
            // key order. This also anchors keys named by later sibling tables.
            if identity_index == self.pending_identity_entries.len() {
                break;
            }
            let (kind, entries) = &mut self.pending_identity_entries[identity_index];
            let kind = *kind;
            let entries = std::mem::take(entries);
            identity_index += 1;
            self.fill_entries(&kind, entries)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ホスト値レジストリとVm公開API
// ---------------------------------------------------------------------------

fn profile_byte(profile: storm_lua_spec::environment::EnvironmentProfile) -> u8 {
    match profile {
        storm_lua_spec::environment::EnvironmentProfile::Game => 0,
        storm_lua_spec::environment::EnvironmentProfile::Extended => 1,
    }
}

/// スナップショットに記録する種別バイト。Component VM は環境プロファイルに
/// かかわらずタグ 2 で識別し、Vehicle / Addon VM への誤復元を拒否する
/// （GAP-T2-02）。
fn snapshot_tag(
    flavor: crate::runner::VmFlavor,
    profile: storm_lua_spec::environment::EnvironmentProfile,
) -> u8 {
    if flavor == crate::runner::VmFlavor::Component {
        2
    } else {
        profile_byte(profile)
    }
}

/// 環境テーブルからドット区切りパスを生lookupで辿ります。
fn resolve_path(environment: &Table, path: &[String]) -> Result<Value, VmError> {
    let mut current = Value::Table(environment.clone());
    for segment in path {
        match current {
            Value::Table(table) => {
                let next = table.raw_get::<Value>(segment.as_str())?;
                if matches!(next, Value::Nil) {
                    return Err(invalid(format!(
                        "registered host path {} does not resolve",
                        path.join(".")
                    )));
                }
                current = next;
            }
            _ => {
                return Err(invalid(format!(
                    "registered host path {} crosses a non-table value",
                    path.join(".")
                )))
            }
        }
    }
    Ok(current)
}

/// レジストリ解決の結果。valuesは歩行中の到達可能性を保証するため保持します。
struct ResolvedSymbols {
    symbols: Vec<Vec<String>>,
    #[allow(dead_code)]
    values: Vec<Value>,
    pointers: HashMap<usize, (u32, bool)>,
}

impl Vm {
    /// ホストが種付けした値（バインディングのテーブルや関数）をスナップショット
    /// レジストリへ登録します。登録されたパスはスナップショット／復元のたびに
    /// 環境から再解決され、捕捉時はシンボリック参照として、復元時は復元先VMの
    /// 現行インスタンスへの再束縛として扱われます。
    ///
    /// パスは文字列キーの列です（例: `&["server", "getVehicleComponents"]`）。
    /// 登録時点で解決できないパスはエラーになります。テーブルを登録すると
    /// そのエントリは歩行対象になり（スクリプトによる変更・削除が往復します）、
    /// 関数を登録すると参照として記録されます。
    pub fn register_host_path(&mut self, path: &[&str]) -> Result<(), VmError> {
        self.ensure_idle()?;
        if path.is_empty() || path.iter().any(|segment| segment.is_empty()) {
            return Err(invalid("host path must have non-empty segments"));
        }
        let owned: Vec<String> = path.iter().map(|segment| segment.to_string()).collect();
        let value = resolve_path(&self.environment, &owned)?;
        self.record_resolved_host_path(owned, value);
        Ok(())
    }

    /// Remove a symbol deliberately removed by a host profile before source execution.
    pub fn unregister_host_path(&mut self, path: &[&str]) -> Result<(), VmError> {
        self.ensure_idle()?;
        self.host_paths.retain(|registered| {
            !registered
                .iter()
                .map(String::as_str)
                .eq(path.iter().copied())
        });
        let owned: Vec<String> = path.iter().map(|segment| segment.to_string()).collect();
        self.host_provenance.remove(&owned);
        Ok(())
    }

    pub(crate) fn unregister_host_prefix(&mut self, prefix: &[String]) {
        self.host_paths
            .retain(|registered| !registered.starts_with(prefix));
        self.host_provenance
            .retain(|registered, _| !registered.starts_with(prefix));
    }

    fn record_resolved_host_path(&mut self, path: Vec<String>, value: Value) {
        match value {
            Value::Table(_) | Value::Function(_) => {
                if self.host_provenance.insert(path.clone(), value).is_none()
                    && !self.host_paths.contains(&path)
                {
                    self.host_paths.push(path);
                }
            }
            _ => {
                if !self.host_paths.contains(&path) {
                    self.host_paths.push(path.clone());
                }
                self.host_provenance.remove(&path);
            }
        }
    }

    pub(crate) fn note_host_path(&mut self, path: Vec<String>) {
        // 登録時点の解決値をルート化した来歴として記録する。これにより、
        // スクリプトによる差し替え・削除とホスト値を正確に区別し、GC再利用も防ぐ。
        match resolve_path(&self.environment, &path) {
            Ok(value) => self.record_resolved_host_path(path, value),
            Err(_) => {
                if !self.host_paths.contains(&path) {
                    self.host_paths.push(path.clone());
                }
                self.host_provenance.remove(&path);
            }
        }
    }

    /// 構築時に種付けされた環境エントリを一括登録します（標準ライブラリの関数と
    /// テーブル、およびその直接の関数／テーブルエントリ）。
    pub(crate) fn register_seed_host_paths(&mut self) -> Result<(), VmError> {
        let mut top: Vec<(String, Value)> = Vec::with_capacity(16);
        for pair in self.environment.pairs::<Value, Value>() {
            let (key, value) = pair?;
            if let Value::String(name) = key {
                if let Ok(name) = name.to_str() {
                    top.push((name.to_string(), value));
                }
            }
        }
        self.host_paths.reserve(96);
        self.host_provenance.reserve(96);
        for (name, value) in &top {
            if matches!(value, Value::Table(_) | Value::Function(_)) {
                let path = vec![name.clone()];
                self.host_provenance.insert(path.clone(), value.clone());
                self.host_paths.push(path);
            }
        }
        for (name, value) in &top {
            if let Value::Table(table) = value {
                for pair in table.pairs::<Value, Value>() {
                    let (key, entry) = pair?;
                    if let Value::String(entry_name) = key {
                        if let Ok(entry_name) = entry_name.to_str() {
                            if matches!(entry, Value::Table(_) | Value::Function(_)) {
                                let path = vec![name.clone(), entry_name.to_string()];
                                self.host_provenance.insert(path.clone(), entry);
                                self.host_paths.push(path);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// 登録パス群を解決します。再配置指定のない通常パスはルート化済みの来歴値を
    /// 用い、スクリプトが該当グローバルを削除・上書き・ラップしていても元のホスト
    /// 実体への参照を正しくシンボル化します。スカラは同一性を持たないため除外します。
    fn resolve_host_symbols(&self, paths: &[Vec<String>]) -> Result<ResolvedSymbols, VmError> {
        let mut symbols: Vec<Vec<String>> = Vec::new();
        let mut values: Vec<Value> = Vec::new();
        let mut pointers: HashMap<usize, (u32, bool)> = HashMap::new();
        let mut paths: Vec<_> = paths.iter().collect();
        paths.sort();
        paths.dedup();
        for path in paths {
            let value = if let Some(capture_path) = self.snapshot_capture_paths.get(path) {
                let value = resolve_path(&self.environment, capture_path)?;
                if !matches!(value, Value::Table(_) | Value::Function(_)) {
                    return Err(invalid(format!(
                        "snapshot capture path {} does not resolve to a table or function",
                        capture_path.join(".")
                    )));
                }
                value
            } else if let Some(origin) = self.host_provenance.get(path) {
                origin.clone()
            } else {
                continue;
            };
            match &value {
                Value::Table(table) => {
                    let symbol = symbols.len() as u32;
                    symbols.push(path.clone());
                    values.push(value.clone());
                    pointers
                        .entry(Value::Table(table.clone()).to_pointer() as usize)
                        .or_insert((symbol, true));
                }
                Value::Function(function) => {
                    let symbol = symbols.len() as u32;
                    symbols.push(path.clone());
                    values.push(value.clone());
                    pointers
                        .entry(Value::Function(function.clone()).to_pointer() as usize)
                        .or_insert((symbol, false));
                }
                _ => {}
            }
        }
        Ok(ResolvedSymbols {
            symbols,
            values,
            pointers,
        })
    }

    /// アイドル境界でVMヒープ全体をスナップショットします。再実行なしで
    /// `Vm::restore` が再現できるバイナリ（形式バージョン付き）を返します。
    /// 失敗したVM・実行中コールバックがあるVMは型付きエラーで拒否します。
    /// Capture order is stable across equivalent scalar-key graphs, host path
    /// registration orders and anchored identity keys. Unanchored table/function
    /// keys retain first-reach order; arbitrary graph isomorphism is not resolved.
    /// Returns ErrorKind::Limit if the aggregate copied string payload exceeds
    /// 64 MiB (including repeated references), before copying the offending string.
    pub fn snapshot(&self) -> Result<Vec<u8>, VmError> {
        self.ensure_idle()?;
        if self.has_require_loader {
            return Err(unsupported("development require cache has no continuation"));
        }
        for path in self.snapshot_capture_paths.keys() {
            if !self.host_paths.contains(path) {
                return Err(invalid(format!(
                    "snapshot capture path {} is not a registered host path",
                    path.join(".")
                )));
            }
        }
        let resolved = self.resolve_host_symbols(&self.host_paths)?;
        let symbols = resolved.symbols;
        let registry = resolved.pointers;
        let globals_ptr = Value::Table(self.lua.globals()).to_pointer() as usize;
        let mut capture = Capture {
            lua: &self.lua,
            captured_string_bytes: 0,
            registry,
            globals_ptr,
            symbols,
            protos: Vec::new(),
            proto_ids: HashMap::new(),
            proto_ptr_ids: HashMap::new(),
            proto_ptr_offset: None,
            tables: Vec::new(),
            table_ids: HashMap::new(),
            host_tables: Vec::new(),
            host_table_ids: HashMap::new(),
            closures: Vec::new(),
            closure_ids: HashMap::new(),
            cells: Vec::new(),
            cell_ids: HashMap::new(),
            pending_tables: Vec::new(),
            pending_cells: Vec::new(),
            pending_identity_entries: Vec::new(),
        };
        let root = capture.convert(&Value::Table(self.environment.clone()), false)?;
        if root != GVal::Table(0) {
            return Err(invalid(
                "environment table was not assigned the snapshot root id",
            ));
        }
        capture.run()?;
        let mut cells = Vec::with_capacity(capture.cells.len());
        for cell in capture.cells {
            cells.push(
                cell.ok_or_else(|| invalid("snapshot capture left an upvalue cell unfilled"))?,
            );
        }
        let data = SnapshotData {
            profile: snapshot_tag(self.vm_flavor, self.environment_profile),
            symbols: capture.symbols,
            protos: capture.protos,
            tables: capture.tables,
            host_tables: capture.host_tables,
            closures: capture.closures,
            cells,
            root,
        };
        data.validate_references()?;
        Ok(data.encode())
    }

    /// スナップショットをこのVMへ復元します。対象VMは、スナップショット元と
    /// 同じ環境プロファイルで構築され、同じホスト値が種付け・登録されている
    /// 必要があります。環境テーブルは同一オブジェクトのまま内容だけが
    /// スナップショット時点の状態へ戻ります（同期コールバック等が保持する
    /// 環境ハンドルと矛盾しません）。エラー時にVMが部分的に変更されている
    /// 可能性があるため、エラーを受けたらこのVMは破棄してください。
    pub fn restore(&mut self, blob: &[u8]) -> Result<(), VmError> {
        self.ensure_idle()?;
        if self.has_require_loader {
            return Err(unsupported("development require cache has no continuation"));
        }
        self.idle_thread = None;
        let data = SnapshotData::decode(blob)?;
        data.validate_references()?;
        if data.profile != snapshot_tag(self.vm_flavor, self.environment_profile) {
            return Err(invalid(
                "snapshot environment profile does not match this VM",
            ));
        }
        if data.tables.is_empty() || data.root != GVal::Table(0) {
            return Err(invalid("snapshot root is not the environment table"));
        }
        // シンボルは内容を書き換える前に全て解決します（来歴ハンドルを優先する
        // ため、同一VM上でスクリプトが標準ライブラリを削除した後でも巻き戻せます）。
        let mut symbols: Vec<Value> = Vec::with_capacity(data.symbols.len());
        for path in &data.symbols {
            let value = match self.host_provenance.get(path) {
                Some(origin) => origin.clone(),
                None => resolve_path(&self.environment, path).map_err(|_| {
                    invalid(format!(
                        "snapshot host path {} is not present in this VM",
                        path.join(".")
                    ))
                })?,
            };
            if !matches!(value, Value::Table(_) | Value::Function(_)) {
                return Err(invalid(format!(
                    "snapshot host path {} does not resolve to a table or function in this VM",
                    path.join(".")
                )));
            }
            symbols.push(value);
        }
        let mut restore = Restore {
            lua: &self.lua,
            data: &data,
            symbols,
            tables: Vec::with_capacity(data.tables.len()),
            closures: Vec::with_capacity(data.closures.len()),
            cell_getters: Vec::with_capacity(data.cells.len()),
        };
        restore.tables.push(self.environment.clone());
        for _ in 1..data.tables.len() {
            restore.tables.push(self.lua.create_table()?);
        }
        if !data.cells.is_empty() {
            let factory = cell_donor_factory(&self.lua)?;
            for _ in 0..data.cells.len() {
                let getter: Function = factory.call(())?;
                restore.cell_getters.push(getter);
            }
        }
        // 複数のクロージャ定義が共有するプロトタイプは、定義ごとに
        // lua_load するとコード実体が定義数だけ複製されてしまう。
        // 共有プロトタイプについては工場チャンク経由で実体化し、
        // プロトタイプを1つだけ保持する（アップ値セルは定義ごとに
        // 結合し直すため、クロージャの同一性は保たれる）。工場が使え
        // ない形式では従来の定義単位ロードへフォールバックする。
        let mut proto_refs: HashMap<u32, usize> = HashMap::with_capacity(data.closures.len());
        for def in &data.closures {
            *proto_refs.entry(def.proto).or_default() += 1;
        }
        let mut factories: HashMap<u32, (Function, Vec<i32>)> = HashMap::new();
        let mut rebind_source: Option<Function> = None;
        for def in &data.closures {
            let dump = &data.protos[def.proto as usize];
            let function = if proto_refs[&def.proto] > 1 {
                if let std::collections::hash_map::Entry::Vacant(entry) = factories.entry(def.proto)
                {
                    if let Some(built) = proto_factory(&self.lua, dump) {
                        entry.insert(built);
                    }
                }
                match factories.get(&def.proto) {
                    Some((factory, rebind)) => {
                        if !rebind.is_empty() {
                            // OP_CLOSURE のキャッシュ無効化: 工場のアップ値
                            // 実体を新しいセルへ差し替えてから呼ぶ。
                            let donor = match &rebind_source {
                                Some(existing) => existing,
                                None => rebind_source.insert(cell_donor_factory(&self.lua)?),
                            };
                            let getter: Function = donor.call(())?;
                            for &slot in rebind {
                                join_upvalue(&self.lua, factory, slot, &getter)?;
                            }
                        }
                        Some(factory.call::<Function>(())?)
                    }
                    None => None,
                }
            } else {
                None
            };
            let function = match function {
                Some(function) => function,
                None => load_prototype(&self.lua, dump)?,
            };
            let count = upvalue_count(&self.lua, &function)?;
            if count != def.cells.len() {
                return Err(invalid(format!(
                    "snapshot closure upvalue count {count} does not match the recorded {}",
                    def.cells.len()
                )));
            }
            for (offset, cell) in def.cells.iter().enumerate() {
                join_upvalue(
                    &self.lua,
                    &function,
                    offset as i32 + 1,
                    &restore.cell_getters[*cell as usize],
                )?;
            }
            restore.closures.push(function);
        }
        for (id, def) in data.tables.iter().enumerate().skip(1) {
            let table = restore.tables[id].clone();
            for (key, value) in &def.entries {
                let key = restore.value(key)?;
                let value = restore.value(value)?;
                table.raw_set(key, value)?;
            }
        }
        let root_def = &data.tables[0];
        restore.reconcile(&self.environment.clone(), root_def, false)?;
        for (symbol, def) in &data.host_tables {
            let table = match &restore.symbols[*symbol as usize] {
                Value::Table(table) => table.clone(),
                _ => {
                    return Err(invalid(
                        "snapshot host table symbol did not resolve to a table in this VM",
                    ))
                }
            };
            restore.reconcile(&table, def, true)?;
        }
        for (index, content) in data.cells.iter().enumerate() {
            let value = restore.value(content)?;
            set_upvalue_value(&self.lua, &restore.cell_getters[index], 1, value)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 復元（グラフの実体化）
// ---------------------------------------------------------------------------

struct Restore<'a> {
    lua: &'a Lua,
    data: &'a SnapshotData,
    symbols: Vec<Value>,
    tables: Vec<Table>,
    closures: Vec<Function>,
    cell_getters: Vec<Function>,
}

/// テーブルキーの借用フィンガープリント（調整時の既存キー照合用、文字列コピー不要）。
#[derive(PartialEq, Eq, Hash)]
enum GKeyRef<'a> {
    Bool(bool),
    Int(i64),
    Num(u64),
    Str(&'a [u8]),
    Ref(usize),
}

impl Restore<'_> {
    fn gkey_ref<'a>(&self, key: &'a GVal) -> Result<GKeyRef<'a>, VmError> {
        Ok(match key {
            GVal::Bool(v) => GKeyRef::Bool(*v),
            GVal::Int(v) => GKeyRef::Int(*v),
            GVal::Num(bits) => GKeyRef::Num(*bits),
            GVal::Str(bytes) => GKeyRef::Str(bytes.as_slice()),
            GVal::Table(id) => {
                GKeyRef::Ref(Value::Table(self.tables[*id as usize].clone()).to_pointer() as usize)
            }
            GVal::HostTable(id) => {
                let symbol = self.data.host_tables[*id as usize].0;
                match &self.symbols[symbol as usize] {
                    Value::Table(t) => GKeyRef::Ref(Value::Table(t.clone()).to_pointer() as usize),
                    _ => {
                        return Err(invalid(
                            "snapshot host table symbol did not resolve to a table in this VM",
                        ))
                    }
                }
            }
            GVal::Closure(id) => GKeyRef::Ref(
                Value::Function(self.closures[*id as usize].clone()).to_pointer() as usize,
            ),
            GVal::HostRef(symbol) => {
                match &self.symbols[*symbol as usize] {
                    Value::Function(f) => {
                        GKeyRef::Ref(Value::Function(f.clone()).to_pointer() as usize)
                    }
                    _ => return Err(invalid(
                        "snapshot host function symbol did not resolve to a function in this VM",
                    )),
                }
            }
            GVal::Nil => return Err(invalid("snapshot table key cannot be nil")),
            GVal::HostEntry => {
                return Err(invalid(
                    "snapshot host entry reference appeared outside a host table",
                ))
            }
        })
    }

    fn value_matches_gval(&self, current: &Value, target: &GVal, host: bool) -> bool {
        match (current, target) {
            (v, GVal::HostEntry) if host && !matches!(v, Value::Nil) => true,
            (Value::Boolean(a), GVal::Bool(b)) => a == b,
            (Value::Integer(a), GVal::Int(b)) => a == b,
            (Value::Number(a), GVal::Num(bits)) => a.to_bits() == *bits,
            (Value::String(s), GVal::Str(bytes)) => s.as_bytes().as_ref() == bytes.as_slice(),
            (Value::Function(_), GVal::HostRef(symbol)) => {
                current.to_pointer() == self.symbols[*symbol as usize].to_pointer()
            }
            (Value::Table(_), GVal::HostTable(id)) => {
                let symbol = self.data.host_tables[*id as usize].0;
                current.to_pointer() == self.symbols[symbol as usize].to_pointer()
            }
            _ => false,
        }
    }

    fn value(&self, value: &GVal) -> Result<Value, VmError> {
        Ok(match value {
            GVal::Nil => Value::Nil,
            GVal::Bool(v) => Value::Boolean(*v),
            GVal::Int(v) => Value::Integer(*v),
            GVal::Num(bits) => Value::Number(f64::from_bits(*bits)),
            GVal::Str(bytes) => Value::String(self.lua.create_string(bytes)?),
            GVal::Table(id) => Value::Table(self.tables[*id as usize].clone()),
            GVal::HostTable(id) => {
                let symbol = self.data.host_tables[*id as usize].0;
                match &self.symbols[symbol as usize] {
                    Value::Table(table) => Value::Table(table.clone()),
                    _ => {
                        return Err(invalid(
                            "snapshot host table symbol did not resolve to a table in this VM",
                        ))
                    }
                }
            }
            GVal::Closure(id) => Value::Function(self.closures[*id as usize].clone()),
            GVal::HostRef(symbol) => {
                match &self.symbols[*symbol as usize] {
                    Value::Function(function) => Value::Function(function.clone()),
                    _ => return Err(invalid(
                        "snapshot host function symbol did not resolve to a function in this VM",
                    )),
                }
            }
            GVal::HostEntry => {
                return Err(invalid(
                    "snapshot host entry reference appeared outside a host table",
                ))
            }
        })
    }

    /// 既存テーブル（環境ルート／ホストテーブル）の内容を、捕捉されたエントリ
    /// 集合と正確に一致させます。捕捉時に無かったキーは削除され、既に同一の
    /// ホスト参照／スカラ値を持つキーは不要な再作成・再書込みを省略します。
    fn reconcile(&self, table: &Table, def: &TableDef, host: bool) -> Result<(), VmError> {
        let mut captured: HashMap<GKeyRef<'_>, usize> = HashMap::with_capacity(def.entries.len());
        for (idx, (key, _)) in def.entries.iter().enumerate() {
            captured.insert(self.gkey_ref(key)?, idx);
        }
        let unique_keys = captured.len() == def.entries.len();
        let mut already_same = vec![false; def.entries.len()];
        let mut to_remove: Vec<Value> = Vec::new();
        for pair in table.pairs::<Value, Value>() {
            let (cur_key, cur_val) = pair?;
            let matched_idx = match &cur_key {
                Value::Boolean(v) => captured.get(&GKeyRef::Bool(*v)).copied(),
                Value::Integer(v) => captured.get(&GKeyRef::Int(*v)).copied(),
                Value::Number(v) => captured.get(&GKeyRef::Num(v.to_bits())).copied(),
                Value::String(s) => {
                    let bytes = s.as_bytes();
                    captured.get(&GKeyRef::Str(bytes.as_ref())).copied()
                }
                other => captured
                    .get(&GKeyRef::Ref(other.to_pointer() as usize))
                    .copied(),
            };
            match matched_idx {
                None => to_remove.push(cur_key),
                Some(idx) if unique_keys => {
                    if self.value_matches_gval(&cur_val, &def.entries[idx].1, host) {
                        already_same[idx] = true;
                    }
                }
                Some(_) => {}
            }
        }
        for key in to_remove {
            table.raw_set(key, Value::Nil)?;
        }
        for (idx, (key, value)) in def.entries.iter().enumerate() {
            if already_same[idx] {
                continue;
            }
            let key = self.value(key)?;
            if host && matches!(value, GVal::HostEntry) {
                if matches!(table.raw_get::<Value>(key)?, Value::Nil) {
                    return Err(invalid(
                        "snapshot host table entry is missing in this VM's host table",
                    ));
                }
                continue;
            }
            let value = self.value(value)?;
            table.raw_set(key, value)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// テスト
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::type_complexity
)]
mod tests {
    use super::*;
    use crate::value::LuaValue;
    use std::sync::atomic::Ordering as AtomicOrdering;
    use std::sync::{atomic::AtomicI64, Arc};
    use storm_lua_spec::environment::EnvironmentProfile;

    fn vm() -> Vm {
        Vm::new(crate::ExecutionLimits::default()).unwrap()
    }

    fn check(target: &mut Vm, expression: &str) -> bool {
        let source = format!("__check = ({expression})");
        target.execute(source.as_bytes(), "check").unwrap();
        target.global("__check").unwrap() == LuaValue::Bool(true)
    }

    fn canon(value: LuaValue) -> LuaValue {
        match value {
            LuaValue::Table(mut entries) => {
                for (key, value) in &mut entries {
                    let replaced = std::mem::replace(value, LuaValue::Nil);
                    *value = canon(replaced);
                    let replaced_key = std::mem::replace(key, LuaValue::Nil);
                    *key = canon(replaced_key);
                }
                entries.sort_by(|a, b| format!("{:?}", a.0).cmp(&format!("{:?}", b.0)));
                LuaValue::Table(entries)
            }
            other => other,
        }
    }

    fn canonical_vm(reverse: bool) -> Vm {
        let mut target = vm();
        let names = if reverse {
            ["omega", "alpha"]
        } else {
            ["alpha", "omega"]
        };
        for name in names {
            target
                .register_function("math", name, Arc::new(|_| Ok(vec![LuaValue::Integer(17)])))
                .unwrap();
        }
        let alpha: Value = target
            .environment
            .raw_get::<Table>("math")
            .unwrap()
            .raw_get("alpha")
            .unwrap();
        target
            .environment
            .raw_get::<Table>("math")
            .unwrap()
            .raw_set("alias_alpha", alpha)
            .unwrap();
        let aliases = if reverse {
            ["alpha", "alias_alpha"]
        } else {
            ["alias_alpha", "alpha"]
        };
        for name in aliases {
            target.unregister_host_path(&["math", name]).unwrap();
        }
        for name in aliases {
            target.register_host_path(&["math", name]).unwrap();
        }
        let math: Table = target.environment.raw_get("math").unwrap();
        target.environment.raw_set("library_alias", math).unwrap();
        let table_aliases = if reverse {
            ["math", "library_alias"]
        } else {
            ["library_alias", "math"]
        };
        for name in table_aliases {
            target.unregister_host_path(&[name]).unwrap();
        }
        for name in table_aliases {
            target.register_host_path(&[name]).unwrap();
        }
        target
            .environment
            .raw_set("build_reverse", reverse)
            .unwrap();
        // One source/prototype layout, independently allocated heaps and opposite
        // insertion/allocation paths. No build selector remains in the image.
        target
            .execute(
                br#"
local reverse = build_reverse
build_reverse = nil
local function make(n)
    return function(d) n = n + d; return n end, function() return n end
end
local shared, other
if reverse then other = {}; shared = {} else shared = {}; other = {} end
shared.self = shared
other.shared = shared
local bump, read, bump2, read2
if reverse then
    bump2, read2 = make(20); bump, read = make(9)
else
    bump, read = make(9); bump2, read2 = make(20)
end
local hidden = {}
function hidden_key() return hidden end
local entries = {
    {false, 'false'}, {true, 'true'}, {-math.huge, 'min'},
    {-9223372036854775807-1, 'imin'}, {-2.5, 'neg'}, {-2, 'integer'},
    {-0.0, 'zero'}, {0.25, 'fraction'}, {2, 'two'}, {9007199254740993, 'large'},
    {9223372036854775807, 'imax'}, {math.huge, 'max'},
    {'a', shared}, {'a\0b', other}, {'\255', 'bytes'},
    {shared, other}, {other, shared}, {hidden, 'upvalue key'},
    {bump, 'bump key'}, {read, 'read key'}, {math.alpha, 'host key'}, {math, 'host table key'}
}
a_identity = {}
local first, last, step = 1, #entries, 1
if reverse then first, last, step = #entries, 1, -1 end
for i = first, last, step do a_identity[entries[i][1]] = entries[i][2] end
a_identity[a_identity] = 'self key'
z_anchors = {shared=shared, other=other, bump=bump, read=read, bump2=bump2, read2=read2}
math.snapshot_extra = {shared=shared, owner=a_identity}
cached = math.alpha
"#,
                "canonical-state",
            )
            .unwrap();
        target
    }

    #[test]
    fn independent_construction_orders_produce_identical_images() {
        let source = canonical_vm(false);
        let left = source.snapshot().unwrap();
        assert!(left == source.snapshot().unwrap(), "repeat capture differs");
        let right = canonical_vm(true).snapshot().unwrap();
        assert!(left == right, "independent construction images differ");
        let data = SnapshotData::decode(&left).unwrap();
        assert!(data.symbols.windows(2).all(|p| p[0] < p[1]));
        let mixed = data.tables[0]
            .entries
            .iter()
            .find_map(|(key, value)| {
                if *key == GVal::Str(b"a_identity".to_vec()) {
                    if let GVal::Table(id) = value {
                        return Some(*id);
                    }
                }
                None
            })
            .unwrap();
        let keys: Vec<_> = data.tables[mixed as usize]
            .entries
            .iter()
            .map(|(key, _)| key.clone())
            .collect();
        let scalar_keys = vec![
            GVal::Bool(false),
            GVal::Bool(true),
            GVal::Num(f64::NEG_INFINITY.to_bits()),
            GVal::Int(i64::MIN),
            GVal::Num((-2.5_f64).to_bits()),
            GVal::Int(-2),
            GVal::Int(0),
            GVal::Num(0.25_f64.to_bits()),
            GVal::Int(2),
            GVal::Int(9007199254740993),
            GVal::Int(i64::MAX),
            GVal::Num(f64::INFINITY.to_bits()),
            GVal::Str(b"a".to_vec()),
            GVal::Str(b"a\0b".to_vec()),
            GVal::Str(vec![255]),
        ];
        assert_eq!(&keys[..scalar_keys.len()], scalar_keys);
        // Table identities precede function identities, with symbolic host
        // identities before graph-reach anchors within each type.
        assert!(matches!(keys[15], GVal::HostTable(_)));
        assert!(keys[16..20].iter().all(|key| matches!(key, GVal::Table(_))));
        assert!(matches!(keys[20], GVal::HostRef(_)));
        assert!(keys[21..].iter().all(|key| matches!(key, GVal::Closure(_))));
        let alpha = data
            .symbols
            .iter()
            .position(|p| p == &["math", "alias_alpha"])
            .unwrap() as u32;
        assert!(data
            .tables
            .iter()
            .flat_map(|t| &t.entries)
            .any(|(_, v)| *v == GVal::HostRef(alpha)));
        assert!(data
            .host_tables
            .iter()
            .any(|(symbol, _)| data.symbols[*symbol as usize] == ["library_alias"]));
    }

    #[test]
    fn canonical_image_round_trips_bit_for_bit_and_preserves_state() {
        let image = canonical_vm(false).snapshot().unwrap();
        let mut target = canonical_vm(true);
        target.restore(&image).unwrap();
        assert!(
            image == target.snapshot().unwrap(),
            "recapture changed canonical bytes"
        );
        assert!(check(
            &mut target,
            "z_anchors.shared.self==z_anchors.shared and z_anchors.other.shared==z_anchors.shared"
        ));
        assert!(check(&mut target, "a_identity[z_anchors.shared]==z_anchors.other and a_identity[z_anchors.other]==z_anchors.shared and a_identity[hidden_key()]=='upvalue key'"));
        assert!(check(&mut target, "a_identity[z_anchors.bump]=='bump key' and a_identity[math.alpha]=='host key' and a_identity[math]=='host table key'"));
        assert!(check(&mut target, "z_anchors.bump(3)==12 and z_anchors.read()==12 and z_anchors.read2()==20 and z_anchors.bump2(1)==21"));
        assert!(check(
            &mut target,
            "library_alias==math and cached==math.alias_alpha and cached()==17 and math.snapshot_extra.owner==a_identity"
        ));
    }

    #[test]
    fn version_one_noncanonical_symbol_and_entry_orders_still_restore() {
        let canonical = canonical_vm(false).snapshot().unwrap();
        let mut old_order = SnapshotData::decode(&canonical).unwrap();
        let count = old_order.symbols.len() as u32;
        old_order.symbols.reverse();
        for def in old_order
            .tables
            .iter_mut()
            .chain(old_order.host_tables.iter_mut().map(|(symbol, def)| {
                *symbol = count - 1 - *symbol;
                def
            }))
        {
            def.entries.reverse();
            for (key, value) in &mut def.entries {
                for value in [key, value] {
                    if let GVal::HostRef(symbol) = value {
                        *symbol = count - 1 - *symbol;
                    }
                }
            }
        }
        for cell in &mut old_order.cells {
            if let GVal::HostRef(symbol) = cell {
                *symbol = count - 1 - *symbol;
            }
        }
        assert_eq!(FORMAT_VERSION, 1);
        let old_image = old_order.encode();
        assert!(old_image != canonical);
        let mut target = canonical_vm(true);
        target.restore(&old_image).unwrap();
        assert!(
            canonical == target.snapshot().unwrap(),
            "version-one ordering changed restore semantics"
        );
        assert!(check(
            &mut target,
            "cached()==17 and z_anchors.bump(2)==11 and z_anchors.read()==11"
        ));
    }

    #[test]
    fn canonical_capture_matches_a_fresh_process() {
        const CHILD: &str = "STORM_VM_CANONICAL_CAPTURE_CHILD";
        if let Some(path) = std::env::var_os(CHILD) {
            std::fs::write(path, canonical_vm(true).snapshot().unwrap()).unwrap();
            return;
        }
        // create_new protects concurrent invocations without a new dependency.
        let path = std::env::temp_dir().join(format!(
            "storm-vm-canonical-{}.snapshot",
            std::process::id()
        ));
        drop(
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .unwrap(),
        );
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "snapshot::tests::canonical_capture_matches_a_fresh_process",
                "--nocapture",
            ])
            .env(CHILD, &path)
            .status()
            .unwrap();
        let child = std::fs::read(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert!(status.success());
        assert!(
            canonical_vm(false).snapshot().unwrap() == child,
            "fresh-process image differs"
        );
    }

    #[test]
    fn numeric_key_comparison_preserves_integer_precision() {
        for (integer, number, expected) in [
            (9007199254740993, 9007199254740992.0, Ordering::Greater),
            (i64::MAX, 9223372036854775808.0, Ordering::Less),
            (i64::MIN, -9223372036854775808.0, Ordering::Equal),
            (-2, -2.5, Ordering::Greater),
            (0, 0.25, Ordering::Less),
            (0, -0.25, Ordering::Greater),
            (i64::MAX, f64::INFINITY, Ordering::Less),
            (i64::MIN, f64::NEG_INFINITY, Ordering::Greater),
        ] {
            assert_eq!(compare_integer_number(integer, number), expected);
        }
    }

    #[test]
    fn unanchored_identity_keys_round_trip_without_claiming_canonical_order() {
        let mut source = vm();
        source
            .execute(
                b"t = {}; t[{}] = 'table'; t[{}] = 'table'; t[function() return 3 end] = 'function'; t[function() return 4 end] = 'function'",
                "unanchored",
            )
            .unwrap();
        let image = source.snapshot().unwrap();
        let mut target = vm();
        target.restore(&image).unwrap();
        target.execute(b"table_count=0; function_count=0; for k,v in pairs(t) do if type(k)=='table' and v=='table' then table_count=table_count+1 elseif type(k)=='function' and v=='function' and (k()==3 or k()==4) then function_count=function_count+1 end end", "check-unanchored").unwrap();
        assert!(check(&mut target, "table_count==2 and function_count==2"));
    }

    #[test]
    fn unsupported_key_classes_remain_typed_errors() {
        struct Dummy;
        impl mlua::UserData for Dummy {}
        let source = vm();
        let table = source.lua.create_table().unwrap();
        source
            .environment
            .raw_set("key_test", table.clone())
            .unwrap();
        let metatable_key = source.lua.create_table().unwrap();
        metatable_key.set_metatable(Some(source.lua.create_table().unwrap()));
        for key in [
            Value::Thread(
                source
                    .lua
                    .create_thread(source.lua.load("return 1").into_function().unwrap())
                    .unwrap(),
            ),
            Value::UserData(source.lua.create_userdata(Dummy).unwrap()),
            Value::LightUserData(mlua::LightUserData(std::ptr::null_mut())),
            Value::Table(metatable_key),
            Value::Function(source.lua.create_function(|_, ()| Ok(())).unwrap()),
        ] {
            table.raw_set(key.clone(), true).unwrap();
            assert_eq!(source.snapshot().unwrap_err().kind, ErrorKind::Unsupported);
            table.raw_set(key, Value::Nil).unwrap();
        }
        // The HostEntry exception remains restricted to value positions.
        let host: Table = source.environment.raw_get("math").unwrap();
        let key = source.lua.create_function(|_, ()| Ok(())).unwrap();
        host.raw_set(key, true).unwrap();
        assert_eq!(source.snapshot().unwrap_err().kind, ErrorKind::Unsupported);
    }

    fn relocated_vm(calls: Arc<AtomicI64>) -> Vm {
        let mut vm = vm();
        let mut bindings = crate::bindings::HostBindings::default();
        bindings.snapshot_capture_paths.insert(
            vec!["math".into(), "hostCall".into()],
            vec!["moved".into(), "step".into()],
        );
        // Like Addon construction, snapshot configuration precedes slot registration.
        vm.install_bindings(&bindings).unwrap();
        vm.register_function(
            "math",
            "hostCall",
            Arc::new(move |_| {
                let next = calls.fetch_add(1, AtomicOrdering::Relaxed) + 1;
                Ok(vec![LuaValue::Integer(next)])
            }),
        )
        .unwrap();
        vm
    }

    #[test]
    fn relocated_capture_symbols_restore_before_namespace_reconciliation() {
        let source_calls = Arc::new(AtomicI64::new(0));
        let mut source = relocated_vm(Arc::clone(&source_calls));
        source
            .execute(
                b"moved={step=math.hostCall}; math.hostCall=nil; stored=moved.step; marker=7",
                "bootstrap",
            )
            .unwrap();
        let blob = source.snapshot().unwrap();
        let data = SnapshotData::decode(&blob).unwrap();
        assert_eq!(FORMAT_VERSION, 1);
        assert!(data
            .symbols
            .contains(&vec!["math".into(), "hostCall".into()]));
        assert!(!data.symbols.contains(&vec!["moved".into(), "step".into()]));
        drop(source);

        let target_calls = Arc::new(AtomicI64::new(40));
        let mut target = relocated_vm(Arc::clone(&target_calls));
        target.restore(&blob).unwrap();
        assert!(check(
            &mut target,
            "marker==7 and math.hostCall==nil and stored==moved.step"
        ));
        assert!(check(&mut target, "stored()==41 and moved.step()==42"));
        assert_eq!(source_calls.load(AtomicOrdering::Relaxed), 0);
        assert_eq!(target_calls.load(AtomicOrdering::Relaxed), 42);

        // A second capture/restore also works; no bootstrap or source replay is needed.
        let next = target.snapshot().unwrap();
        let mut fresh = relocated_vm(Arc::clone(&target_calls));
        fresh.restore(&next).unwrap();
        assert!(check(&mut fresh, "math.hostCall==nil and stored()==43"));
    }

    #[test]
    fn missing_or_scalar_capture_path_refuses_without_mutating_vm() {
        let calls = Arc::new(AtomicI64::new(0));
        let mut source = relocated_vm(Arc::clone(&calls));
        source.execute(b"marker=23", "state").unwrap();
        for setup in ["moved=nil", "moved={}", "moved=42", "moved={step=1}"] {
            source.execute(setup.as_bytes(), "break-path").unwrap();
            let error = source.snapshot().unwrap_err();
            assert_eq!(error.kind, ErrorKind::InvalidArgument);
            assert!(error.message.contains("moved.step"), "{error}");
            // No fallback to the still-present original host path, and no failed latch.
            assert!(check(
                &mut source,
                "marker==23 and type(math.hostCall)=='function'"
            ));
            assert_eq!(calls.load(AtomicOrdering::Relaxed), 0);
        }
        source
            .execute(b"moved={step=math.hostCall}; math.hostCall=nil", "repair")
            .unwrap();
        let blob = source.snapshot().unwrap();
        let mut target = relocated_vm(Arc::clone(&calls));
        target.restore(&blob).unwrap();
        assert!(check(&mut target, "marker==23 and moved.step()==1"));
    }

    #[test]
    fn missing_restore_symbol_refuses_before_changing_live_state() {
        let mut source = relocated_vm(Arc::new(AtomicI64::new(0)));
        source
            .execute(
                b"moved={step=math.hostCall}; math.hostCall=nil; marker=7",
                "bootstrap",
            )
            .unwrap();
        let blob = source.snapshot().unwrap();
        let mut target = vm();
        target
            .execute(b"marker=99; nested={n=5}", "live-state")
            .unwrap();
        let before = canon(target.global("nested").unwrap());
        let error = target.restore(&blob).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidArgument);
        assert!(error.message.contains("math.hostCall"), "{error}");
        assert_eq!(canon(target.global("nested").unwrap()), before);
        assert!(check(
            &mut target,
            "marker==99 and moved==nil and math.abs(-3)==3"
        ));
        // Refusal left the same VM usable, including for a repaired restore.
        target
            .register_function(
                "math",
                "hostCall",
                Arc::new(|_| Ok(vec![LuaValue::Integer(88)])),
            )
            .unwrap();
        target.restore(&blob).unwrap();
        assert!(check(
            &mut target,
            "marker==7 and moved.step()==88 and math.hostCall==nil"
        ));
    }

    #[test]
    fn capture_path_configuration_is_validated_without_loosening_game_bindings() {
        let mut target = vm();
        target.execute(b"marker=99", "state").unwrap();
        let mut bindings = crate::bindings::HostBindings::default();
        bindings
            .snapshot_capture_paths
            .insert(vec![], vec!["moved".into()]);
        assert_eq!(
            target.install_bindings(&bindings).unwrap_err().kind,
            ErrorKind::InvalidArgument
        );
        assert!(target.snapshot_capture_paths.is_empty());
        assert!(check(&mut target, "marker==99 and moved==nil"));
        bindings.snapshot_capture_paths.clear();
        bindings
            .snapshot_capture_paths
            .insert(vec!["unregistered".into()], vec!["moved".into()]);
        target.install_bindings(&bindings).unwrap();
        let error = target.snapshot().unwrap_err();
        assert!(error.message.contains("not a registered host path"));
        assert!(check(&mut target, "marker==99"));
        bindings
            .values
            .insert("marker".into(), LuaValue::Integer(0));
        assert_eq!(
            target.install_bindings(&bindings).unwrap_err().kind,
            ErrorKind::InvalidArgument
        );
        assert!(check(&mut target, "marker==99"));
    }

    #[test]
    fn capture_refuses_shared_string_duplication_over_budget() {
        let source = vm();
        let shared = source.lua.create_string(vec![b'x'; 1024 * 1024]).unwrap();
        let aliases = source.lua.create_table().unwrap();
        // Only one 1 MiB Lua string, but 65 MiB of per-reference snapshot copies.
        for index in 1..=65 {
            aliases.raw_set(index, shared.clone()).unwrap();
        }
        source
            .environment
            .raw_set("aliases", aliases.clone())
            .unwrap();

        let error = source.snapshot().map(|_| ()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Limit);
        assert_eq!(
            error.message,
            "snapshot capture string byte budget exceeded"
        );

        // Refusal leaves the source usable, and a new capture gets a fresh budget.
        for index in 3..=65 {
            aliases.raw_set(index, Value::Nil).unwrap();
        }
        let blob = source.snapshot().unwrap();
        let mut target = vm();
        target.restore(&blob).unwrap();
        assert_eq!(blob, target.snapshot().unwrap());
    }

    #[test]
    fn shared_binary_strings_round_trip_unchanged() {
        let source = vm();
        let bytes = [b'a', 0, 0xff, b'z'].repeat(32);
        let shared = source.lua.create_string(&bytes).unwrap();
        let aliases = source.lua.create_table().unwrap();
        for index in 1..=3 {
            aliases.raw_set(index, shared.clone()).unwrap();
        }
        source.environment.raw_set("aliases", aliases).unwrap();

        let blob = source.snapshot().unwrap();
        let mut target = vm();
        target.restore(&blob).unwrap();
        assert_eq!(blob, target.snapshot().unwrap());
        let aliases: Table = target.environment.raw_get("aliases").unwrap();
        for index in 1..=3 {
            let restored: mlua::String = aliases.raw_get(index).unwrap();
            assert_eq!(&*restored.as_bytes(), bytes.as_slice());
        }
    }

    #[test]
    fn shared_table_identity_round_trips() {
        let mut source = vm();
        source
            .execute(
                b"shared = { n = 1 }\na = { x = shared }\nb = { y = shared }",
                "t",
            )
            .unwrap();
        let blob = source.snapshot().unwrap();
        let mut target = vm();
        target.restore(&blob).unwrap();
        assert!(check(&mut target, "a.x == b.y"));
        target.execute(b"a.x.n = 41", "t").unwrap();
        assert!(check(&mut target, "b.y.n == 41"));
    }

    #[test]
    fn cyclic_table_round_trips() {
        let mut source = vm();
        source
            .execute(
                b"t = { name = \"root\" }\nt.self = t\nt.child = { parent = t }",
                "t",
            )
            .unwrap();
        let blob = source.snapshot().unwrap();
        let mut target = vm();
        target.restore(&blob).unwrap();
        assert!(check(
            &mut target,
            "t.self == t and t.child.parent == t and t.child ~= t"
        ));
    }

    #[test]
    fn closures_sharing_an_upvalue_still_share_after_restore() {
        let mut source = vm();
        source
            .execute(
                b"local counter = 0\nfunction bump() counter = counter + 1 end\nfunction read() return counter end",
                "t",
            )
            .unwrap();
        let blob = source.snapshot().unwrap();
        let mut target = vm();
        target.restore(&blob).unwrap();
        target.execute(b"bump()\nbump()", "t").unwrap();
        target.execute(b"v = read()", "t").unwrap();
        assert_eq!(target.global("v").unwrap(), LuaValue::Integer(2));
    }

    #[test]
    fn mutated_upvalue_state_round_trips() {
        let mut source = vm();
        source
            .execute(
                b"local box = { v = 10 }\nfunction get() return box.v end\nfunction set(x) box.v = x end",
                "t",
            )
            .unwrap();
        source.execute(b"set(99)", "t").unwrap();
        let blob = source.snapshot().unwrap();
        let mut target = vm();
        target.restore(&blob).unwrap();
        target.execute(b"v = get()", "t").unwrap();
        assert_eq!(target.global("v").unwrap(), LuaValue::Integer(99));
    }

    #[test]
    fn nested_data_and_strings_round_trip() {
        let mut source = vm();
        source
            .execute(
                b"deep = { l1 = { l2 = { s = \"hello\", n = 1.5, i = -7, b = true } } }\nraw = \"a\\0b\"",
                "t",
            )
            .unwrap();
        let blob = source.snapshot().unwrap();
        let mut target = vm();
        target.restore(&blob).unwrap();
        let expected = LuaValue::record([(
            "l1",
            LuaValue::record([(
                "l2",
                LuaValue::record([
                    ("s", LuaValue::text("hello")),
                    ("n", LuaValue::Number(1.5)),
                    ("i", LuaValue::Integer(-7)),
                    ("b", LuaValue::Bool(true)),
                ]),
            )]),
        )]);
        assert_eq!(canon(target.global("deep").unwrap()), canon(expected));
        assert_eq!(
            target.global("raw").unwrap(),
            LuaValue::Bytes(vec![b'a', 0, b'b'])
        );
    }

    #[test]
    fn restore_into_the_same_vm_rewinds_state() {
        let mut source = vm();
        source.execute(b"x = 1", "t").unwrap();
        let blob = source.snapshot().unwrap();
        source.execute(b"x = 2", "t").unwrap();
        source.restore(&blob).unwrap();
        assert_eq!(source.global("x").unwrap(), LuaValue::Integer(1));
    }

    fn seed_host(vm: &mut Vm, counter: Arc<AtomicI64>) {
        let table = vm.lua.create_table().unwrap();
        let counted = Arc::clone(&counter);
        table
            .raw_set(
                "count",
                vm.lua
                    .create_function(move |_, ()| {
                        Ok(counted.fetch_add(1, AtomicOrdering::Relaxed) + 1)
                    })
                    .unwrap(),
            )
            .unwrap();
        let pinged = Arc::clone(&counter);
        table
            .raw_set(
                "ping",
                vm.lua
                    .create_function(move |_, ()| {
                        pinged.fetch_add(100, AtomicOrdering::Relaxed);
                        Ok(())
                    })
                    .unwrap(),
            )
            .unwrap();
        table.raw_set("label", "original").unwrap();
        table.raw_set("temp", "x").unwrap();
        vm.environment.raw_set("host", table).unwrap();
        vm.register_host_path(&["host"]).unwrap();
        vm.register_host_path(&["host", "count"]).unwrap();
    }

    #[test]
    fn host_table_mutations_round_trip_and_entries_rebind_to_fresh_instances() {
        let old_counter = Arc::new(AtomicI64::new(0));
        let mut source = vm();
        seed_host(&mut source, Arc::clone(&old_counter));
        source
            .execute(
                b"host.label = \"changed\"\nhost.extra = 5\nhost.temp = nil",
                "t",
            )
            .unwrap();
        let blob = source.snapshot().unwrap();

        let new_counter = Arc::new(AtomicI64::new(0));
        let mut target = vm();
        seed_host(&mut target, Arc::clone(&new_counter));
        target.restore(&blob).unwrap();

        assert!(check(
            &mut target,
            "host.label == \"changed\" and host.extra == 5 and host.temp == nil"
        ));
        target.execute(b"n = host.count()", "t").unwrap();
        assert_eq!(target.global("n").unwrap(), LuaValue::Integer(1));
        assert_eq!(new_counter.load(AtomicOrdering::Relaxed), 1);
        assert_eq!(old_counter.load(AtomicOrdering::Relaxed), 0);
        // パス未登録の直接エントリ（HostEntry規則）も復元先の新しい実体に束縛される。
        target.execute(b"host.ping()", "t").unwrap();
        assert_eq!(new_counter.load(AtomicOrdering::Relaxed), 101);
        assert_eq!(old_counter.load(AtomicOrdering::Relaxed), 0);
    }

    #[test]
    fn host_function_stored_in_a_script_table_rebinds() {
        let mut source = vm();
        source.execute(b"stored = { f = math.sin }", "t").unwrap();
        let blob = source.snapshot().unwrap();
        let mut target = vm();
        target.restore(&blob).unwrap();
        assert!(check(
            &mut target,
            "stored.f == math.sin and stored.f(0) == 0"
        ));
    }

    #[test]
    fn script_replacement_at_a_host_path_round_trips_as_a_closure() {
        // R7-29: ホストパス上の値をスクリプトが差し替えた場合、その
        // 差し替えはホスト組み込みではなく通常のクロージャとして
        // 往復しなければならない（組み込みへ黙って戻ってはならない）。
        let mut source = vm();
        source
            .execute(
                b"math.sin = function(x) return 42 end\nsaved = math.sin",
                "t",
            )
            .unwrap();
        let blob = source.snapshot().unwrap();
        let mut target = vm();
        target.restore(&blob).unwrap();
        assert!(check(
            &mut target,
            "saved(0) == 42 and math.sin(0) == 42 and saved == math.sin"
        ));
        // 差し替えていない組み込みは従来どおり振る舞う（対照）。
        assert!(check(&mut target, "math.cos(0) == 1"));
    }

    #[test]
    fn many_closures_sharing_one_prototype_restore_within_the_original_budget() {
        // R7-30/R7-33: 1つの大きなプロトタイプを共有する多数の
        // クロージャは、復元時にプロトタイプが複製されないこと。
        // 既定上限より小さい上限でも復元でき、各クロージャの
        // アップ値の独立性と同一性が保たれることで確認する。
        let big = "a".repeat(40_000);
        let script = format!(
            "local function make(v) return function() return v, \"{big}\" end end\n\
             holders = {{}}\nfor i = 1, 200 do holders[i] = make(i) end"
        );
        let mut source = vm();
        source.execute(script.as_bytes(), "t").unwrap();
        let blob = source.snapshot().unwrap();
        // プロトタイプはスナップショット内に1つだけ保持される。
        let data = SnapshotData::decode(&blob).unwrap();
        assert!(data.protos.len() <= 3, "protos: {}", data.protos.len());
        assert!(blob.len() < 200_000, "blob bytes: {}", blob.len());

        let limits = crate::ExecutionLimits {
            instruction_budget: Some(std::num::NonZeroU64::new(1_000_000).unwrap()),
            lua_memory_bytes: Some(std::num::NonZeroUsize::new(4 * 1024 * 1024).unwrap()),
            time_limit: None,
            max_source_bytes: None,
        };
        let mut target = Vm::new(limits).unwrap();
        target.restore(&blob).unwrap();
        target
            .execute(b"v1, s1 = holders[137]()\nv2, s2 = holders[3]()", "t")
            .unwrap();
        assert_eq!(target.global("v1").unwrap(), LuaValue::Integer(137));
        assert_eq!(target.global("v2").unwrap(), LuaValue::Integer(3));
        assert!(check(
            &mut target,
            "#s1 == 40000 and s1 == s2 and holders[1] ~= holders[2]"
        ));
    }

    #[test]
    fn extended_profile_wrapped_pcall_round_trips() {
        let mut source = Vm::with_environment(
            crate::ExecutionLimits::default(),
            EnvironmentProfile::Extended,
        )
        .unwrap();
        source.execute(b"saved_pcall = pcall", "t").unwrap();
        let blob = source.snapshot().unwrap();
        let mut target = Vm::with_environment(
            crate::ExecutionLimits::default(),
            EnvironmentProfile::Extended,
        )
        .unwrap();
        target.restore(&blob).unwrap();
        assert!(check(&mut target, "saved_pcall == pcall"));
    }

    #[test]
    fn thread_in_graph_is_a_typed_error() {
        let source = vm();
        let function = source.lua.create_function(|_, ()| Ok(())).unwrap();
        let thread = source.lua.create_thread(function).unwrap();
        source
            .environment
            .raw_set("bad_thread", Value::Thread(thread))
            .unwrap();
        let error = source.snapshot().unwrap_err();
        assert_eq!(error.kind, ErrorKind::Unsupported);
        assert!(error.message.contains("thread"), "{}", error.message);
    }

    #[test]
    fn userdata_in_graph_is_a_typed_error() {
        struct Dummy;
        impl mlua::UserData for Dummy {}
        let source = vm();
        let userdata = source.lua.create_userdata(Dummy).unwrap();
        source
            .environment
            .raw_set("bad_userdata", Value::UserData(userdata))
            .unwrap();
        let error = source.snapshot().unwrap_err();
        assert_eq!(error.kind, ErrorKind::Unsupported);
        assert!(error.message.contains("userdata"), "{}", error.message);
    }

    #[test]
    fn metatable_in_graph_is_a_typed_error() {
        let source = vm();
        let table = source.lua.create_table().unwrap();
        let metatable = source.lua.create_table().unwrap();
        table.set_metatable(Some(metatable));
        source.environment.raw_set("bad_mt", table).unwrap();
        let error = source.snapshot().unwrap_err();
        assert_eq!(error.kind, ErrorKind::Unsupported);
        assert!(error.message.contains("metatable"), "{}", error.message);
    }

    #[test]
    fn snapshot_and_restore_refuse_a_busy_vm() {
        let mut source = vm();
        let function = source.lua.load("return 1").into_function().unwrap();
        let thread = source.lua.create_thread(function).unwrap();
        source.pending = Some(thread);
        assert_eq!(source.snapshot().unwrap_err().kind, ErrorKind::Busy);
        assert_eq!(source.restore(&[]).unwrap_err().kind, ErrorKind::Busy);
    }

    #[test]
    fn snapshot_refuses_a_failed_vm() {
        // 通常のLuaエラーは回復可能（VMは失敗しない）ため、失敗状態は
        // メモリ上限超過で作る。
        let mut source = Vm::new(crate::ExecutionLimits {
            lua_memory_bytes: Some(std::num::NonZeroUsize::new(256 * 1024).unwrap()),
            ..crate::ExecutionLimits::default()
        })
        .unwrap();
        assert!(source
            .execute(
                b"s = '' for i = 1, 100000 do s = s .. '0123456789' end",
                "t"
            )
            .is_err());
        assert!(source.is_failed());
        assert_eq!(source.snapshot().unwrap_err().kind, ErrorKind::Failed);
    }

    #[test]
    fn component_snapshots_are_rejected_by_other_flavors() {
        // GAP-T2-02: Component VM のスナップショットはタグ 2 で識別され、
        // 標準 VM へは復元できず、Component VM 同士では往復する。
        let mut source = Vm::with_component_environment(
            crate::ExecutionLimits::default(),
            storm_lua_spec::environment::EnvironmentProfile::Game,
        )
        .unwrap();
        source.execute(b"marker = 41", "t").unwrap();
        let blob = source.snapshot().unwrap();
        let mut standard = vm();
        assert_eq!(
            standard.restore(&blob).unwrap_err().kind,
            ErrorKind::InvalidArgument
        );
        let mut component = Vm::with_component_environment(
            crate::ExecutionLimits::default(),
            storm_lua_spec::environment::EnvironmentProfile::Game,
        )
        .unwrap();
        component.restore(&blob).unwrap();
        assert_eq!(
            component.global("marker").unwrap(),
            crate::value::LuaValue::Integer(41)
        );
        // 逆方向（標準 → Component）も拒否する。
        let mut plain = vm();
        plain.execute(b"marker = 1", "t").unwrap();
        let plain_blob = plain.snapshot().unwrap();
        assert_eq!(
            component.restore(&plain_blob).unwrap_err().kind,
            ErrorKind::InvalidArgument
        );
    }

    #[test]
    fn corrupt_and_wrong_version_blobs_are_typed_errors() {
        let mut target = vm();
        assert_eq!(
            target.restore(&[1, 2, 3]).unwrap_err().kind,
            ErrorKind::InvalidArgument
        );
        let source = vm();
        let mut blob = source.snapshot().unwrap();
        let mut pristine = vm();
        pristine.restore(&blob).unwrap();
        blob[8] = 99;
        assert_eq!(
            target.restore(&blob).unwrap_err().kind,
            ErrorKind::Unsupported
        );
    }

    // --- Adversarial Regression Tests (BUG-T2-01 .. BUG-T2-05, BUG-T2-08) ---

    #[test]
    fn bug_t2_01_shared_prototype_capturing_outer_or_env_upvalues_restores_without_abort() {
        // BUG-T2-01: When >= 2 closures share a prototype that captures an outer upvalue
        // or _ENV (instack == 0), build_proto_factory_chunk previously emitted
        // cl->nupvalues > 0 with cl->p->sizeupvalues == 0, aborting in lua_upvaluejoin.
        let mut source = vm();
        source
            .execute(
                br#"
                global_bias = 100
                local shared_outer = 10
                local function make(own)
                    return function(delta)
                        shared_outer = shared_outer + delta
                        own = own + 1
                        return global_bias + shared_outer + own
                    end
                end
                f1 = make(1)
                f2 = make(5)
                -- Also exercise pure instack==0 shared prototype (only _ENV + outer upvalue).
                local function make_factory(seed)
                    return function()
                        return function() seed = seed + 1; return global_bias + seed end
                    end
                end
                e1 = make_factory(3)()
                e2 = make_factory(10)()
            "#,
                "bug-t2-01",
            )
            .unwrap();
        let blob = source.snapshot().unwrap();
        let mut target = vm();
        target.restore(&blob).unwrap();
        assert_eq!(blob, target.snapshot().unwrap());
        assert!(check(
            &mut target,
            "f1(2) == 114 and f2(3) == 121 and f1(0) == 118"
        ));
        assert!(check(
            &mut target,
            "e1 ~= e2 and e1() == 104 and e2() == 111 and e1() == 105"
        ));
    }

    #[test]
    fn bug_t2_02_crt_random_debug_log_host_bindings_and_monkey_patched_builtins_round_trip() {
        struct FixedCrt;
        impl crate::random::CrtRandomSource for FixedCrt {
            fn draw(&self) -> Result<u32, String> {
                Ok(16384)
            }
            fn seed(&self, _: u32) -> Result<(), String> {
                Ok(())
            }
        }
        let make_configured = |bonus: i64| {
            let mut target = Vm::with_environment(
                crate::ExecutionLimits::default(),
                EnvironmentProfile::Extended,
            )
            .unwrap();
            target.install_crt_random(Arc::new(FixedCrt)).unwrap();
            target.enable_debug_log().unwrap();
            let mut bindings = crate::bindings::HostBindings::default();
            bindings.values.insert("print".into(), LuaValue::Nil);
            bindings
                .values
                .insert("custom.base".into(), LuaValue::Integer(10));
            bindings.functions.insert(
                "topFn".into(),
                Arc::new(move |_| Ok(vec![LuaValue::Integer(bonus)])),
            );
            bindings.functions.insert(
                "custom.calc".into(),
                Arc::new(move |args| {
                    let n = match args.first() {
                        Some(LuaValue::Integer(v)) => *v,
                        _ => 0,
                    };
                    Ok(vec![LuaValue::Integer(n + bonus)])
                }),
            );
            target.install_bindings(&bindings).unwrap();
            target
        };

        let mut source = make_configured(5);
        source
            .execute(
                br#"
                local rnd = math.random
                local log = debug.log
                local calc = custom.calc
                local top = topFn
                local real_sin = math.sin
                -- Monkey-patch math.sin while retaining the original C function in an upvalue,
                -- delete math.cos, and delete tonumber / table entirely.
                math.sin = function(x) return real_sin(x) + 1 end
                math.cos = nil
                tonumber = nil
                local real_table = table
                table = 42
                function step()
                    log("stepped")
                    return rnd(), calc(custom.base), top(), math.sin(0), real_table.concat({"a", "b"}, "-")
                end
            "#,
                "bug-t2-02",
            )
            .unwrap();

        let blob = source.snapshot().unwrap();
        let mut target = make_configured(20);
        target.restore(&blob).unwrap();
        assert_eq!(blob, target.snapshot().unwrap());
        target
            .execute(b"r, c, t, s, joined = step()", "verify")
            .unwrap();
        assert!(check(
            &mut target,
            "r == 0.5 and c == 30 and t == 20 and s == 1.0 and joined == 'a-b' and math.cos == nil and tonumber == nil and table == 42 and print == nil"
        ));
        assert_eq!(target.drain_log_records().len(), 1);

        // Rewinding the same mutated VM back to a pre-deletion snapshot also succeeds.
        let fresh_blob = make_configured(20).snapshot().unwrap();
        target.restore(&fresh_blob).unwrap();
        assert!(check(
            &mut target,
            "type(math.cos) == 'function' and type(tonumber) == 'function' and type(table) == 'table'"
        ));
    }

    #[test]
    fn bug_t2_03_malformed_and_deeply_nested_prototype_chunks_fail_closed_without_crashing() {
        let mut source = vm();
        source
            .execute(b"function f() return 1 end", "proto-seed")
            .unwrap();
        let valid_blob = source.snapshot().unwrap();
        let valid_data = SnapshotData::decode(&valid_blob).unwrap();
        let base_proto = valid_data.protos[0].clone();

        // 1. Corrupt upvalue debug name count > sizeupvalues (would heap-overflow f->upvalues in lundump.c).
        let mut crafted = Vec::new();
        crafted.extend_from_slice(&base_proto[..33]);
        crafted.push(0); // root cl->nupvalues = 0
        crafted.push(0); // source = NULL
        crafted.extend_from_slice(&0_i32.to_le_bytes()); // linedefined
        crafted.extend_from_slice(&0_i32.to_le_bytes()); // lastlinedefined
        crafted.extend_from_slice(&[0, 0, 2]); // numparams, is_vararg, maxstacksize
        crafted.extend_from_slice(&1_i32.to_le_bytes()); // sizecode = 1
        crafted.extend_from_slice(&(38_u32 | (1 << 23)).to_le_bytes()); // OP_RETURN 0 1
        crafted.extend_from_slice(&0_i32.to_le_bytes()); // sizek = 0
        crafted.extend_from_slice(&0_i32.to_le_bytes()); // sizeupvalues = 0
        crafted.extend_from_slice(&0_i32.to_le_bytes()); // sizep = 0
        crafted.extend_from_slice(&0_i32.to_le_bytes()); // sizelineinfo = 0
        crafted.extend_from_slice(&0_i32.to_le_bytes()); // sizelocvars = 0
        crafted.extend_from_slice(&1000_i32.to_le_bytes()); // sizeupvalnames = 1000 (> sizeupvalues=0!)
        for _ in 0..1000 {
            crafted.extend_from_slice(&[2, b'x']);
        }

        // 2. LUA_TSHRSTR (tag 4) followed by NULL string (0x00) -> NULL TString* segfault in lundump.c.
        let mut null_const_str = Vec::new();
        null_const_str.extend_from_slice(&base_proto[..33]);
        null_const_str.push(0);
        null_const_str.push(0);
        null_const_str.extend_from_slice(&0_i32.to_le_bytes());
        null_const_str.extend_from_slice(&0_i32.to_le_bytes());
        null_const_str.extend_from_slice(&[0, 0, 2]);
        null_const_str.extend_from_slice(&1_i32.to_le_bytes());
        null_const_str.extend_from_slice(&(38_u32 | (1 << 23)).to_le_bytes());
        null_const_str.extend_from_slice(&1_i32.to_le_bytes()); // sizek = 1
        null_const_str.extend_from_slice(&[4, 0]); // LUA_TSHRSTR + NULL DumpString!
        null_const_str.extend_from_slice(&0_i32.to_le_bytes()); // sizeupvalues
        null_const_str.extend_from_slice(&0_i32.to_le_bytes()); // sizep
        null_const_str.extend_from_slice(&0_i32.to_le_bytes());
        null_const_str.extend_from_slice(&0_i32.to_le_bytes());
        null_const_str.extend_from_slice(&0_i32.to_le_bytes());

        // 3. Invalid constant tag 99 -> lua_assert(0) in lundump.c.
        let mut bad_const_tag = null_const_str.clone();
        let tag_pos = 33 + 1 + 1 + 8 + 3 + 4 + 4 + 4;
        bad_const_tag[tag_pos] = 99;

        // 4. Mismatched cl->nupvalues (2) vs Proto.sizeupvalues (0).
        let mut mismatched_upvals = crafted[..crafted.len() - 2000 - 4].to_vec();
        mismatched_upvals.extend_from_slice(&0_i32.to_le_bytes());
        mismatched_upvals[33] = 2;

        // 5. Trailing garbage inside prototype dump.
        let mut trailing_proto = base_proto.clone();
        trailing_proto.extend_from_slice(b"EXTRA");

        // 6. Deeply nested child Protos (depth 600 -> unbounded C recursion stack overflow in LoadProtos).
        let mut deep_proto = Vec::new();
        deep_proto.extend_from_slice(&base_proto[..33]);
        deep_proto.push(0);
        let depth = 600usize;
        for level in 0..depth {
            deep_proto.push(0); // source = NULL
            deep_proto.extend_from_slice(&0_i32.to_le_bytes());
            deep_proto.extend_from_slice(&0_i32.to_le_bytes());
            deep_proto.extend_from_slice(&[0, 0, 2]);
            deep_proto.extend_from_slice(&1_i32.to_le_bytes());
            deep_proto.extend_from_slice(&(38_u32 | (1 << 23)).to_le_bytes());
            deep_proto.extend_from_slice(&0_i32.to_le_bytes()); // sizek
            deep_proto.extend_from_slice(&0_i32.to_le_bytes()); // sizeupvalues
            let children = i32::from(level + 1 < depth);
            deep_proto.extend_from_slice(&children.to_le_bytes()); // sizep
        }
        for _ in 0..depth {
            deep_proto.extend_from_slice(&0_i32.to_le_bytes()); // sizelineinfo
            deep_proto.extend_from_slice(&0_i32.to_le_bytes()); // sizelocvars
            deep_proto.extend_from_slice(&0_i32.to_le_bytes()); // sizeupvalnames
        }

        for (label, bad_proto) in [
            ("crafted_upval_names", crafted),
            ("null_const_str", null_const_str),
            ("bad_const_tag", bad_const_tag),
            ("mismatched_upvals", mismatched_upvals),
            ("trailing_proto", trailing_proto),
            ("deep_proto", deep_proto),
        ] {
            let mut data = SnapshotData::decode(&valid_blob).unwrap();
            data.protos[0] = bad_proto;
            if label != "trailing_proto" {
                data.closures[0].cells.clear();
            }
            let encoded = data.encode();
            let mut target = vm();
            target.execute(b"sentinel = 77", "sentinel").unwrap();
            let err = target.restore(&encoded).unwrap_err();
            assert!(
                matches!(err.kind, ErrorKind::InvalidArgument | ErrorKind::Limit),
                "{label}: unexpected kind {:?}",
                err.kind
            );
            // Rejected before mutating target VM!
            assert_eq!(target.global("sentinel").unwrap(), LuaValue::Integer(77));
        }
    }

    #[test]
    fn bug_t2_04_decode_rejects_trailing_garbage_invalid_booleans_and_empty_symbol_segments() {
        let mut source = vm();
        source.execute(b"flag = true", "bool").unwrap();
        let blob = source.snapshot().unwrap();

        // 1. Trailing garbage after root GVal.
        let mut with_trailing = blob.clone();
        with_trailing.push(0);
        let mut target = vm();
        assert_eq!(
            target.restore(&with_trailing).unwrap_err().kind,
            ErrorKind::InvalidArgument
        );

        // 2. Non-canonical boolean byte (2 instead of 0/1).
        let mut data = SnapshotData::decode(&blob).unwrap();
        data.tables[0] = TableDef {
            entries: vec![(GVal::Str(b"flag".to_vec()), GVal::Bool(true))],
        };
        let mut raw = data.encode();
        // Locate `flag` entry: Str("flag") = [4, 4,0,0,0, b'f',b'l',b'a',b'g'], Bool(true) = [1, 1]
        let needle = b"\x04\x04\x00\x00\x00flag\x01\x01";
        let pos = raw
            .windows(needle.len())
            .position(|w| w == needle)
            .expect("flag entry present");
        raw[pos + needle.len() - 1] = 2;
        assert_eq!(
            SnapshotData::decode(&raw).unwrap_err().kind,
            ErrorKind::InvalidArgument
        );

        // 3. Empty symbol segment (`len == 0`).
        let mut empty_seg = SnapshotData::decode(&blob).unwrap();
        empty_seg.symbols.push(vec![String::new()]);
        let raw_seg = empty_seg.encode();
        assert_eq!(
            SnapshotData::decode(&raw_seg).unwrap_err().kind,
            ErrorKind::InvalidArgument
        );
    }

    #[test]
    fn bug_t2_05_aggregate_node_budget_and_corrupt_table_entries_fail_closed_before_mutation() {
        // 1. Aggregate node cap: 4 tables * 1,500,000 entries = 6,000,000 (> MAX_NODES = 4,194,304),
        // even though each individual table is < MAX_NODES.
        let mut w = Writer::new();
        w.buf.extend_from_slice(MAGIC);
        w.u32(FORMAT_VERSION);
        w.u8(0); // profile Game
        w.u32(0); // symbols
        w.u32(0); // protos
        w.u32(4); // 4 tables
        for _ in 0..4 {
            w.u32(1_500_000);
            for i in 0..1_500_000_i64 {
                w.u8(1);
                w.u8(u8::from(i % 2 == 0));
                w.u8(1);
                w.u8(1);
            }
        }
        w.u32(0); // host_tables
        w.u32(0); // closures
        w.u32(0); // cells
        w.gval(&GVal::Table(0));
        assert_eq!(
            SnapshotData::decode(&w.buf).map(|_| ()).unwrap_err().kind,
            ErrorKind::Limit
        );

        // 2. Corrupt table entries: Nil key, NaN key, integral float key (1.0 / -0.0),
        // Nil value, duplicate keys, misplaced HostEntry, duplicate host_table symbol.
        let base_blob = vm().snapshot().unwrap();
        let bad_cases: Vec<(&str, Box<dyn Fn(&mut SnapshotData)>)> = vec![
            (
                "nil_key",
                Box::new(|d| {
                    d.tables.push(TableDef {
                        entries: vec![(GVal::Nil, GVal::Int(1))],
                    });
                }),
            ),
            (
                "nan_key",
                Box::new(|d| {
                    d.tables.push(TableDef {
                        entries: vec![(GVal::Num(f64::NAN.to_bits()), GVal::Int(1))],
                    });
                }),
            ),
            (
                "integral_float_key",
                Box::new(|d| {
                    d.tables.push(TableDef {
                        entries: vec![(GVal::Num((1.0_f64).to_bits()), GVal::Int(1))],
                    });
                }),
            ),
            (
                "neg_zero_float_key",
                Box::new(|d| {
                    d.tables.push(TableDef {
                        entries: vec![(GVal::Num((-0.0_f64).to_bits()), GVal::Int(1))],
                    });
                }),
            ),
            (
                "nil_value",
                Box::new(|d| {
                    d.tables.push(TableDef {
                        entries: vec![(GVal::Int(1), GVal::Nil)],
                    });
                }),
            ),
            (
                "duplicate_key",
                Box::new(|d| {
                    d.tables.push(TableDef {
                        entries: vec![
                            (GVal::Str(b"dup".to_vec()), GVal::Int(1)),
                            (GVal::Str(b"dup".to_vec()), GVal::Int(2)),
                        ],
                    });
                }),
            ),
            (
                "host_entry_in_plain_table",
                Box::new(|d| {
                    d.tables.push(TableDef {
                        entries: vec![(GVal::Str(b"f".to_vec()), GVal::HostEntry)],
                    });
                }),
            ),
            (
                "duplicate_host_table_symbol",
                Box::new(|d| {
                    let first = d.host_tables[0].0;
                    d.host_tables.push((first, TableDef::default()));
                }),
            ),
        ];

        for (label, mutate) in bad_cases {
            let mut data = SnapshotData::decode(&base_blob).unwrap();
            mutate(&mut data);
            let encoded = data.encode();
            let mut target = vm();
            target.execute(b"alive = 91", "alive").unwrap();
            let err = target.restore(&encoded).unwrap_err();
            assert_eq!(
                err.kind,
                ErrorKind::InvalidArgument,
                "{label}: expected InvalidArgument, got {err:?}"
            );
            assert_eq!(
                target.global("alive").unwrap(),
                LuaValue::Integer(91),
                "{label}: target VM was mutated before validation"
            );
        }

        // Pre-allocation closure upvalue arity check in validate_references():
        let mut closure_vm = vm();
        closure_vm
            .execute(
                b"f = (function() local x = 7 return function() return x end end)()",
                "closure",
            )
            .unwrap();
        let closure_blob = closure_vm.snapshot().unwrap();
        let mut bad_closure = SnapshotData::decode(&closure_blob).unwrap();
        assert!(!bad_closure.closures.is_empty());
        let dup_cell = bad_closure.closures[0].cells[0];
        bad_closure.closures[0].cells.push(dup_cell);
        assert!(
            bad_closure.validate_references().is_err(),
            "validate_references must reject closure whose cells.len() != proto upvalue count"
        );
    }

    #[test]
    fn bug_t2_08_require_loader_vm_refuses_snapshot_and_restore_explicitly() {
        let mut target = Vm::with_environment(
            crate::ExecutionLimits::default(),
            EnvironmentProfile::Extended,
        )
        .unwrap();
        let loader = crate::source::RequireLoader::new(|_| {
            Ok(crate::source::SourceChunk {
                source: b"loaded = 1".to_vec(),
                name: "@mod.lua".into(),
            })
        });
        target.install_require_loader(&loader).unwrap();
        let snap_err = target.snapshot().unwrap_err();
        assert_eq!(snap_err.kind, ErrorKind::Unsupported);
        assert!(
            snap_err.message.contains("require"),
            "unexpected message: {}",
            snap_err.message
        );
        let valid_blob = Vm::with_environment(
            crate::ExecutionLimits::default(),
            EnvironmentProfile::Extended,
        )
        .unwrap()
        .snapshot()
        .unwrap();
        let restore_err = target.restore(&valid_blob).unwrap_err();
        assert_eq!(restore_err.kind, ErrorKind::Unsupported);
    }

    #[test]
    fn numeric_subtypes_nan_payloads_neg_zero_cyclic_keys_and_shared_env_round_trip() {
        let mut source = vm();
        source
            .execute(
                br#"
                neg_zero = -0.0
                pos_zero = 0.0
                nan_val = 0.0 / 0.0
                imin = -9223372036854775807 - 1
                imax = 9223372036854775807
                f_imin = -9223372036854775808.0
                -- Cyclic table keys (mutual and self)
                cyc_a = {}
                cyc_b = {}
                cyc_a[cyc_a] = cyc_b
                cyc_a[cyc_b] = cyc_a
                cyc_b[cyc_a] = cyc_b
                -- Custom _ENV upvalue shared by closures
                local inc, get, swap
                do
                    local _ENV = { v = 10 }
                    inc = function(d) v = v + d end
                    get = function() return v end
                    swap = function() _ENV = { v = 999 } end
                end
                env_inc, env_get, env_swap = inc, get, swap
                env_inc(5)
            "#,
                "edge-cases",
            )
            .unwrap();
        // Inject a distinct quiet-NaN payload into a table value to verify bit-exact preservation.
        let custom_nan = f64::from_bits(0x7ff8_0000_1234_5678);
        source
            .environment
            .raw_set("custom_nan", Value::Number(custom_nan))
            .unwrap();

        let blob = source.snapshot().unwrap();
        let mut target = vm();
        target.restore(&blob).unwrap();
        assert_eq!(blob, target.snapshot().unwrap());
        assert!(check(
            &mut target,
            "1 / neg_zero == -math.huge and 1 / pos_zero == math.huge and nan_val ~= nan_val"
        ));
        assert_eq!(target.global("imin").unwrap(), LuaValue::Integer(i64::MIN));
        assert_eq!(target.global("imax").unwrap(), LuaValue::Integer(i64::MAX));
        assert_eq!(
            target.global("f_imin").unwrap(),
            LuaValue::Number(-9223372036854775808.0)
        );
        assert!(check(
            &mut target,
            "math.type(imin) == 'integer' and math.type(imax) == 'integer' and math.type(f_imin) == 'float'"
        ));
        assert!(check(
            &mut target,
            "cyc_a[cyc_a] == cyc_b and cyc_a[cyc_b] == cyc_a and cyc_b[cyc_a] == cyc_b"
        ));
        assert!(check(
            &mut target,
            "env_get() == 15 and (env_inc(7) or env_get() == 22) and (env_swap() or env_get() == 999)"
        ));
        let restored_nan: Value = target.environment.raw_get("custom_nan").unwrap();
        match restored_nan {
            Value::Number(v) => assert_eq!(v.to_bits(), 0x7ff8_0000_1234_5678),
            other => panic!("expected Value::Number, got {other:?}"),
        }
    }
}
