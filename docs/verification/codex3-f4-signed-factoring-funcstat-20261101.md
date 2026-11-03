# Codex 第3回指摘4（signed_factoring の Funcstat ターゲット書換え）の修正 — 検証記録

対象: `crates/storm-lua-minify/src/passes/signed_factoring.rs`
担当: w9 第62サイクル

## 指摘（Codex 第3回 #4）

```lua
local t = { xxxxx = 3 }
local a, b = t.xxxxx, t.xxxxx
function t.xxxxx() return 1 end
return a, b, type(t.xxxxx)   -- 素: (3, 3, "function")
```

因数分解したメンバー式 `t.xxxxx` が Funcstat のターゲットにも
現れると、再利用がターゲットまでキャリアへ書き換えて
`function <carrier>()` となり、`type(t.xxxxx)` が `"number"` の
ままになった。第221ラウンドは Assign ターゲットのみを保護して
おり、Funcstat が未対応だった。

## 修正

第221の Assign 保護を Funcstat にも対称に適用した（4箇所）:

- 出現走査（`inspect_signed_expression`）: Funcstat のターゲットを
  走査しない（関数子は `Node::Function` で入口チェックにより
  もともと入らない）。
- 一致数え（`count_matched_bid`）: Funcstat では関数子だけを数え、
  ターゲットを一致として数えない。
- 書換え（`rewrite_signed`）: Funcstat のターゲットをそのまま保持し、
  関数子の中だけを書き換える。
- 分割判定（`statement_writes_group_member`）: Funcstat のメンバー
  ターゲット（メソッド定義含む）をメンバー書込みとして扱う。

回帰テスト2件を追加（ターゲットが verbatim に残ること、範囲内の
Funcstat メンバー書込みでグループが分割されること）。

## 検証

- signed_factoring 単体テスト: 19件 → 21件、全通過。
  ワークスペース全体: 1,630 件全通過（失敗0）。
- c59 再現スイートとの差分は r3-04 の2構成のみで、全て意図した
  方向（最適化結果が素の `(3, 3, "function")` に一致。読みの
  因数分解自体は片方の構成で引き続き効いている）。
- Tier FULL: コーパス 16,620 行が base／head とも SHA-256
  `743431aa4bb3142c3661c78dfdbf63219052704c8d763fbd35cda85d73279be7`
  でバイト一致（fallback 0）。
