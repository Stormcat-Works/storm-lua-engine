//! Load-legality validation mirroring the project VM's load path, without
//! executing the source.
//!
//! The project VM loads a chunk as: dialect rewrite (see [`crate::dialect`],
//! notably the R210 goto-wrap for locals unreferenced after the target
//! label) followed by a parse-only verdict. This module reproduces that
//! verdict — rewrite first, then parse only, never executed — so the
//! compiler can judge load legality on the *source* program before any
//! pass removes or moves the constructs legality depends on
//! (candidates S10/S14).
//!
//! # Verdict backends (pluggable)
//!
//! The parse verdict after the rewrite is a replaceable component:
//!
//! - With the `backend-mlua` feature, the verdict is the vendored PUC
//!   Lua 5.3 parser itself ([`mlua::Chunk::into_function`]), exactly the
//!   parser the runtime loads with. This is the reference verdict; every
//!   native consumer that needs reference-exact legality enables the
//!   feature (runtime hosts, the conformance harness, release tooling).
//! - Without the feature, the verdict falls back to this project's own
//!   parser (`storm-lua-syntax`) plus a dedicated checker in this
//!   module that re-implements the PUC load rules the gate exists for:
//!   label resolution and duplicates, the goto-into-scope rule
//!   (including the block-final label exception), the 200
//!   active-locals cap, break outside loops, return-not-last, and
//!   `...` outside vararg functions. The fallback exists so
//!   compiler-only consumers (notably the wasm32-unknown-unknown
//!   compiler adapter) can build without the C Lua backend. Known
//!   residuals versus the reference verdict: the upvalue cap (255) and
//!   register limits for huge expressions are not reproduced (beyond
//!   the single-call argument cap, which is context-free and is
//!   reproduced), the
//!   implicit `self` parameter of methods can shift the locals count
//!   by one near the cap, the own parser's nesting-depth limit (128)
//!   can reject deeply nested sources the reference parser accepts at
//!   this gate (the full compile rejects them under both backends,
//!   since the compiler itself parses with the own parser), and
//!   non-UTF-8 sources are rejected (the own parser works on text).
//!   Treat the fallback as an S12-class
//!   approximation: execution results of accepted programs are
//!   unaffected, but the accept/reject boundary is not reference-exact
//!   in those corners.
//! - A host can also inject its own verdict with
//!   [`check_load_legality_with`], bypassing both built-in backends.
//!
//! A source the VM accepts (e.g. a goto over a local the R210 wrap
//! legalizes) passes here as well under the reference backend; only
//! sources the VM itself would reject at load are rejected. Syntax
//! errors are also rejected here, but callers in the compile path run
//! this check after their own parse, so syntax diagnostics keep their
//! existing shape.

use crate::dialect;

