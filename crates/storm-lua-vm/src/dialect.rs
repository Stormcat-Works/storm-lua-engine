//! ロード時の方言変換。vendored な PUC Lua 5.3 のパーサ／実行意味論が、
//! 本プロジェクトの基準意味論（ Stormworks 方言として採用した規則）と
//! 異なる2点について、ロード前にソースを等価形へ書き換える。
//!
//! (a) 多重代入の重複ターゲット: PUC はストアを逆順に行うため最終値が
//!     first-wins になる（ローカル宣言文だけは last-wins で、文の種類で
//!     挙動が割れている）。基準意味論は last-wins。ターゲット式と右辺を
//!     一時ローカルへ評価してから、PUC と同じ逆順にストアする形へ
//!     書き換える。ただし重複ターゲットは最後の出現だけをストアする
//!     ため最終値が last-wins になり、異なるターゲット間のストア順序
//!     （_ENV の再束縛が他のストアの前か後か等）は PUC と一致する。
//!     アドレス部の評価時点も PUC に合わせる（第306ラウンド Q09）:
//!     同一関数のローカルをそのまま参照する base／キーと、upvalue の
//!     base はストア時点で読まれる（レジスタ／セルの遅延読み）ため
//!     一時表へは凍結せず、ストア直前にスナップショットして使う。
//!     それ以外の部分（複合式、鎖の途中、グローバル名、upvalue の
//!     キー）は PUC が左辺走査の段階で実体化するため、右辺より先に
//!     一時表へ凍結する。暗黙の `_ENV` base も upvalue そのもので
//!     あるためストア時点で読まれる（T06 形状B）。また素の名前
//!     ターゲットが `_ENV` 自体を再束縛する文では、PUC は競合回避
//!     のためその文の大域名ストアを左辺走査時点の環境へ固定する
//!     （check_conflict 相当）ため、大域名ターゲットは走査時点の
//!     `_ENV` を base とする Index ストアとして扱う（T06 形状A）。
//! (b) ローカル宣言を飛び越えて同一ブロック内のラベルへ跳ぶ goto:
//!     交差したローカルがラベル以降で一切参照されない場合、その区間を
//!     `do ... end` で包み、ラベルを元のブロックへ残すことで受理させる。
//!     ラベル後に交差ローカルへの参照がある形は変換せず、従来どおり
//!     ロードエラーになる（未初期化のローカルを観測し得るため）。
//!
//! 変換はトークン列ベースで保守的に行う: 構造を確信できない箇所が
//! あれば変換せず、元のソースを返す。改行は追加・削除しないため、
//! 行番号は保存される。

use std::borrow::Cow;
use std::collections::HashSet;

/// 書き換えパーサ専用ワーカースレッドのスタック量（R12）。
///
/// パーサは入れ子に比例してネイティブスタックを消費する。深さ上限
/// （[`MAX_PARSE_DEPTH`]）に届く前に呼出側スレッドのスタックが尽きると、
/// debug ビルド＋小さいスタック（2MiB 級）の組合せでプロセスが異常終了
/// した（run6 R12）。書き換えを十分なスタックを持つ専用スレッドで実行
/// することで、受理範囲（深さ上限）を変えずに呼出側のスタック量への
/// 依存を断つ。仮想予約であり、実際に触れたページだけが消費される。
const REWRITE_STACK_SIZE: usize = 256 * 1024 * 1024;

/// ロード直前に方言変換を適用する。変換不要／不能なら元のバイト列を返す。
///
/// ワーカー生成の失敗とワーカー内 panic は、どちらもロードエラーとして
/// 失敗する（S03／潜在リスクの是正、R308）。生成失敗時に呼出側スタック
/// で本体を実行する分岐は、debug ビルド＋小さい呼出側スタック（2MiB 級）
/// ＋生成失敗の複合条件でプロセスを異常終了させた（S03）。panic 時に
/// 元ソースを返す分岐は、未変換の方言ソースを後段へ渡し、形状により
/// 無言の意味乖離（多重代入の first-wins 化）や誤ったロード拒否を
/// 招き得る。生成失敗がその時点で正当な入力を拒否し得る点は否めない
/// が、異常終了や無言の意味乖離より正しい失敗である。
///
/// wasm32 ではスレッド自体が存在せず生成は常時失敗するため、その場
/// 実行はフォールバック分岐ではなくプラットフォーム構成である。
/// wasm のスタック溢れはホストへ見える trap となり、パーサの深さ上限
/// が再帰を bound する。ネイティブの小スタック異常終了という S03 の
/// ハザードは wasm には存在しない。
pub fn rewrite(source: &[u8]) -> Result<Cow<'_, [u8]>, String> {
    // 専用ワーカースレッド上で書き換え本体を実行する（R12）。
    #[cfg(target_arch = "wasm32")]
    {
        Ok(rewrite_impl(source))
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::thread::scope(|scope| {
            match std::thread::Builder::new()
                .name("storm-lua-dialect-rewrite".to_string())
                .stack_size(REWRITE_STACK_SIZE)
                .spawn_scoped(scope, || rewrite_impl(source))
            {
                Ok(handle) => handle.join().map_err(|_| {
                    "dialect rewrite worker panicked; the unrewritten source is not loaded"
                        .to_string()
                }),
                Err(error) => Err(format!(
                    "dialect rewrite worker could not be spawned: {error}"
                )),
            }
        })
    }
}

fn rewrite_impl(source: &[u8]) -> Cow<'_, [u8]> {
    let mut current: Vec<u8> = source.to_vec();
    // (b) は1回の包みごとに再解析する（オフセットがずれるため）。
    for _ in 0..64 {
        let Some(tokens) = tokenize(&current) else {
            break;
        };
        let mut parser = Parser {
            source: &current,
            toks: &tokens,
            blocks: Vec::new(),
            depth: std::rc::Rc::new(std::cell::Cell::new(0)),
            scopes: Vec::new(),
            pending_frame_names: Vec::new(),
        };
        if parser.parse_chunk().is_err() {
            break;
        }
        match find_goto_wrap(&current, &tokens, &parser.blocks) {
            Some((insert_do_at, insert_end_at)) => {
                // 挿入するキーワードは前後のトークンと結合してはならない。
                // 挿入位置の直前バイトが名前・数値の継続文字の場合（plain
                // プリンタの `a=b::done::end` のような糊付け形）、スペース
                // を挟まないと `b` + `end` が識別子 `bend` に、`9` + `end`
                // が不正数値 `9end` になる（候補S11）。改行は増やさない
                // ため行番号は保存される。
                fn glued_before(source: &[u8], at: usize) -> bool {
                    at > 0 && (source[at - 1].is_ascii_alphanumeric() || source[at - 1] == b'_')
                }
                let mut next = Vec::with_capacity(current.len() + 10);
                next.extend_from_slice(&current[..insert_do_at]);
                if glued_before(&current, insert_do_at) {
                    next.push(b' ');
                }
                next.extend_from_slice(b"do ");
                next.extend_from_slice(&current[insert_do_at..insert_end_at]);
                if glued_before(&current, insert_end_at) {
                    next.push(b' ');
                }
                next.extend_from_slice(b"end ");
                next.extend_from_slice(&current[insert_end_at..]);
                current = next;
            }
            None => break,
        }
    }
    // (a) は文単位の置換を不動点まで繰り返す。関数式の内側にある代入は
    // 外側の代入の置換範囲に含まれるため、1回の走査では内側が残る
    // （内側は外側の置換テキストにそのままコピーされ、次の走査で処理
    // される）。置換は新しい重複代入を生まないため、この反復は終了する。
    for _ in 0..64 {
        let Some(tokens) = tokenize(&current) else {
            break;
        };
        let all_names: HashSet<&str> = tokens
            .iter()
            .filter(|t| t.kind == Tok::Name)
            .map(|t| text(&current, t))
            .collect();
        let mut parser = Parser {
            source: &current,
            toks: &tokens,
            blocks: Vec::new(),
            depth: std::rc::Rc::new(std::cell::Cell::new(0)),
            scopes: Vec::new(),
            pending_frame_names: Vec::new(),
        };
        if parser.parse_chunk().is_err() {
            break;
        }
        let mut edits = collect_assign_edits(&current, &tokens, &parser.blocks, &all_names);
        if edits.is_empty() {
            break;
        }
        // 開始位置の昇順、同点では外側（終了位置の降順）を先に。
        edits.sort_by_key(|e| (e.0, std::cmp::Reverse(e.1)));
        let mut out = Vec::with_capacity(current.len() + edits.len() * 32);
        let mut pos = 0usize;
        let mut applied = false;
        for (start, end, text) in &edits {
            if *start < pos {
                // 適用済みの外側の編集に含まれる内側の編集。今回の走査
                // では触らず、次の走査で書き換える。
                continue;
            }
            out.extend_from_slice(&current[pos..*start]);
            out.extend_from_slice(text);
            pos = *end;
            applied = true;
        }
        if !applied {
            break;
        }
        out.extend_from_slice(&current[pos..]);
        current = out;
    }
    if current == source {
        Cow::Borrowed(source)
    } else {
        Cow::Owned(current)
    }
}

fn text<'a>(source: &'a [u8], tok: &Token) -> &'a str {
    std::str::from_utf8(&source[tok.start..tok.end]).unwrap_or("")
}

// ---------------------------------------------------------------------------
// トークナイザ

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok {
    Name,
    Keyword,
    Number,
    Str,
    Op,
}

#[derive(Debug, Clone, Copy)]
struct Token {
    kind: Tok,
    start: usize,
    end: usize,
}

fn is_keyword(s: &str) -> bool {
    matches!(
        s,
        "and"
            | "break"
            | "do"
            | "else"
            | "elseif"
            | "end"
            | "false"
            | "for"
            | "function"
            | "goto"
            | "if"
            | "in"
            | "local"
            | "nil"
            | "not"
            | "or"
            | "repeat"
            | "return"
            | "then"
            | "true"
            | "until"
            | "while"
    )
}

