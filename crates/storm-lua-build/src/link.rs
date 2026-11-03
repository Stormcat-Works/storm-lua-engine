//! 依存 DFS + 静的展開（リンク）+ 連結オフセット区間表（設計 §3.2 / §10 実装ノート3）。
//!
//! `link_project` は `validate_project`（P1a）の結果を土台に:
//! - entry から到達するモジュールを DFS で辿り、`module-not-found` / `require-cycle` を診断化する
//!   （entry から到達しないモジュールはモジュールレベル DCE 対象。診断は出さない）
//! - エラー診断が一つもなければ、ソーステキストレベルで1本の Lua ソースへ静的展開する。
//!   各モジュール本体は連結チャンク先頭で予約名グローバルへ代入される関数として定義し、
//!   最初に require された位置でその関数を呼び出す（結果が nil なら `true` へ正規化する）。
//!   本体を import 側の字句スコープ内へ直接継ぐと、本来グローバルへ解決すべき自由名が
//!   import 側ローカルに捕捉される（Codex 第3回指摘7）ため、本体の定義位置は利用者
//!   ローカルが一切宣言されるより前に置く。結果変数をローカルにすると連結先チャンクの
//!   ローカル予算（200）を圧迫する（指摘15）ため、結果変数は予約名グローバルとする。
//!   2回目以降の require は結果変数への参照に置換する（標準 Lua の require 意味論と等価）。
//!   チャンクレベルで `...` を使うモジュールだけは、関数化すると `...` の意味が変わるため
//!   従来の inline 展開（do ブロック + 巻き上げ束縛、早期 return は IIFE）にフォールバック
//!   する。ambient メンバーも注入位置がチャンク先頭で捕捉が起きないため inline 展開を使う。
//! - 出力の任意行から由来モジュール・元行へ逆引きできる連結オフセット区間表を返す
//!   （Source Map（P4）はこの区間表を正本にして別途生成する。ここでは作らない）。
//!
//! 原文のコピー区間はUTF-8バイト範囲で保持する。同一行にある複数の展開境界も
//! 正確に区別できる。行フィールドとlookup_source_lineは粗い行対応の投影であり、
//! 詳細なmap・診断にはlookup_source_byteを使用する。合成部分に元位置を付けない。

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
pub use storm_lua_analysis::structure::{analyze_structure, StructuralAnalysis};

use std::ops::Range;
use storm_lua_analysis::diagnostic::Diagnostic;
use storm_lua_analysis::project::{AmbientMember, AmbientModuleKey, LuaProject, ModuleAnalysis};
use storm_lua_analysis::require_scan::RequireSite;
use storm_lua_syntax::ast::{Ast, Node, NodeId, TableField};
use storm_lua_syntax::lexer::Lexer;
use storm_lua_syntax::numeric::quote_lua;
use storm_lua_syntax::source_position::LineIndex;

/// Verbatim generated/original byte correspondence, with a coarse line projection.
/// `output_start_line..=output_end_line`（1-based, 両端含む）の各行 L は、
/// 元モジュール `module` のソース中 `source_start_line + (L - output_start_line)` 行目に対応する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedRange {
    pub output_start_line: u32,
    pub output_end_line: u32,
    pub module: String,
    pub source_start_line: u32,
    /// Inclusive generated UTF-8 byte offset of this verbatim slice.
    pub output_start_byte: usize,
    /// Exclusive generated UTF-8 byte offset; generated glue is outside the slice.
    pub output_end_byte: usize,
    /// Inclusive original UTF-8 byte offset in `module`.
    pub source_start_byte: usize,
}

impl LinkedRange {
    /// この区間が出力行 `output_line` を含むなら、対応する元ソース行を返す。
    pub fn source_line_for(&self, output_line: u32) -> Option<u32> {
        if output_line < self.output_start_line || output_line > self.output_end_line {
            return None;
        }
        Some(self.source_start_line + (output_line - self.output_start_line))
    }
}

/// `link_project` の結果。
pub struct LinkResult {
    /// `validate_project` の診断 + `module-not-found` / `require-cycle`。
    pub diagnostics: Vec<Diagnostic>,
    /// エラー診断が1件でもあれば `None`（設計 §6: エラー時はリンク済みソースを返さない）。
    pub linked_source: Option<String>,
    /// リンクされたモジュールキー（実行順 = 各モジュールの本体が最初に実行され始める順）。
    /// entry が先頭。リンクに失敗した場合は空。
    pub used_modules: Vec<String>,
    /// 出力行 → 由来モジュール・元行の逆引き表。リンクに失敗した場合は空。
    pub ranges: Vec<LinkedRange>,
    /// 注入された ambient メンバー。ルート名 → メンバー名の配列（設計 §6）。
    /// tree shaking 後の実際の注入結果であり、`used_modules` から到達しない ambient 参照は
    /// 含まれない。リンクに失敗した場合は空。
    pub injected_ambient: BTreeMap<String, Vec<String>>,
}

/// `出力の任意行 → 由来モジュール・元行` を区間表から逆引きする（テスト・診断ツール向け補助）。
pub fn lookup_source_line(ranges: &[LinkedRange], output_line: u32) -> Option<(&str, u32)> {
    ranges.iter().find_map(|r| {
        r.source_line_for(output_line)
            .map(|line| (r.module.as_str(), line))
    })
}

/// Locate an exact original byte in ordered, nonoverlapping verbatim ranges.
/// Generated glue, EOF and removed source have no origin. The caller must use
/// the same source snapshots as the linker; a byte need not be a UTF-8 boundary.
pub fn lookup_source_byte(
    ranges: &[LinkedRange],
    output_byte: usize,
) -> Option<(&LinkedRange, usize)> {
    let i = ranges
        .partition_point(|range| range.output_start_byte <= output_byte)
        .checked_sub(1)?;
    let range = &ranges[i];
    if output_byte >= range.output_end_byte {
        return None;
    }
    let source_byte = range
        .source_start_byte
        .checked_add(output_byte - range.output_start_byte)?;
    Some((range, source_byte))
}

// Edits blank bytes without moving offsets. Subtract their explicitly recorded
// intervals from original slices; never infer provenance by comparing strings.
pub(crate) fn exclude_blanked_ranges(
    project: &LuaProject,
    code: &str,
    ranges: Vec<LinkedRange>,
    mut removed: Vec<Range<usize>>,
) -> Result<Vec<LinkedRange>, String> {
    removed.sort_by_key(|span| span.start);
    let mut merged: Vec<Range<usize>> = Vec::new();
    for span in removed {
        if let Some(last) = merged.last_mut() {
            if span.start <= last.end {
                last.end = last.end.max(span.end);
                continue;
            }
        }
        merged.push(span);
    }
    let output_index = LineIndex::new(code);
    let mut originals = BTreeMap::new();
    let mut retained = Vec::new();
    for range in ranges {
        if !originals.contains_key(&range.module) {
            let text = crate::source_map::source_text(project, &range.module)
                .ok_or("linked source snapshot missing")?;
            originals.insert(range.module.clone(), LineIndex::new(text));
        }
        let original = &originals[&range.module];
        let mut append = |start: usize, end: usize| -> Result<(), String> {
            if start >= end {
                return Ok(());
            }
            let source_start_byte = range.source_start_byte + start - range.output_start_byte;
            let output_start_line = output_index
                .byte_position(start)
                .ok_or("linked start boundary invalid")?
                .0;
            let output_end_line = output_index
                .byte_position(end)
                .ok_or("linked end boundary invalid")?
                .0
                - u32::from(code.as_bytes()[end - 1] == b'\n');
            let source_start_line = original
                .byte_position(source_start_byte)
                .ok_or("linked original boundary invalid")?
                .0;
            retained.push(LinkedRange {
                output_start_line,
                output_end_line,
                module: range.module.clone(),
                source_start_line,
                output_start_byte: start,
                output_end_byte: end,
                source_start_byte,
            });
            Ok(())
        };
        let mut cursor = range.output_start_byte;
        let first = merged.partition_point(|span| span.end <= cursor);
        for span in &merged[first..] {
            if span.start >= range.output_end_byte {
                break;
            }
            append(cursor, span.start.min(range.output_end_byte))?;
            cursor = cursor.max(span.end).min(range.output_end_byte);
        }
        append(cursor, range.output_end_byte)?;
    }
    Ok(retained)
}

/// モジュール本体がチャンクレベル（ネストした関数本体の中を除く）で `...` を使うか。
/// ラッパを可変長関数にし、glue が require 意味論どおりモジュールキーを引数として
/// 渡す必要があるかの判定に使う（候補L1）。
fn uses_chunk_vararg(ast: &Ast, id: NodeId) -> bool {
    match ast.node(id) {
        Node::Vararg => true,
        // ネストした関数本体は独立した可変引数スコープなので辿らない。
        Node::Function(..) => false,
        _ => {
            let mut found = false;
            storm_lua_syntax::ast_utils::for_each_child(ast, id, &mut |child| {
                if !found && uses_chunk_vararg(ast, child) {
                    found = true;
                }
            });
            found
        }
    }
}

/// バイト `i` が行区切りの先頭ならそのバイト長を返す（ペアは2、単独は1、非改行は0）。
/// lexer の意味論に合わせ、`\n`、`\r`、`\r\n`、`\n\r` はいずれも1つの改行として扱う。
fn newline_len(bytes: &[u8], i: usize) -> usize {
    let b = bytes[i];
    if b != b'\n' && b != b'\r' {
        return 0;
    }
    if i + 1 < bytes.len() && bytes[i + 1] != b && (bytes[i + 1] == b'\n' || bytes[i + 1] == b'\r')
    {
        2
    } else {
        1
    }
}

/// (1-based line, 1-based col) を対応するソースのバイトオフセットへ変換する。
/// col はバイト単位（lexer の列カウントと同じ意味論。§ P0 の位置サイドテーブル前提）。
/// 行の区切りも lexer と同じ意味論（`newline_len`）で数える。`\n` だけを数えると
/// CR 行末のソースでパーサ位置とずれて、文字の途中を指したり無効な連結結果になる。
fn line_col_to_byte(source: &str, line: u32, col: u32) -> usize {
    let bytes = source.as_bytes();
    let mut byte = 0usize;
    let mut cur_line = 1u32;
    let mut i = 0usize;
    while i < bytes.len() && cur_line < line {
        let len = newline_len(bytes, i);
        if len > 0 {
            cur_line += 1;
            i += len;
            byte = i;
        } else {
            i += 1;
        }
    }
    byte + (col as usize - 1)
}