/// Parse-only verdict over an already-rewritten source: the vendored
/// PUC Lua 5.3 parser (reference backend).
#[cfg(feature = "backend-mlua")]
fn parse_verdict(rewritten: &[u8]) -> Result<(), String> {
    let lua = mlua::Lua::new();
    lua.load(rewritten)
        .set_name("load-legality")
        .set_mode(mlua::ChunkMode::Text)
        .into_function()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Parse-only verdict over an already-rewritten source: this project's
/// own parser plus the PUC load-rule checker in [`legality`] (fallback
/// backend; see the module documentation for the acceptance-set
/// differences from the reference backend).
#[cfg(not(feature = "backend-mlua"))]
fn parse_verdict(rewritten: &[u8]) -> Result<(), String> {
    let Ok(text) = std::str::from_utf8(rewritten) else {
        return Err(
            "load-legality fallback: source is not valid UTF-8 (reference backend disabled)"
                .to_string(),
        );
    };
    let (ast, root) = storm_lua_syntax::parser::parse_source(text).map_err(|e| e.to_string())?;
    legality::check(&ast, root)
}

/// PUC ロード規則の自前検査（代替判定の意味論）。
///
/// `storm-lua-syntax` のパースだけでは、PUC がロード時に拒否する
/// 意味論クラスの違法（ラベル解決、局所変数のスコープへの跳躍、
/// 200 個の有効局所変数上限、ループ外 break、最終文でない return、
/// 非可変長関数の `...`）を受理してしまうため、AST を歩いて
/// PUC の規則を再現する。対象は第299ラウンドのゲートが守る
/// S10/S14 の各機構であり、参照判定との既知の残差は次に限る:
/// upvalue 上限（255）と巨大な式に対するレジスタ上限は再現しない
/// （単一コールの引数上限だけは文脈自由なので再現する）、
/// メソッドの暗黙 `self` の数え方が上限近傍でずれうる、
/// 自前パーサの入れ子深さ上限（128）により参照判定がゲートで受理する
/// 深い入れ子を拒否しうる（コンパイル自体は両バックエンドとも同じ
/// 自前パーサで拒否するため生成結果の不一致にはならない）、
/// UTF-8 でないソースは拒否する。
#[cfg(not(feature = "backend-mlua"))]
mod legality {
    use storm_lua_syntax::{Ast, Node, SymbolId};

    /// PUC Lua 5.3 の有効局所変数上限（LUAI_MAXVARS）。
    const MAX_ACTIVE_LOCALS: usize = 200;

    #[derive(Default)]
    struct BlockInfo {
        parent: Option<usize>,
        index_in_parent: usize,
        predeclared: usize,
        stmt_locals: Vec<usize>,
        labels: Vec<(SymbolId, usize)>,
        /// repeat 本体か。until 条件は本体スコープ内で評価されるため、
        /// 本体末尾のラベルは交差した局所変数のスコープの外とはみなせず、
        /// PUC のブロック末尾ラベル例外が適用されない（Q15）。
        repeat_body: bool,
    }

    struct GotoRec {
        name: SymbolId,
        block: usize,
        index: usize,
    }

    struct LabelRec {
        name: SymbolId,
        block: usize,
        index: usize,
        last: bool,
    }

    #[derive(Default)]
    struct FnWork {
        blocks: Vec<BlockInfo>,
        gotos: Vec<GotoRec>,
        labels: Vec<LabelRec>,
    }

    struct Walker<'a> {
        ast: &'a Ast,
        active_locals: usize,
        loop_depth: usize,
        vararg: bool,
    }

    /// ブロック `block` 自身の中で、文 `index` の直前までに有効な
    /// 局所変数の数（先行宣言分＋文由来分）。`skip_own` のときは文由来分を
    /// 数えない（ブロック末尾ラベルの PUC 例外）。
    ///
    /// goto のスコープ侵入判定はこの「ラベルのブロック内」での比較で
    /// 行う。祖先ブロック分の寄与は goto 側とラベル側で同一であり、
    /// 内側ブロックで消える局所変数まで合算した総数比較は、消える分と
    /// 侵入する分が相殺されて侵入を見逃す（Q14）。
    fn active_in_block(blocks: &[BlockInfo], block: usize, index: usize, skip_own: bool) -> usize {
        let info = &blocks[block];
        let mut total = info.predeclared;
        if !skip_own {
            total += info.stmt_locals.iter().take(index).sum::<usize>();
        }
        total
    }

    /// `from` ブロックから祖先 `ancestor` ブロックへ見たときの、
    /// `ancestor` 内での文位置。`from` が `ancestor` 自身なら `index`、
    /// そうでなければ `ancestor` の子にあたるブロックの `index_in_parent`。
    fn position_in_ancestor(
        blocks: &[BlockInfo],
        from: usize,
        index: usize,
        ancestor: usize,
    ) -> usize {
        let mut current = from;
        let mut idx = index;
        while current != ancestor {
            let info = &blocks[current];
            idx = info.index_in_parent;
            current = info
                .parent
                .expect("label block is an ancestor of the goto block");
        }
        idx
    }

    pub fn check(ast: &Ast, root: u32) -> Result<(), String> {
        let mut walker = Walker {
            ast,
            active_locals: 0,
            loop_depth: 0,
            vararg: true,
        };
        // チャンク自体は可変長引数の関数としてロードされる。
        walker.walk_function(root, true, 0)
    }

    impl<'a> Walker<'a> {
        fn node(&self, id: u32) -> &'a Node {
            self.ast.node(id)
        }

        fn new_block(
            work: &mut FnWork,
            parent: Option<usize>,
            index_in_parent: usize,
            predeclared: usize,
            stmt_count: usize,
            repeat_body: bool,
        ) -> usize {
            work.blocks.push(BlockInfo {
                parent,
                index_in_parent,
                predeclared,
                stmt_locals: vec![0; stmt_count],
                labels: Vec::new(),
                repeat_body,
            });
            work.blocks.len() - 1
        }

        fn walk_function(&mut self, body: u32, vararg: bool, params: usize) -> Result<(), String> {
            let saved = (self.active_locals, self.loop_depth, self.vararg);
            // 引数は本体ブロックの predeclared として walk_block が加算する。
            // ここでも加算すると二重計数になる（Q12: 引数101個が 202 と数え
            // られ誤拒否された）。
            self.active_locals = 0;
            self.loop_depth = 0;
            self.vararg = vararg;
            // 引数超過の早期エラーでも外側の状態は必ず復元する。復元を
            // 飛ばすと、呼出側ブロックの終了処理（active_locals の巻き戻し）
            // が現在の総数より大きい分を引くことになり、debug では減算
            // オーバーフローで panic した（R09）。
            let result = if params > MAX_ACTIVE_LOCALS {
                Err("load-legality fallback: too many local variables (PUC limit 200)".to_string())
            } else {
                let mut work = FnWork::default();
                self.walk_block(body, None, 0, params, &mut work)
                    .and_then(|()| self.resolve_gotos(&work))
            };
            let (active, loops, outer_vararg) = saved;
            self.active_locals = active;
            self.loop_depth = loops;
            self.vararg = outer_vararg;
            result
        }

        fn walk_block(
            &mut self,
            block_node: u32,
            parent: Option<usize>,
            index_in_parent: usize,
            predeclared: usize,
            work: &mut FnWork,
        ) -> Result<(), String> {
            self.walk_block_kind(
                block_node,
                parent,
                index_in_parent,
                predeclared,
                work,
                false,
            )
        }

        #[allow(clippy::too_many_arguments)]
        fn walk_block_kind(
            &mut self,
            block_node: u32,
            parent: Option<usize>,
            index_in_parent: usize,
            predeclared: usize,
            work: &mut FnWork,
            repeat_body: bool,
        ) -> Result<(), String> {
            let Node::Block(stmts) = self.node(block_node) else {
                return Err("load-legality fallback: expected a block node".to_string());
            };
            let stmts = stmts.clone();
            let block = Self::new_block(
                work,
                parent,
                index_in_parent,
                predeclared,
                stmts.len(),
                repeat_body,
            );
            // ブロック末尾ラベルの判定: ラベルの後ろに残る文が全てラベル
            // なら、そのラベルも末尾ラベルとして扱う（R10: PUC の末尾例外
            // は末尾の連続ラベル列全体に及び、列の先頭も例外対象になる。
            // 最後の1つだけを末尾扱いすると列の先頭への goto を誤って
            // スコープ侵入と判定する）。
            let mut suffix_all_labels = vec![false; stmts.len() + 1];
            suffix_all_labels[stmts.len()] = true;
            for i in (0..stmts.len()).rev() {
                suffix_all_labels[i] =
                    matches!(self.node(stmts[i]), Node::Label(_)) && suffix_all_labels[i + 1];
            }
            // ラベルはブロック全体から見えるため先に登録する（重複は拒否）。
            for (index, stmt) in stmts.iter().enumerate() {
                if let Node::Label(name) = self.node(*stmt) {
                    let name = *name;
                    if work.blocks[block].labels.iter().any(|(n, _)| *n == name) {
                        return Err(
                            "load-legality fallback: duplicate label in the same block".to_string()
                        );
                    }
                    work.blocks[block].labels.push((name, index));
                    work.labels.push(LabelRec {
                        name,
                        block,
                        index,
                        last: suffix_all_labels[index + 1],
                    });
                }
            }
            self.active_locals += predeclared;
            if self.active_locals > MAX_ACTIVE_LOCALS {
                return Err(
                    "load-legality fallback: too many local variables (PUC limit 200)".to_string(),
                );
            }
            let mut declared_here = predeclared;
            let mut result = Ok(());
            for (index, stmt) in stmts.iter().enumerate() {
                if let Node::Return(_) = self.node(*stmt) {
                    if index + 1 != stmts.len() {
                        result = Err(
                            "load-legality fallback: return must be the last statement of a block"
                                .to_string(),
                        );
                        break;
                    }
                }
                if let Err(e) = self.walk_stmt(*stmt, block, index, work) {
                    result = Err(e);
                    break;
                }
                declared_here += work.blocks[block].stmt_locals[index];
            }
            // 会計が正しければ declared_here は加算分と正確に一致する。
            // saturating は、将来の会計漏れが万一あっても panic ではなく
            // 判定エラーとして現れるための防御（R09 の教訓）。
            self.active_locals = self.active_locals.saturating_sub(declared_here);
            result
        }

        fn child_block(
            &mut self,
            body: u32,
            parent: usize,
            index: usize,
            predeclared: usize,
            work: &mut FnWork,
        ) -> Result<(), String> {
            self.walk_block(body, Some(parent), index, predeclared, work)
        }

        fn child_block_repeat(
            &mut self,
            body: u32,
            parent: usize,
            index: usize,
            work: &mut FnWork,
        ) -> Result<(), String> {
            self.walk_block_kind(body, Some(parent), index, 0, work, true)
        }

        fn declare(
            &mut self,
            work: &mut FnWork,
            block: usize,
            index: usize,
            count: usize,
        ) -> Result<(), String> {
            work.blocks[block].stmt_locals[index] = count;
            self.active_locals += count;
            if self.active_locals > MAX_ACTIVE_LOCALS {
                return Err(
                    "load-legality fallback: too many local variables (PUC limit 200)".to_string(),
                );
            }
            Ok(())
        }

        fn walk_stmt(
            &mut self,
            stmt: u32,
            block: usize,
            index: usize,
            work: &mut FnWork,
        ) -> Result<(), String> {
            match self.node(stmt).clone() {
                Node::Block(_) => self.child_block(stmt, block, index, 0, work),
                Node::Do(body) => self.child_block(body, block, index, 0, work),
                Node::While(cond, body) => {
                    self.walk_expr(cond, work)?;
                    self.loop_depth += 1;
                    let r = self.child_block(body, block, index, 0, work);
                    self.loop_depth -= 1;
                    r
                }
                Node::Repeat(body, cond) => {
                    self.loop_depth += 1;
                    let r = self.child_block_repeat(body, block, index, work);
                    let r = r.and_then(|()| self.walk_expr(cond, work));
                    self.loop_depth -= 1;
                    r
                }
                Node::If(arms, els) => {
                    for arm in &arms {
                        self.walk_expr(arm.cond, work)?;
                        self.child_block(arm.body, block, index, 0, work)?;
                    }
                    if let Some(body) = els {
                        self.child_block(body, block, index, 0, work)?;
                    }
                    Ok(())
                }
                Node::Fornum(_, start, end, step, body) => {
                    self.walk_expr(start, work)?;
                    self.walk_expr(end, work)?;
                    if let Some(step) = step {
                        self.walk_expr(step, work)?;
                    }
                    self.loop_depth += 1;
                    // PUC は制御変数のほかに index／limit／step の隠し局所変数
                    // 3つを数える（Q13: 合計4）。
                    let r = self.child_block(body, block, index, 4, work);
                    self.loop_depth -= 1;
                    r
                }
                Node::Forin(names, exprs, body) => {
                    let count = names.len();
                    for e in &exprs {
                        self.walk_expr(*e, work)?;
                    }
                    self.loop_depth += 1;
                    // PUC は宣言名のほかに iterator／state／control の隠し
                    // 局所変数3つを数える（Q13）。
                    let r = self.child_block(body, block, index, count + 3, work);
                    self.loop_depth -= 1;
                    r
                }
                Node::Funcstat(target, value) => {
                    self.walk_expr(target, work)?;
                    self.walk_expr(value, work)
                }
                Node::Localfunc(_, value) => {
                    // 名前は関数本体より前に自ブロックで有効になる。
                    self.declare(work, block, index, 1)?;
                    self.walk_expr(value, work)
                }
                Node::Local(names, inits) => {
                    for e in &inits {
                        self.walk_expr(*e, work)?;
                    }
                    let count = names.len();
                    self.declare(work, block, index, count)
                }
                Node::Return(exprs) => {
                    for e in &exprs {
                        self.walk_expr(*e, work)?;
                    }
                    Ok(())
                }
                Node::Callstat(e) => self.walk_expr(e, work),
                Node::Assign(targets, values) => {
                    for e in targets.iter().chain(values.iter()) {
                        self.walk_expr(*e, work)?;
                    }
                    Ok(())
                }
                Node::Break => {
                    if self.loop_depth == 0 {
                        Err("load-legality fallback: break outside a loop".to_string())
                    } else {
                        Ok(())
                    }
                }
                Node::Goto(name) => {
                    work.gotos.push(GotoRec { name, block, index });
                    Ok(())
                }
                Node::Label(_) => Ok(()),
                _ => Err("load-legality fallback: unexpected statement node".to_string()),
            }
        }

        fn walk_expr(&mut self, expr: u32, work: &mut FnWork) -> Result<(), String> {
            match self.node(expr).clone() {
                Node::Function(params, vararg, body) => {
                    self.walk_function(body, vararg, params.len())
                }
                Node::Vararg => {
                    if self.vararg {
                        Ok(())
                    } else {
                        Err(
                            "load-legality fallback: cannot use '...' outside a vararg function"
                                .to_string(),
                        )
                    }
                }
                Node::Table(fields) => {
                    for field in &fields {
                        match field {
                            storm_lua_syntax::TableField::Arr(v) => self.walk_expr(*v, work)?,
                            storm_lua_syntax::TableField::Name(_, v) => self.walk_expr(*v, work)?,
                            storm_lua_syntax::TableField::KVar(k, v) => {
                                self.walk_expr(*k, work)?;
                                self.walk_expr(*v, work)?;
                            }
                        }
                    }
                    Ok(())
                }
                Node::Un(_, e) | Node::Paren(e) => self.walk_expr(e, work),
                Node::Bin(_, l, r) => {
                    self.walk_expr(l, work)?;
                    self.walk_expr(r, work)
                }
                Node::Call(f, args, _) => {
                    // A single call's register window (function register
                    // + arguments + one) exceeds the 255-register file
                    // whenever the argument count alone reaches 254, in
                    // any context, so the reference parser always
                    // rejects it (R309 T07 boundary). Context-dependent
                    // register overflow below this count remains a
                    // documented residual.
                    if args.len() >= 254 {
                        return Err(
                            "load-legality fallback: too many registers (call arguments)"
                                .to_string(),
                        );
                    }
                    self.walk_expr(f, work)?;
                    for a in &args {
                        self.walk_expr(*a, work)?;
                    }
                    Ok(())
                }
                Node::Index(o, k, _) => {
                    self.walk_expr(o, work)?;
                    self.walk_expr(k, work)
                }
                Node::Methodname(o, _) => self.walk_expr(o, work),
                _ => Ok(()),
            }
        }

        fn resolve_gotos(&self, work: &FnWork) -> Result<(), String> {
            for goto in &work.gotos {
                // 最も内側の可視ラベルを探す（自ブロックから外側へ）。
                let mut found: Option<&LabelRec> = None;
                let mut block = goto.block;
                loop {
                    if let Some(label) = work
                        .labels
                        .iter()
                        .find(|l| l.block == block && l.name == goto.name)
                    {
                        found = Some(label);
                        break;
                    }
                    match work.blocks[block].parent {
                        Some(parent) => block = parent,
                        None => break,
                    }
                }
                let Some(label) = found else {
                    return Err("load-legality fallback: no visible label for goto".to_string());
                };
                // 侵入判定はラベルのブロック内だけで比較する（Q14）。
                // ブロック末尾ラベルの例外は repeat 本体には適用しない:
                // until 条件が本体スコープ内で交差局所変数を参照しうるため、
                // 末尾ラベルもスコープ内にあるものとして扱う（Q15）。
                let skip_own = label.last && !work.blocks[label.block].repeat_body;
                let label_active =
                    active_in_block(&work.blocks, label.block, label.index, skip_own);
                let goto_pos =
                    position_in_ancestor(&work.blocks, goto.block, goto.index, label.block);
                let goto_active = active_in_block(&work.blocks, label.block, goto_pos, false);
                if label_active > goto_active {
                    return Err(
                        "load-legality fallback: goto jumps into the scope of a local variable"
                            .to_string(),
                    );
                }
            }
            Ok(())
        }
    }
}

