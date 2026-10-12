# Scope review evidence

`scope-review.md` が結果本文。`structured-corpus.jsonl` は有効な原文 480 件の全記録。`findings-matrix.jsonl` は既定設定・exact・safe・pass 単独/無効化の比較。`target-probes*.jsonl` は zero_cost_newlines=false の初期探索で、結果本文の default 再現は後続 matrix でも確認した。

`harness.rs` と `Cargo.toml.template` から別の Cargo consumer を作れる。template の `REVIEWED_CHECKOUT` を検証対象 checkout のパスに置換し、harness.rs をその consumer の src/main.rs として保存し、保存済み Cargo.lock も consumer 直下へコピーする。ビルドは環境の Cargo target policy に従い、今回と同じ計測なら `CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 cargo build --manifest-path <consumer>/Cargo.toml --offline` を使う。

`MINIFY_AUDIT_EXE` に生成された minify-scope-audit のパスを指定し、レビュー成果物を置いたリポジトリの root から `python3 docs/verification/minify-review-20261012/evidence/scope/structured_corpus.py` 等を実行すると再検証できる。generator は対応する JSONL を上書きするため、保存済み証跡を保持する場合は別 checkout / コピーへ出力する。

補助ログの `transform_us` は初期の immutable-synthesis-shadow.jsonl では Lua 実行を含む総時間であり、ベンチの数値として使っていない。benchmark JSONL の transform_us は parse + pass + print のみ。

今回のハーネスは Lua 5.3 の引数/戻り値/closure/table/state semantics を見るための synthetic host であり、Stormworks の画面・Composite境界の適合検証ではない。原文と生成物はそれぞれ新規 VM で実行する。Lua allocation identity を tostring(pointer) で比較する用途には使わず、boolean identity tests をソースに記述する。

追加 coverage: `closed-fields-matrix.jsonl` は alias / colon method / dynamic string keys / pairs copy / string implicit methods 等の 15 原文 × 4 モード = 60 件。57 一致、3 compile rejection（S9 で説明）。scope-renaming と closed-field-renaming の正常例を既存不具合と区別して保存した。