fn line_of(s: &str) -> u32 {
    1 + count_newlines(s)
}

fn sanitize_key_for_ident(key: &str) -> String {
    key.chars()
        .map(|c| if c == '.' { '_' } else { c })
        .collect()
}

/// `reserved`（プロジェクト内の全識別子）と衝突しない変数名を生成し、`reserved` へ登録する。
/// サイレントに壊さない: 衝突時は連番で機械的に回避する（無限ループはしない — 連番は必ず尽きるまで
/// 増加し続け、`reserved` は有限集合なのでいずれ必ず未使用の名前に到達する）。
fn fresh_var_name(reserved: &mut HashSet<String>, key: &str) -> String {
    let base = format!("__stormmin_link_{}", sanitize_key_for_ident(key));
    let mut candidate = base.clone();
    let mut n = 2u32;
    while reserved.contains(&candidate) {
        candidate = format!("{base}_{n}");
        n += 1;
    }
    reserved.insert(candidate.clone());
    candidate
}

/// チャンク関数（root ブロック）の中で同時に有効になるローカルの最大数。
/// フォールバック側ロード判定（storm-lua-vm の load_legality）と同じ
/// 計数規則（for の隠れローカルを含む）で、関数本体には入らない
/// （別フレームであり、レジストリは upvalue として無料で参照される）。
fn chunk_peak_locals(ast: &Ast, root: NodeId) -> usize {
    fn walk_block(ast: &Ast, block: NodeId, base: usize) -> usize {
        let Node::Block(statements) = ast.node(block) else {
            return base;
        };
        let mut cur = base;
        let mut peak = cur;
        for statement in statements {
            peak = peak.max(walk_stmt(ast, *statement, &mut cur));
        }
        peak
    }
    fn walk_stmt(ast: &Ast, stmt: NodeId, cur: &mut usize) -> usize {
        match ast.node(stmt) {
            Node::Local(names, _) => {
                *cur += names.len();
                *cur
            }
            Node::Localfunc(..) => {
                *cur += 1;
                *cur
            }
            Node::Do(block) => walk_block(ast, *block, *cur),
            Node::While(_, block) | Node::Repeat(block, _) => walk_block(ast, *block, *cur),
            Node::If(arms, else_block) => {
                let mut peak = *cur;
                for arm in arms {
                    peak = peak.max(walk_block(ast, arm.body, *cur));
                }
                if let Some(block) = else_block {
                    peak = peak.max(walk_block(ast, *block, *cur));
                }
                peak
            }
            Node::Fornum(_, _, _, _, block) => walk_block(ast, *block, *cur + 4),
            Node::Forin(names, _, block) => walk_block(ast, *block, *cur + names.len() + 3),
            _ => *cur,
        }
    }
    walk_block(ast, root, 0)
}

/// レジストリをチャンクローカルとして追加すると生成物のレジスタ
/// ピークが上限（255）を超える見積り境界（R08）。見積り＋1（レジストリ
/// 分）が 255 を超える場合はラップ形式へ切り替える。
const REGISTER_WRAP_THRESHOLD: usize = 254;

/// entry チャンクフレームのレジスタ使用量の上限見積り（R08）。
///
/// PUC のレジスタ割当は「有効ローカル＋式の一時領域」で決まる。
/// 見積りは PUC の割当規則に沿った保守的な上限であり、過大見積りは
/// 安全側（ラップ形式は意味論等価）に倒れる。関数本体は別フレームの
/// ため入らない（[`chunk_peak_locals`] と同じ範囲）。
fn chunk_peak_registers(ast: &Ast, root: NodeId) -> usize {
    /// 式を新規レジスタへ評価するのに要する一時レジスタ数の上限。
    fn expr_temps(ast: &Ast, e: NodeId) -> usize {
        match ast.node(e) {
            Node::Nil
            | Node::Bool(_)
            | Node::Vararg
            | Node::Num(_)
            | Node::Str(_)
            | Node::Name(_)
            | Node::Function(..) => 1,
            Node::Paren(inner) | Node::Un(_, inner) => expr_temps(ast, *inner),
            Node::Bin(_, l, r) => expr_temps(ast, *l).max(1 + expr_temps(ast, *r)),
            Node::Index(base, key, _) => expr_temps(ast, *base) + expr_temps(ast, *key) + 1,
            Node::Call(f, args, _) => expr_temps(ast, *f).max(list_peak(ast, args) + 1) + 1,
            Node::Table(fields) => {
                let mut peak = 1;
                for (i, field) in fields.iter().enumerate() {
                    let t = match field {
                        TableField::Arr(v) => expr_temps(ast, *v),
                        TableField::Name(_, v) => expr_temps(ast, *v),
                        TableField::KVar(k, v) => expr_temps(ast, *k) + expr_temps(ast, *v),
                    };
                    peak = peak.max(i + t + 1);
                }
                peak
            }
            _ => 1,
        }
    }
    /// 式リストを連続レジスタへ評価する際のピーク（基底からの相対）。
    fn list_peak(ast: &Ast, values: &[NodeId]) -> usize {
        let mut peak = 0;
        for (i, v) in values.iter().enumerate() {
            peak = peak.max(i + expr_temps(ast, *v));
        }
        peak
    }
    /// ブロック内のレジスタピーク。`cur` は入口の有効ローカル数で、
    /// 出口ではブロック内宣言分が加わった値になる（呼出側は必要なら
    /// 入口値を保持して使う）。
    fn walk_block(ast: &Ast, block: NodeId, cur: &mut usize) -> usize {
        let Node::Block(statements) = ast.node(block) else {
            return *cur;
        };
        let mut peak = *cur;
        for statement in statements {
            peak = peak.max(walk_stmt(ast, *statement, cur));
            peak = peak.max(*cur);
        }
        peak
    }
    fn walk_stmt(ast: &Ast, stmt: NodeId, cur: &mut usize) -> usize {
        match ast.node(stmt) {
            Node::Local(names, values) => {
                let p = *cur + list_peak(ast, values);
                *cur += names.len();
                p.max(*cur)
            }
            Node::Localfunc(..) => {
                *cur += 1;
                *cur
            }
            Node::Assign(targets, values) => {
                // Index ターゲットの (base, キー) は右辺評価中も保持される。
                let mut held = 0usize;
                let mut lhs_peak = 0usize;
                // 左辺走査中、先行ターゲットが実体化した (base, キー) は
                // 後続ターゲットの base／キー評価の間もレジスタを保持
                // する（T08）。ターゲットごとに独立に見積もると、2番目
                // 以降のターゲットの巨大なキー式が保持分を見落として
                // 上限内と誤判定される。ドットキーは定数でレジスタを
                // 使わないため base の1つ分だけが残る。
                let mut scanned = 0usize;
                for t in targets {
                    if let Node::Index(base, key, dot) = ast.node(*t) {
                        held += 2;
                        let kp = if *dot { 0 } else { expr_temps(ast, *key) };
                        lhs_peak = lhs_peak.max(scanned + expr_temps(ast, *base) + kp);
                        scanned += 1 + usize::from(!*dot);
                    }
                }
                *cur + lhs_peak.max(held + list_peak(ast, values).max(targets.len()))
            }
            Node::Return(values) => *cur + list_peak(ast, values),
            Node::Callstat(call) => *cur + expr_temps(ast, *call),
            Node::Funcstat(target, value) => {
                let t = match ast.node(*target) {
                    Node::Index(base, key, dot) => {
                        expr_temps(ast, *base) + if *dot { 0 } else { expr_temps(ast, *key) }
                    }
                    _ => 0,
                };
                *cur + t.max(expr_temps(ast, *value))
            }
            Node::Do(block) => {
                let mut inner = *cur;
                walk_block(ast, *block, &mut inner)
            }
            Node::While(cond, block) => {
                let mut inner = *cur;
                (*cur + expr_temps(ast, *cond)).max(walk_block(ast, *block, &mut inner))
            }
            Node::Repeat(block, cond) => {
                // 条件式は本体ブロックのスコープ内で評価されるため、
                // 本体を抜けた時点のローカル数の上に一時領域が乗る。
                let mut inner = *cur;
                walk_block(ast, *block, &mut inner).max(inner + expr_temps(ast, *cond))
            }
            Node::If(arms, else_block) => {
                let mut peak = *cur;
                for arm in arms {
                    peak = peak.max(*cur + expr_temps(ast, arm.cond));
                    let mut inner = *cur;
                    peak = peak.max(walk_block(ast, arm.body, &mut inner));
                }
                if let Some(block) = else_block {
                    let mut inner = *cur;
                    peak = peak.max(walk_block(ast, *block, &mut inner));
                }
                peak
            }
            Node::Fornum(_, a, b, c, block) => {
                let mut exprs = vec![*a, *b];
                if let Some(step) = c {
                    exprs.push(*step);
                }
                let mut inner = *cur + 4;
                (*cur + list_peak(ast, &exprs)).max(walk_block(ast, *block, &mut inner))
            }
            Node::Forin(names, exprs, block) => {
                let mut inner = *cur + names.len() + 3;
                (*cur + list_peak(ast, exprs)).max(walk_block(ast, *block, &mut inner))
            }
            _ => *cur,
        }
    }
    let mut cur = 0usize;
    walk_block(ast, root, &mut cur)
}

/// モジュール本体がチャンクの `_ENV` セル自体を再束縛するか（R07）。
///
/// 共有セルの観測可能な差異は「セルの再束縛」だけである: フィールド
/// 書込み（`_ENV.x = v` や大域名への代入）は、セルが共有でも個別でも
/// 同じ環境表を書き換えるため等価であり、`local _ENV` 宣言は元々
/// そのモジュール自身のセルを作る。再束縛（`_ENV = ...`、`function
/// _ENV() ...`）がプロジェクトのどこにも無ければ、共有セルと
/// モジュールごとのセルは観測等価になる。シャドウされた書込み
/// （内側の `local _ENV` への代入）も検出するが、それは安全側の
/// 過検出である（下記 define_module の条件付き生成を参照）。
fn ast_rebinds_env(ast: &Ast, root: NodeId) -> bool {
    let is_env =
        |id: NodeId| matches!(ast.node(id), Node::Name(s) if ast.strings.get(*s) == "_ENV");
    let mut found = false;
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |id| match ast.node(id) {
        Node::Assign(targets, _) if targets.iter().any(|t| is_env(*t)) => {
            found = true;
        }
        Node::Funcstat(target, _) if is_env(*target) => {
            found = true;
        }
        _ => {}
    });
    found
}