/// Returns `Ok(())` when the project VM would accept `source` at load,
/// or the verdict backend's error message when it would reject it.
///
/// The verdict backend is selected at compile time (see the module
/// documentation): reference-exact with `backend-mlua`, the own-parser
/// approximation without it.
pub fn check_load_legality(source: &[u8]) -> Result<(), String> {
    let rewritten = dialect::rewrite(source)?;
    parse_verdict(&rewritten)
}

/// Load-legality check with a host-supplied verdict: applies the
/// dialect rewrite, then asks `verdict` whether the rewritten source
/// parses. `verdict` receives the rewritten bytes and returns the
/// parser's error message on rejection, mirroring
/// [`check_load_legality`]'s contract.
pub fn check_load_legality_with(
    source: &[u8],
    verdict: impl Fn(&[u8]) -> Result<(), String>,
) -> Result<(), String> {
    let rewritten = dialect::rewrite(source)?;
    verdict(&rewritten)
}

#[cfg(all(test, feature = "backend-mlua"))]
mod tests {
    use super::check_load_legality;

    fn assert_reject(src: &str) {
        assert!(
            check_load_legality(src.as_bytes()).is_err(),
            "expected rejection, accepted: {src}"
        );
    }

    fn assert_accept(src: &str) {
        assert!(
            check_load_legality(src.as_bytes()).is_ok(),
            "expected acceptance, rejected: {src} -> {:?}",
            check_load_legality(src.as_bytes()).err()
        );
    }