fn tokenize(source: &[u8]) -> Option<Vec<Token>> {
    let mut toks = Vec::new();
    let mut i = 0usize;
    let n = source.len();
    // shebang は PUC と同様に読み飛ばす。
    if n > 0 && source[0] == b'#' {
        while i < n && source[i] != b'\n' {
            i += 1;
        }
    }
    // UTF-8 BOM
    if source.starts_with(b"\xef\xbb\xbf") {
        i = 3;
    }
    while i < n {
        let c = source[i];
        match c {
            b' ' | b'\t' | b'\r' | b'\n' | 0x0b | 0x0c => {
                i += 1;
            }
            b'-' if i + 1 < n && source[i + 1] == b'-' => {
                // コメント
                let mut j = i + 2;
                if j < n && source[j] == b'[' {
                    if let Some(end) = long_bracket_end(source, j) {
                        i = end;
                        continue;
                    }
                }
                while j < n && source[j] != b'\n' {
                    j += 1;
                }
                i = j;
            }
            b'"' | b'\'' => {
                let mut j = i + 1;
                let mut closed = false;
                while j < n {
                    match source[j] {
                        b'\\' => {
                            j += 2;
                        }
                        b'\n' => return None,
                        x if x == c => {
                            j += 1;
                            closed = true;
                            break;
                        }
                        _ => {
                            j += 1;
                        }
                    }
                }
                if !closed {
                    return None;
                }
                toks.push(Token {
                    kind: Tok::Str,
                    start: i,
                    end: j,
                });
                i = j;
            }
            b'[' if long_bracket_end(source, i).is_some() => {
                let end = long_bracket_end(source, i).unwrap_or(i + 1);
                toks.push(Token {
                    kind: Tok::Str,
                    start: i,
                    end,
                });
                i = end;
            }
            b'0'..=b'9' => {
                let mut j = i;
                while j < n {
                    let d = source[j];
                    if d.is_ascii_alphanumeric() || d == b'.' || d == b'_' {
                        j += 1;
                        if (d == b'e' || d == b'E' || d == b'p' || d == b'P')
                            && j < n
                            && (source[j] == b'+' || source[j] == b'-')
                        {
                            j += 1;
                        }
                    } else {
                        break;
                    }
                }
                toks.push(Token {
                    kind: Tok::Number,
                    start: i,
                    end: j,
                });
                i = j;
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'_' | 0x80..=0xff => {
                let mut j = i;
                while j < n {
                    let d = source[j];
                    if d.is_ascii_alphanumeric() || d == b'_' || d >= 0x80 {
                        j += 1;
                    } else {
                        break;
                    }
                }
                let s = std::str::from_utf8(&source[i..j]).ok()?;
                toks.push(Token {
                    kind: if is_keyword(s) {
                        Tok::Keyword
                    } else {
                        Tok::Name
                    },
                    start: i,
                    end: j,
                });
                i = j;
            }
            _ => {
                // 演算子・区切り。多文字演算子を優先する。
                let rest = &source[i..];
                let len = if rest.starts_with(b"...") {
                    3
                } else if rest.starts_with(b"==")
                    || rest.starts_with(b"~=")
                    || rest.starts_with(b"<=")
                    || rest.starts_with(b">=")
                    || rest.starts_with(b"..")
                    || rest.starts_with(b"//")
                    || rest.starts_with(b"<<")
                    || rest.starts_with(b">>")
                    || rest.starts_with(b"::")
                {
                    2
                } else {
                    1
                };
                toks.push(Token {
                    kind: Tok::Op,
                    start: i,
                    end: i + len,
                });
                i += len;
            }
        }
    }
    Some(toks)
}

/// `[` で始まるロングブラケットについて、閉じ終わりの直後を返す。
fn long_bracket_end(source: &[u8], start: usize) -> Option<usize> {
    let n = source.len();
    let mut j = start + 1;
    let mut level = 0usize;
    while j < n && source[j] == b'=' {
        level += 1;
        j += 1;
    }
    if j >= n || source[j] != b'[' {
        return None;
    }
    j += 1;
    if j < n && source[j] == b'\n' {
        j += 1;
    }
    let mut k = j;
    while k < n {
        if source[k] == b']' {
            let mut m = k + 1;
            let mut l = 0usize;
            while m < n && source[m] == b'=' {
                l += 1;
                m += 1;
            }
            if l == level && m < n && source[m] == b']' {
                return Some(m + 1);
            }
            k = m;
        } else {
            k += 1;
        }
    }
    None
}

// ---------------------------------------------------------------------------
// ブロック構造スキャナ

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StmtKind {
    Empty,
    Label,
    Goto,
    LocalDecl,
    LocalFunc,
    FuncStat,
    BlockStmt,
    Return,
    Break,
    Assign,
    Expr,
}

#[derive(Debug, Clone)]
struct Stmt {
    kind: StmtKind,
    /// トークン範囲 [first, last) （toks への添字）
    first: usize,
    last: usize,
    /// Label / Goto の対象名
    name: Option<String>,
    /// LocalDecl / LocalFunc が宣言する名前
    decl_names: Vec<String>,
    /// Assign のターゲット
    targets: Vec<Target>,
    /// Assign の右辺の先頭トークン添字
    rhs_first: Option<usize>,
    /// Assign の右辺要素のトークン範囲（パーサが数えた実績）。
    rhs_elements: Vec<(usize, usize)>,
    /// Assign 文の位置で有効な同一関数内ローカルの数（Q10 の上限判定用。
    /// Assign 以外では未使用）。
    active_locals: usize,
}

impl Stmt {
    fn simple(kind: StmtKind, first: usize, last: usize) -> Self {
        Stmt {
            kind,
            first,
            last,
            name: None,
            decl_names: Vec::new(),
            targets: Vec::new(),
            rhs_first: None,
            rhs_elements: Vec::new(),
            active_locals: 0,
        }
    }
}

#[derive(Debug, Clone)]
enum Target {
    Name {
        name: String,
        /// その名前ターゲット位置での解決先（Q09 の分類機械）。
        /// T06: 素の名前ターゲットが実効 `_ENV` を再束縛するかと、
        /// 大域名ターゲットが環境固定の対象になるかの判定に使う。
        resolved: Resolved,
    },
    Index {
        base_first: usize,
        base_last: usize,
        key: IndexKey,
        /// base が単一の名前トークンで、その束縛が同一関数のローカル
        /// または upvalue のとき true。PUC はその場合 base をストア時点で
        /// 読む（レジスタ／セル）ため、右辺より先に凍結してはならない
        /// （Q09）。鎖の途中やグローバル名の base は左辺走査で実体化
        /// されるため false（凍結が正しい）。
        base_late: bool,
        /// キーが単一の名前トークンで、その束縛が同一関数のローカルの
        /// とき true。PUC はその場合キーをストア時点でレジスタから読む。
        /// upvalue／グローバルのキーは左辺走査でレジスタへ読み出される
        /// ため false（Q09 のキー変形で実測）。
        key_late: bool,
        /// ブラケットキーが（何重かの括弧を剥がして）単一の名前の場合、
        /// その名前トークンの範囲。PUC では括弧付きの単一ローカル読みも
        /// レジスタ参照のままで、評価時点は括弧なしと同一（R03）。
        /// key_late の判定と、スナップショットに使う名前の切り出し、
        /// ターゲット同一性の正規化に使う。
        key_name_span: Option<(usize, usize)>,
    },
}

#[derive(Debug, Clone)]
enum IndexKey {
    Dot(String),
    Bracket(usize, usize),
}

#[derive(Debug)]
struct Block {
    stmts: Vec<Stmt>,
    /// ブロックの終端キーワードのトークン添字。
    last: usize,
    is_repeat: bool,
    until_range: Option<(usize, usize)>,
}

/// パーサが追跡する字句スコープ1段分。`boundary` は関数境界
/// （関数本体ブロックの外側に置かれる引数フレーム）。
struct ScopeFrame {
    boundary: bool,
    names: Vec<String>,
}

/// 名前の解決先。書換え対象の評価時点（Q09）と有効ローカル数（Q10）の
/// 判定に使う、トークンパーサの保守的な近似。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resolved {
    /// 同一関数のローカル（レジスタ）。
    Local,
    /// 外側関数のローカル（upvalue セル）。
    Upvalue,
    /// どの字句スコープにも宣言がない（グローバル）。
    Global,
}

struct Parser<'a> {
    source: &'a [u8],
    toks: &'a [Token],
    blocks: Vec<Block>,
    /// 再帰の深さ（Q11: 深い入れ子でネイティブスタックを溢れさせず、
    /// parse 失敗として書換えを諦めるための上限管理）。ガードが
    /// &mut self のメソッド呼出しと共存できるよう Rc 経由で共有する。
    depth: std::rc::Rc<std::cell::Cell<usize>>,
    /// 字句スコープのスタック（内側が末尾）。
    scopes: Vec<ScopeFrame>,
    /// 次に開くブロックのフレームへ種として渡す名前（for のループ変数）。
    pending_frame_names: Vec<String>,
}

/// 書換えパーサの再帰上限。PUC 自身が受理する入れ子は C 呼出し上限
/// （LUAI_MAXCCALLS=200）のため高々数百段であり、本プロジェクトの
/// AST パーサの上限（128）より緩い。実際のスタック安全域は書き換えを
/// 実行する専用ワーカースレッド（[`REWRITE_STACK_SIZE`]）が担保する
/// ため、呼出側スレッドのスタック量には依存しない（R12 以前は 8MiB
/// スタックでの実測に依存しており、debug＋2MiB で異常終了した）。
const MAX_PARSE_DEPTH: usize = 1000;

struct DepthGuard {
    depth: std::rc::Rc<std::cell::Cell<usize>>,
}

impl Drop for DepthGuard {
    fn drop(&mut self) {
        self.depth.set(self.depth.get() - 1);
    }
}

type PResult<T> = Result<T, ()>;