fn collect_reserved_names(modules: &BTreeMap<String, ModuleAnalysis>) -> HashSet<String> {
    let mut set = HashSet::new();
    for analysis in modules.values() {
        for s in analysis.ast.strings.all_strings() {
            set.insert(s);
        }
    }
    set
}

/// 展開先の出力バッファと、そのバッファ内座標で記録された区間表。
/// モジュール定義（defs）と本体（main）を別バッファで組み立て、最後に連結して
/// main 側の区間をずらすために分ける。
#[derive(Default)]
struct Sink {
    output: String,
    ranges: Vec<LinkedRange>,
}

impl Sink {
    /// Append generated glue without inventing a source location. A later verbatim
    /// slice on the same output line may still provide its real source mapping.
    fn append_generated(&mut self, text: &str) {
        self.output.push_str(text);
    }

    /// モジュール `key` の元ソース `source[start..end]` を一切改変せずそのまま追記する。
    fn append_source_slice(&mut self, key: &str, source: &str, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let text = &source[start..end];
        let source_start_line = 1 + count_newlines(&source[..start]);
        let out_start = line_of(&self.output);
        let output_start_byte = self.output.len();
        self.output.push_str(text);
        let newline_count = count_newlines(text);
        let out_end = if text.ends_with('\n') || text.ends_with('\r') {
            out_start + newline_count - 1
        } else {
            out_start + newline_count
        };
        self.ranges.push(LinkedRange {
            output_start_line: out_start,
            output_end_line: out_end,
            module: key.to_string(),
            source_start_line,
            output_start_byte,
            output_end_byte: self.output.len(),
            source_start_byte: start,
        });
    }

    /// 生成ラッパの閉じ行（`end` 等、1行分）を追記する。
    ///
    /// Lua は本体終端で判明するロードエラー（未解決 goto 等）を、関数本体を
    /// 閉じた時点の先読みトークンの行で報告する。参照チャンクでは先読みは
    /// EOF であり、その行がモジュールの EOF 行になる。ラッパの閉じ行は生成行
    /// で区間を持たないため、そのままでは先読みが後続の生成文へ進み、行の
    /// 逆引きが NONE になる。そこで閉じ行を「モジュール自身の EOF 行」へ
    /// 対応づけ（ラッパ定義では閉じの直後に空文 `;` を置き、先読みを閉じ行に
    /// 留める）ことで帰属を回復する。バイト区間は一切変更しない（由来
    /// バイトの捏造はしない）:
    /// (a) 本体が改行で終わり、最終 verbatim 区間が出力末尾に届いている場合、
    ///     閉じ行をその区間の行範囲に含める。最終区間は元ソース末尾まで届いて
    ///     いるため、行の affine 対応で EOF 行に一致する。
    /// (b) 本体が改行で終わらず、最終行にコメント開始 `--` が無い場合は、
    ///     閉じトークンを最終行へ相乗りさせる。末尾改行の無いソースでは
    ///     EOF 行 = 最終行であり、既存区間がそのまま対応する。
    /// どちらも使えない形状（最終行がコメント等）は従来どおり区間外とする。
    fn close_wrapper(&mut self, closing: &str) {
        let ends_line = self.output.ends_with('\n') || self.output.ends_with('\r');
        if ends_line {
            let out_len = self.output.len();
            let mut extended = false;
            if let Some(last) = self.ranges.last_mut() {
                if last.output_end_byte == out_len {
                    last.output_end_line += 1;
                    extended = true;
                }
            }
            if extended {
                self.output.push_str(closing);
                self.output.push('\n');
                return;
            }
        } else {
            let last_line = self.output.rsplit(['\n', '\r']).next().unwrap_or("");
            if !last_line.contains("--") {
                self.output.push(' ');
                self.output.push_str(closing);
                self.output.push('\n');
                return;
            }
        }
        self.output.push('\n');
        self.output.push_str(closing);
        self.output.push('\n');
    }
}

/// レジストリの配置方式（R06〜R08）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum RegistryMode {
    /// チャンク先頭のローカルとして宣言する（通常形）。
    ChunkLocal,
    /// `do` ブロックで全体を包み、レジストリをブロックローカル、
    /// entry 本体を可変長ラッパ関数の本体として配置する。レジストリは
    /// 定義群とラッパ関数から upvalue として参照され、entry フレームの
    /// ローカル／レジスタ予算を一切消費しない。
    Wrapped,
}

struct Expander<'a> {
    project: &'a LuaProject,
    modules: &'a BTreeMap<String, ModuleAnalysis>,
    var_names: HashMap<String, String>,
    /// inline 本体（ambient 方式）を既に発行したモジュール。ラッパ方式の
    /// 初期化 glue は全 require 位置へ発行するため、この集合は使わない。
    expanded: HashSet<String>,
    /// ラッパ定義を defs へ既に発行したモジュール。
    defined: HashSet<String>,
    /// 発行済みラッパ定義（テキスト, そのテキスト内座標の区間表）。
    defs: Vec<(String, Vec<LinkedRange>)>,
    /// 全モジュールのローダ／結果を保持するチャンク先頭のローカル
    /// レジストリ表の名前（Q06〜Q08）。`__stormmin_link_` 接頭辞を持ち、
    /// プロジェクト内の全文字列と衝突しないことが保証される。字句的な
    /// ローカルであるため、利用者の `local _ENV` に遮蔽されず、モジュール
    /// 本体内の関数からは upvalue として参照される。
    registry: String,
    /// レジストリの配置方式。entry のチャンクフレームに余裕がある場合は
    /// チャンク先頭のローカルとして宣言する。entry がローカル上限
    /// （200）またはレジスタ上限（255）近傍を使い切っている場合は
    /// ラップ形式にし、レジストリを entry フレームの外（do ブロックの
    /// ローカル）に置くことで、字句的可視性を境界条件でも保つ
    /// （R06〜R08。旧来のグローバル宣言フォールバックは、利用者の
    /// `local _ENV` 遮蔽下で参照できず R06 の失敗を招いたため廃止）。
    registry_mode: RegistryMode,
    /// モジュール定義を自前の `_ENV` セルで包むか（R07）。プロジェクト
    /// 内のどこか（entry／到達可能なモジュール／ambient）に `_ENV`
    /// セルの再束縛がある場合のみ包む。再束縛が無ければ共有セルと
    /// 観測等価であり、包む必要はない。無条件に包むと生成テキストへ
    /// `_ENV` 識別子が入り、プロジェクト自身に `_ENV` が無い場合でも
    /// minify パイプラインの _ENV ハザードゲート
    /// （environment_checks::lexical_reason）が発火して全リンク
    /// プロジェクトが保守的 minify へ倒れてしまう。再束縛を持つ
    /// プロジェクトは元々そのゲートの対象であるため、条件付き生成
    /// では新規の回帰は生じない。
    env_isolation: bool,
    /// モジュールキー → チャンクレベル `...` を使うか（ラッパを可変長にする判定）。
    vararg: HashMap<String, bool>,
    /// `"root.member"` 合成キー → ambient `source` テキスト。`project.modules` には
    /// ambient のソースは含まれないため、`expand_dependency` が参照できるよう別に持つ。
    ambient_sources: HashMap<String, &'a str>,
}

impl<'a> Expander<'a> {
    fn new(
        project: &'a LuaProject,
        modules: &'a BTreeMap<String, ModuleAnalysis>,
        _order: &[String],
        env_isolation: bool,
    ) -> Self {
        let mut reserved = collect_reserved_names(modules);
        // レジストリはチャンク先頭の単一ローカル表にする（Q06〜Q08）。
        // 旧方式の予約名グローバルは、利用者の `local _ENV` がある位置の
        // require では解決できず（Q08）、初期化 glue が最初の require
        // 位置にしか無いため goto で飛び越すと未初期化のローダを値として
        // 読んでしまい（Q06）、false を返すモジュールの再読込もできな
        // かった（Q07）。ローカル1個の予算消費と引き換えに、字句解決で
        // 全てを解消する。
        let registry = fresh_var_name(&mut reserved, "modules");
        // 配置方式は entry のチャンクフレームの使用量で決める: ローカル
        // 上限（200）にもレジスタ上限見積りにも余裕がある場合のみ
        // チャンクローカルとし、それ以外はラップ形式にする（R06〜R08）。
        let registry_mode = modules
            .get(&project.entry)
            .map(|entry| {
                let locals = chunk_peak_locals(&entry.ast, entry.root);
                let registers = chunk_peak_registers(&entry.ast, entry.root);
                if locals < 200 && registers < REGISTER_WRAP_THRESHOLD {
                    RegistryMode::ChunkLocal
                } else {
                    RegistryMode::Wrapped
                }
            })
            .unwrap_or(RegistryMode::ChunkLocal);
        let var_names = HashMap::new();
        let mut ambient_sources = HashMap::new();
        for (root, namespace) in &project.ambient {
            for (member, definition) in &namespace.members {
                if let AmbientMember::Module { source } = definition {
                    ambient_sources.insert(format!("{root}.{member}"), source.as_str());
                }
            }
        }
        Self {
            project,
            modules,
            var_names,
            expanded: HashSet::new(),
            defined: HashSet::new(),
            defs: Vec::new(),
            registry,
            registry_mode,
            env_isolation,
            vararg: HashMap::new(),
            ambient_sources,
        }
    }

