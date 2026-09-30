//! 依存 DFS + 静的展開（リンク）+ 連結オフセット区間表（設計 §3.2 / §10 実装ノート3）。
//!
//! `link_project` は `validate_project`（P1a）の結果を土台に:
//! - entry から到達するモジュールを DFS で辿り、`module-not-found` / `require-cycle` を診断化する
//!   （entry から到達しないモジュールはモジュールレベル DCE 対象。診断は出さない）
//! - エラー診断が一つもなければ、ソーステキストレベルで1本の Lua ソースへ静的展開する。
//!   各モジュール本体はそれが最初に require された位置に、do ブロック + 巻き上げ束縛
//!   （末尾 return 以外の早期 return を持つモジュールは IIFE）として挿入する。
//!   2回目以降の require は巻き上げ変数への参照に置換する（標準 Lua の require 意味論と等価）。
//! - 出力の任意行から由来モジュール・元行へ逆引きできる連結オフセット区間表を返す
//!   （Source Map（P4）はこの区間表を正本にして別途生成する。ここでは作らない）。
//!
//! 原文のコピー区間はUTF-8バイト範囲で保持する。同一行にある複数の展開境界も
//! 正確に区別できる。行フィールドとlookup_source_lineは粗い行対応の投影であり、
//! 詳細なmap・診断にはlookup_source_byteを使用する。合成部分に元位置を付けない。

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
pub use storm_lua_analysis::structure::{analyze_structure, StructuralAnalysis};

use storm_lua_analysis::diagnostic::Diagnostic;
use storm_lua_analysis::project::{AmbientMember, AmbientModuleKey, LuaProject, ModuleAnalysis};
use storm_lua_analysis::require_scan::RequireSite;
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::lexer::Lexer;
use storm_lua_syntax::numeric::quote_lua;

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

/// モジュール本体の展開形（設計 §10 実装ノート3）。
enum Shape {
    /// return が無い（トップレベルに Return が一つも無い）。require 結果は `true`。
    NoReturn,
    /// トップレベルブロックの最後の文がただ一つの Return であり、かつそれ以外に
    /// トップレベルで到達する Return が無い（ネストした if/while/for の中の早期 return を含まない）。
    /// do ブロックで安全に展開できる。
    TailReturn(NodeId),
    /// 上記以外（早期 return あり）。`return` が「モジュールの初期化処理からの脱出」として
    /// 正しく機能するよう IIFE で包む。
    Iife,
}

/// トップレベルブロック直下（ネストした if/while/for/do の中を含み、
/// ネストした関数本体は含まない = 別スコープなので辿らない）に現れる Return をすべて集める。
fn collect_top_level_returns(ast: &Ast, block_id: NodeId, out: &mut Vec<NodeId>) {
    let Node::Block(stmts) = ast.node(block_id) else {
        unreachable!("collect_top_level_returns は Block ノードにのみ呼ばれる");
    };
    let stmts = stmts.clone();
    for stmt_id in stmts {
        match ast.node(stmt_id) {
            Node::Return(_) => out.push(stmt_id),
            Node::Do(b) => collect_top_level_returns(ast, *b, out),
            Node::While(_, b) => collect_top_level_returns(ast, *b, out),
            Node::Repeat(b, _) => collect_top_level_returns(ast, *b, out),
            Node::If(arms, eb) => {
                let arms = arms.clone();
                let eb = *eb;
                for arm in &arms {
                    collect_top_level_returns(ast, arm.body, out);
                }
                if let Some(eb) = eb {
                    collect_top_level_returns(ast, eb, out);
                }
            }
            Node::Fornum(_, _, _, _, body) => collect_top_level_returns(ast, *body, out),
            Node::Forin(_, _, body) => collect_top_level_returns(ast, *body, out),
            // Funcstat / Localfunc / それ以外の文は新しい関数スコープを開始するか、
            // Return を含み得ないため辿らない。
            _ => {}
        }
    }
}