    #[test]
    fn used_barrier_goto_is_rejected() {
        // 候補S10 の真の核: ラベル後にバリア局所変数を使う形は VM も拒否する。
        assert_reject("local a = 1 goto done local b = 2 ::done:: return a + b");
        assert_reject(
            "local function f() local a = 1 goto d local b = 2 ::d:: return b end emit(f())",
        );
    }

    #[test]
    fn dialect_wrapped_goto_is_accepted() {
        // 第210ラウンドの方言規則: ラベル後に参照されないバリアは包みで受理。
        assert_accept("local a = 1 goto done local b = 2 ::done:: return a");
        assert_accept("local a = 1 goto d local b = 2 ::d:: emit(a)");
        // ブロック末尾ラベルは PUC 自体が受理する。
        assert_accept("local function f() goto l local x = 5 ::l:: end return f()");
        // 糊付け形も S11 修正後は受理される。
        assert_accept(
            "local function f() local a = 1 goto done local b = 2 a = b::done:: end return f()",
        );
        assert_accept("local s = 0 for i = 1, 2 do goto cont local y = 9::cont:: ; end return s");
    }

    #[test]
    fn laundered_constructs_are_rejected() {
        // 候補S14 の各機構: ソース時点で VM が拒否する形は全て拒否される。
        // 201 局所変数（チャンク／関数）。
        let mut chunk201 = String::from("local ");
        chunk201.push_str(&vec!["v"; 201].join(", "));
        chunk201.push_str(" = 1 return v1");
        assert_reject(&chunk201);
        let mut fn201 = String::from("local function f(");
        fn201.push_str(&vec!["p"; 201].join(", "));
        fn201.push_str(") return p1 end return f()");
        assert_reject(&fn201);
        // 未定義ラベル・重複ラベル。
        assert_reject("goto nowhere return 1");
        assert_reject("::a:: ::a:: return 1");
        // ループ外の break・最終文でない return・非可変長関数の `...`。
        assert_reject("break return 1");
        assert_reject("local function f() return 1 local x = 2 end return f()");
        assert_reject("local function f() return ... end return f()");
        // ラベルを含む関数内の 201 局所変数（A2 の once-called 形の核）。
        let mut body = String::from("local function f() local ");
        body.push_str(&vec!["v"; 201].join(", "));
        body.push_str(" = 1 return v1 end return f()");
        assert_reject(&body);
    }