    /// `key` の元ソーステキストを取得する。通常モジュールは `project.modules`、
    /// ambient `"root.member"` 合成キーは `ambient_sources` から引く。
    #[expect(
        clippy::expect_used,
        reason = "Only validated project or injected ambient keys reach expansion; the source maps are immutable during linking"
    )]
    fn source_of(&self, key: &str) -> &str {
        self.project
            .modules
            .get(key)
            .map(String::as_str)
            .or_else(|| self.ambient_sources.get(key).copied())
            .expect("expand_dependency は project.modules か ambient_sources のいずれかに存在するキーにのみ呼ばれる")
    }

    /// ambient メンバーを出力チャンク先頭へ注入する（設計 §4.2）。
    /// `order` は `ambient_closure_order` が返す依存順（postorder: 依存が先）。
    /// `self.modules` には呼び出し前に ambient source が `"root.member"` 合成キーで
    /// マージ済みであること（`link_project` が担う）。
    fn inject_ambient(&mut self, sink: &mut Sink, order: &[AmbientModuleKey]) {
        if order.is_empty() {
            return;
        }
        let mut roots_seen = HashSet::new();
        for (root, _) in order {
            if roots_seen.insert(root.clone()) {
                sink.append_generated(&format!("{root} = {{}}\n"));
            }
        }
        for (root, member) in order {
            let synthetic_key = format!("{root}.{member}");
            let target = format!("{root}[{}]", quote_lua(member));
            self.var_names.insert(synthetic_key.clone(), target);
            self.expand_inline(sink, &synthetic_key);
        }
    }

    fn run(&mut self, sink: &mut Sink) {
        let entry = self.project.entry.clone();
        let end = self.project.modules[&entry].len();
        self.append_transformed_region(sink, &entry, end);
    }

    /// defs（モジュール定義群）と main を連結し、main 側区間の座標をずらして返す。
    fn finish(self, main: Sink) -> (String, Vec<LinkedRange>) {
        let mut main = main;
        // チャンク終端で判明するロードエラーは連結チャンクの EOF 行で報告
        // される。entry 本体が改行で終わり最終区間が出力末尾に届いている
        // 場合、その区間の行範囲を EOF 行まで伸ばし、entry 自身の EOF 行へ
        // 対応づける（バイト区間は変更しない。Sink::close_wrapper の (a) と
        // 同じ扱い）。
        if main.output.ends_with('\n') || main.output.ends_with('\r') {
            let out_len = main.output.len();
            if let Some(last) = main.ranges.last_mut() {
                if last.output_end_byte == out_len {
                    last.output_end_line += 1;
                }
            }
        }
        let wrapped = !self.defs.is_empty() && self.registry_mode == RegistryMode::Wrapped;
        let mut output = String::new();
        let mut ranges = Vec::new();
        let mut line_shift = 0u32;
        let mut byte_shift = 0usize;
        if !self.defs.is_empty() {
            // レジストリ宣言は全モジュール定義より前に置く（defs は
            // ラッパ定義だけを含む）。生成行であり区間は持たない。
            // ラップ形式では宣言が do ブロックのローカルになり、entry
            // 本体（可変長ラッパ関数の本体）からは upvalue として
            // 参照される（R06〜R08）。
            let declaration = if wrapped {
                format!("do\nlocal {} = {{}}\n", self.registry)
            } else {
                format!("local {} = {{}}\n", self.registry)
            };
            line_shift += count_newlines(&declaration);
            byte_shift += declaration.len();
            output.push_str(&declaration);
        }
        for (text, def_ranges) in &self.defs {
            output.push_str(text);
            for range in def_ranges {
                ranges.push(shift_range(range, line_shift, byte_shift));
            }
            line_shift += count_newlines(text);
            byte_shift += text.len();
        }
        if wrapped {
            // entry 本体を可変長ラッパ関数の本体として配置する。
            // チャンクへの引数はそのまま転送され、本体の return は
            // 関数の return として外側の return が伝播する。本体終端で
            // 判明するロードエラーの先読みはこの後の閉じ行に届くため、
            // 先頭で行った EOF 行への区間拡張（+1行）がそのまま効く。
            let header = "return (function(...)\n";
            line_shift += count_newlines(header);
            byte_shift += header.len();
            output.push_str(header);
        }
        output.push_str(&main.output);
        for range in &main.ranges {
            ranges.push(shift_range(range, line_shift, byte_shift));
        }
        if wrapped {
            // 閉じ行は本体の最終行に相乗りさせる（最終行がコメントを
            // 含む場合は行を分ける）。Sink::close_wrapper の扱いに倣う。
            let suffix = if main.output.ends_with('\n') || main.output.ends_with('\r') {
                "end)(...)\nend\n"
            } else {
                let last_line = main.output.rsplit(['\n', '\r']).next().unwrap_or("");
                if last_line.contains("--") {
                    "\nend)(...)\nend\n"
                } else {
                    " end)(...)\nend\n"
                }
            };
            output.push_str(suffix);
        }
        (output, ranges)
    }

    /// モジュール `key` にラッパ方式（先頭定義 + 呼び出し）を使えるか。
    /// 使えないのは ambient 合成キーだけ（注入位置がチャンク先頭に固定され、
    /// 結果変数が `root[member]` 式のため）。チャンクレベルで `...` を使う
    /// モジュールは、可変長ラッパ + キー引数で意味を保存できる（候補L1）。
    fn wrapper_applies(&mut self, key: &str) -> bool {
        !self.ambient_sources.contains_key(key)
    }

    /// モジュール `key` がチャンクレベルで `...` を使うか（結果はキャッシュする）。
    fn module_vararg(&mut self, key: &str) -> bool {
        if let Some(&cached) = self.vararg.get(key) {
            return cached;
        }
        let analysis = &self.modules[key];
        let value = uses_chunk_vararg(&analysis.ast, analysis.root);
        self.vararg.insert(key.to_string(), value);
        value
    }

    /// `key` の require 文を展開・置換しつつ `source[0..region_end)` を追記する。
    /// `region_end` はモジュール全文の長さ（NoReturn/Iife）か、末尾 return 文の開始位置（TailReturn）。
    fn append_transformed_region(&mut self, sink: &mut Sink, key: &str, region_end: usize) {
        let source = self.source_of(key).to_string();
        let sites: Vec<RequireSite> = self.modules[key].requires.clone();
        let mut cursor = 0usize;
        for site in &sites {
            let (stmt_start, stmt_end) = require_stmt_byte_span(&source, site);
            debug_assert!(
                stmt_end <= region_end,
                "require 文の位置がモジュール展開領域を超えている(内部不変条件違反)"
            );
            sink.append_source_slice(key, &source, cursor, stmt_start);
            self.append_require_replacement(sink, site);
            cursor = stmt_end;
        }
        sink.append_source_slice(key, &source, cursor, region_end);
    }

    /// モジュール `key` のレジストリスロット式 `REG["key"]`。
    fn registry_slot(&self, key: &str) -> String {
        format!("{}[{}]", self.registry, quote_lua(key))
    }

    fn append_require_replacement(&mut self, sink: &mut Sink, site: &RequireSite) {
        let value_expr = if self.wrapper_applies(&site.key) {
            if !self.defined.contains(&site.key) {
                self.define_module(&site.key);
            }
            let slot = self.registry_slot(&site.key);
            // 標準 require はローダへ (モジュール名, ローダデータ) の
            // 2引数を渡す（preload 系のローダデータは nil）。静的リンク
            // のローダも同じ形で呼び、チャンクレベル `...` が引数の個数
            // まで観測できるようにする（T09）。可変長でないラッパでも
            // 引数は観測されないだけで害はない。
            let args = format!("{}, nil", quote_lua(site.key.as_bytes()));
            // 初期化 glue は全ての require 位置へ発行する（Q06）。旧方式は
            // 最初の位置にしか発行せず、goto で飛び越した後続位置が未初期化
            // のローダ関数を「モジュールの値」として読んでいた。ガードは
            // package.loaded と同じ truthiness 判定: ローダはスロット [1] に
            // 残り続け、結果が false のモジュールは次の require で再読込
            // される（Q07。nil 結果は true へ正規化され再読込されない）。
            // 再実行（goto ループ等）でも、初期化済みなら素通りする
            // （候補L3 の性質はこの形でも保たれる）。
            sink.append_generated(&format!(
                "if not {slot}[2] then\n{slot}[2] = {slot}[1]({args})\nif {slot}[2] == nil then {slot}[2] = true end\nend\n"
            ));
            format!("{slot}[2]")
        } else {
            if !self.expanded.contains(&site.key) {
                self.expand_inline(sink, &site.key);
            }
            self.var_names[&site.key].clone()
        };
        if let Some(name) = &site.binding {
            sink.append_generated(&format!("local {name} = {value_expr}\n"));
        }
    }

    /// モジュール `key` の本体を、連結チャンク先頭で結果変数へ代入される関数として
    /// 定義する（defs へ発行）。定義位置では利用者ローカルが一切スコープに無いため、
    /// 本体の自由名は本来の意味論どおりグローバル環境へ解決される（指摘7）。
    /// 本体末尾のラベルは関数本体の最終文のままになるため、終端ラベルへの goto も
    /// 壊れない（指摘14）。
    fn define_module(&mut self, key: &str) {
        self.defined.insert(key.to_string());
        let slot = self.registry_slot(key);
        let source = self.source_of(key).to_string();
        let mut sink = Sink::default();
        // チャンクレベル `...` を使うモジュールはラッパ自体を可変長にし、glue が
        // モジュールキーを渡すことで require がチャンクへ渡す引数を再現する（候補L1）。
        // ローダはレジストリスロットの [1] に置き、結果の [2] と分離する
        // （結果でローダを上書きしないことが Q07 の再読込可能性の前提）。
        let params = if self.module_vararg(key) { "..." } else { "" };
        // R07 の分離が有効な場合、定義は自前の _ENV セルで包む:
        // ラッパ関数は定義時点（チャンク先頭）の _ENV を自身のセル
        // として捕捉するため、本体の自由名は実大域環境へ束縛され、
        // entry が実行時に _ENV を差し替えてもモジュールへは漏れない
        // （標準 require のモジュール環境と同じ）。モジュール自身に
        // よる _ENV の差し替えは、そのモジュールのセルにだけ効く。
        let (prefix, closing) = if self.env_isolation {
            (
                format!("do local _ENV = _ENV\n{slot} = {{ function({params})\n"),
                "end}; end",
            )
        } else {
            (format!("{slot} = {{ function({params})\n"), "end};")
        };
        sink.append_generated(&prefix);
        self.append_transformed_region(&mut sink, key, source.len());
        // 閉じの直後の `;` は空文であり、パーサの先読みトークンを閉じ行に
        // 留めるためのもの: 本体終端で判明するロードエラーは先読みトークンの
        // 行で報告されるため、これが無いと次の生成文（呼出し glue）の行が
        // 指され、どの区間にも属さず逆引きが NONE になる。閉じ行は
        // close_wrapper がモジュールの EOF 行へ対応づける。分離時の
        // 末尾の `end` は _ENV ラップの do を閉じる。
        sink.close_wrapper(closing);
        self.defs.push((sink.output, sink.ranges));
    }

    /// ambient メンバー `key` の本体を現在位置（チャンク先頭の注入位置）へ
    /// inline 展開する。プロジェクトモジュールは全てラッパ方式で展開されるため、
    /// ここへ来るのは ambient 合成キーだけである。
    /// 形は常時 IIFE とする。do ブロック形は本体のラベル／goto 名前空間が注入先
    /// チャンクと共有され（候補L2）、チャンクレベル `...` が注入先チャンクの
    /// 可変引数を観測してしまう（候補L1 形3）。関数スコープで隔離し、可変長
    /// メンバーには合成キーを引数として渡してモジュールチャンクの引数を再現する。
    fn expand_inline(&mut self, sink: &mut Sink, key: &str) {
        if self.expanded.contains(key) {
            return;
        }
        self.expanded.insert(key.to_string());

        let var = self.var_names[key].clone();
        let source = self.source_of(key).to_string();
        let vararg = uses_chunk_vararg(&self.modules[key].ast, self.modules[key].root);
        let (params, args) = if vararg {
            ("...", format!("{}, nil", quote_lua(key.as_bytes())))
        } else {
            ("", String::new())
        };
        // R07 の分離が有効な場合は IIFE も自前の _ENV セルで包む
        // （define_module と同じ条件・同じ理由）。ただしセルは
        // define_module と同様、本体関数の外（do ブロック側）に置く。
        // 本体フレームの内側に置くと本体が宣言するローカル（上限
        // 200）とレジスタ（上限 255）の予算を +1 圧迫し、上限
        // ちょうどの ambient 本体が、元はロード可能でも生成物では
        // ロード不可になる（S02）。外に置けば本体フレームの予算は
        // 元のチャンクと同一になり、セルは upvalue として捕捉される。
        if self.env_isolation {
            sink.append_generated(&format!(
                "do local _ENV = _ENV\n{var} = (function({params})\n"
            ));
        } else {
            sink.append_generated(&format!("{var} = (function({params})\n"));
        }
        self.append_transformed_region(sink, key, source.len());
        if self.env_isolation {
            sink.close_wrapper(&format!("end)({args}) end"));
        } else {
            sink.close_wrapper(&format!("end)({args})"));
        }
        sink.append_generated(&format!("if {var} == nil then {var} = true end\n"));
    }
}