fn classify_shape(ast: &Ast, root: NodeId) -> Shape {
    let Node::Block(stmts) = ast.node(root) else {
        unreachable!("classify_shape はモジュールのルート Block にのみ呼ばれる");
    };
    let mut returns = Vec::new();
    collect_top_level_returns(ast, root, &mut returns);
    if returns.is_empty() {
        return Shape::NoReturn;
    }
    let last_is_tail_return = stmts
        .last()
        .map(|&id| matches!(ast.node(id), Node::Return(_)))
        .unwrap_or(false);
    if last_is_tail_return && returns.len() == 1 {
        Shape::TailReturn(returns[0])
    } else {
        Shape::Iife
    }
}

/// (1-based line, 1-based col) を対応するソースのバイトオフセットへ変換する。
/// col はバイト単位（lexer の列カウントと同じ意味論。§ P0 の位置サイドテーブル前提）。
fn line_col_to_byte(source: &str, line: u32, col: u32) -> usize {
    let mut byte = 0usize;
    let mut cur_line = 1u32;
    if line > 1 {
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                cur_line += 1;
                if cur_line == line {
                    byte = i + 1;
                    break;
                }
            }
        }
    }
    byte + (col as usize - 1)
}

fn line_of(s: &str) -> u32 {
    1 + s.bytes().filter(|&b| b == b'\n').count() as u32
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

fn collect_reserved_names(modules: &BTreeMap<String, ModuleAnalysis>) -> HashSet<String> {
    let mut set = HashSet::new();
    for analysis in modules.values() {
        for s in analysis.ast.strings.all_strings() {
            set.insert(s);
        }
    }
    set
}

struct Expander<'a> {
    project: &'a LuaProject,
    modules: &'a BTreeMap<String, ModuleAnalysis>,
    var_names: HashMap<String, String>,
    expanded: HashSet<String>,
    output: String,
    ranges: Vec<LinkedRange>,
    /// `"root.member"` 合成キー → ambient `source` テキスト。`project.modules` には
    /// ambient のソースは含まれないため、`expand_dependency` が参照できるよう別に持つ。
    ambient_sources: HashMap<String, &'a str>,
}