    #[test]
    fn legal_programs_are_accepted() {
        assert_accept("local a = 1 return a");
        assert_accept("local t = {} for i = 1, 10 do t[i] = i * 2 end return t[3]");
        assert_accept("local function f(...) return ... end return f(1, 2)");
        assert_accept("local x = 0 repeat x = x + 1 until x >= 3 return x");
        assert_accept("::top:: local i = 0 i = i + 1 if i < 2 then goto top end return i");
    }

    #[test]
    fn codex_run5_cases() {
        // Codex リリース前レビュー run5 の Q12〜Q16（第306ラウンドで修正）。
        // 参照判定（mlua）と代替判定で同一の受理集合になることを固定する。
        // Q12: 通常関数の引数は1回だけ数える（101個・200個は受理）。
        for count in [101usize, 200] {
            let params = (0..count)
                .map(|i| format!("p{i}"))
                .collect::<Vec<_>>()
                .join(",");
            assert_accept(&format!(
                "local function f({params}) return 7 end return f()"
            ));
        }
        // Q13: for の隠し局所変数（数値 for は制御変数+3、generic for は
        // 宣言名+3）も 200 上限に数える。
        for (decls, expect_ok) in [(196usize, true), (197, false)] {
            let names = (0..decls)
                .map(|i| format!("v{i}"))
                .collect::<Vec<_>>()
                .join(",");
            let numeric = format!("local {names} = 1 for i = 1, 1 do end return 7");
            let generic = format!("local {names} = 1 for k in pairs({{}}) do end return 7");
            if expect_ok {
                assert_accept(&numeric);
                assert_accept(&generic);
            } else {
                assert_reject(&numeric);
                assert_reject(&generic);
            }
        }
        // Q14: 内側ブロックで消える局所変数は、外側のラベルへの
        // スコープ侵入を相殺しない。
        assert_reject("do local y = 1 goto L end local x = 2 ::L:: return x");
        // Q15: repeat 本体末尾のラベルは until 条件のスコープ内にある。
        assert_reject("repeat goto L local x = 2 ::L:: until x == 2");
        // Q16: return の後の余分なセミコロンは受理しない（1つは合法）。
        assert_reject("return 7;;");
        assert_accept("return 7;");
    }