impl<'a> Parser<'a> {
    fn tok_text(&self, i: usize) -> &'a str {
        if i < self.toks.len() {
            text(self.source, &self.toks[i])
        } else {
            ""
        }
    }

    fn is_kw(&self, i: usize, kw: &str) -> bool {
        i < self.toks.len() && self.toks[i].kind == Tok::Keyword && self.tok_text(i) == kw
    }

    fn is_op(&self, i: usize, op: &str) -> bool {
        i < self.toks.len() && self.toks[i].kind == Tok::Op && self.tok_text(i) == op
    }

    /// 再帰の深さガード（Q11）。上限を超えたら parse 失敗を返し、
    /// 呼出側は書換えを諦めて元のソースをそのまま使う（ロード判定は
    /// バックエンド側が通常のエラーで拒否する）。
    fn enter(&self) -> PResult<DepthGuard> {
        let depth = self.depth.get();
        if depth >= MAX_PARSE_DEPTH {
            return Err(());
        }
        self.depth.set(depth + 1);
        Ok(DepthGuard {
            depth: self.depth.clone(),
        })
    }

    /// 名前を現在の字句スコープで解決する。関数境界を越えた先で見つかれば
    /// upvalue、どこにもなければグローバル。
    fn resolve(&self, name: &str) -> Resolved {
        let mut crossed_boundary = false;
        for frame in self.scopes.iter().rev() {
            if frame.names.iter().any(|n| n == name) {
                return if crossed_boundary {
                    Resolved::Upvalue
                } else {
                    Resolved::Local
                };
            }
            if frame.boundary {
                crossed_boundary = true;
            }
        }
        Resolved::Global
    }

    /// 現在の関数の中で有効なローカルの総数（最も内側の関数境界までの
    /// フレームの合計。境界が無ければチャンク全体）。
    fn active_locals_now(&self) -> usize {
        let mut total = 0;
        for frame in self.scopes.iter().rev() {
            total += frame.names.len();
            if frame.boundary {
                break;
            }
        }
        total
    }

    fn parse_chunk(&mut self) -> PResult<()> {
        let (next, _) = self.parse_block(0, &[])?;
        if next != self.toks.len() {
            return Err(());
        }
        Ok(())
    }

    fn parse_block(&mut self, i: usize, terminators: &[&str]) -> PResult<(usize, usize)> {
        self.parse_block_scoped(i, terminators)
            .map(|(next, block, _frame)| (next, block))
    }

    /// parse_block に加え、外したスコープフレームを返す形。repeat の
    /// until 条件式は本体ブロックのスコープ内で評価されるため、本体
    /// フレームを一時的に戻して条件を読む必要がある（R04）。
    fn parse_block_scoped(
        &mut self,
        i: usize,
        terminators: &[&str],
    ) -> PResult<(usize, usize, ScopeFrame)> {
        let _depth = self.enter()?;
        self.scopes.push(ScopeFrame {
            boundary: false,
            names: std::mem::take(&mut self.pending_frame_names),
        });
        let result = self.parse_block_body(i, terminators);
        let frame = self.scopes.pop().unwrap_or(ScopeFrame {
            boundary: false,
            names: Vec::new(),
        });
        result.map(|(next, block)| (next, block, frame))
    }

    fn parse_block_body(&mut self, mut i: usize, terminators: &[&str]) -> PResult<(usize, usize)> {
        let block_idx = self.blocks.len();
        self.blocks.push(Block {
            stmts: Vec::new(),
            last: i,
            is_repeat: false,
            until_range: None,
        });
        loop {
            if i >= self.toks.len() {
                break;
            }
            if self.toks[i].kind == Tok::Keyword && terminators.contains(&self.tok_text(i)) {
                break;
            }
            let (stmt, next) = self.parse_stmt(i)?;
            if next <= i {
                return Err(());
            }
            // 宣言名はその文より後でのみ可視になるため、文の解析が終わって
            // からフレームへ登録する（LocalFunc は本体解析前に parse_local
            // 側で登録済みのため、ここでは LocalDecl のみ）。
            if stmt.kind == StmtKind::LocalDecl {
                if let Some(frame) = self.scopes.last_mut() {
                    frame.names.extend(stmt.decl_names.iter().cloned());
                }
            }
            self.blocks[block_idx].stmts.push(stmt);
            i = next;
        }
        self.blocks[block_idx].last = i;
        Ok((i, block_idx))
    }

    fn parse_stmt(&mut self, i: usize) -> PResult<(Stmt, usize)> {
        let t = self.toks[i];
        match t.kind {
            Tok::Op if self.tok_text(i) == ";" => {
                Ok((Stmt::simple(StmtKind::Empty, i, i + 1), i + 1))
            }
            Tok::Op if self.tok_text(i) == "::" => {
                if i + 2 < self.toks.len()
                    && self.toks[i + 1].kind == Tok::Name
                    && self.is_op(i + 2, "::")
                {
                    let mut s = Stmt::simple(StmtKind::Label, i, i + 3);
                    s.name = Some(self.tok_text(i + 1).to_string());
                    Ok((s, i + 3))
                } else {
                    Err(())
                }
            }
            Tok::Keyword => match self.tok_text(i) {
                "goto" => {
                    if i + 1 < self.toks.len() && self.toks[i + 1].kind == Tok::Name {
                        let mut s = Stmt::simple(StmtKind::Goto, i, i + 2);
                        s.name = Some(self.tok_text(i + 1).to_string());
                        Ok((s, i + 2))
                    } else {
                        Err(())
                    }
                }
                "break" => Ok((Stmt::simple(StmtKind::Break, i, i + 1), i + 1)),
                "return" => {
                    let end = self.skip_expr_list(i + 1, true)?;
                    Ok((Stmt::simple(StmtKind::Return, i, end), end))
                }
                "local" => self.parse_local(i),
                "function" => {
                    let next = self.parse_function_body_after_name(i)?;
                    Ok((Stmt::simple(StmtKind::FuncStat, i, next), next))
                }
                "do" => {
                    let (term, _) = self.parse_block(i + 1, &["end"])?;
                    if !self.is_kw(term, "end") {
                        return Err(());
                    }
                    Ok((Stmt::simple(StmtKind::BlockStmt, i, term + 1), term + 1))
                }
                "if" => {
                    let mut next = self.skip_expr(i + 1)?;
                    loop {
                        if !self.is_kw(next, "then") {
                            return Err(());
                        }
                        let (term, _) = self.parse_block(next + 1, &["elseif", "else", "end"])?;
                        if self.is_kw(term, "elseif") {
                            next = self.skip_expr(term + 1)?;
                            continue;
                        }
                        if self.is_kw(term, "else") {
                            let (term2, _) = self.parse_block(term + 1, &["end"])?;
                            if !self.is_kw(term2, "end") {
                                return Err(());
                            }
                            return Ok((
                                Stmt::simple(StmtKind::BlockStmt, i, term2 + 1),
                                term2 + 1,
                            ));
                        }
                        if self.is_kw(term, "end") {
                            return Ok((Stmt::simple(StmtKind::BlockStmt, i, term + 1), term + 1));
                        }
                        return Err(());
                    }
                }
                "while" => {
                    let cond_end = self.skip_expr(i + 1)?;
                    if !self.is_kw(cond_end, "do") {
                        return Err(());
                    }
                    let (term, _) = self.parse_block(cond_end + 1, &["end"])?;
                    if !self.is_kw(term, "end") {
                        return Err(());
                    }
                    Ok((Stmt::simple(StmtKind::BlockStmt, i, term + 1), term + 1))
                }
                "for" => {
                    let mut j = i + 1;
                    // ループ変数名を集めておき、本体ブロックのフレームに種として
                    // 渡す（本体内の名前解決でループ変数が見えるように）。
                    let mut loop_vars: Vec<String> = Vec::new();
                    // 名前（＋数値 for の = ）／ in の前まで進め、残りは式リストとして読む
                    loop {
                        if j >= self.toks.len() {
                            return Err(());
                        }
                        if self.is_kw(j, "do") {
                            break;
                        }
                        if self.is_kw(j, "in") {
                            j += 1;
                            // イテレータ式リスト
                            loop {
                                if self.is_kw(j, "do") {
                                    break;
                                }
                                j = self.skip_expr(j)?;
                                if self.is_op(j, ",") {
                                    j += 1;
                                    continue;
                                }
                                if self.is_kw(j, "do") {
                                    break;
                                }
                                return Err(());
                            }
                            break;
                        }
                        if self.toks[j].kind == Tok::Name
                            || self.is_op(j, ",")
                            || self.is_op(j, "=")
                        {
                            if self.toks[j].kind == Tok::Name {
                                loop_vars.push(self.tok_text(j).to_string());
                            }
                            if self.is_op(j, "=") {
                                // 数値 for の式リスト
                                j += 1;
                                loop {
                                    if self.is_kw(j, "do") {
                                        break;
                                    }
                                    j = self.skip_expr(j)?;
                                    if self.is_op(j, ",") {
                                        j += 1;
                                        continue;
                                    }
                                    if self.is_kw(j, "do") {
                                        break;
                                    }
                                    return Err(());
                                }
                                break;
                            }
                            j += 1;
                            continue;
                        }
                        return Err(());
                    }
                    if !self.is_kw(j, "do") {
                        return Err(());
                    }
                    // PUC は for の隠れローカルも有効ローカル数に数える:
                    // 数値 for は制御変数のほかに3つ（index/limit/step）、
                    // generic for は宣言名のほかに3つ（iterator/state/
                    // control）。代替ロード判定の会計（load_legality の
                    // Q13 修正）と揃える（R05）。識別子として現れ得ない
                    // 文字列で枠だけを確保し、名前解決には一致させない。
                    loop_vars.extend((0..3).map(|_| " stormvm for-hidden".to_string()));
                    self.pending_frame_names = loop_vars;
                    let (term, _) = self.parse_block(j + 1, &["end"])?;
                    if !self.is_kw(term, "end") {
                        return Err(());
                    }
                    Ok((Stmt::simple(StmtKind::BlockStmt, i, term + 1), term + 1))
                }
                "repeat" => {
                    let (term, bidx, frame) = self.parse_block_scoped(i + 1, &["until"])?;
                    if !self.is_kw(term, "until") {
                        return Err(());
                    }
                    let cond_start = term + 1;
                    // until 条件式は本体ブロックのスコープ内で評価される
                    // （PUC）。本体フレームを戻した状態で条件を読むこと
                    // で、条件式内の関数が捕捉する本体ローカルが upvalue
                    // として解決される（R04: フレームを外したままだと
                    // 大域名として扱われ、根底のローカルと異なる時点
                    // 分類になっていた）。
                    self.scopes.push(frame);
                    let cond = self.skip_expr(cond_start);
                    self.scopes.pop();
                    let next = cond?;
                    self.blocks[bidx].is_repeat = true;
                    self.blocks[bidx].until_range = Some((cond_start, next));
                    self.blocks[bidx].last = next;
                    Ok((Stmt::simple(StmtKind::BlockStmt, i, next), next))
                }
                _ => Err(()),
            },
            Tok::Name | Tok::Op => self.parse_expr_stmt(i),
            _ => Err(()),
        }
    }

    fn parse_local(&mut self, i: usize) -> PResult<(Stmt, usize)> {
        let mut s = Stmt::simple(StmtKind::LocalDecl, i, i + 1);
        let mut j = i + 1;
        if self.is_kw(j, "function") {
            if j + 1 >= self.toks.len() || self.toks[j + 1].kind != Tok::Name {
                return Err(());
            }
            s.kind = StmtKind::LocalFunc;
            s.decl_names.push(self.tok_text(j + 1).to_string());
            // ローカル関数名は自身の本体の中から（upvalue として）可視な
            // ため、本体の解析前にフレームへ登録する。
            let func_name = self.tok_text(j + 1).to_string();
            if let Some(frame) = self.scopes.last_mut() {
                frame.names.push(func_name);
            }
            let next = self.parse_function_body(j + 2, false)?;
            s.last = next;
            return Ok((s, next));
        }
        loop {
            if j >= self.toks.len() || self.toks[j].kind != Tok::Name {
                return Err(());
            }
            s.decl_names.push(self.tok_text(j).to_string());
            j += 1;
            if self.is_op(j, ",") {
                j += 1;
                continue;
            }
            break;
        }
        if self.is_op(j, "=") {
            let next = self.skip_expr_list(j + 1, false)?;
            s.last = next;
            return Ok((s, next));
        }
        s.last = j;
        Ok((s, j))
    }

    fn parse_function_body_after_name(&mut self, i: usize) -> PResult<usize> {
        let mut j = i + 1;
        if j >= self.toks.len() || self.toks[j].kind != Tok::Name {
            return Err(());
        }
        j += 1;
        let mut method = false;
        loop {
            if self.is_op(j, ".") {
                if j + 1 >= self.toks.len() || self.toks[j + 1].kind != Tok::Name {
                    return Err(());
                }
                j += 2;
                continue;
            }
            if self.is_op(j, ":") {
                if j + 1 >= self.toks.len() || self.toks[j + 1].kind != Tok::Name {
                    return Err(());
                }
                method = true;
                j += 2;
            }
            break;
        }
        self.parse_function_body(j, method)
    }

    /// 引数リスト `(` から本体終わり（end の直後）まで。
    fn parse_function_body(&mut self, i: usize, method: bool) -> PResult<usize> {
        if !self.is_op(i, "(") {
            return Err(());
        }
        let mut j = i;
        let mut depth = 0i32;
        loop {
            if j >= self.toks.len() {
                return Err(());
            }
            if self.is_op(j, "(") {
                depth += 1;
            } else if self.is_op(j, ")") {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            j += 1;
        }
        // 引数名（とメソッドの暗黙 self）を関数境界フレームとして積んで
        // から本体を解析する: 本体内の名前解決がこの境界で区切られる。
        let mut params: Vec<String> = (i + 1..j)
            .filter(|&k| self.toks[k].kind == Tok::Name)
            .map(|k| self.tok_text(k).to_string())
            .collect();
        if method {
            params.insert(0, "self".to_string());
        }
        self.scopes.push(ScopeFrame {
            boundary: true,
            names: params,
        });
        let body = self.parse_block(j + 1, &["end"]);
        self.scopes.pop();
        let (term, _) = body?;
        if !self.is_kw(term, "end") {
            return Err(());
        }
        Ok(term + 1)
    }

    fn parse_function_expr(&mut self, i: usize) -> PResult<usize> {
        self.parse_function_body(i + 1, false)
    }

    fn parse_expr_stmt(&mut self, i: usize) -> PResult<(Stmt, usize)> {
        let mut targets: Vec<Target> = Vec::new();
        let mut j = i;
        loop {
            let (target, next, had_call) = self.parse_target(j)?;
            j = next;
            if had_call {
                let end = self.skip_expr_rest(j)?;
                return Ok((Stmt::simple(StmtKind::Expr, i, end), end));
            }
            targets.push(target);
            if self.is_op(j, ",") {
                j += 1;
                continue;
            }
            break;
        }
        if self.is_op(j, "=") && !self.is_op(j + 1, "=") {
            let rhs_first = j + 1;
            let end = self.skip_expr_list(rhs_first, false)?;
            // 右辺要素の範囲をここで数える（呼出引数のリスト走査と
            // 状態を共有しないため、skip_expr_list の戻りからは得ない）。
            let mut rhs_elements = Vec::new();
            {
                let mut at = rhs_first;
                loop {
                    let start = at;
                    at = self.skip_expr(at)?;
                    rhs_elements.push((start, at));
                    if self.is_op(at, ",") {
                        at += 1;
                        continue;
                    }
                    break;
                }
            }
            let mut s = Stmt::simple(StmtKind::Assign, i, end);
            s.targets = targets;
            s.rhs_first = Some(rhs_first);
            s.rhs_elements = rhs_elements;
            s.active_locals = self.active_locals_now();
            return Ok((s, end));
        }
        // 代入でも呼出でもない形（不正な Lua）。変換対象外。
        let end = self.skip_expr_rest(j)?;
        Ok((Stmt::simple(StmtKind::Expr, i, end), end))
    }

    /// 呼出サフィックスを持たない prefix 式を読む。3番目は呼出が現れたか。
    fn parse_target(&mut self, i: usize) -> PResult<(Target, usize, bool)> {
        let mut j = i;
        if j >= self.toks.len() {
            return Err(());
        }
        let mut current: Option<Target> = None;
        let mut base_span: (usize, usize);
        if self.toks[j].kind == Tok::Name {
            let name = self.tok_text(j).to_string();
            let resolved = self.resolve(&name);
            current = Some(Target::Name { name, resolved });
            base_span = (j, j + 1);
            j += 1;
        } else if self.is_op(j, "(") {
            let end = self.skip_balanced_parens(j)?;
            base_span = (i, end);
            j = end;
        } else {
            return Err(());
        }
        loop {
            if self.is_op(j, ".") {
                if j + 1 >= self.toks.len() || self.toks[j + 1].kind != Tok::Name {
                    return Err(());
                }
                let key = self.tok_text(j + 1).to_string();
                current = Some(Target::Index {
                    base_first: base_span.0,
                    base_last: base_span.1,
                    key: IndexKey::Dot(key),
                    base_late: false,
                    key_late: false,
                    key_name_span: None,
                });
                base_span = (base_span.0, j + 2);
                j += 2;
                continue;
            }
            if self.is_op(j, "[") {
                let key_end = self.skip_expr(j + 1)?;
                if !self.is_op(key_end, "]") {
                    return Err(());
                }
                current = Some(Target::Index {
                    base_first: base_span.0,
                    base_last: base_span.1,
                    key: IndexKey::Bracket(j + 1, key_end),
                    base_late: false,
                    key_late: false,
                    key_name_span: None,
                });
                base_span = (base_span.0, key_end + 1);
                j = key_end + 1;
                continue;
            }
            if self.is_op(j, ":")
                || self.is_op(j, "(")
                || self.is_op(j, "{")
                || (j < self.toks.len() && self.toks[j].kind == Tok::Str)
            {
                // 呼出サフィックス
                let placeholder = current.unwrap_or(Target::Name {
                    name: String::new(),
                    resolved: Resolved::Global,
                });
                return Ok((placeholder, j, true));
            }
            break;
        }
        match current {
            Some(mut t) => {
                // 最終形について Q09 の評価時点分類を行う。
                let (base_late, key_late, key_name_span) = match &t {
                    Target::Index {
                        base_first,
                        base_last,
                        key,
                        ..
                    } => {
                        let base_name = if *base_last == *base_first + 1
                            && self.toks[*base_first].kind == Tok::Name
                        {
                            Some(self.tok_text(*base_first))
                        } else {
                            None
                        };
                        let base_late = match base_name {
                            Some(name) => {
                                let resolved = self.resolve(name);
                                matches!(resolved, Resolved::Local | Resolved::Upvalue)
                                    // 暗黙の `_ENV` base は環境 upvalue
                                    // そのものであり、PUC はストア時点で
                                    // 読む（T06 形状B）。ローカルの
                                    // `_ENV` で覆われている場合は通常の
                                    // ローカル base として既に late。
                                    || (name == "_ENV" && resolved == Resolved::Global)
                            }
                            None => false,
                        };
                        // 括弧付きキー `(k)` も、PUC では括弧なしと同じく
                        // レジスタ参照のままでストア時点に読まれる（R03）。
                        // 括弧を剥がして単一の名前になれば、その名前で分類する。
                        let key_name_span = match key {
                            IndexKey::Bracket(f, l) => {
                                stripped_single_name(self.source, self.toks, *f, *l)
                            }
                            IndexKey::Dot(_) => None,
                        };
                        let key_late = match key_name_span {
                            Some((f, _)) => self.resolve(self.tok_text(f)) == Resolved::Local,
                            None => false,
                        };
                        (base_late, key_late, key_name_span)
                    }
                    Target::Name { .. } => (false, false, None),
                };
                if let Target::Index {
                    base_late: b,
                    key_late: k,
                    key_name_span: s,
                    ..
                } = &mut t
                {
                    *b = base_late;
                    *k = key_late;
                    *s = key_name_span;
                }
                Ok((t, j, false))
            }
            None => Err(()),
        }
    }

    fn skip_balanced_parens(&mut self, i: usize) -> PResult<usize> {
        let mut depth = 0i32;
        let mut j = i;
        loop {
            if j >= self.toks.len() {
                return Err(());
            }
            if self.is_kw(j, "function") {
                j = self.parse_function_expr(j)?;
                continue;
            }
            if self.is_op(j, "(") {
                depth += 1;
            } else if self.is_op(j, ")") {
                depth -= 1;
                if depth == 0 {
                    return Ok(j + 1);
                }
            }
            j += 1;
        }
    }

    fn skip_expr(&mut self, i: usize) -> PResult<usize> {
        let j = self.skip_prefix_atom(i)?;
        self.skip_expr_rest(j)
    }

    /// 単項演算子の列＋atom を読む。
    fn skip_prefix_atom(&mut self, mut i: usize) -> PResult<usize> {
        loop {
            if self.is_op(i, "-")
                || self.is_op(i, "#")
                || self.is_op(i, "~")
                || self.is_kw(i, "not")
            {
                i += 1;
                continue;
            }
            break;
        }
        self.skip_atom(i)
    }

    fn skip_expr_rest(&mut self, mut j: usize) -> PResult<usize> {
        loop {
            if j >= self.toks.len() {
                return Ok(j);
            }
            let t = self.toks[j];
            match t.kind {
                Tok::Op => match self.tok_text(j) {
                    "+" | "-" | "*" | "/" | "//" | "%" | "^" | ".." | "==" | "~=" | "<=" | ">="
                    | "<" | ">" | "&" | "|" | "~" | "<<" | ">>" => {
                        j = self.skip_binary_rhs(j + 1)?;
                    }
                    "." => {
                        if j + 1 >= self.toks.len() || self.toks[j + 1].kind != Tok::Name {
                            return Err(());
                        }
                        j += 2;
                    }
                    ":" => {
                        if j + 1 >= self.toks.len() || self.toks[j + 1].kind != Tok::Name {
                            return Err(());
                        }
                        j += 2;
                        j = self.skip_call_args(j)?;
                    }
                    "[" => {
                        let end = self.skip_expr(j + 1)?;
                        if !self.is_op(end, "]") {
                            return Err(());
                        }
                        j = end + 1;
                    }
                    "(" => {
                        j = self.skip_call_args(j)?;
                    }
                    "{" => {
                        j = self.skip_constructor(j)?;
                    }
                    _ => return Ok(j),
                },
                Tok::Str => {
                    j = self.skip_call_args(j)?;
                }
                Tok::Keyword => match self.tok_text(j) {
                    "and" | "or" => {
                        j = self.skip_binary_rhs(j + 1)?;
                    }
                    _ => return Ok(j),
                },
                _ => return Ok(j),
            }
        }
    }

    /// 二項演算子の右辺（単項＋atom まで。後続サフィックスは呼び出し側ループが処理する）。
    fn skip_binary_rhs(&mut self, i: usize) -> PResult<usize> {
        self.skip_prefix_atom(i)
    }

    fn skip_call_args(&mut self, j: usize) -> PResult<usize> {
        let _depth = self.enter()?;
        if self.is_op(j, "(") {
            if self.is_op(j + 1, ")") {
                return Ok(j + 2);
            }
            let end = self.skip_expr_list(j + 1, false)?;
            if !self.is_op(end, ")") {
                return Err(());
            }
            Ok(end + 1)
        } else if j < self.toks.len() && self.toks[j].kind == Tok::Str {
            Ok(j + 1)
        } else if self.is_op(j, "{") {
            self.skip_constructor(j)
        } else {
            Err(())
        }
    }

    fn skip_constructor(&mut self, i: usize) -> PResult<usize> {
        let mut j = i + 1;
        loop {
            if j >= self.toks.len() {
                return Err(());
            }
            if self.is_op(j, "}") {
                return Ok(j + 1);
            }
            if self.is_op(j, ",") || self.is_op(j, ";") {
                j += 1;
                continue;
            }
            if self.is_op(j, "[") {
                // [ expr ] = expr
                let key_end = self.skip_expr(j + 1)?;
                if !self.is_op(key_end, "]") {
                    return Err(());
                }
                j = key_end + 1;
                if self.is_op(j, "=") {
                    j = self.skip_expr(j + 1)?;
                }
                continue;
            }
            let next = self.skip_expr(j)?;
            j = next;
            if self.is_op(j, "=") {
                j = self.skip_expr(j + 1)?;
            }
        }
    }

    fn skip_atom(&mut self, i: usize) -> PResult<usize> {
        // 式の入れ子（括弧・コンストラクタ・単項演算子列）はここを再帰する
        // ため、深さガードを入口に置く（Q11）。
        let _depth = self.enter()?;
        if i >= self.toks.len() {
            return Err(());
        }
        let t = self.toks[i];
        match t.kind {
            Tok::Number | Tok::Str | Tok::Name => Ok(i + 1),
            Tok::Keyword => match self.tok_text(i) {
                "nil" | "true" | "false" => Ok(i + 1),
                "function" => self.parse_function_expr(i),
                _ => Err(()),
            },
            Tok::Op => match self.tok_text(i) {
                "..." => Ok(i + 1),
                "(" => {
                    let end = self.skip_expr(i + 1)?;
                    if !self.is_op(end, ")") {
                        return Err(());
                    }
                    Ok(end + 1)
                }
                "{" => self.skip_constructor(i),
                "-" | "#" | "~" => self.skip_prefix_atom(i + 1),
                _ => Err(()),
            },
        }
    }

    /// カンマ区切りの式リストを読み飛ばす。allow_empty=true の場合は
    /// 文開始・終端トークンで空リストとして止まる（return 用）。
    fn skip_expr_list(&mut self, mut i: usize, allow_empty: bool) -> PResult<usize> {
        if allow_empty {
            if i >= self.toks.len() {
                return Ok(i);
            }
            if self.is_op(i, ";") || self.is_op(i, "::") {
                return Ok(i);
            }
            if self.toks[i].kind == Tok::Keyword
                && matches!(
                    self.tok_text(i),
                    "end"
                        | "else"
                        | "elseif"
                        | "until"
                        | "local"
                        | "if"
                        | "while"
                        | "do"
                        | "for"
                        | "repeat"
                        | "return"
                        | "break"
                        | "goto"
                        | "then"
                )
            {
                return Ok(i);
            }
        }
        loop {
            i = self.skip_expr(i)?;
            if self.is_op(i, ",") {
                i += 1;
                continue;
            }
            return Ok(i);
        }
    }
}