/// 区間を、前に連結されるテキストの行数・バイト数だけずらす。
fn shift_range(range: &LinkedRange, line_shift: u32, byte_shift: usize) -> LinkedRange {
    LinkedRange {
        output_start_line: range.output_start_line + line_shift,
        output_end_line: range.output_end_line + line_shift,
        module: range.module.clone(),
        source_start_line: range.source_start_line,
        output_start_byte: range.output_start_byte + byte_shift,
        output_end_byte: range.output_end_byte + byte_shift,
        source_start_byte: range.source_start_byte,
    }
}

/// 改行の数を数える。lexer と同じ意味論（`newline_len`）で、
/// `\n`、`\r`、`\r\n`、`\n\r` はいずれも1つの改行として数える。
fn count_newlines(s: &str) -> u32 {
    let bytes = s.as_bytes();
    let mut count = 0u32;
    let mut i = 0usize;
    while i < bytes.len() {
        let len = newline_len(bytes, i);
        if len > 0 {
            count += 1;
            i += len;
        } else {
            i += 1;
        }
    }
    count
}

/// require 文（`local NAME = require("KEY")` または `require("KEY")`。括弧省略形も可）の
/// ソーステキスト上のバイト区間 `[start, end)` を求める。`start` は文の先頭トークンの位置
/// （`RequireSite::stmt_range`）から、`end` は require 呼び出しの引数までを実際に再字句解析して
/// 厳密に確定する（P1a の scan_requires が既に検証済みの2形式のみが対象であることが前提）。
#[expect(
    clippy::expect_used,
    reason = "The source was fully parsed and this static require site was validated before token boundaries are recovered"
)]
fn require_stmt_byte_span(source: &str, site: &RequireSite) -> (usize, usize) {
    let start = line_col_to_byte(source, site.stmt_range.line, site.stmt_range.col);
    let mut lex = Lexer::new(&source[start..]);
    if site.binding.is_some() {
        lex.next().expect("require 文は 'local' で始まる(束縛あり)");
        lex.next().expect("束縛変数名");
        lex.next().expect("'='");
    }
    lex.next().expect("'require'");
    let next = lex.next().expect("require の引数トークン");
    if next.v == "(" {
        lex.next().expect("文字列リテラル引数");
        lex.next().expect("')'");
    }
    (start, start + lex.pos())
}

/// `validate_project`（P1a）+ 依存 DFS（`module-not-found` / `require-cycle`）の結果。
/// `link_project`（静的展開）と `analyze`（リント）が共有する「パリティ規則」の実体。
/// **両者はここを土台にするため、error 診断集合は常に一致する。**
/// `LuaProject` をリンクする（設計 §3.2 / §10 実装ノート3）。
///
/// - entry から到達しないモジュールはリンク対象外（モジュールレベル DCE。診断なし）
/// - `validate_project` の診断（`entry-not-found` / `invalid-module-key` / `syntax-error` /
///   require 制限違反）に加え、依存 DFS で `module-not-found` / `require-cycle` を診断化する
/// - entry から到達するモジュールの Error（または module 無しのプロジェクト全体診断）が
///   1件でもあれば `linked_source` は `None`。到達不能モジュールの Error は診断に残すが
///   リンクは継続する（タスク2: §7-2。診断集合は `analyze` とのパリティを維持したまま、
///   `ok`/失敗判定だけを到達集合基準にする）。
pub fn link_project(project: &LuaProject) -> LinkResult {
    link_analyzed_project(project, analyze_structure(project))
}

// The public build driver also needs the parsed modules for its restriction
// checks. Consume that same analysis after those checks instead of parsing and
// resolving the whole project a second time. This is not a persistent cache.
pub(crate) fn link_analyzed_project(
    project: &LuaProject,
    structural: StructuralAnalysis,
) -> LinkResult {
    if structural.has_blocking_error(project) {
        return LinkResult {
            diagnostics: structural.diagnostics,
            linked_source: None,
            used_modules: Vec::new(),
            ranges: Vec::new(),
            injected_ambient: BTreeMap::new(),
        };
    }

    let StructuralAnalysis {
        diagnostics,
        mut modules,
        used_modules,
        reachable_modules: _,
        ambient_modules,
    } = structural;

    // ambient 使用の閉包解決（設計 §4.2）: entry から到達するモジュールが直接使った
    // ambient メンバーを起点に、ambient `source` 同士の相互依存を DFS で辿って必要な
    // メンバーをすべて集める。到達しないモジュールの使用は無視する（tree shaking）。
    // ここに現れる使用はすべて `scan_ambient_refs` が「正当」と判定済みのもの
    // （unknown-member/environment-only/root-escapes 等は既に structural error になっており、
    // その場合はこの行へ到達しない）ので、`ambient_modules` に必ず存在する前提でよい。
    let mut direct_ambient_usage: BTreeSet<AmbientModuleKey> = BTreeSet::new();
    for key in &used_modules {
        if let Some(analysis) = modules.get(key) {
            for usage in &analysis.ambient_usages {
                direct_ambient_usage.insert((usage.root.clone(), usage.member.clone()));
            }
        }
    }
    let ambient_order = ambient_closure_order(&direct_ambient_usage, &ambient_modules);

    // ambient source を `"root.member"` 合成キーで通常モジュール群にマージする
    // （§2.1 のルート名予約により実際のモジュールキーとは衝突しない）。
    // これにより `Expander::expand_dependency` を ambient にもそのまま再利用できる。
    for (key, analysis) in ambient_modules {
        modules.insert(format!("{}.{}", key.0, key.1), analysis);
    }

    // R07 の _ENV 分離は、実際にリンクされる集合（到達可能モジュールと
    // ambient 閉包）のどこかに _ENV セルの再束縛がある場合にのみ有効化
    // する（到達しないモジュールの再束縛は生成物に現れないため数えない）。
    let ambient_keys: Vec<String> = ambient_order
        .iter()
        .map(|(root, member)| format!("{root}.{member}"))
        .collect();
    let env_isolation = used_modules
        .iter()
        .map(String::as_str)
        .chain(ambient_keys.iter().map(String::as_str))
        .any(|key| {
            modules
                .get(key)
                .is_some_and(|analysis| ast_rebinds_env(&analysis.ast, analysis.root))
        });
    let mut expander = Expander::new(project, &modules, &used_modules, env_isolation);
    let mut sink = Sink::default();
    expander.inject_ambient(&mut sink, &ambient_order);
    expander.run(&mut sink);
    let (output, ranges) = expander.finish(sink);

    let mut injected_ambient: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (root, member) in &ambient_order {
        injected_ambient
            .entry(root.clone())
            .or_default()
            .push(member.clone());
    }

    LinkResult {
        diagnostics,
        linked_source: Some(output),
        used_modules,
        ranges,
        injected_ambient,
    }
}

/// 直接使用された ambient メンバー集合から、ambient `source` 同士の相互依存を辿って
/// 依存が先・依存元が後の順（postorder）に並べる（設計 §4.2 の閉包解決）。
/// 循環がある場合は `visited` ガードにより無限再帰にはならない
/// （2回目以降の到達は単純に無視する。require モジュールの「2回目以降の require は
/// 初回実行結果を再利用する」のと同じ考え方 — 注入は各メンバーにつき高々1回）。
fn ambient_closure_order(
    direct: &BTreeSet<AmbientModuleKey>,
    ambient_modules: &BTreeMap<AmbientModuleKey, ModuleAnalysis>,
) -> Vec<AmbientModuleKey> {
    let mut visited = HashSet::new();
    let mut order = Vec::new();
    for key in direct {
        visit_ambient_dependency(key, ambient_modules, &mut visited, &mut order);
    }
    order
}