    #[test]
    fn codex_run6_cases() {
        // Codex リリース前レビュー第2回（run6）の R09・R10
        // （第307ラウンドで修正）。参照判定と代替判定で同一の受理集合に
        // なることを固定する。
        // R09: 引数201個の関数は通常の上限エラーで拒否される（代替判定
        // ではかつて debug で減算オーバーフロー panic した。assert が
        // 完走すること自体が panic の不在を示す）。
        let params201 = (0..201)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        assert_reject(&format!("local x;function f({params201})end"));
        assert_reject(&format!(
            "local function g() local a,b,c function f({params201}) end end return g"
        ));
        // R10: 末尾の連続ラベル列の先頭もブロック末尾ラベルとして
        // 扱い、受理する。
        assert_accept("do goto L end local x=2 ::L:: ::M::");
    }

    #[test]
    fn codex_run8_cases() {
        // Codex リリース前レビュー第4回（run8）の T10（第309ラウンドで
        // 修正）。文法そのものの妥当性であり、文書化済み残差（上限・
        // 符号化の境界）には含まれない。参照判定と代替判定で同一の
        // 受理集合になることを固定する。
        // (a) 括弧式の代入先。
        assert_reject("(x)=1;return x");
        // (b) 数値リテラルとキーワードの糊付け（字句分割の誤り）。
        assert_reject("return 1and 2");
        // (c) 呼出し式の代入先。
        assert_reject("a,f()=1,2;return 7");
        // 対照: 正当な var 形は受理が続く。
        assert_accept("(t).x=1");
        assert_accept("t.x,t[1]=1,2");
        assert_accept("return 1 and 2");
        // T07 の境界: 方言書換え後の形で 252/253 は受理、254 は
        // 単一コールの引数上限により両バックエンドとも拒否する。
        let regsrc = |n: usize| format!("a,a=type({}),type(0);return a", vec!["0"; n].join(","));
        assert_accept(&regsrc(252));
        assert_accept(&regsrc(253));
        assert_reject(&regsrc(254));
    }
}