// ---------------------------------------------------------------------------
// (b) goto の包み

fn find_goto_wrap(source: &[u8], toks: &[Token], blocks: &[Block]) -> Option<(usize, usize)> {
    for block in blocks {
        let stmts = &block.stmts;
        for (gi, g) in stmts.iter().enumerate() {
            if g.kind != StmtKind::Goto {
                continue;
            }
            // 候補M2: この goto が包みの対象でなくても走査全体を打ち切らず、
            // 次の goto を調べる（打ち切るとチャンク全体で最大1回しか包めない）。
            let Some(label_name) = g.name.as_ref() else {
                continue;
            };
            let Some(li) = stmts.iter().position(|s| {
                s.kind == StmtKind::Label && s.name.as_deref() == Some(label_name.as_str())
            }) else {
                continue;
            };
            if li <= gi {
                continue;
            }
            let mut crossed_names: HashSet<&str> = HashSet::new();
            let mut first_crossed: Option<&Stmt> = None;
            for s in &stmts[gi + 1..li] {
                if s.kind == StmtKind::LocalDecl || s.kind == StmtKind::LocalFunc {
                    if first_crossed.is_none() {
                        first_crossed = Some(s);
                    }
                    for n in &s.decl_names {
                        crossed_names.insert(n.as_str());
                    }
                }
            }
            let Some(first_crossed) = first_crossed else {
                continue;
            };
            let label_stmt = &stmts[li];
            // ラベル以降（ブロック終端＋until 条件）に交差名への参照がないこと
            let scan_from = label_stmt.last;
            let scan_to = if block.is_repeat {
                block.until_range.map(|(_, e)| e).unwrap_or(block.last)
            } else {
                block.last
            };
            // goto より前の文で宣言された同一ブロックのローカルは、ラベル
            // 以降でもその束縛を読むため、暗黙の _ENV 参照にはならない。
            let predeclared: HashSet<&str> = stmts[..gi]
                .iter()
                .flat_map(|s| s.decl_names.iter().map(String::as_str))
                .collect();
            if referenced_after(
                source,
                toks,
                &crossed_names,
                &predeclared,
                scan_from,
                scan_to,
            ) {
                continue;
            }
            // 区間内のラベルを区間外から参照する goto がないこと
            let seg_names: HashSet<&str> = stmts[gi + 1..li]
                .iter()
                .filter(|s| s.kind == StmtKind::Label)
                .filter_map(|s| s.name.as_deref())
                .collect();
            if !seg_names.is_empty()
                && goto_outside_segment(
                    source,
                    toks,
                    &seg_names,
                    first_crossed.first,
                    label_stmt.first,
                )
            {
                continue;
            }
            let insert_do_at = toks[first_crossed.first].start;
            let insert_end_at = toks[label_stmt.first].start;
            return Some((insert_do_at, insert_end_at));
        }
    }
    None
}

/// [from, to) のトークン範囲で names のいずれかへの参照があるか。
/// names に _ENV が含まれる場合は、predeclared にない自由名も暗黙の
/// _ENV 参照として数える。
/// フィールド名（.name / :name）、ラベル名、goto 先、ローカル宣言の名前位置は
/// 参照と数えない。それ以外はすべて参照（安全側）と数える。
fn referenced_after(
    source: &[u8],
    toks: &[Token],
    names: &HashSet<&str>,
    predeclared: &HashSet<&str>,
    from: usize,
    to: usize,
) -> bool {
    // 交差ローカルに _ENV が含まれる場合、ラベル以降の自由名参照は
    // 暗黙の _ENV 参照である（グローバルアクセスは _ENV 経由）。
    // ただし goto 以前に宣言された同一ブロックのローカルは除く。
    // 外側スコープのローカルや引数へ解決される名前は安全側として
    // 参照に数える（包まない判断は素の Lua 5.3 の挙動を保つ）。
    let crossed_env = names.contains("_ENV");
    let mut i = from;
    while i < to && i < toks.len() {
        let t = &toks[i];
        if t.kind == Tok::Keyword {
            match text(source, t) {
                "goto" => {
                    i += 2;
                    continue;
                }
                "local" => {
                    // 宣言名は読み飛ばす。初期化子は通常走査する。
                    i += 1;
                    if i < to
                        && toks[i].kind == Tok::Keyword
                        && text(source, &toks[i]) == "function"
                    {
                        i += 2; // function とその名前
                        continue;
                    }
                    while i < to && toks[i].kind == Tok::Name {
                        i += 1;
                        if i < to && toks[i].kind == Tok::Op && text(source, &toks[i]) == "," {
                            i += 1;
                            continue;
                        }
                        break;
                    }
                    continue;
                }
                _ => {
                    i += 1;
                    continue;
                }
            }
        }
        if t.kind == Tok::Op && text(source, t) == "::" {
            // ラベル定義 :: name ::
            i += 3;
            continue;
        }
        if t.kind == Tok::Name {
            let name = text(source, t);
            if names.contains(name) || (crossed_env && !predeclared.contains(name)) {
                if i > 0 {
                    let prev = &toks[i - 1];
                    if prev.kind == Tok::Op
                        && (text(source, prev) == "." || text(source, prev) == ":")
                    {
                        i += 1;
                        continue;
                    }
                }
                return true;
            }
        }
        i += 1;
    }
    false
}

/// 区間 [seg_first, seg_last) の外側にある goto が seg_names のラベルを指すか。
fn goto_outside_segment(
    source: &[u8],
    toks: &[Token],
    seg_names: &HashSet<&str>,
    seg_first: usize,
    seg_last: usize,
) -> bool {
    let mut i = 0usize;
    while i + 1 < toks.len() {
        if toks[i].kind == Tok::Keyword
            && text(source, &toks[i]) == "goto"
            && toks[i + 1].kind == Tok::Name
            && (i < seg_first || i >= seg_last)
            && seg_names.contains(text(source, &toks[i + 1]))
        {
            return true;
        }
        i += 1;
    }
    false
}

// ---------------------------------------------------------------------------
// (a) 多重代入の書き換え

fn target_identity(source: &[u8], toks: &[Token], t: &Target) -> String {
    match t {
        Target::Name { name: n, .. } => format!("name:{n}"),
        Target::Index {
            base_first,
            base_last,
            key,
            ..
        } => {
            let mut base = String::new();
            for t in &toks[*base_first..*base_last] {
                base.push_str(text(source, t));
                base.push('\u{1}');
            }
            let key_id = match key {
                IndexKey::Dot(n) => format!("dot:{n}"),
                IndexKey::Bracket(f, l) => {
                    // 括弧の有無は対象の同一性に影響しない（R03）:
                    // `t[k]` と `t[(k)]` は同じ場所を指すため、単一の
                    // 名前キー（括弧剥がし後）は名前そのもので同一視する。
                    // 実行時の == ガードが最終的な同一性を裁定する。
                    if let Some((nf, _)) = stripped_single_name(source, toks, *f, *l) {
                        format!("brname:{}", text(source, &toks[nf]))
                    } else {
                        let mut s = String::new();
                        for t in &toks[*f..*l] {
                            s.push_str(text(source, t));
                            s.push('\u{1}');
                        }
                        format!("br:{s}")
                    }
                }
            };
            format!("index:{base}|{key_id}")
        }
    }
}