fn visit_ambient_dependency(
    key: &AmbientModuleKey,
    ambient_modules: &BTreeMap<AmbientModuleKey, ModuleAnalysis>,
    visited: &mut HashSet<AmbientModuleKey>,
    order: &mut Vec<AmbientModuleKey>,
) {
    if visited.contains(key) {
        return;
    }
    visited.insert(key.clone());
    if let Some(analysis) = ambient_modules.get(key) {
        for usage in &analysis.ambient_usages {
            let dep = (usage.root.clone(), usage.member.clone());
            visit_ambient_dependency(&dep, ambient_modules, visited, order);
        }
    }
    order.push(key.clone());
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use storm_lua_analysis::diagnostic::codes;
    use storm_lua_analysis::project::AmbientNamespace;

    fn project(entry: &str, modules: &[(&str, &str)]) -> LuaProject {
        LuaProject {
            entry: entry.to_string(),
            modules: modules
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ambient: BTreeMap::new(),
        }
    }

    fn codes_of(diagnostics: &[Diagnostic]) -> Vec<&'static str> {
        diagnostics.iter().map(|d| d.code).collect()
    }

    #[test]
    fn links_simple_binding_module_with_no_return() {
        let p = project(
            "main",
            &[
                ("main", "local util = require(\"util\")\nutil.f()\n"),
                ("util", "function f() end\n"),
            ],
        );
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.expect("linked source");
        eprintln!("---\n{src}\n---");
        assert_eq!(
            result.used_modules,
            vec!["main".to_string(), "util".to_string()]
        );
        // 本体は先頭でレジストリのローダとして定義され、require 位置の
        // glue が初期化して結果を読む（Q06〜Q08）。
        assert!(src.contains("local __stormmin_link_modules = {}"));
        assert!(src.contains("__stormmin_link_modules[\"util\"] = { function()"));
        assert!(src.contains(
            "__stormmin_link_modules[\"util\"][2] = __stormmin_link_modules[\"util\"][1](\"util\", nil)"
        ));
        assert!(src.contains("= true"));
        assert!(src.contains("local util = __stormmin_link_modules[\"util\"][2]"));
        // レジストリ以外の生成名はローカル化されない。
        assert!(!src.contains("local __stormmin_link_util"));
    }

    #[test]
    fn register_estimate_counts_targets_retained_before_a_wide_key() {
        // T08: the second target's key is a 250-argument call. Its
        // evaluation runs with the first target's materialized base
        // and key still in registers, so the entry peak crosses the
        // wrap threshold and the registry must move out of the entry
        // frame; estimating each target independently stayed under
        // it and produced an unloadable chunk-local form.
        let wide = format!("type({})", "1,".repeat(249) + "1");
        let main = format!("require(\"m\");\n({{}})[{{}}],({{}})[{wide}]=1,1\nreturn 7");
        let p = project("main", &[("main", &main), ("m", "return 9")]);
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.expect("linked source");
        assert!(
            src.starts_with("do\nlocal __stormmin_link_modules = {}"),
            "{src}"
        );
        // The same statement with a single target fits the entry
        // frame (its key evaluation retains nothing before it), so
        // the registry stays a plain chunk local there.
        let main_single = format!("require(\"m\");\n({{}})[{wide}]=1\nreturn 7");
        let p = project("main", &[("main", &main_single), ("m", "return 9")]);
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.expect("linked source");
        assert!(
            src.starts_with("local __stormmin_link_modules = {}"),
            "{src}"
        );
    }

    #[test]
    fn vararg_module_uses_vararg_wrapper_and_receives_its_key() {
        // 候補L1: チャンクレベル `...` は require がモジュールチャンクへ渡す引数
        // （モジュールキー）を観測する。ラッパを可変長にし glue がキーを渡す
        // ことで、do ブロック inline 形（連結チャンクの引数を観測する意味変化）
        // と非可変長 IIFE 形（構文エラー）の両方を回避する。
        let p = project(
            "main",
            &[
                ("main", "local v = require(\"m\")\nOUT = v\n"),
                ("m", "OUT = select(\"#\", ...)\nreturn 5\n"),
            ],
        );
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.expect("linked source");
        assert!(
            src.contains("__stormmin_link_modules[\"m\"] = { function(...)"),
            "{src}"
        );
        assert!(
            src.contains(
                "__stormmin_link_modules[\"m\"][2] = __stormmin_link_modules[\"m\"][1](\"m\", nil)"
            ),
            "{src}"
        );
    }

    #[test]
    fn vararg_module_nested_in_wrapper_module_stays_loadable() {
        // 候補L1 形2: ラッパ方式モジュールから require される可変長モジュールも
        // 自前の可変長ラッパで定義され、外側の function() の内側へ do ブロックで
        // 落ちて構文エラーにならない。
        let p = project(
            "main",
            &[
                ("main", "local v = require(\"a\")\n"),
                ("a", "local b = require(\"bmod\")\nreturn b\n"),
                ("bmod", "OUT = select(\"#\", ...)\nreturn 9\n"),
            ],
        );
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.expect("linked source");
        assert!(
            src.contains("__stormmin_link_modules[\"bmod\"] = { function(...)"),
            "{src}"
        );
        assert!(
            src.contains("__stormmin_link_modules[\"bmod\"][2] = __stormmin_link_modules[\"bmod\"][1](\"bmod\", nil)"),
            "{src}"
        );
    }

    #[test]
    fn vararg_module_labels_are_scoped_by_the_wrapper_function() {
        // 候補L2: 自チャンク内に可視ラベルを持たない goto を含むモジュールは
        // ラッパ関数の内側で定義され、連結先チャンク（entry）の同名ラベルへ
        // 束縛されない（参照意味論ではモジュール側のロードエラーになる）。
        let p = project(
            "main",
            &[
                ("main", "require(\"m\")\n::L::\nOUT = 1\n"),
                ("m", "local a = ...\ngoto L\nOUT = 2\n"),
            ],
        );
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.expect("linked source");
        let def_pos = src
            .find("__stormmin_link_modules[\"m\"] = { function(...)")
            .expect("vararg wrapper def");
        let goto_pos = src.find("goto L").expect("module goto");
        let entry_pos = src.find("::L::").expect("entry label");
        assert!(def_pos < goto_pos && goto_pos < entry_pos, "{src}");
    }

    /// 1-based line number of the wrapper's closing end line for the
    /// first module definition in a linked source.
    fn wrapper_end_line(src: &str) -> u32 {
        let lines: Vec<&str> = src.lines().collect();
        let def = lines
            .iter()
            .position(|l| l.contains("= { function"))
            .expect("module definition line");
        lines[def + 1..]
            .iter()
            .position(|l| {
                l.trim() == "end"
                    || l.trim() == "end;"
                    || l.trim() == "end};"
                    || l.trim() == "end}; end"
            })
            .map(|off| (def + 2 + off) as u32)
            .expect("wrapper end line")
    }

    #[test]
    fn wrapper_end_line_maps_to_module_eof_line() {
        // Cycle 70 task 3: a load error reported past a wrapped
        // module's body (e.g. an unresolved goto, which the reference
        // VM reports at the line after the last body line) lands on
        // the wrapper's generated end line. That line maps back to
        // the module's end-of-source line instead of NONE.
        let p = project(
            "main",
            &[
                ("main", "require(\"m\")\n"),
                ("m", "local a = 1\ngoto done\n"),
            ],
        );
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.expect("linked source");
        let line = wrapper_end_line(&src);
        assert_eq!(lookup_source_line(&result.ranges, line), Some(("m", 3)));
        // The byte-level contract is unchanged: glue bytes have no
        // source byte.
        let byte = src
            .lines()
            .take(line as usize - 1)
            .map(|l| l.len() + 1)
            .sum::<usize>();
        assert_eq!(lookup_source_byte(&result.ranges, byte + 1), None);
    }

    #[test]
    fn wrapper_end_shares_final_line_without_trailing_newline() {
        // Without a trailing newline the closing token shares the
        // body's final line, so an end-of-chunk error on that line
        // maps to the module's last line.
        let p = project(
            "main",
            &[
                ("main", "require(\"m\")\n"),
                ("m", "local a = 1\ngoto done"),
            ],
        );
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.expect("linked source");
        assert!(src.contains("goto done end"), "{src}");
        let line = src
            .lines()
            .position(|l| l.contains("goto done end"))
            .expect("shared line") as u32
            + 1;
        assert_eq!(lookup_source_line(&result.ranges, line), Some(("m", 2)));
    }

    #[test]
    fn wrapper_end_line_stays_unmapped_after_final_comment_line() {
        // A closing token glued after a final comment line would be
        // commented out, so the wrapper keeps its own line and the
        // conservative no-mapping behaviour.
        let p = project(
            "main",
            &[("main", "require(\"m\")\n"), ("m", "goto done\n-- tail")],
        );
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.expect("linked source");
        let line = wrapper_end_line(&src);
        assert_eq!(lookup_source_line(&result.ranges, line), None);
    }

    #[test]
    fn entry_eof_line_maps_to_entry_source() {
        // The entry chunk's end-of-source line (where the reference
        // VM reports an unresolved goto when the source ends with a
        // newline) maps back to the entry module.
        let p = project("main", &[("main", "local a = 1\ngoto done\n")]);
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        assert_eq!(lookup_source_line(&result.ranges, 3), Some(("main", 3)));
    }

    #[test]
    fn wrapper_glue_is_guarded_against_top_level_goto_reexecution() {
        // 候補L3: トップレベル goto ループで同じ require 文へ再到達しうる。
        // 結果スロットの truthiness ガードにより、2周目に結果値（数値）を
        // 呼び直したり本体を再実行したりしない（Q06〜Q08 の再設計後も
        // この性質を保つ）。
        let p = project(
            "main",
            &[
                (
                    "main",
                    "local i = 0\n::top::\ni = i + 1\nlocal v = require(\"m\")\nif i < 2 then goto top end\n",
                ),
                ("m", "return 42\n"),
            ],
        );
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.expect("linked source");
        assert!(
            src.contains("if not __stormmin_link_modules[\"m\"][2] then"),
            "{src}"
        );
        assert_eq!(
            src.matches(
                "__stormmin_link_modules[\"m\"][2] = __stormmin_link_modules[\"m\"][1](\"m\", nil)"
            )
            .count(),
            1,
            "{src}"
        );
    }

    #[test]
    fn ambient_vararg_member_is_wrapped_in_vararg_iife_with_its_key() {
        // 候補L1 形1（ambient 経由）: メンバー源のチャンクレベル `...` は
        // 非可変長 IIFE では構文エラーになる。可変長 IIFE とし、合成キーを
        // 引数として渡してモジュールチャンクの引数を再現する。
        let p = ambient_project(
            "main",
            &[("main", "OUT = sim.probe\n")],
            "sim",
            &[(
                "probe",
                module_member(
                    "local n = select(\"#\", ...)\nif n > 99 then\n return -1\nend\nreturn 2\n",
                ),
            )],
        );
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.expect("linked source");
        assert!(src.contains("sim[\"probe\"] = (function(...)"), "{src}");
        assert!(src.contains("end)(\"sim.probe\", nil)"), "{src}");
    }

    #[test]
    fn ambient_member_labels_are_iife_scoped() {
        // 候補L2（ambient 側）: メンバー本体は常時 IIFE で包まれるため、その
        // ラベル／goto は注入先チャンクの同名ラベルと名前空間を共有しない。
        let p = ambient_project(
            "main",
            &[("main", "::L::\nOUT = sim.probe\n")],
            "sim",
            &[("probe", module_member("goto L\nOUT = 1\n::L::\nreturn 2\n"))],
        );
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.expect("linked source");
        assert!(src.contains("sim[\"probe\"] = (function()"), "{src}");
    }

    #[test]
    fn links_tail_return_module() {
        let p = project(
            "main",
            &[
                (
                    "main",
                    "local util = require(\"util\")\nlocal x = util.add(1, 2)\n",
                ),
                (
                    "util",
                    "local M = {}\nfunction M.add(a, b) return a + b end\nreturn M\n",
                ),
            ],
        );
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.expect("linked source");
        eprintln!("---\n{src}\n---");
        assert!(!src.contains("require"));
    }

    #[test]
    fn line_col_to_byte_counts_cr_and_pairs_like_the_lexer() {
        // パーサ（lexer）は \n、\r、\r\n、\n\r のいずれも1つの改行として
        // 数える。バイト変換が \n だけを数えると CR 行末でずれる。
        assert_eq!(line_col_to_byte("--x\u{1F600}\rreturn 3", 2, 1), 8);
        assert_eq!(line_col_to_byte("--x\u{1F600}\r\nreturn 3", 2, 1), 9);
        assert_eq!(line_col_to_byte("a\n\rb", 2, 1), 3);
        assert_eq!(line_col_to_byte("a\rb\rc", 3, 1), 4);
        assert_eq!(line_col_to_byte("abc", 1, 2), 1);
        assert_eq!(count_newlines("a\rb\r\nc\n\rd"), 3);
        assert_eq!(count_newlines("a\nb\nc"), 2);
    }

    #[test]
    fn links_module_with_lone_cr_and_unicode_comment() {
        // Codex 第2回指摘8: CR の直後の return 位置を \n 基準で探すと
        // 絵文字の UTF-8 の途中を指して panic していた。
        let p = project(
            "main",
            &[
                ("main", "local m = require(\"m\")\nreturn m\n"),
                ("m", "--x\u{1F600}\rreturn 3"),
            ],
        );
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.expect("linked source");
        // ラッパ方式では return は本体内のまま保持される。
        assert!(src.contains("return 3"), "{src}");
    }

    #[test]
    fn links_module_with_lone_cr_statements() {
        // CR 行末の文が壊れた形で連結され、無効な Lua になっていた。
        let p = project(
            "main",
            &[
                ("main", "local m = require(\"m\")\nreturn m\n"),
                ("m", "local x=1\rreturn x"),
            ],
        );
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.expect("linked source");
        assert!(src.contains("local x=1"), "{src}");
        assert!(src.contains("return x"), "{src}");
    }

    #[test]
    fn links_entry_with_require_after_cr_lines() {
        // require 文の開始位置も CR 行末で正しく求まること。
        let p = project(
            "main",
            &[
                ("main", "local a=1\rlocal m=require(\"m\")\rreturn m + a"),
                ("m", "return 41"),
            ],
        );
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.expect("linked source");
        assert!(!src.contains("require"), "{src}");
        assert!(src.contains("return 41"), "{src}");
    }

    #[test]
    fn links_early_return_module_via_iife() {
        let p = project(
            "main",
            &[
                ("main", "local m = require(\"m\")\n"),
                ("m", "if true then\n  return 1\nend\nreturn 2\n"),
            ],
        );
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.expect("linked source");
        eprintln!("---\n{src}\n---");
        // 早期 return はラッパ関数内の return としてそのまま保持される。
        assert!(src.contains("__stormmin_link_modules[\"m\"] = { function()"));
    }

    #[test]
    fn reuses_module_result_on_second_require() {
        let p = project(
            "main",
            &[
                (
                    "main",
                    "local a = require(\"shared\")\nlocal b = require(\"shared\")\n",
                ),
                ("shared", "return {}\n"),
            ],
        );
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.expect("linked source");
        eprintln!("---\n{src}\n---");
        // 定義は1回だけ。初期化 glue は全 require 位置へ発行されるが
        // （Q06）、結果スロットのガードにより本体の実行は1回だけで、
        // 2回目の require は結果の読み出しになる。
        assert_eq!(
            src.matches("__stormmin_link_modules[\"shared\"] = { function()")
                .count(),
            1
        );
        assert_eq!(
            src.matches(
                "__stormmin_link_modules[\"shared\"][2] = __stormmin_link_modules[\"shared\"][1](\"shared\", nil)"
            )
            .count(),
            2
        );
        assert!(src.contains("local b = __stormmin_link_modules[\"shared\"][2]"));
    }

    #[test]
    fn module_body_does_not_capture_importer_locals() {
        // Codex 第3回指摘7: モジュールの `x=2` はグローバルへの書込みであり、
        // import 側の `local x` を書き換えてはならない。構造的には、本体の定義が
        // import 側ローカルの宣言より前に来ることで捕捉を回避する。
        let p = project(
            "main",
            &[
                ("main", "local x=1\nrequire(\"m\")\nreturn x"),
                ("m", "x=2"),
            ],
        );
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.expect("linked source");
        let def_pos = src
            .find("__stormmin_link_modules[\"m\"] = { function()")
            .expect("module defined as wrapper");
        let local_pos = src.find("local x=1").expect("entry local present");
        assert!(def_pos < local_pos, "{src}");
        // 本体は関数の中にそのまま入る（改行で終わらない本体では閉じ
        // トークンが最終行を共有する。課題3の帰属改善）。
        assert!(src.contains("function()\nx=2 end};"), "{src}");
    }

    #[test]
    fn module_with_terminal_label_keeps_label_last_in_body() {
        // Codex 第3回指摘14: 本体末尾のラベルの後に glue が付くと、そのラベルへの
        // goto が「ローカルのスコープ内への跳躍」になり素の Lua 5.3 でロード不可に
        // なる。ラッパ方式ではラベルが関数本体の最終文のままになる。
        let p = project(
            "main",
            &[
                ("main", "require(\"m\")\nreturn 7"),
                ("m", "goto L\nlocal x\n::L::"),
            ],
        );
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.expect("linked source");
        // 閉じトークンがラベルの行を共有してもラベルはブロックの最終文の
        // ままであり、文が後に続くわけではない（raw Lua 5.3 と方言 VM の
        // 双方でロード可能であることを w9 cycle 70 で実測）。スコープ内
        // 跳躍になるのはラベルの後に文が続く場合だけである。
        assert!(src.contains("::L:: end"), "{src}");
    }

    #[test]
    fn linker_adds_no_locals_to_the_chunk() {
        // Codex 第3回指摘15: entry が 200 ローカルを使い切っている場合、
        // リンカがローカルを追加すると連結結果がロード不可になる。
        // ラップ形式ではレジストリは do ブロックのローカルとなり、
        // entry 本体はラッパ関数の中に置かれるため、entry フレームの
        // 予算は一切消費せず、字句的可視性も保たれる（R06: 旧来の
        // グローバル宣言フォールバックは `local _ENV` 遮蔽下で参照
        // できず廃止した）。
        let locals: Vec<String> = (0..200).map(|i| format!("p{i}")).collect();
        let main = format!("local {};require(\"m\");return 7", locals.join(","));
        let p = project("main", &[("main", main.as_str()), ("m", "G=1")]);
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.expect("linked source");
        assert!(
            src.starts_with("do\nlocal __stormmin_link_modules = {}\n"),
            "{src}"
        );
        assert!(src.contains("return (function(...)\n"), "{src}");
        assert!(src.trim_end().ends_with("end"), "{src}");
        // グローバル宣言フォールバックは存在しない。
        assert!(!src.contains("\n__stormmin_link_modules = {}"), "{src}");
    }

    #[test]
    fn wrapped_entry_eof_line_maps_to_entry_source() {
        // ラップ形式でも、entry 本体終端で判明するロードエラー
        // （未解決 goto）の先読み行（ラッパの閉じ行）は entry 自身の
        // EOF 行へ対応づく。
        let locals: Vec<String> = (0..200).map(|i| format!("p{i}")).collect();
        let main = format!("local {}\nrequire(\"m\")\ngoto done\n", locals.join(","));
        let p = project("main", &[("main", main.as_str()), ("m", "return 1\n")]);
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.expect("linked source");
        let close_line = src
            .lines()
            .position(|l| l.contains("end)(...)"))
            .expect("wrapper close line") as u32
            + 1;
        // entry は3行（local 行・require 行・goto 行）＋ EOF 行 = 4行目。
        assert_eq!(
            lookup_source_line(&result.ranges, close_line),
            Some(("main", 4))
        );
    }

    #[test]
    fn linker_uses_local_registry_below_the_local_limit() {
        // 上限まで1つ余裕があればレジストリはローカルで宣言される。
        let locals: Vec<String> = (0..199).map(|i| format!("p{i}")).collect();
        let main = format!("local {};require(\"m\");return 7", locals.join(","));
        let p = project("main", &[("main", main.as_str()), ("m", "G=1")]);
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.expect("linked source");
        assert!(src.contains("local __stormmin_link_modules = {}"), "{src}");
    }

    #[test]
    fn bare_require_without_binding_is_dropped_on_reuse() {
        let p = project(
            "main",
            &[
                ("main", "require(\"side\")\nrequire(\"side\")\n"),
                ("side", "sim = sim\n"),
            ],
        );
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.expect("linked source");
        eprintln!("---\n{src}\n---");
    }

    #[test]
    fn unreachable_module_syntax_error_is_reported_but_does_not_block_linking() {
        let p = project("main", &[("main", "return 1\n"), ("dead", "local a = (\n")]);
        let result = link_project(&p);
        // dead は構文エラーだが entry から到達しない。validate_project は全モジュールを
        // 検証するため syntax-error は診断として残る（analyze とのパリティ仕様どおり）が、
        // 到達不能モジュールの Error は ok/失敗判定に影響しない（タスク2: §7-2）ので
        // リンクは成功する。
        assert!(result
            .diagnostics
            .iter()
            .any(|d| d.code == codes::SYNTAX_ERROR && d.module.as_deref() == Some("dead")));
        assert!(result.linked_source.is_some());
    }

    #[test]
    fn module_not_found_is_reported() {
        let p = project("main", &[("main", "local x = require(\"missing\")\n")]);
        let result = link_project(&p);
        assert_eq!(codes_of(&result.diagnostics), vec![codes::MODULE_NOT_FOUND]);
        assert!(result.linked_source.is_none());
    }

    #[test]
    fn require_cycle_is_reported_including_self_require() {
        let p = project(
            "main",
            &[
                ("main", "local a = require(\"a\")\n"),
                ("a", "local b = require(\"b\")\nreturn 1\n"),
                ("b", "local a2 = require(\"a\")\nreturn 2\n"),
            ],
        );
        let result = link_project(&p);
        assert_eq!(codes_of(&result.diagnostics), vec![codes::REQUIRE_CYCLE]);
        assert!(result.linked_source.is_none());

        let p2 = project(
            "main",
            &[
                ("main", "local x = require(\"main2\")\n"),
                ("main2", "local y = require(\"main2\")\nreturn 1\n"),
            ],
        );
        let result2 = link_project(&p2);
        assert_eq!(codes_of(&result2.diagnostics), vec![codes::REQUIRE_CYCLE]);
    }

    #[test]
    fn linked_range_round_trips_output_lines_to_source_lines() {
        let p = project(
            "main",
            &[
                ("main", "local util = require(\"util\")\nlocal y = util.x\n"),
                ("util", "local z = 1\nreturn z\n"),
            ],
        );
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.unwrap();
        eprintln!("---\n{src}\n---");
        for (i, line) in src.lines().enumerate() {
            let line_no = (i + 1) as u32;
            if let Some((module, source_line)) = lookup_source_line(&result.ranges, line_no) {
                eprintln!("out L{line_no} <- {module}:{source_line}  |  {line}");
            }
        }
        // "local y = util.x" は main の2行目。
        let (m, l) = lookup_source_line(&result.ranges, src.lines().count() as u32)
            .expect("last line should be mapped");
        assert_eq!(m, "main");
        assert_eq!(l, 2);
    }

    // --- P3: ambient 注入・tree shaking・閉包解決（設計 §4.2） ---

    fn ambient_project(
        entry: &str,
        modules: &[(&str, &str)],
        root: &str,
        members: &[(&str, AmbientMember)],
    ) -> LuaProject {
        let mut p = project(entry, modules);
        let namespace = AmbientNamespace {
            members: members
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        };
        p.ambient.insert(root.to_string(), namespace);
        p
    }

    fn module_member(source: &str) -> AmbientMember {
        AmbientMember::Module {
            source: source.to_string(),
        }
    }

    #[test]
    fn used_ambient_member_is_injected_and_unused_sibling_is_not() {
        let p = ambient_project(
            "main",
            &[("main", "local clamp = sim.clamp\nreturn clamp(1, 0, 1)\n")],
            "sim",
            &[
                (
                    "clamp",
                    module_member("return function(x, lo, hi) return x end"),
                ),
                ("unused", module_member("return 1")),
            ],
        );
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        assert_eq!(
            result.injected_ambient.get("sim").map(Vec::as_slice),
            Some(["clamp".to_string()].as_slice())
        );
        let src = result.linked_source.expect("linked source");
        eprintln!("---\n{src}\n---");
        assert!(src.contains("sim = {}"));
        assert!(src.contains("sim[\"clamp\"]"));
        assert!(!src.contains("sim[\"unused\"]"));
    }

    #[test]
    fn no_ambient_usage_means_nothing_is_injected() {
        let p = ambient_project(
            "main",
            &[("main", "return 1\n")],
            "sim",
            &[("clamp", module_member("return function(x) return x end"))],
        );
        let result = link_project(&p);
        assert!(result.diagnostics.is_empty());
        assert!(result.injected_ambient.is_empty());
        let src = result.linked_source.expect("linked source");
        assert!(!src.contains("sim"));
    }

    #[test]
    fn ambient_env_isolation_keeps_full_local_budget_body_loadable() {
        // S02: ambient 本体がローカル上限（200）を使い切っている場合、
        // R07 の _ENV 隔離セルが本体フレームの内側にあると生成物が
        // ロード不可になる。セルは define_module と同じく本体関数の
        // 外（do ブロック側）に置かれ、本体フレームの予算は元の
        // チャンクと同一でなければならない。
        let locals: Vec<String> = (0..200).map(|i| format!("v{i}")).collect();
        let member = format!("local {};return 7", locals.join(","));
        let p = ambient_project(
            "main",
            &[("main", "_ENV=_ENV\nreturn lib.m\n")],
            "lib",
            &[("m", module_member(&member))],
        );
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.expect("linked source");
        assert!(
            src.contains("do local _ENV = _ENV\nlib[\"m\"] = (function()"),
            "{src}"
        );
        assert!(
            storm_lua_vm::load_legality::check_load_legality(src.as_bytes()).is_ok(),
            "{src}"
        );
    }

    #[test]
    fn ambient_env_isolation_keeps_full_register_budget_body_loadable() {
        // S02（レジスタ形）: 本体が戻り値 254 個でレジスタ上限
        // 近傍を使う場合も、隔離セルが本体フレームのレジスタを
        // 消費してはならない。ロード判定フォールバックはレジスタ
        // 上限を再現しないため、ここでは生成形（セルが IIFE の外）
        // を固定し、実ロードは検証ドライバ側で確認する。
        let values: Vec<String> = (0..254).map(|i| i.to_string()).collect();
        let member = format!("return {}", values.join(","));
        let p = ambient_project(
            "main",
            &[("main", "_ENV=_ENV\nreturn lib.m\n")],
            "lib",
            &[("m", module_member(&member))],
        );
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let src = result.linked_source.expect("linked source");
        assert!(
            src.contains("do local _ENV = _ENV\nlib[\"m\"] = (function()"),
            "{src}"
        );
        assert!(!src.contains("(function()\ndo local _ENV"), "{src}");
    }

    #[test]
    fn ambient_sibling_dependency_is_injected_via_closure() {
        // "math" だけを使うが、その source が "clamp" を参照するので両方注入される。
        let p = ambient_project(
            "main",
            &[("main", "local m = sim.math\nreturn m.clampToUnit(5)\n")],
            "sim",
            &[
                (
                    "clamp",
                    module_member("return function(x, lo, hi) return x end"),
                ),
                (
                    "math",
                    module_member(
                        "local M = {}\nfunction M.clampToUnit(x) return sim.clamp(x, 0, 1) end\nreturn M\n",
                    ),
                ),
            ],
        );
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        let mut injected = result
            .injected_ambient
            .get("sim")
            .cloned()
            .unwrap_or_default();
        injected.sort();
        assert_eq!(injected, vec!["clamp".to_string(), "math".to_string()]);
        let src = result.linked_source.expect("linked source");
        eprintln!("---\n{src}\n---");
        // 依存(clamp)が依存元(math)より先に注入されている(postorder)。
        let clamp_pos = src.find("sim[\"clamp\"]").expect("clamp injected");
        let math_pos = src.find("sim[\"math\"]").expect("math injected");
        assert!(clamp_pos < math_pos);
    }

    #[test]
    fn self_referencing_ambient_member_does_not_infinite_loop() {
        // 循環（a が a 自身を参照）でも visited ガードにより無限再帰せず、注入は1回のみ。
        let p = ambient_project(
            "main",
            &[("main", "return sim.a\n")],
            "sim",
            &[("a", module_member("local x = sim.a\nreturn 1"))],
        );
        let result = link_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            codes_of(&result.diagnostics)
        );
        assert_eq!(
            result.injected_ambient.get("sim").map(Vec::as_slice),
            Some(["a".to_string()].as_slice())
        );
    }

    #[test]
    fn require_in_ambient_source_is_reported() {
        let p = ambient_project(
            "main",
            &[("main", "return sim.a\n")],
            "sim",
            &[("a", module_member("local x = require(\"nope\")\nreturn 1"))],
        );
        let result = link_project(&p);
        assert_eq!(
            codes_of(&result.diagnostics),
            vec![codes::REQUIRE_IN_AMBIENT]
        );
        assert!(result.linked_source.is_none());
    }

    #[test]
    fn ambient_violation_blocks_linking() {
        let p = ambient_project(
            "main",
            &[("main", "local s = sim\nreturn 1\n")],
            "sim",
            &[("a", module_member("return 1"))],
        );
        let result = link_project(&p);
        assert_eq!(
            codes_of(&result.diagnostics),
            vec![codes::AMBIENT_ROOT_ESCAPES]
        );
        assert!(result.linked_source.is_none());
    }

    #[test]
    fn module_key_colliding_with_ambient_root_is_reported() {
        let p = ambient_project(
            "main",
            &[("main", "return 1\n"), ("sim.helper", "return 1\n")],
            "sim",
            &[("a", module_member("return 1"))],
        );
        let result = link_project(&p);
        assert!(result
            .diagnostics
            .iter()
            .any(|d| d.code == codes::INVALID_MODULE_KEY
                && d.module.as_deref() == Some("sim.helper")));
    }
}