#[cfg(all(test, not(feature = "backend-mlua")))]
mod fallback_tests {
    use super::check_load_legality;

    fn assert_reject(src: &str) {
        assert!(
            check_load_legality(src.as_bytes()).is_err(),
            "expected rejection, accepted: {src}"
        );
    }

    fn assert_accept(src: &str) {
        assert!(
            check_load_legality(src.as_bytes()).is_ok(),
            "expected acceptance, rejected: {src} -> {:?}",
            check_load_legality(src.as_bytes()).err()
        );
    }

    // 代替判定（自前パーサ＋PUC ロード規則チェッカ）は、参照判定と
    // 同じ公開ケース集合で固定する。意図的に残る残差（upvalue 上限、
    // レジスタ上限、UTF-8 でないソース）はモジュール文書に明記した
    // 範囲であり、以下のケースには含まれない。

    #[test]
    fn used_barrier_goto_is_rejected() {
        assert_reject("local a = 1 goto done local b = 2 ::done:: return a + b");
        assert_reject(
            "local function f() local a = 1 goto d local b = 2 ::d:: return b end emit(f())",
        );
    }

    #[test]
    fn dialect_wrapped_goto_is_accepted() {
        assert_accept("local a = 1 goto done local b = 2 ::done:: return a");
        assert_accept("local a = 1 goto d local b = 2 ::d:: emit(a)");
        assert_accept("local function f() goto l local x = 5 ::l:: end return f()");
        assert_accept(
            "local function f() local a = 1 goto done local b = 2 a = b::done:: end return f()",
        );
        assert_accept("local s = 0 for i = 1, 2 do goto cont local y = 9::cont:: ; end return s");
    }

    #[test]
    fn laundered_constructs_are_rejected() {
        let mut chunk201 = String::from("local ");
        chunk201.push_str(&vec!["v"; 201].join(", "));
        chunk201.push_str(" = 1 return v1");
        assert_reject(&chunk201);
        let mut fn201 = String::from("local function f(");
        fn201.push_str(&vec!["p"; 201].join(", "));
        fn201.push_str(") return p1 end return f()");
        assert_reject(&fn201);
        assert_reject("goto nowhere return 1");
        assert_reject("::a:: ::a:: return 1");
        assert_reject("break return 1");
        assert_reject("local function f() return 1 local x = 2 end return f()");
        assert_reject("local function f() return ... end return f()");
        let mut body = String::from("local function f() local ");
        body.push_str(&vec!["v"; 201].join(", "));
        body.push_str(" = 1 return v1 end return f()");
        assert_reject(&body);
    }

    #[test]
    fn legal_programs_are_accepted() {
        assert_accept("local a = 1 return a");
        assert_accept("local t = {} for i = 1, 10 do t[i] = i * 2 end return t[3]");
        assert_accept("local function f(...) return ... end return f(1, 2)");
        assert_accept("local x = 0 repeat x = x + 1 until x >= 3 return x");
        assert_accept("::top:: local i = 0 i = i + 1 if i < 2 then goto top end return i");
    }