fn collect_assign_edits(
    source: &[u8],
    toks: &[Token],
    blocks: &[Block],
    all_names: &HashSet<&str>,
) -> Vec<(usize, usize, Vec<u8>)> {
    // 一時変数名: 生成する名前そのもの（アドレス表と値表の2つ）が
    // チャンク内のどの識別子とも衝突しないよう、接頭辞を伸ばす。
    // 検査対象は生成名そのものであり、範囲の上限は設けない
    // （Codex 第3回 #13）。
    let max_targets = blocks
        .iter()
        .flat_map(|block| &block.stmts)
        .map(|stmt| stmt.targets.len())
        .max()
        .unwrap_or(0);
    let mut prefix = "__stormvm_t".to_string();
    while all_names.contains(format!("{prefix}a").as_str())
        || all_names.contains(format!("{prefix}v").as_str())
        || (1..=max_targets).any(|i| all_names.contains(format!("{prefix}s{i}").as_str()))
    {
        prefix.push('_');
    }
    let addr_temp = format!("{prefix}a");
    let value_temp = format!("{prefix}v");
    let mut edits = Vec::new();
    for block in blocks {
        for stmt in &block.stmts {
            if stmt.kind != StmtKind::Assign || stmt.targets.len() < 2 {
                continue;
            }
            let Some(rhs_first) = stmt.rhs_first else {
                continue;
            };
            let identities: Vec<String> = stmt
                .targets
                .iter()
                .map(|t| target_identity(source, toks, t))
                .collect();
            let mut seen: HashSet<String> = HashSet::new();
            let mut has_dup = false;
            for id in &identities {
                if !seen.insert(id.clone()) {
                    has_dup = true;
                }
            }
            if !has_dup {
                continue;
            }
            // Index の base は Name 起点チェーンに限る（括弧式 base は不採用）
            let mut ok = true;
            for t in &stmt.targets {
                if let Target::Index { base_first, .. } = t {
                    if toks[*base_first].kind != Tok::Name {
                        ok = false;
                    }
                }
            }
            if !ok {
                continue;
            }
            let stmt_start = toks[stmt.first].start;
            let stmt_end = toks[stmt.last - 1].end;
            // 足場の設計（Codex 第3回 #12）: 置換は do ブロックで囲い、
            // 一時変数はアドレス表と値表の最大2ローカルに抑える。囲まない
            // 素朴な形では、一時ローカルがブロック末尾まで残って後続の
            // goto がそのスコープへ跳ぶ形になったり、文が並ぶほど同時
            // ローカル数が 200 上限を圧迫したりする。アドレスのうち
            // PUC が左辺走査で実体化する部分は右辺より先に出現順で評価
            // する（PUC の評価順。遅延読みの部分は Q09 の設計参照）。
            // 置換テキストはバイト列として構築し、コピーする断片（右辺・
            // base・キー）は元のバイト列そのものを使う（Codex 第3回 #9）。
            // 評価時点の設計（Q09）: PUC が左辺走査で実体化する部分
            // （複合 base/キー、グローバル名、upvalue のキー）だけを右辺
            // より先にアドレス表へ凍結する。同一関数のローカルである
            // base/キーと upvalue の base は PUC がストア時点で読むため
            // 凍結せず、ストア直前に同じスロットへスナップショットする。
            // ドットキーは定数のためスロットを使わない。
            // 例外（R02）: 遅延読みのはずの base／キー名が、同一文の
            // 後続の素の名前ターゲットでも代入される場合、PUC は左辺
            // 走査の時点でその名前を退避する（check_conflict 相当）。
            // ストアは逆順に行われるため、退避しなければ再束縛後の
            // 値でストアしてしまう。その部分は「競合する名前ターゲット
            // の走査時点」に位置づけた凍結として右辺より先に評価する。
            // R03: 括弧付きの単一ローカルキー `(k)` も括弧なしと同じ
            // 遅延読みであり、スナップショットは内側の名前で行う。
            struct TargetPlan {
                /// Name ターゲットのストア先テキスト。
                name: Option<String>,
                /// Index ターゲットのペア番号（1始まり）。
                pair: usize,
                /// ストア直前にスナップショットする base 名（遅延読み）。
                late_base: Option<Vec<u8>>,
                /// ストア直前にスナップショットするキー名（遅延読み）。
                late_key: Option<Vec<u8>>,
                /// ドットキーの名前（アクセサは文字列リテラルになる）。
                dot_key: Option<String>,
                /// 重複の省略を静的に行えるか。名前ターゲットは場所が
                /// 変数そのもので静的に決まる。T06 形状A で Index 化した
                /// 大域名ターゲットも、base が凍結した同一の環境表で
                /// キーが文字列定数のため場所は静的に同一と証明できる。
                /// 通常の Index ターゲットは実行時ガードで裁定する。
                static_dedup: bool,
            }
            // 同一文内で `name` を代入する素の名前ターゲットのうち、
            // 位置 `from` 以降で最初に現れるものの位置（R02 の競合判定）。
            let conflict_target = |from: usize, name: &str| -> Option<usize> {
                stmt.targets[from..]
                    .iter()
                    .position(|u| matches!(u, Target::Name { name: n, .. } if n == name))
                    .map(|off| from + off)
            };
            // T06 形状A: 素の名前ターゲットが実効 `_ENV`（ローカルの
            // `_ENV` で覆われていない大域名／upvalue の `_ENV`）を再束縛
            // する文では、PUC は競合回避のため、その文の大域名ストアの
            // 環境を左辺走査時点の `_ENV` へ固定する（check_conflict
            // 相当）。大域名ターゲットは凍結した `_ENV` を base とする
            // Index ストアとして計画する。
            let env_rebind = stmt.targets.iter().any(|t| {
                matches!(t, Target::Name { name, resolved }
                    if name == "_ENV" && *resolved != Resolved::Local)
            });
            let mut plans: Vec<TargetPlan> = Vec::new();
            // 右辺より先に評価する凍結代入列。各要素は
            // (並びキー主: 左辺走査の位置, 並びキー副, スロット番号, 式)。
            // 通常の凍結は自ターゲットの位置（base=副0、キー=副1）に、
            // R02 の競合凍結は競合する名前ターゲットの位置（副2）に
            // 位置づける — PUC が左辺走査中に退避する順序そのもの。
            let mut frozen_fills: Vec<(usize, usize, usize, Vec<u8>)> = Vec::new();
            let mut pair_counter = 0usize;
            for (ti, t) in stmt.targets.iter().enumerate() {
                match t {
                    Target::Name { name, resolved } => {
                        if env_rebind && *resolved == Resolved::Global && name != "_ENV" {
                            pair_counter += 1;
                            let p = pair_counter;
                            frozen_fills.push((ti, 0, 2 * p - 1, b"_ENV".to_vec()));
                            plans.push(TargetPlan {
                                name: None,
                                pair: p,
                                late_base: None,
                                late_key: None,
                                dot_key: Some(name.clone()),
                                static_dedup: true,
                            });
                        } else {
                            plans.push(TargetPlan {
                                name: Some(name.clone()),
                                pair: 0,
                                late_base: None,
                                late_key: None,
                                dot_key: None,
                                static_dedup: true,
                            });
                        }
                    }
                    Target::Index {
                        base_first,
                        base_last,
                        key,
                        base_late,
                        key_late,
                        key_name_span,
                    } => {
                        pair_counter += 1;
                        let p = pair_counter;
                        let late_base = if *base_late {
                            // base_late の base は単一の名前トークン。
                            let base_name = text(source, &toks[*base_first]);
                            match conflict_target(ti + 1, base_name) {
                                // 同一文の後続で base 自身が代入される:
                                // PUC は左辺走査で退避する（R02）。
                                Some(j) => {
                                    frozen_fills.push((
                                        j,
                                        2,
                                        2 * p - 1,
                                        slice_tokens(source, toks, *base_first, *base_last),
                                    ));
                                    None
                                }
                                None => Some(slice_tokens(source, toks, *base_first, *base_last)),
                            }
                        } else {
                            frozen_fills.push((
                                ti,
                                0,
                                2 * p - 1,
                                slice_tokens(source, toks, *base_first, *base_last),
                            ));
                            None
                        };
                        let (late_key, dot_key) = match key {
                            IndexKey::Dot(nm) => (None, Some(nm.clone())),
                            IndexKey::Bracket(f, l) => {
                                if *key_late {
                                    // key_late のとき key_name_span は必ず
                                    // Some（分類側の不変条件）。スナップ
                                    // ショットは括弧の内側の名前で行う。
                                    let (nf, nl) = key_name_span.unwrap_or((*f, *l));
                                    let key_name = text(source, &toks[nf]);
                                    match conflict_target(ti + 1, key_name) {
                                        Some(j) => {
                                            frozen_fills.push((
                                                j,
                                                2,
                                                2 * p,
                                                slice_tokens(source, toks, nf, nl),
                                            ));
                                            (None, None)
                                        }
                                        None => (Some(slice_tokens(source, toks, nf, nl)), None),
                                    }
                                } else {
                                    frozen_fills.push((
                                        ti,
                                        1,
                                        2 * p,
                                        slice_tokens(source, toks, *f, *l),
                                    ));
                                    (None, None)
                                }
                            }
                        };
                        plans.push(TargetPlan {
                            name: None,
                            pair: p,
                            late_base,
                            late_key,
                            dot_key,
                            static_dedup: false,
                        });
                    }
                }
            }
            // 左辺走査順に安定ソートする（同キーは計画順 = ターゲット順）。
            frozen_fills.sort_by_key(|(major, minor, _, _)| (*major, *minor));
            // ストア先テキストと、重複判定ガードで使う (base, キー) アクセサ。
            let base_accessor = |p: usize| format!("{addr_temp}[{}]", 2 * p - 1);
            let key_accessor = |plan: &TargetPlan| -> String {
                match &plan.dot_key {
                    Some(nm) => format!("\"{nm}\""),
                    None => format!("{addr_temp}[{}]", 2 * plan.pair),
                }
            };
            let store_text = |plan: &TargetPlan| -> String {
                match &plan.name {
                    Some(name) => name.clone(),
                    None => format!("{}[{}]", base_accessor(plan.pair), key_accessor(plan)),
                }
            };
            // 右辺の要素範囲はパーサが数えた実績を使う（関数本体内の
            // カンマ等で誤分割しない）。得られなければ文全体を1要素
            // として扱い、逐次形は選ばれない。
            let rhs_elements: Vec<(usize, usize)> = if stmt.rhs_elements.is_empty() {
                vec![(rhs_first, stmt.last)]
            } else {
                stmt.rhs_elements.clone()
            };
            // 要素ごとのレジスタ幅の近似: 各括弧グループの最上位要素数
            // に祖先グループの段数を加えた最大値（呼出の引数レジスタ等）。
            let element_width = |range: &(usize, usize)| -> usize {
                let mut best = 1usize;
                let mut stack: Vec<usize> = Vec::new();
                for t in &toks[range.0..range.1] {
                    if t.kind != Tok::Op {
                        continue;
                    }
                    match text(source, t) {
                        "(" | "[" | "{" => stack.push(0),
                        ")" | "]" | "}" => {
                            if let Some(c) = stack.pop() {
                                best = best.max(c + 1 + stack.len());
                            }
                        }
                        "," => {
                            if let Some(c) = stack.last_mut() {
                                *c += 1;
                            }
                        }
                        _ => {}
                    }
                }
                best
            };
            let rhs_peak = rhs_elements
                .iter()
                .enumerate()
                .map(|(k, range)| k + element_width(range))
                .max()
                .unwrap_or(0);
            let has_original_index = stmt
                .targets
                .iter()
                .any(|t| matches!(t, Target::Index { .. }));
            // 逐次形（T07）: Index ターゲットが無く、右辺要素数と
            // ターゲット数が一致する文では、値表を作らず要素ごとに
            // 一時ローカルへ順に評価する。各初期化子は自スロットが
            // 有効になる前に評価されるため、書換えはレジスタ中立に
            // なる（253 引数の原文が受理される形を保てる）。要素数が
            // 一致しない形（末尾呼出の多値展開を含む）は表形へ回す。
            let sequential = !has_original_index && rhs_elements.len() == stmt.targets.len();
            // Q10: 一時ローカルが有効ローカル数に加わって 200 上限を
            // 超える文は書き換えない。書き換え前の PUC 形のままであれ
            // ばロード可能であり、全体を落とすよりその文だけ方言差異
            // （first-wins）に留める方が正しい。必要数は形で異なる
            // （逐次形は要素ごとのスロット＋アドレス表、表形は値表＋
            // アドレス表）。
            let needed_temps = if sequential {
                stmt.targets.len() + usize::from(pair_counter > 0)
            } else {
                1 + usize::from(pair_counter > 0)
            };
            if stmt.active_locals + needed_temps > 200 {
                continue;
            }
            // T07: 書換え側のレジスタ会計。原文が受理される文でも、
            // 一時変数の分だけピークが膨らんで PUC の 255 上限を超える
            // 形は書き換えない（逐次形はアドレス表の有無だけ、表形は
            // 値表＋アドレス表の2つが上乗せになる）。
            let overhead = if sequential {
                usize::from(pair_counter > 0)
            } else {
                2
            };
            if stmt.active_locals + rhs_peak + overhead > 255 {
                continue;
            }
            if sequential {
                let mut replacement: Vec<u8> = Vec::new();
                replacement.extend_from_slice(b"do ");
                if pair_counter > 0 {
                    replacement.extend_from_slice(b"local ");
                    replacement.extend_from_slice(addr_temp.as_bytes());
                    replacement.extend_from_slice(b" = {}; ");
                    for (_, _, slot, expr) in &frozen_fills {
                        replacement.extend_from_slice(addr_temp.as_bytes());
                        replacement.push(b'[');
                        replacement.extend_from_slice(slot.to_string().as_bytes());
                        replacement.extend_from_slice(b"] = ");
                        replacement.extend_from_slice(expr);
                        replacement.extend_from_slice(b"; ");
                    }
                }
                for (idx, range) in rhs_elements.iter().enumerate() {
                    replacement.extend_from_slice(b"local ");
                    replacement.extend_from_slice(format!("{prefix}s{}", idx + 1).as_bytes());
                    replacement.extend_from_slice(b" = ");
                    replacement.extend_from_slice(&slice_tokens(source, toks, range.0, range.1));
                    replacement.extend_from_slice(b"; ");
                }
                // ストアは表形と同じく逆順・last-wins（逐次形の計画は
                // 全て静的省略が可能なものだけ）。
                let mut stored: HashSet<String> = HashSet::new();
                for (idx, plan) in plans.iter().enumerate().rev() {
                    if !stored.insert(identities[idx].clone()) {
                        continue;
                    }
                    replacement.extend_from_slice(b"; ");
                    replacement.extend_from_slice(store_text(plan).as_bytes());
                    replacement.extend_from_slice(b" = ");
                    replacement.extend_from_slice(format!("{prefix}s{}", idx + 1).as_bytes());
                }
                replacement.extend_from_slice(b" end");
                let orig_newlines = source[stmt_start..stmt_end]
                    .iter()
                    .filter(|&&b| b == b'\n')
                    .count();
                let new_newlines = replacement.iter().filter(|&&b| b == b'\n').count();
                replacement.extend(std::iter::repeat_n(
                    b'\n',
                    orig_newlines.saturating_sub(new_newlines),
                ));
                replacement.push(b';');
                edits.push((stmt_start, stmt_end, replacement));
                continue;
            }
            let rhs_text = slice_tokens(source, toks, rhs_first, stmt.last);
            let mut replacement: Vec<u8> = Vec::new();
            replacement.extend_from_slice(b"do ");
            if pair_counter > 0 {
                replacement.extend_from_slice(b"local ");
                replacement.extend_from_slice(addr_temp.as_bytes());
                replacement.extend_from_slice(b" = {}; ");
                for (_, _, slot, expr) in &frozen_fills {
                    replacement.extend_from_slice(addr_temp.as_bytes());
                    replacement.push(b'[');
                    replacement.extend_from_slice(slot.to_string().as_bytes());
                    replacement.extend_from_slice(b"] = ");
                    replacement.extend_from_slice(expr);
                    replacement.extend_from_slice(b"; ");
                }
            }
            replacement.extend_from_slice(b"local ");
            replacement.extend_from_slice(value_temp.as_bytes());
            replacement.extend_from_slice(b" = {");
            replacement.extend_from_slice(&rhs_text);
            replacement.extend_from_slice(b"}");
            // ストアは PUC と同じ逆順に行い、重複ターゲットについては
            // 最後の出現だけを残す（Codex 第3回 #8）。ただしテキスト同一は
            // 場所同一を保証しない（候補M1）: 不純なキー／ベース式や、兄弟
            // ターゲットの評価によるキー変数の書き換えで、テキスト同一の
            // Index ターゲットが出現ごとに異なる場所へ凍結されうる。
            // Index ターゲットについては、実効の (base, key) ペアが後続の
            // 同一 identity ターゲットと実行時に一致する場合だけストアを
            // 省略する。比較は `==` で行う（この方言環境には raw* 系関数が
            // 存在しないため生成コードからは呼べない）。数値・文字列・
            // 真偽値キーは値の一致がそのまま同一スロットを意味し、テーブル
            // は同一性で一致する。残る隅は __eq を備えた別オブジェクト同士を
            // プログラム自身の __eq が等しいと判定する場合だけで、その場合は
            // プログラムの定義する等値性に従う。Name ターゲットの場所は
            // 変数そのものであり、テキスト同一なら常に同一場所なので
            // 従来どおり静的に省略する。
            let mut stored: HashSet<String> = HashSet::new();
            for (idx, plan) in plans.iter().enumerate().rev() {
                if plan.static_dedup && !stored.insert(identities[idx].clone()) {
                    continue;
                }
                // 遅延読みの部分はストア（と、その前の重複判定ガード）の
                // 直前にスナップショットする。ガードが比較する (base,
                // キー) ペアは、各ターゲットが実際にストアした／する
                // 場所そのものになる。
                if let Some(name) = &plan.late_base {
                    replacement.extend_from_slice(b"; ");
                    replacement
                        .extend_from_slice(format!("{} = ", base_accessor(plan.pair)).as_bytes());
                    replacement.extend_from_slice(name);
                }
                if let Some(name) = &plan.late_key {
                    replacement.extend_from_slice(b"; ");
                    replacement
                        .extend_from_slice(format!("{addr_temp}[{}] = ", 2 * plan.pair).as_bytes());
                    replacement.extend_from_slice(name);
                }
                let mut guards: Vec<usize> = Vec::new();
                if !plan.static_dedup {
                    for j in idx + 1..stmt.targets.len() {
                        if identities[j] == identities[idx] {
                            guards.push(j);
                        }
                    }
                }
                let target = store_text(plan);
                if guards.is_empty() {
                    replacement.extend_from_slice(b"; ");
                    replacement.extend_from_slice(target.as_bytes());
                    replacement.extend_from_slice(b" = ");
                    replacement.extend_from_slice(value_temp.as_bytes());
                    replacement.push(b'[');
                    replacement.extend_from_slice((idx + 1).to_string().as_bytes());
                    replacement.push(b']');
                } else {
                    replacement.extend_from_slice(b"; if not (");
                    for (n, &j) in guards.iter().enumerate() {
                        if n > 0 {
                            replacement.extend_from_slice(b" or ");
                        }
                        replacement.extend_from_slice(
                            format!(
                                "{} == {} and {} == {}",
                                base_accessor(plan.pair),
                                base_accessor(plans[j].pair),
                                key_accessor(plan),
                                key_accessor(&plans[j])
                            )
                            .as_bytes(),
                        );
                    }
                    replacement.extend_from_slice(b") then ");
                    replacement.extend_from_slice(target.as_bytes());
                    replacement.extend_from_slice(b" = ");
                    replacement.extend_from_slice(value_temp.as_bytes());
                    replacement.push(b'[');
                    replacement.extend_from_slice((idx + 1).to_string().as_bytes());
                    replacement.push(b']');
                    replacement.extend_from_slice(b" end");
                }
            }
            replacement.extend_from_slice(b" end");
            // 改行数の保存（Codex 第3回 #17）: 再構成したターゲット並びは
            // 元のターゲット範囲内にあった改行（`t.\nx` 等）を含まない
            // ため、そのままでは後続コードの行番号がずれる。元の文の
            // 範囲にあった改行の不足分を、文の境界である末尾（生成した
            // ストアの後ろ・空文の前）に空白として補い、チャンク全体の
            // 行数を保存する。
            let orig_newlines = source[stmt_start..stmt_end]
                .iter()
                .filter(|&&b| b == b'\n')
                .count();
            let new_newlines = replacement.iter().filter(|&&b| b == b'\n').count();
            replacement.extend(std::iter::repeat_n(
                b'\n',
                orig_newlines.saturating_sub(new_newlines),
            ));
            // 末尾に空文を置く: 元の文が区切りなしで次の文に続く minified 形
            // （呼出で終わる代入の直後に次の文が続く形）で、生成テキストと後続の識別子が結合しないため。
            replacement.push(b';');
            edits.push((stmt_start, stmt_end, replacement));
        }
    }
    edits
}