impl<'a> Expander<'a> {
    fn new(
        project: &'a LuaProject,
        modules: &'a BTreeMap<String, ModuleAnalysis>,
        order: &[String],
    ) -> Self {
        let mut reserved = collect_reserved_names(modules);
        let mut var_names = HashMap::new();
        let mut hoisted = Vec::new();
        // order[0] は entry。entry 自身は require されないので巻き上げ変数を持たない。
        for key in order.iter().skip(1) {
            let name = fresh_var_name(&mut reserved, key);
            var_names.insert(key.clone(), name.clone());
            hoisted.push(name);
        }
        let mut output = String::new();
        if !hoisted.is_empty() {
            output.push_str("local ");
            output.push_str(&hoisted.join(", "));
            output.push('\n');
        }
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
            output,
            ranges: Vec::new(),
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
    fn inject_ambient(&mut self, order: &[AmbientModuleKey]) {
        if order.is_empty() {
            return;
        }
        let mut roots_seen = HashSet::new();
        for (root, _) in order {
            if roots_seen.insert(root.clone()) {
                self.append_generated(&format!("{root} = {{}}\n"));
            }
        }
        for (root, member) in order {
            let synthetic_key = format!("{root}.{member}");
            let target = format!("{root}[{}]", quote_lua(member));
            self.var_names.insert(synthetic_key.clone(), target);
            self.expand_dependency(&synthetic_key);
        }
    }

    fn run(&mut self) {
        let entry = self.project.entry.clone();
        let end = self.project.modules[&entry].len();
        self.append_transformed_region(&entry, end);
    }

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
        let newline_count = text.matches('\n').count() as u32;
        let out_end = if text.ends_with('\n') {
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

    /// `key` の require 文を展開・置換しつつ `source[0..region_end)` を追記する。
    /// `region_end` はモジュール全文の長さ（NoReturn/Iife）か、末尾 return 文の開始位置（TailReturn）。
    fn append_transformed_region(&mut self, key: &str, region_end: usize) {
        let source = self.source_of(key).to_string();
        let sites: Vec<RequireSite> = self.modules[key].requires.clone();
        let mut cursor = 0usize;
        for site in &sites {
            let (stmt_start, stmt_end) = require_stmt_byte_span(&source, site);
            debug_assert!(
                stmt_end <= region_end,
                "require 文の位置がモジュール展開領域を超えている(内部不変条件違反)"
            );
            self.append_source_slice(key, &source, cursor, stmt_start);
            self.append_require_replacement(site);
            cursor = stmt_end;
        }
        self.append_source_slice(key, &source, cursor, region_end);
    }

    fn append_require_replacement(&mut self, site: &RequireSite) {
        if !self.expanded.contains(&site.key) {
            self.expand_dependency(&site.key);
        }
        if let Some(name) = &site.binding {
            let var = self.var_names[&site.key].clone();
            self.append_generated(&format!("local {name} = {var}\n"));
        }
    }

    /// モジュール `key` の本体を、それが最初に require された位置へ展開する
    /// （設計 §10 実装ノート3: do ブロック + 巻き上げ束縛、早期 return は IIFE）。
    fn expand_dependency(&mut self, key: &str) {
        if self.expanded.contains(key) {
            return;
        }
        self.expanded.insert(key.to_string());

        let var = self.var_names[key].clone();
        let source = self.source_of(key).to_string();
        let analysis = &self.modules[key];
        let shape = classify_shape(&analysis.ast, analysis.root);

        match shape {
            Shape::NoReturn => {
                self.append_generated("do\n");
                self.append_transformed_region(key, source.len());
                self.append_generated(&format!("\n{var} = true\nend\n"));
            }
            Shape::TailReturn(return_stmt_id) => {
                #[expect(
                    clippy::expect_used,
                    reason = "The validated parser position table belongs to this exact return statement"
                )]
                let (r_line, r_col) = self.modules[key]
                    .positions
                    .get(return_stmt_id)
                    .expect("return 文は parser が常に位置をマークする");
                let return_start = line_col_to_byte(&source, r_line, r_col);
                let Node::Return(exprs) = self.modules[key].ast.node(return_stmt_id) else {
                    unreachable!("TailReturn は Return ノードにのみ立つ");
                };
                let has_exprs = !exprs.is_empty();

                self.append_generated("do\n");
                self.append_transformed_region(key, return_start);
                if has_exprs {
                    self.append_generated(&format!("\n{var} ="));
                    // `return` キーワード直後(6バイト)から先頭部の残り(値・末尾コメント等)をそのまま流用。
                    // require の戻り値は最初の値のみが使われる標準 Lua 意味論と同じく、
                    // 単一 var への複数値代入は先頭値のみが束縛される(Lua の代入意味論)。
                    self.append_source_slice(
                        key,
                        &source,
                        return_start + "return".len(),
                        source.len(),
                    );
                    self.append_generated(&format!(
                        "\nif {var} == nil then {var} = true end\nend\n"
                    ));
                } else {
                    self.append_generated(&format!("\n{var} = true"));
                    self.append_source_slice(
                        key,
                        &source,
                        return_start + "return".len(),
                        source.len(),
                    );
                    self.append_generated("\nend\n");
                }
            }
            Shape::Iife => {
                self.append_generated(&format!("{var} = (function()\n"));
                self.append_transformed_region(key, source.len());
                self.append_generated(&format!(
                    "\nend)()\nif {var} == nil then {var} = true end\n"
                ));
            }
        }
    }
}

fn count_newlines(s: &str) -> u32 {
    s.bytes().filter(|&b| b == b'\n').count() as u32
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
    let structural = analyze_structure(project);

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

    let mut expander = Expander::new(project, &modules, &used_modules);
    expander.inject_ambient(&ambient_order);
    expander.run();

    let mut injected_ambient: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (root, member) in &ambient_order {
        injected_ambient
            .entry(root.clone())
            .or_default()
            .push(member.clone());
    }

    LinkResult {
        diagnostics,
        linked_source: Some(expander.output),
        used_modules,
        ranges: expander.ranges,
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
        assert!(src.contains("do\n"));
        assert!(src.contains("= true"));
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
        assert!(src.contains("(function()"));
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
        // 2回目の require は展開されず、変数参照のみになる。
        assert_eq!(src.matches("do\n").count(), 1);
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