    #[test]
    fn codex_run5_cases() {
        // Codex リリース前レビュー run5 の Q12〜Q16（第306ラウンドで修正）。
        // 参照判定（mlua）と代替判定で同一の受理集合になることを固定する。
        // Q12: 通常関数の引数は1回だけ数える（101個・200個は受理）。
        for count in [101usize, 200] {
            let params = (0..count)
                .map(|i| format!("p{i}"))
                .collect::<Vec<_>>()
                .join(",");
            assert_accept(&format!(
                "local function f({params}) return 7 end return f()"
            ));
        }
        // Q13: for の隠し局所変数（数値 for は制御変数+3、generic for は
        // 宣言名+3）も 200 上限に数える。
        for (decls, expect_ok) in [(196usize, true), (197, false)] {
            let names = (0..decls)
                .map(|i| format!("v{i}"))
                .collect::<Vec<_>>()
                .join(",");
            let numeric = format!("local {names} = 1 for i = 1, 1 do end return 7");
            let generic = format!("local {names} = 1 for k in pairs({{}}) do end return 7");
            if expect_ok {
                assert_accept(&numeric);
                assert_accept(&generic);
            } else {
                assert_reject(&numeric);
                assert_reject(&generic);
            }
        }
        // Q14: 内側ブロックで消える局所変数は、外側のラベルへの
        // スコープ侵入を相殺しない。
        assert_reject("do local y = 1 goto L end local x = 2 ::L:: return x");
        // Q15: repeat 本体末尾のラベルは until 条件のスコープ内にある。
        assert_reject("repeat goto L local x = 2 ::L:: until x == 2");
        // Q16: return の後の余分なセミコロンは受理しない（1つは合法）。
        assert_reject("return 7;;");
        assert_accept("return 7;");
    }

    #[test]
    fn codex_run6_cases() {
        // Codex リリース前レビュー第2回（run6）の R09・R10
        // （第307ラウンドで修正）。参照判定と代替判定で同一の受理集合に
        // なることを固定する。
        // R09: 引数201個の関数は通常の上限エラーで拒否される（代替判定
        // ではかつて debug で減算オーバーフロー panic した。assert が
        // 完走すること自体が panic の不在を示す）。
        let params201 = (0..201)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        assert_reject(&format!("local x;function f({params201})end"));
        assert_reject(&format!(
            "local function g() local a,b,c function f({params201}) end end return g"
        ));
        // R10: 末尾の連続ラベル列の先頭もブロック末尾ラベルとして
        // 扱い、受理する。
        assert_accept("do goto L end local x=2 ::L:: ::M::");
    }

    #[test]
    fn codex_run8_cases() {
        // Codex リリース前レビュー第4回（run8）の T10（第309ラウンドで
        // 修正）。文法そのものの妥当性であり、文書化済み残差（上限・
        // 符号化の境界）には含まれない。参照判定と代替判定で同一の
        // 受理集合になることを固定する。
        // (a) 括弧式の代入先。
        assert_reject("(x)=1;return x");
        // (b) 数値リテラルとキーワードの糊付け（字句分割の誤り）。
        assert_reject("return 1and 2");
        // (c) 呼出し式の代入先。
        assert_reject("a,f()=1,2;return 7");
        // 対照: 正当な var 形は受理が続く。
        assert_accept("(t).x=1");
        assert_accept("t.x,t[1]=1,2");
        assert_accept("return 1 and 2");
        // T07 の境界: 方言書換え後の形で 252/253 は受理、254 は
        // 単一コールの引数上限により両バックエンドとも拒否する。
        let regsrc = |n: usize| format!("a,a=type({}),type(0);return a", vec!["0"; n].join(","));
        assert_accept(&regsrc(252));
        assert_accept(&regsrc(253));
        assert_reject(&regsrc(254));
    }

    #[test]
    fn parser_nesting_limit_is_a_documented_residual() {
        // R11: 自前パーサの入れ子深さ上限（128）でゲートが拒否する形は
        // モジュール文書に明記した残差（参照判定はゲートで受理する。
        // コンパイル自体は両バックエンドとも同じパーサ上限で拒否する
        // ため、生成結果の不一致にはならない）。
        let src = "do ".repeat(128) + &"end ".repeat(128);
        let gate = check_load_legality(src.as_bytes());
        assert!(
            matches!(&gate, Err(e) if e.contains("nesting")),
            "expected the documented nesting residual, got: {gate:?}"
        );
    }

    #[test]
    fn plain_syntax_errors_are_rejected() {
        assert_reject("local a = ");
        assert_reject("local a = 1 return a +");
    }
}