fn slice_tokens(source: &[u8], toks: &[Token], first: usize, last: usize) -> Vec<u8> {
    if first >= last {
        return Vec::new();
    }
    let start = toks[first].start;
    let end = toks[last - 1].end;
    source[start..end].to_vec()
}

/// トークン範囲 `[f, l)` が単一の名前を括弧で包んだ形（括弧なし・何重
/// でも）なら、その名前トークンの範囲を返す。それ以外（複合式、呼出、
/// 途中で閉じる括弧など）は None。PUC では括弧付きの単一名前読みは
/// レジスタ参照のままで、評価時点も同一性も括弧なしと同一（R03）。
fn stripped_single_name(
    source: &[u8],
    toks: &[Token],
    mut f: usize,
    mut l: usize,
) -> Option<(usize, usize)> {
    loop {
        if l == f + 1 && toks[f].kind == Tok::Name {
            return Some((f, l));
        }
        if l >= f + 2
            && text(source, &toks[f]) == "("
            && text(source, &toks[l - 1]) == ")"
            && parens_wrap(source, toks, f, l)
        {
            f += 1;
            l -= 1;
            continue;
        }
        return None;
    }
}

/// `toks[f]` の `(` が `toks[l - 1]` の `)` と対応するか（範囲内で
/// 深さが 0 に戻るのが末尾ちょうどの場合のみ true）。
fn parens_wrap(source: &[u8], toks: &[Token], f: usize, l: usize) -> bool {
    let mut depth = 0i32;
    for (off, t) in toks[f..l].iter().enumerate() {
        match text(source, t) {
            "(" => depth += 1,
            ")" => {
                depth -= 1;
                if depth == 0 {
                    return f + off == l - 1;
                }
            }
            _ => {}
        }
    }
    false
}

// ---------------------------------------------------------------------------

// 書き換えの意味論はバックエンド実行で検証するため、テストは
// backend-mlua 有効時に限る（フィーチャ無効時は書き換え自体は有効だが、
// 実行を伴う検証は行わない）。
#[cfg(all(test, feature = "backend-mlua"))]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::rewrite;
    use crate::runner::Vm;
    use crate::ExecutionLimits;
    use std::borrow::Cow;

    /// プログラムを実行し、成功したかどうかを返す。値の検証はプログラム内の
    /// expect（条件が偽なら nil 参照で実行時エラーにする）で行う。
    fn run(source: &str) -> bool {
        let mut vm = match Vm::new(ExecutionLimits::default()) {
            Ok(vm) => vm,
            Err(_) => return false,
        };
        vm.execute(source.as_bytes(), "dialect-test").is_ok()
    }

    const EXPECT: &str =
        "local function expect(c) if not c then local t = nil local _ = t.f end end\n";

    // ---- (a) 多重代入 ----

    #[test]
    fn multiple_assignment_env_rebind_freezes_global_stores() {
        // T06 shape A: a plain `_ENV` target rebinds the environment
        // itself. PUC freezes the statement's global stores to the
        // scan-time environment (check_conflict), so `x` lands in the
        // old environment even though the stores run in reverse
        // order, and the read after the statement sees the new,
        // empty environment. The duplicate values are equal, so no
        // first/last-wins reading explains the difference.
        let src = format!("{EXPECT} x,x,_ENV=1,1,{{}} expect(x == nil)");
        assert!(run(&src));
        // Same with the stores landing before the rebind target.
        let src = format!("{EXPECT} x,_ENV,_ENV=1,{{}},{{}} expect(x == nil)");
        assert!(run(&src));
    }

    #[test]
    fn multiple_assignment_env_base_is_read_at_store_time() {
        // T06 shape B: the implicit `_ENV` base of an index target
        // is the environment upvalue itself, read at store time like
        // any upvalue base -- not frozen before the right-hand side.
        // `f` replaces `_ENV` while the right-hand side evaluates,
        // so both stores land in the new environment.
        let src = format!(
            "{EXPECT} local function f() _ENV = {{}} return 1 end \
             _ENV.x, _ENV.x = f(), 1 expect(x == 1)"
        );
        assert!(run(&src));
    }

    #[test]
    fn register_wide_assignment_stays_loadable_at_the_boundary() {
        // T07: the rewrite must not inflate registers past what the
        // original needs. 253 call arguments are PUC-legal in the
        // original and must stay executable identically; 254 are
        // rejected by PUC in the original and stay rejected.
        // No EXPECT prefix here: its local would itself occupy a
        // register and push even the original 253 form over the cap.
        let args = |n: usize| "0,".repeat(n - 1) + "0";
        let src = format!("a,a=type({}),type(0) return a", args(253));
        assert!(run(&src));
        let src = format!("a,a=type({}),type(0) return a", args(254));
        assert!(!run(&src));
        let src = format!("a,a=type({}),type(0) return a", args(252));
        assert!(run(&src));
    }

    #[test]
    fn multiple_assignment_duplicate_globals_last_wins() {
        let src = format!("{EXPECT} d, m, d = 1, 2, 3 expect(d == 3 and m == 2)");
        assert!(run(&src));
    }

    #[test]
    fn multiple_assignment_duplicate_locals_last_wins() {
        let src = format!("{EXPECT} local d, m d, m, d = 1, 2, 3 expect(d == 3 and m == 2)");
        assert!(run(&src));
    }

    #[test]
    fn multiple_assignment_duplicate_table_fields_last_wins() {
        let src = format!("{EXPECT} local t = {{}} t.x, t.x = 1, 2 expect(t.x == 2)");
        assert!(run(&src));
    }

    #[test]
    fn multiple_assignment_duplicate_index_last_wins() {
        let src = format!("{EXPECT} local t = {{}} t[1], t[1] = 10, 20 expect(t[1] == 20)");
        assert!(run(&src));
    }

    #[test]
    fn multiple_assignment_impure_duplicate_index_keys_store_both() {
        // 候補M1: キー式が不純だと、テキスト同一の Index ターゲットでも
        // 凍結される場所が異なる（k() が 1, 2 を返す）。両ストアが必要で、
        // テキスト identity による静的な省略は最初のストアを落とす。
        let src = format!(
            "{EXPECT} local t = {{}} local i = 0 local function k() i = i + 1 return i end \
             t[k()], t[k()] = 10, 20 expect(t[1] == 10 and t[2] == 20)"
        );
        assert!(run(&src));
    }

    #[test]
    fn multiple_assignment_sibling_call_mutating_key_keeps_both_stores() {
        // 候補M1 兄弟形: 兄弟ターゲットの呼出しがキー用変数を書き換える。
        // PUC では単一ローカル名のキーはレジスタ参照のままストア時点で
        // 読まれるため、PUC 自身は両ストアとも x=2 のスロットへ落とす
        // （素の PUC の結果は t[1]=nil, t[2]=10 の first-wins）。書換えは
        // R210 の last-wins で、実効の場所が一致する先行ストアをガードで
        // 省くため t[2]=30 が残る（第306ラウンド Q09 で実測し直した）。
        let src = format!(
            "{EXPECT} local t, u = {{}}, {{}} local x = 1 \
             local function f() x = x + 1 return 0 end \
             t[x], u[f()], t[x] = 10, 20, 30 \
             expect(t[1] == nil and t[2] == 30 and u[0] == 20 and x == 2)"
        );
        assert!(run(&src));
    }

    #[test]
    fn multiple_assignment_target_base_reread_after_rhs() {
        // Q09 base 変種（Codex run5 の再現）: base のローカル変数自体が
        // 右辺の呼出しで差し替わる。PUC は base をストア時点で読むため
        // 両ストアとも新しい表へ落ちる。
        let src = format!(
            "{EXPECT} local t = {{}} local old = t \
             local function f() t = {{}} return 4 end \
             t.x, t.x = f(), 4 \
             expect(old.x == nil and t.x == 4)"
        );
        assert!(run(&src));
    }

    #[test]
    fn multiple_assignment_target_key_reread_after_rhs() {
        // Q09 キー変種: キーのローカル変数が右辺の呼出しで書き換わる。
        // PUC は (nil, 1)（両ストアとも "b" へ落ち first-wins）、書換えは
        // R210 の last-wins で (nil, 2) になる。落ちるスロットが "b" で
        // あることが本件の要点。
        let src = format!(
            "{EXPECT} local t = {{}} local k = \"a\" \
             local function f() k = \"b\" return 1 end \
             t[k], t[k] = f(), 2 \
             expect(t.a == nil and t.b == 2)"
        );
        assert!(run(&src));
    }

    #[test]
    fn multiple_assignment_late_key_with_distinct_targets() {
        // 重複でないターゲットでもキーの遅延読みは効く: t[k] は右辺の
        // 呼出し後の k で決まる。
        let src = format!(
            "{EXPECT} local t = {{}} local k = \"a\" \
             local function f() k = \"b\" return 1 end \
             t[k], t.a = f(), 2 \
             expect(t.b == 1 and t.a == 2)"
        );
        assert!(run(&src));
    }

    #[test]
    fn multiple_assignment_upvalue_base_reread_after_rhs() {
        // upvalue の base もストア時点読み（セルの現在値）。内側関数の
        // 多重代入で base 変数が右辺で差し替わる形。
        let src = format!(
            "{EXPECT} local t = {{}} local old = t \
             local function f() t = {{}} return 4 end \
             local function g() t.x, t.x = f(), 5 end \
             g() expect(old.x == nil and t.x == 5)"
        );
        assert!(run(&src));
    }

    #[test]
    fn multiple_assignment_base_reassigned_by_later_plain_target() {
        // R02: 同一文の後続の素の名前ターゲットが base 自身を代入する
        // 形。PUC は左辺走査の時点で base を退避するため、両ストアとも
        // 旧い表へ落ちる（素の PUC は old.x=4, t.x=nil。遅延読みのまま
        // だと再束縛後の表へ落ちて old.x=nil, t.x=4 になる）。
        let src = format!(
            "{EXPECT} local t = {{}} local old = t \
             t.x, t.x, t = 4, 4, {{}} \
             expect(old.x == 4 and t.x == nil)"
        );
        assert!(run(&src));
        // upvalue base の同形（内側関数から捕捉した base）。
        let src = format!(
            "{EXPECT} local t = {{}} local old = t \
             local function g() t.x, t.x, t = 4, 4, {{}} end \
             g() expect(old.x == 4 and t.x == nil)"
        );
        assert!(run(&src));
        // 素の名前ターゲットが中央にある形: 先行する Index ターゲットの
        // base はやはり左辺走査で退避される（PUC 実測で old.x=4,
        // t.x=nil。第307ラウンドの形状差分プローブで確認）。
        let src = format!(
            "{EXPECT} local t = {{}} local old = t \
             t.x, t, t.x = 4, {{}}, 4 \
             expect(old.x == 4 and t.x == nil)"
        );
        assert!(run(&src));
    }

    #[test]
    fn multiple_assignment_key_reassigned_by_later_plain_target() {
        // R02 のキー版: キーのローカルが同一文の後続で代入される場合も
        // PUC は左辺走査でキーを退避する（両ストアとも旧いキーへ）。
        let src = format!(
            "{EXPECT} local t = {{}} local k = 1 \
             t[k], t[k], k = 10, 20, 2 \
             expect(t[1] == 20 and t[2] == nil and k == 2)"
        );
        assert!(run(&src));
        // 素の名前ターゲットが中央にある形も同じ（PUC 実測 t[1]=7,
        // t[2]=nil。形状差分プローブで確認）。
        let src = format!(
            "{EXPECT} local t = {{}} local k = 1 \
             t[k], k, t[k] = 7, 2, 7 \
             expect(t[1] == 7 and t[2] == nil and k == 2)"
        );
        assert!(run(&src));
    }

    #[test]
    fn multiple_assignment_parenthesized_key_reread_after_rhs() {
        // R03: 括弧付きキー `(k)` も括弧なしと同じくストア時点読み。
        // 右辺の呼出しが k を書き換えると、両ストアとも新しいキーへ
        // 落ちる（素の PUC は t[1]=nil, t[2]=4）。
        let src = format!(
            "{EXPECT} local t = {{}} local k = 1 \
             local function f() k = 2 return 4 end \
             t[(k)], t[(k)] = f(), 4 \
             expect(t[1] == nil and t[2] == 4)"
        );
        assert!(run(&src));
        // 二重括弧も同じ。
        let src = format!(
            "{EXPECT} local t = {{}} local k = 1 \
             local function f() k = 2 return 4 end \
             t[((k))], t[((k))] = f(), 4 \
             expect(t[1] == nil and t[2] == 4)"
        );
        assert!(run(&src));
    }

    #[test]
    fn multiple_assignment_parenthesized_key_shares_identity() {
        // R03: `t[k]` と `t[(k)]` は同一ターゲットとして重複扱いに
        // なる（last-wins の対象になる）。
        let src = format!(
            "{EXPECT} local t = {{}} local k = 1 \
             t[k], t[(k)] = 1, 2 \
             expect(t[1] == 2)"
        );
        assert!(run(&src));
    }

    #[test]
    fn repeat_until_condition_sees_body_locals_as_upvalues() {
        // R04: until 条件式は本体ブロックのスコープ内で評価される。
        // 条件内の IIFE が捕捉する本体ローカル t は upvalue であり、
        // その多重代入の base はストア時点読みになる（素の PUC は
        // old.x=nil, t.x=4。フレームを外して読むと t が大域名扱いに
        // なり base が早期凍結されて old.x=4, t.x=nil になる）。
        let src = format!(
            "{EXPECT} local a, b \
             local function probe() \
               repeat \
                 local t = {{}} local old = t \
                 local function f() t = {{}} return 4 end \
               until (function() t.x, t.x = f(), 4 a, b = old.x, t.x return true end)() \
             end \
             probe() expect(a == nil and b == 4)"
        );
        assert!(run(&src));
    }

    #[test]
    fn for_loop_hidden_locals_count_toward_temp_budget() {
        // R05: 196 ローカル（EXPECT の expect 関数を含む）＋数値 for
        // 本体内の重複代入。for の隠れローカル（制御変数＋3枠）を
        // 数えると一時変数の追加で上限を超えるため、その文は
        // 書き換えず PUC 形（first-wins）のまま残す。書き換えると
        // 生成形がロード不能になるのが元の不具合。
        let mut src = String::from(EXPECT);
        for i in 0..194 {
            src.push_str(&format!("local v{i} = {i} "));
        }
        src.push_str("local x for i = 1, 1 do x, x = 1, 2 end expect(x == 1)");
        assert!(run(&src));
        // generic for も同じ（宣言名＋3枠）。
        let mut src = String::from(EXPECT);
        for i in 0..194 {
            src.push_str(&format!("local v{i} = {i} "));
        }
        src.push_str(
            "local x for k in pairs({}) do x, x = 1, 2 end \
             for k in pairs({1}) do x, x = 3, 4 end expect(x == 3)",
        );
        assert!(run(&src));
    }

    #[test]
    fn rewrite_spawn_failure_is_a_load_error_not_an_abort() {
        // S03: ワーカー生成に失敗した場合、ロード可否判定と実行の
        // 両 API が通常エラーで返ること。生成失敗は子プロセス側で
        // RLIMIT_AS を絞って注入する（256MiB スタックの予約が失敗
        // する）。旧分岐（呼出側スタックで本体を実行）は debug
        // ビルド＋2MiB スタックで SIGABRT したため、子プロセスが
        // 正常終了すること自体が回帰の証拠になる。子側ではエラーが
        // 生成失敗のものであることまで確認し、生成が成功して別の
        // ロードエラーになる経路では合格しないようにする。
        // macOS では RLIMIT_AS を設定できない（sh の ulimit -v が
        // "cannot modify limit: Invalid argument" で失敗し、上限が
        // 効かないまま子が走る）ため、この注入は macOS では成立
        // しない。対象を macOS 以外の unix に限る（Windows は従来
        // から unix 限定のため対象外）。生成失敗時のエラー経路
        // そのものは Linux の CI で固定する。
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            if std::env::var_os("STORM_DIALECT_SPAWN_FAIL_CHILD").is_some() {
                let src = "do ".repeat(309) + &"end ".repeat(309);
                let handle = std::thread::Builder::new()
                    .stack_size(2 * 1024 * 1024)
                    .spawn(move || {
                        let legality = crate::load_legality::check_load_legality(src.as_bytes());
                        let err = legality.expect_err("legality must fail");
                        assert!(err.contains("could not be spawned"), "{err}");
                        let mut vm = Vm::new(ExecutionLimits::default()).expect("vm");
                        let err = vm
                            .execute(src.as_bytes(), "s03-child")
                            .expect_err("execute must fail");
                        assert!(err.to_string().contains("could not be spawned"), "{err}");
                    })
                    .expect("spawn caller thread");
                handle.join().expect("caller thread panicked");
                return;
            }
            // 子プロセスは sh の ulimit でアドレス空間を絞って起動
            // する（アドレス空間上限つきで exec するのと等価）。
            let exe = std::env::current_exe().expect("current exe");
            let status = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg("ulimit -v 196608; exec \"$0\" \"$@\"")
                .arg(&exe)
                .arg("--exact")
                .arg("dialect::tests::rewrite_spawn_failure_is_a_load_error_not_an_abort")
                .arg("--nocapture")
                .env("STORM_DIALECT_SPAWN_FAIL_CHILD", "1")
                .status()
                .expect("run child");
            assert!(status.success(), "child status: {status}");
        }
    }

    #[test]
    fn rewrite_survives_deep_nesting_on_a_small_stack() {
        // R12: 小さいスタック（2MiB）のスレッドから呼んでも、深い
        // 入れ子の書き換えで異常終了しない。書き換え本体は専用の
        // 大スタックワーカースレッドで実行される。深さ上限ガードに
        // より書き換えは諦められ、元ソースがそのまま返る。
        let src = "return function() ".repeat(499) + "return 1 " + &"end ".repeat(499);
        let handle = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(move || rewrite(src.as_bytes()) == Ok(Cow::Borrowed(src.as_bytes())))
            .expect("spawn small-stack thread");
        assert!(handle.join().expect("rewrite thread panicked"));
    }

    #[test]
    fn multiple_assignment_global_base_frozen_before_rhs() {
        // グローバル名の base は PUC が左辺走査で実体化するため、右辺で
        // 差し替わってもストアは旧い表へ落ちる（凍結が正しい側）。
        let src = format!(
            "{EXPECT} gt = {{}} local old = gt \
             function gf() gt = {{}} return 4 end \
             gt.x, gt.x = gf(), 5 \
             expect(old.x == 5 and gt.x == nil)"
        );
        assert!(run(&src));
    }

    #[test]
    fn multiple_assignment_skipped_when_temps_would_exceed_local_limit() {
        // Q10: 有効ローカル 199 + 一時 2 = 201 で上限超過になる文は
        // 書き換えず、PUC 形のまま残す（全体を落とさない）。
        let names = (1..=199)
            .map(|i| format!("v{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let src = format!("local {names} gt.x, gt.x = 1, 2");
        let out = super::rewrite(src.as_bytes()).expect("rewrite");
        assert_eq!(out.as_ref(), src.as_bytes());
        // 198 ローカルなら一時 2 を足しても上限内で、通常どおり書き換える。
        let names = (1..=198)
            .map(|i| format!("v{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let src = format!("local {names} gt.x, gt.x = 1, 2");
        let out = super::rewrite(src.as_bytes()).expect("rewrite");
        assert!(
            String::from_utf8_lossy(&out).contains("__stormvm_t"),
            "not rewritten"
        );
    }

    #[test]
    fn deep_nesting_does_not_abort_rewrite() {
        // Q11: 深い入れ子でも書換えは諦めて元のソースを返すだけであり、
        // プロセスを落とさない（ロード可否はバックエンドが通常エラーで
        // 判定する）。適度な入れ子は従来どおり処理できる。
        let src = format!("{}x = 1{}", "do ".repeat(20_000), "end ".repeat(20_000));
        let out = super::rewrite(src.as_bytes()).expect("rewrite");
        assert_eq!(out.as_ref(), src.as_bytes());
        let src = format!("{}x, x = 1, 2 {}", "do ".repeat(50), "end ".repeat(50));
        let out = super::rewrite(src.as_bytes()).expect("rewrite");
        assert!(
            String::from_utf8_lossy(&out).contains("__stormvm_t"),
            "not rewritten"
        );
    }

    #[test]
    fn multiple_assignment_duplicate_upvalues_last_wins() {
        let src = format!(
            "{EXPECT} local d, m local function set() d, m, d = 1, 2, 3 end set() expect(d == 3 and m == 2)"
        );
        assert!(run(&src));
    }

    #[test]
    fn multiple_assignment_rhs_evaluation_order_preserved() {
        let src = format!(
            "{EXPECT} local log = {{}} local function rec(v) log[#log + 1] = v return v end \
             local a, b a, b, a = rec(1), rec(2), rec(3) \
             expect(a == 3 and b == 2 and log[1] == 1 and log[2] == 2 and log[3] == 3)"
        );
        assert!(run(&src));
    }

    #[test]
    fn multiple_assignment_swap_still_works() {
        let src = format!("{EXPECT} local a, b = 1, 2 a, b = b, a expect(a == 2 and b == 1)");
        assert!(run(&src));
    }

    #[test]
    fn local_declaration_duplicates_unchanged() {
        let src = format!("{EXPECT} local d, m, d = 1, 2, 3 expect(d == 3 and m == 2)");
        assert!(run(&src));
    }

    #[test]
    fn assignment_without_duplicates_is_not_rewritten() {
        assert!(matches!(
            rewrite(b"local x = 1\na, b = x, 2\n"),
            Ok(Cow::Borrowed(_))
        ));
    }

    #[test]
    fn multiple_assignment_followed_by_call_without_separator() {
        // minified 形: 代入文（呼出で終わる）の直後に区切りなしで文が続く。
        // 置換末尾の一時変数名が後続識別子と結合してはならない。
        let src = format!(
            "{EXPECT} local log = {{}} local function g() return 1, 2, 3 end local function oB() log.hit = true end d, m, d = g()oB() expect(d == 3 and m == 2 and log.hit == true)"
        );
        assert!(run(&src));
    }

    // ---- (b) goto とラベル ----

    #[test]
    fn goto_over_local_to_label_with_following_statement() {
        // w6 c39 の h1 形: ラベルの後に交差ローカルを参照しない文がある
        let src = format!(
            "{EXPECT} local out = {{}} local function onTick() goto skip local x = 5 ::skip:: out.hit = true end onTick() expect(out.hit == true)"
        );
        assert!(run(&src));
    }

    #[test]
    fn goto_over_local_label_then_return() {
        let src = format!(
            "{EXPECT} local function f() goto l local x = 5 ::l:: return 7 end expect(f() == 7)"
        );
        assert!(run(&src));
    }

    #[test]
    fn goto_over_local_segment_may_use_the_local() {
        // v10 形: ラベル前の区間内では交差ローカルを参照してよい
        let src = format!(
            "{EXPECT} local function f() local r = 0 goto skip local x = 5 r = x ::skip:: return r end expect(f() == 0)"
        );
        assert!(run(&src));
    }

    #[test]
    fn goto_over_two_locals() {
        let src = format!(
            "{EXPECT} local function f() goto l local a = 1 local b = 2 ::l:: return 3 end expect(f() == 3)"
        );
        assert!(run(&src));
    }

    #[test]
    fn goto_wrap_applies_to_second_goto_in_same_block() {
        // 候補M2: 同一ブロックにラップ可能な goto が2つある場合、走査の
        // 打ち切りで1つ目しか包まれないことがあってはならない。
        let src = format!(
            "{EXPECT} local function f() goto a local x = 1 ::a:: goto b local y = 2 ::b:: return 7 end \
             expect(f() == 7)"
        );
        assert!(run(&src));
    }

    #[test]
    fn goto_wrap_applies_in_second_function() {
        // 候補M2: ラップ可能な goto が2つの関数に1つずつある場合も両方包む。
        let src = format!(
            "{EXPECT} local function f() goto done local x = 1 ::done:: return 1 end \
             local function g() goto fin local y = 2 ::fin:: return 2 end \
             expect(f() == 1 and g() == 2)"
        );
        assert!(run(&src));
    }

    #[test]
    fn goto_wrap_not_aborted_by_preceding_clean_goto() {
        // 候補M2: 交差するローカルのない goto が先行しても、走査はそこで
        // 打ち切られず後続のラップ可能な goto へ到達する。
        let src = format!(
            "{EXPECT} local function f() goto early ::early:: goto done local x = 1 ::done:: return 1 end \
             expect(f() == 1)"
        );
        assert!(run(&src));
    }

    #[test]
    fn goto_over_local_in_repeat_body() {
        let src = format!(
            "{EXPECT} local n = 0 repeat goto l local x = 5 ::l:: n = n + 1 until n >= 2 expect(n == 2)"
        );
        assert!(run(&src));
    }

    #[test]
    fn goto_referencing_crossed_local_after_label_still_rejected() {
        // g1/b7 形: ラベルの後で交差ローカルを参照する形は従来どおり拒否
        let src = "local function f() goto l local x = 5 ::l:: return x end return f()";
        assert!(!run(src));
    }

    #[test]
    fn goto_crossed_local_captured_after_label_still_rejected() {
        let src = "local function f() goto l local x = 5 ::l:: return function() return x end end return f()";
        assert!(!run(src));
    }

    #[test]
    fn goto_crossed_local_in_until_still_rejected() {
        let src = "local n = 0 repeat goto l local x = n ::l:: n = n + 1 until x == nil";
        assert!(!run(src));
    }

    #[test]
    fn goto_label_truly_last_still_works() {
        let src =
            format!("{EXPECT} local function f() goto l local x = 5 ::l:: end f() expect(true)");
        assert!(run(&src));
    }

    #[test]
    fn backward_goto_loop_still_works() {
        let src = format!(
            "{EXPECT} local i = 0 ::top:: i = i + 1 if i < 3 then goto top end expect(i == 3)"
        );
        assert!(run(&src));
    }

    #[test]
    fn nested_assignment_in_function_rhs_does_not_panic() {
        // Codex 第3回 #1: 外側の代入の右辺にある関数式の内側の重複代入は
        // 編集範囲が入れ子になる。panic せず、内側も last-wins になる。
        assert!(run("a,a=function() b,b=1 end"));
        let src = format!(
            "{EXPECT} local f f, f = 0, function() b, b = 1, 2 return b end expect(f() == 2)"
        );
        assert!(run(&src));
    }

    #[test]
    fn goto_over_rewritten_assignment_is_legal() {
        // Codex 第3回 #12: 一時変数は do ブロックに閉じているため、
        // 文を飛び越える goto がそのスコープへ跳ぶ形にならない。
        let src = format!("{EXPECT} local r = 0 goto L a, a = 1, 1 ::L:: r = 7 expect(r == 7)");
        assert!(run(&src));
    }

    #[test]
    fn many_duplicate_assignments_stay_within_local_budget() {
        // Codex 第3回 #12: 文が並んでも一時ローカルは蓄積しない。
        let mut src = String::from(EXPECT);
        for _ in 0..101 {
            src.push_str("a, a = 1, 1; ");
        }
        src.push_str("expect(a == 1)");
        assert!(run(&src));
    }

    #[test]
    fn wide_duplicate_statement_keeps_user_variables() {
        // Codex 第3回 #13（r3-13 形）: 幅の広い文でも一時変数が
        // ユーザーの __stormvm_t65 を遮蔽しない。
        let mut targets = String::new();
        let mut values = String::new();
        for i in 0..22 {
            if i > 0 {
                targets.push(',');
                values.push(',');
            }
            targets.push_str("t.x");
            values.push('0');
        }
        let src = format!(
            "{EXPECT} __stormvm_t65 = 7 local t = {{}} {targets} = {values} expect(__stormvm_t65 == 7)"
        );
        assert!(run(&src));
    }

    #[test]
    fn temp_names_avoid_the_generated_names_themselves() {
        // 生成名そのもの（__stormvm_ta / __stormvm_tv）と同名の
        // ユーザー変数がある場合は接頭辞が伸びて衝突を避ける。
        let src = format!(
            "{EXPECT} __stormvm_ta = 7 __stormvm_tv = 8 local t = {{}} t.x, t.x = 0, 0 \
             expect(__stormvm_ta == 7 and __stormvm_tv == 8)"
        );
        assert!(run(&src));
    }

    #[test]
    fn env_rebinding_statement_keeps_puc_store_order() {
        // Codex 第3回 #8: _ENV,x,x={},1,1 では x のストアが _ENV の
        // 再束縛より前に起きる（旧環境へ書かれる）のが PUC の意味論で、
        // 重複 x の解決だけが last-wins になる。
        let src = format!(
            "{EXPECT} local old = _ENV _ENV, x, x = {{}}, 1, 1 expect(old.x == 1 and x == nil)"
        );
        assert!(run(&src));
    }

    #[test]
    fn rewrite_preserves_raw_bytes_in_string_literals() {
        // Codex 第3回 #9: 文字列リテラル内の生バイト（0xFF）は Lua の
        // バイト文字列の一部であり、書換えで U+FFFD に化けてはならない。
        let src: &[u8] = b"a,a=\"\xff\",\"\xff\"; if string.byte(a)~=255 then local e=nil e() end";
        let out = rewrite(src).expect("rewrite");
        assert!(out.windows(1).any(|w| w == [0xffu8]));
        assert!(!out.windows(3).any(|w| w == [0xefu8, 0xbf, 0xbd]));
        let mut vm = Vm::new(ExecutionLimits::default()).expect("vm");
        assert!(vm.execute(src, "dialect-test").is_ok());
    }

    #[test]
    fn rewrite_preserves_line_numbers_across_multiline_targets() {
        // Codex 第3回 #17: ターゲット範囲内の改行を落とすと後続のエラー
        // 行がずれる。チャンク全体の改行数を保存すること。
        let src = "local t={}; t.\nx,t.\nx=1,1;\nlocal e=nil e()\n";
        let out = rewrite(src.as_bytes()).expect("rewrite");
        assert_eq!(
            out.iter().filter(|&&b| b == b'\n').count(),
            src.bytes().filter(|&b| b == b'\n').count()
        );
        let mut vm = Vm::new(ExecutionLimits::default()).expect("vm");
        let err = vm.execute(src.as_bytes(), "dialect-test").unwrap_err();
        assert!(format!("{err}").contains(":4:"), "{err}");
    }

    #[test]
    fn doubly_nested_assignment_rewrite_reaches_fixpoint() {
        let src = format!(
            "{EXPECT} local f f, f = 0, function() return function() c, c = 1, 2 return c end end \
             expect(f()() == 2)"
        );
        assert!(run(&src));
    }

    #[test]
    fn return_function_expression_does_not_block_rewrite() {
        //  は式を持つ return であり、空の
        // return と誤認して走査全体を諦めてはならない。
        let out =
            rewrite(b"f, f = 0, function() return function() return 1 end end\n").expect("rewrite");
        assert!(matches!(out, Cow::Owned(_)));
    }

    #[test]
    fn goto_over_env_with_implicit_reference_after_label_still_rejected() {
        // Codex 第3回 #16（r3-16 形）: math.pi は暗黙の _ENV 参照なので、
        // local _ENV を飛び越える goto は包まず従来どおり拒否する。
        // （拒否は素の Lua 側の性質で、VM 自体は寛容なため rewrite が
        // Borrowed のままであることをもって確認する。）
        let src = "goto L\nlocal _ENV={}\n::L::\nreturn math.pi\n";
        assert!(matches!(rewrite(src.as_bytes()), Ok(Cow::Borrowed(_))));
    }

    #[test]
    fn goto_over_env_without_any_reference_after_label_is_wrapped() {
        // ラベル以降の名前が全て goto 以前に宣言されたローカルなら
        // _ENV 参照はなく、包んでよい（制御側）。
        let src =
            format!("{EXPECT} local r = 0 goto L local _ENV = {{}} ::L:: r = 7 expect(r == 7)");
        assert!(run(&src));
    }

    #[test]
    fn goto_with_reference_after_label_is_not_rewritten() {
        assert!(matches!(
            rewrite(b"goto l\nlocal x = 5\n::l::\nreturn x\n"),
            Ok(Cow::Borrowed(_))
        ));
    }

    #[test]
    fn goto_wrap_glued_label_identifier_separator() {
        // 候補S11: plain プリンタの糊付け形 `a=b::done::end` は、包みの
        // `end ` 挿入が直前の `b` と結合して `bend` になっていた。
        let src =
            "local function f() local a = 1 goto done local b = 2 a = b::done:: end return f()";
        assert!(run(src));
        assert!(matches!(
            rewrite(src.as_bytes()),
            Ok(Cow::Owned(ref v)) if String::from_utf8_lossy(v).contains("b end ::done::")
        ));
    }

    #[test]
    fn goto_wrap_glued_label_number_separator() {
        // 候補S11 の数値変種: `local y=9::cont::` は `9end` になっていた。
        let src = "local s = 0 for i = 1, 2 do goto cont local y = 9::cont:: ; end return s";
        assert!(run(src));
    }
}
