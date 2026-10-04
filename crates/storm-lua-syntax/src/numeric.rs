//! Numeric and string literal parsing and deterministic spelling.

// Canonical printer numeric/string utilities.
// - num_val / short_num / normalize_num_literal: Lua 数値リテラルの正規化
// - decode_lua_string / quote_lua / qstr: 文字列リテラルの整形
//
// short_num は JS の Number.prototype.toString / toExponential の出力を
// 再現する。Rust の `{:e}` も JS と同じ「最短ラウンドトリップ桁」を生成するため、
// 桁列は両者で一致し、表記規則（指数の + 符号・0 の省略）だけをこちらで再現する。
// 正確性は fixtures/numeric-golden.tsv との差分テストで保証する。

use std::borrow::Cow;

/// JS の Number(s) 相当（10 進）。short_num の候補文字列の roundtrip 検証に使う。
/// 候補は全て well-formed な 10 進表現のため、Rust の f64 パースで足りる。
fn js_decimal(s: &str) -> Option<f64> {
    s.parse::<f64>().ok()
}

/// Lua 数値リテラル → f64。
/// 16 進（0x…p…）は手動パース、それ以外は JS Number 相当（Rust f64 パース）。
pub fn num_val(s: &str) -> f64 {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'0' && (bytes[1] == b'x' || bytes[1] == b'X') {
        return hex_num_val(&s[2..]);
    }
    s.parse::<f64>().unwrap_or(f64::NAN)
}

/// Lua 5.3 の整数リテラルを、値を丸めずに読む。
///
/// AST は歴史的に数値リテラルを文字列で保持しているため、f64 へ変換してから
/// 整数演算を行うと 2^53 より大きい十進値や 64-bit hex 値が壊れる。hex の
/// `0xffffffffffffffff` は Lua の整数ビットパターンとして -1 を表す。
pub fn integer_literal_value(s: &str) -> Option<i64> {
    let unsigned = s.strip_prefix(['+', '-']).unwrap_or(s);
    let negative = s.starts_with('-');
    if let Some(hex) = unsigned
        .strip_prefix("0x")
        .or_else(|| unsigned.strip_prefix("0X"))
    {
        if hex.is_empty() || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let value = u64::from_str_radix(hex, 16).ok()?;
        let value = value as i64;
        return negative.then_some(value.wrapping_neg()).or(Some(value));
    }
    if unsigned.is_empty() || !unsigned.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let value = unsigned.parse::<i64>().ok()?;
    if negative {
        value.checked_neg()
    } else {
        Some(value)
    }
}

/// Return the shortest exact Lua hexadecimal-float spelling derived from the
/// IEEE-754 significand. Integer-source tokens are deliberately excluded so
/// the Lua 5.3 integer/float subtype is never changed. An exponentless hex
/// spelling is emitted only when it retains a fractional point.
pub fn shortest_exact_hex_float_literal(token: &str) -> Option<String> {
    if integer_literal_value(token).is_some() {
        return None;
    }
    let value = num_val(token);
    if !value.is_normal() || value == 0.0 {
        return None;
    }
    let negative = value.is_sign_negative();
    let magnitude = value.abs();
    let bits = magnitude.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32 - 1023;
    let significand = (bits & ((1u64 << 52) - 1)) | (1u64 << 52);

    // Full trailing nibbles only lengthen the mantissa. Strip them once, then
    // explore the remaining 0..3 right shifts and up to three left shifts; the
    // latter can absorb small p-exponents into otherwise-free mantissa bits.
    let trailing = significand.trailing_zeros();
    let nibble_shift = (trailing / 4) * 4;
    let stripped = significand >> nibble_shift;
    let base_exp = exponent - 52 + nibble_shift as i32;
    let residual = trailing - nibble_shift;

    let mut best: Option<String> = None;
    for shift in -3i32..=residual as i32 {
        let shifted = if shift < 0 {
            stripped << (-shift as u32)
        } else {
            stripped >> (shift as u32)
        };
        let shifted_exp = base_exp + shift;
        let digits = format!("{shifted:x}");
        for split in 0..=digits.len() {
            let frac_digits = digits.len() - split;
            let p_exp = shifted_exp + 4 * frac_digits as i32;
            let mut mantissa = String::from("0x");
            if split == 0 {
                mantissa.push('.');
                mantissa.push_str(&digits);
            } else {
                mantissa.push_str(&digits[..split]);
                if split < digits.len() {
                    mantissa.push('.');
                    mantissa.push_str(&digits[split..]);
                }
            }

            let mut forms = Vec::with_capacity(2);
            if p_exp == 0 && split < digits.len() {
                forms.push(mantissa.clone());
            }
            forms.push(format!("{mantissa}p{p_exp}"));

            for unsigned in forms {
                if num_val(&unsigned).to_bits() != magnitude.to_bits() {
                    continue;
                }
                let body = if negative {
                    format!("-{unsigned}")
                } else {
                    unsigned
                };
                if best.as_ref().is_none_or(|current| {
                    body.len() < current.len() || (body.len() == current.len() && body < *current)
                }) {
                    best = Some(body);
                }
            }
        }
    }
    best
}

fn hex_digit(c: u8) -> Option<u32> {
    match c {
        b'0'..=b'9' => Some((c - b'0') as u32),
        b'a'..=b'f' => Some((c - b'a' + 10) as u32),
        b'A'..=b'F' => Some((c - b'A' + 10) as u32),
        _ => None,
    }
}

/// `0[xX](hex.maybe)[pP]exp` の本体部分（0x 以降）を評価する。
/// TS の numVal は `/^0[xX]([0-9a-fA-F]*\.?[0-9a-fA-F]+)(?:[pP]([+-]?\d+))?$/`
/// で完全一致を検証し、不一致なら NaN を返す。ここもそれと同一の境界で NaN を返す
/// （例: 小数指数 `0x1p1.5`、末尾ドット `0x1.`、hex 以外の文字は NaN）。
#[expect(
    clippy::expect_used,
    reason = "The exponent is checked for nonempty ASCII decimal digits immediately before parsing; this is an internal invariant, not input recovery"
)]
fn hex_num_val(rest: &str) -> f64 {
    let (mant, exp_opt) = match rest.find(['p', 'P']) {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };
    // マンティサ部分を 1 パスで評価。'.' は高々 1 個、全桁が hex、末尾に hex 桁が必須。
    let mut v: f64 = 0.0;
    let mut seendot = false;
    let mut any_hex = false;
    let mut n_frac = 0u32;
    for c in mant.bytes() {
        match c {
            b'.' => {
                if seendot {
                    return f64::NAN; // '.' が 2 つ以上（正規表現に一致しない）
                }
                seendot = true;
            }
            _ => match hex_digit(c) {
                Some(d) => {
                    any_hex = true;
                    if seendot {
                        n_frac += 1;
                        v += d as f64 / 16.0_f64.powi(n_frac as i32);
                    } else {
                        v = v * 16.0 + d as f64;
                    }
                }
                None => return f64::NAN, // hex 以外の文字（TS は正規表現で NaN）
            },
        }
    }
    if !any_hex || (seendot && n_frac == 0) {
        return f64::NAN; // 1 桁もない / 末尾ドット（正規表現に一致しない）
    }
    // 指数: [+-]?\d+（整数のみ。小数は TS で NaN）。
    let e: f64 = match exp_opt {
        None => 0.0,
        Some(s) => {
            let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
            if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
                return f64::NAN;
            }
            let negative = s.starts_with('-');
            let val: f64 = digits.parse().expect("指数は数字のみ");
            if negative {
                -val
            } else {
                val
            }
        }
    };
    // TS は `2 ** Number(m[2])`（整数指数は exact）。powf(e) は 2^integer なので 1-ulp では
    // ずれない（subnormal の 2^-1074 等は 373 件のゴールデンで確認済み）。
    v * 2.0_f64.powf(e)
}

/// f64 の最短ラウンドトリップ桁列（符号なし・正の数）を
/// `(digits, exp)` で返す。値 = d0.d1…dk × 10^exp（{:e} と同じ規約）。
///
/// 前提: Rust `{:e}` は Ryu 系の最短ラウンドトリップ桁を生成し、JS の
/// Number.prototype.toString と同じ桁列になる（数値ゴールデン 348 件 + probe
/// 5156 件で実証済み）。万一ここが食い違う入力が見つかったら、まず本関数を
/// 疑うこと（その時は ECMAScript の Number::toString アルゴリズムを再現する）。
#[expect(
    clippy::expect_used,
    reason = "Only finite values reach this helper; Rust LowerExp supplies a bounded decimal exponent and the separator by construction"
)]
fn shortest_digits(v: f64) -> (Vec<u8>, i32) {
    let s = format!("{:e}", v.abs());
    let (mant, exp) = s.split_once('e').expect("Rust {:e} は必ず指数を持つ");
    let exp: i32 = exp.parse().expect("指数を i32 でパース");
    let digits: Vec<u8> = mant
        .bytes()
        .filter(|c| *c != b'.')
        .map(|c| c - b'0')
        .collect();
    (digits, exp)
}

fn digits_to_string(digits: &[u8]) -> String {
    digits.iter().map(|d| (b'0' + d) as char).collect()
}

/// JS の Number.prototype.toString 相当（最短ラウンドトリップ、固定/指数表記）。
fn es_to_string(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v == f64::INFINITY {
        return "Infinity".into();
    }
    if v == f64::NEG_INFINITY {
        return "-Infinity".into();
    }
    if v == 0.0 {
        return "0".into(); // JS は -0 も "0"。
    }
    let (digits, exp) = shortest_digits(v);
    decimal_from_digits(v < 0.0, &digits, exp)
}

fn decimal_from_digits(neg: bool, digits: &[u8], exp: i32) -> String {
    let k = digits.len() as i32;
    let body = if (-6..=20).contains(&exp) {
        if exp >= k {
            // 整数に桁が足りない → ゼロ埋め（整数部は exp+1 桁）
            digits_to_string(digits) + &"0".repeat((exp - k + 1) as usize)
        } else if exp >= 0 {
            // 小数部あり（exp+1 == k のときは整数で小数点は不要）
            let mut s = digits_to_string(digits);
            if exp + 1 < k {
                s.insert((exp + 1) as usize, '.');
            }
            s
        } else {
            // 0.00…ddd
            let mut s = String::from("0.");
            s.push_str(&"0".repeat((-exp - 1) as usize));
            s.push_str(&digits_to_string(digits));
            s
        }
    } else {
        let mut s = digits_to_string(digits);
        if k > 1 {
            s.insert(1, '.');
        }
        s.push('e');
        s.push_str(&format!("{exp:+}"));
        s
    };
    if neg {
        format!("-{body}")
    } else {
        body
    }
}

/// JS の Number.prototype.toExponential（引数なし・最短）相当。
fn es_to_exponential(v: f64) -> String {
    let (digits, exp) = shortest_digits(v);
    exponential_from_digits(v < 0.0, &digits, exp)
}

fn exponential_from_digits(neg: bool, digits: &[u8], exp: i32) -> String {
    let k = digits.len();
    let mut s = String::from(if neg { "-" } else { "" });
    s.push((b'0' + digits[0]) as char);
    if k > 1 {
        s.push('.');
        s.push_str(&digits_to_string(&digits[1..]));
    }
    s.push('e');
    s.push_str(&format!("{exp:+}"));
    s
}

/// Numeric text canonicalization（旧TS正規表現とmigration parity確立済み）。
fn clean_num_text(s: &str) -> String {
    let mut s = s.to_lowercase();
    // TS の replace('e+','e') は先頭 1 回のみ（String::replacen(_,_,1) と同義）
    s = s.replacen("e+", "e", 1);
    // replace(/(\.\d*?[1-9])0+(?=e|$)/, '$1') — 小数部の末尾ゼロを除去。
    // 左端の '.' 以降で「非ゼロ桁 + ゼロ連続（e または文末の直前）」を 1 回だけ置換。
    if let Some(dot) = s.rfind('.') {
        let tail_end = s.find('e').unwrap_or(s.len());
        if tail_end > dot {
            let frac = &s[dot + 1..tail_end];
            let last_nz = frac.bytes().rposition(|c| c != b'0');
            if let Some(li) = last_nz {
                if li < frac.len() - 1 {
                    s = format!("{}{}{}", &s[..dot + 1], &frac[..=li], &s[tail_end..]);
                }
            }
        }
    }
    // replace(/\.0+(?=e|$)/, '') — 小数部がゼロのみなら `0` を残して '.' とゼロを除去。
    {
        let tail_end = s.find('e').unwrap_or(s.len());
        if let Some(dot) = s[..tail_end].rfind('.') {
            let frac = &s[dot + 1..tail_end];
            if !frac.is_empty() && frac.bytes().all(|c| c == b'0') {
                s = format!("{}{}", &s[..dot], &s[tail_end..]);
            }
        }
    }
    // replace(/^(-?)0\./, '$1.') — 先頭の "0." を "." に。
    if let Some(rest) = s.strip_prefix("-0.") {
        s = format!("-.{rest}");
    } else if let Some(rest) = s.strip_prefix("0.") {
        s = format!(".{rest}");
    }
    // exp ∈ [0,6] なら指数表記を桁展開（^(-?)(\d+)(?:e([+-]?\d+))$ 相当）。
    if let Some(ei) = s.rfind('e') {
        let (mant, exp) = s.split_at(ei);
        let exp = &exp[1..];
        let (sign, digits) = match mant.strip_prefix('-') {
            Some(r) => ("-", r),
            None => ("", mant),
        };
        if !digits.is_empty() && digits.bytes().all(|c| c.is_ascii_digit()) {
            if let Ok(n) = exp.parse::<i64>() {
                if (0..=6).contains(&n) {
                    s = format!("{sign}{digits}{}", "0".repeat(n as usize));
                }
            }
        }
    }
    s
}

/// JS の Number.isInteger 相当（有限かつ整数値）。
fn is_integer(v: f64) -> bool {
    v.is_finite() && v.fract() == 0.0
}

/// Uncached shortest numeric literal generation。
/// toPrecision(p) 候補は「最短ラウンドトリップ文字列より短い有効候補」を
/// 生み得ないため（p 桁は必ず最短桁以上、roundtrip 検証で落ちる）、
/// {String, toExponential} の 2 候補で同値の結果になる。決定論は
/// 数値ゴールデンで担保する。
fn short_num_uncached(v: f64) -> String {
    if v.is_nan() {
        return "0/0".into();
    }
    if v == f64::INFINITY {
        return "1/0".into();
    }
    if v == f64::NEG_INFINITY {
        return "-1/0".into();
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0" } else { "0" }.into();
    }
    if is_integer(v) {
        // Every signed 32-bit integer is exactly representable in binary64,
        // and its historical shortest decimal uses fixed notation. Avoid the
        // float formatter, digit vector and reconstruction for common literals.
        // Keep the two existing compact-positive-exponent exceptions verbatim.
        if (i32::MIN as f64..=i32::MAX as f64).contains(&v) {
            return match v {
                100_000.0 => "1e5".into(),
                1_000_000.0 => "1e6".into(),
                _ => (v as i32).to_string(),
            };
        }
        let d = es_to_string(v);
        // Only these two values use the exponential alternative below.
        // Generating it for every integer discarded the result after formatting.
        if matches!(v, 100_000.0 | 1_000_000.0) {
            let mut to_exp = es_to_exponential(v);
            if let Some(i) = to_exp.find(".0e") {
                to_exp.replace_range(i..i + 2, "");
            }
            let e = compact_exponent_text(&to_exp);
            // Keep the frozen numeric printer contract for ordinary values;
            // the release regression specifically requires the two forms that
            // were observed to grow (`1e5` and `1e6`) to retain their compact
            // spelling when they enter the optimizer as f64 values.
            if e.len() < d.len() {
                return e;
            }
        }
        return d;
    }
    // Both spellings use the same shortest round-trip decimal digits.
    // Format those digits once; do not run float-to-decimal twice per candidate.
    let (digits, exp) = shortest_digits(v);
    let c1 = clean_num_text(&decimal_from_digits(v < 0.0, &digits, exp));
    let c2 = clean_num_text(&exponential_from_digits(v < 0.0, &digits, exp));
    let mut best: Option<String> = None;
    // 同長時の tie-break で TS は sort(a.length-b.length || a.localeCompare(b)) を使うが、
    // 全候補は ASCII（数字・'.'・'-'・'e'・'+'）のみなので localeCompare はバイト辞書順と
    // 一致する。よって `c < *b` で同値。また候補は {String, toExponential} の 2 個で、
    // toPrecision(p) 候補は最短ラウンドトリップ桁より短い候補を生まない（roundtrip 検証で
    // 落ちる）ため不要（数値ゴールデン 373 件 + probe 5156 件で実証済み）。
    for c in [c1, c2] {
        if js_decimal(&c) == Some(v) {
            let take = match &best {
                None => true,
                Some(b) => c.len() < b.len() || (c.len() == b.len() && c < *b),
            };
            if take {
                best = Some(c);
            }
        }
    }
    best.unwrap_or_else(|| decimal_from_digits(v < 0.0, &digits, exp))
}

/// Exponential form used only as a size candidate.  Unlike the general
/// canonicalizer this deliberately does not expand small positive exponents:
/// `1e5` is shorter than `100000` and is a valid Lua literal.
fn compact_exponent_text(s: &str) -> String {
    let mut s = s.to_lowercase().replacen("e+", "e", 1);
    if let Some(i) = s.find(".0e") {
        s.replace_range(i..i + 2, "");
    }
    s
}

/// f64 をTS `shortNum` と同じ最短Lua数値表現へ変換する。
pub fn short_num(v: f64) -> String {
    short_num_uncached(v)
}

/// Lua 数値リテラルを最短路正規化する（normalizeNumLiteral 相当）。
pub fn normalize_num_literal(s: &str) -> String {
    let normalized = short_num(num_val(s));
    if normalized == "0/0" {
        return normalized;
    }
    // Canonicalization is an optimization, not a license to make a literal
    // larger.  In particular `1e5` is already shorter than `100000`.
    if normalized.len() > s.len() {
        s.to_string()
    } else {
        normalized
    }
}

/// Lua 文字列リテラルを JS 文字列へデコード（decodeLuaString 相当）。
/// 長い括弧 [==[ … ]==] は内容を切り出す。それ以外は JSON.parse を試み、
/// 失敗したら両端のクォートを外すだけにする（TS と同一の挙動）。
pub fn decode_lua_string(raw: &str) -> String {
    if let Some(rest) = raw.strip_prefix('[') {
        // Lua long strings remove one initial newline after the opening delimiter.
        let Some(level) = rest.find('[') else {
            return String::new();
        };
        let close = format!("]{}]", "=".repeat(level));
        let start = level + 2;
        let Some(end) = raw.rfind(&close) else {
            return String::new();
        };
        if end < start {
            return String::new();
        }
        let mut value = raw[start..end].to_string();
        if value.starts_with("\r\n") {
            value.drain(..2);
        } else if value.starts_with(['\n', '\r']) {
            value.drain(..1);
        }
        return value;
    }
    decode_quoted_lua_string(raw).unwrap_or_else(|| {
        if raw.len() >= 2 {
            raw[1..raw.len() - 1].to_string()
        } else {
            String::new()
        }
    })
}

/// Decode Lua's quoted-string escapes.  Keeping this separate from JSON parsing is
/// important: Lua accepts `\ddd` and `\xhh`, and single-quoted strings as well.
fn decode_quoted_lua_string(raw: &str) -> Option<String> {
    let mut chars = raw.chars();
    let quote = chars.next()?;
    if !matches!(quote, '\'' | '"') || !raw.ends_with(quote) {
        return None;
    }
    let mut out = String::new();
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if !escaped {
            if c == quote {
                break;
            }
            if c == '\\' {
                escaped = true;
            } else {
                out.push(c);
            }
            continue;
        }
        escaped = false;
        match c {
            'a' => out.push('\u{0007}'),
            'b' => out.push('\u{0008}'),
            'f' => out.push('\u{000c}'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'v' => out.push('\u{000b}'),
            '\\' => out.push('\\'),
            '\'' => out.push('\''),
            '"' => out.push('"'),
            '\n' => out.push('\n'),
            '\r' => {
                if chars.as_str().starts_with('\n') {
                    chars.next();
                }
                out.push('\n');
            }
            'z' => {
                while let Some(next) = chars.clone().next() {
                    if !next.is_whitespace() {
                        break;
                    }
                    chars.next();
                }
            }
            'x' => {
                let hi = chars.next()?.to_digit(16)?;
                let lo = chars.next()?.to_digit(16)?;
                out.push(char::from_u32(hi * 16 + lo)?);
            }
            c if c.is_ascii_digit() => {
                let mut value = c.to_digit(10)?;
                for _ in 0..2 {
                    let Some(next) = chars.clone().next() else {
                        break;
                    };
                    if !next.is_ascii_digit() {
                        break;
                    }
                    value = value * 10 + next.to_digit(10)?;
                    chars.next();
                }
                if value > 255 {
                    return None;
                }
                out.push(char::from_u32(value)?);
            }
            _ => return None,
        }
    }
    Some(out)
}

/// JSON 文字列（"…"）をデコードする最小実装。失敗時は None。
/// TS の JSON.parse が受ける文字列（Lua 文字列リテラル）のみを対象とするため、
/// Lua 独自の \ddd / \x エスケープはここでは受け付けない（TS と同じく失敗→フォールバック）。
#[allow(dead_code)]
fn json_parse_string(raw: &str) -> Option<String> {
    let mut chars = raw.chars();
    if chars.next()? != '"' {
        return None;
    }
    let mut out = String::new();
    let mut closed = false;
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                closed = true;
                break;
            }
            '\\' => {
                let e = chars.next()?;
                match e {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'b' => out.push('\u{0008}'),
                    'f' => out.push('\u{000C}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        let mut code = 0u32;
                        for _ in 0..4 {
                            let h = chars.next()?.to_digit(16)?;
                            code = code * 16 + h;
                        }
                        // サロゲートペア（高位サロゲートの直後に \uXXXX が続く場合のみ）
                        if (0xD800..=0xDBFF).contains(&code) {
                            if chars.next()? == '\\' && chars.next()? == 'u' {
                                let mut lo = 0u32;
                                for _ in 0..4 {
                                    let h = chars.next()?.to_digit(16)?;
                                    lo = lo * 16 + h;
                                }
                                // 低位サロゲート範囲の検証（不正なら fallback）。
                                // TS の JSON.parse は単独サロゲートを返すが、Rust は
                                // 単独サロゲートを String で保持できないため parity
                                // 対象外（lexer.rs と同じ方針）。ここで範囲を外れたら
                                // None を返し、クォート除去の fallback へ落ちる
                                // （未チェックのまま引くと u32 がアンダーフローする）。
                                if !(0xDC00..=0xDFFF).contains(&lo) {
                                    return None;
                                }
                                let c = 0x10000 + ((code - 0xD800) << 10) + (lo - 0xDC00);
                                out.push(char::from_u32(c)?);
                            } else {
                                return None;
                            }
                        } else {
                            out.push(char::from_u32(code)?);
                        }
                    }
                    _ => return None,
                }
            }
            c => out.push(c),
        }
    }
    if !closed || chars.next().is_some() {
        return None;
    }
    Some(out)
}

/// JSON.stringify(s) 相当（文字列のみ）。printer の qstr で使う。
fn json_stringify(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// JSON.stringify(s) 相当。TS `quoteLua` と同じく常に double-quoted form。
pub fn quote_lua(s: &str) -> String {
    json_stringify(s)
}

/// printer の qstr 相当。JSON 形式、シングルクォート形式、Lua long-bracket
/// 形式のうち最短の安全な表現を選ぶ。
/// 長さ比較は byte 長で行うが、JS の UTF-16 長と比較結果は一致する:
/// 複数バイト文字は両候補にそのまま現れ、差分となるエスケープ（`"`/`'`/`\\`/
/// `\n`/`\r`/制御文字）は全て ASCII 1 文字のため、候補間の長さ差は byte/UTF-16
/// どちらでも同値になる。
pub fn qstr(raw: &str) -> String {
    qstr_with_long_overhead(raw, 0)
}

/// A long string immediately after the opening bracket of an index would be
/// lexed as a call argument (`t[[...]]`) rather than an indexed expression.
/// The printer inserts one disambiguating space there, so account for that byte
/// before deciding whether the long-bracket spelling is actually shorter.
pub(crate) fn qstr_bracket_key(raw: &str) -> String {
    qstr_with_long_overhead(raw, 1)
}

fn qstr_with_long_overhead(raw: &str, long_overhead: usize) -> String {
    let value = decoded_quote_value(raw);
    let (quoted_len, quote) = quote_shape(&value);
    if let Some((long_len, level)) = shorter_long_bracket_shape(&value, quoted_len, long_overhead) {
        let mut out = String::with_capacity(long_len);
        out.push('[');
        out.extend(std::iter::repeat_n('=', level));
        out.push('[');
        out.push_str(&value);
        out.push(']');
        out.extend(std::iter::repeat_n('=', level));
        out.push(']');
        return out;
    }
    if quote == '"' {
        return json_stringify(&value);
    }
    let mut out = String::with_capacity(quoted_len);
    out.push('\'');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// Common API keys and generated payloads are already unescaped strings. Borrow
/// only when the entire quoted body is unambiguous; all other forms retain the
/// public decoder's existing escape, long-bracket and malformed-input behavior.
fn decoded_quote_value(raw: &str) -> Cow<'_, str> {
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 && matches!(bytes[0], b'\'' | b'"') && bytes.last() == Some(&bytes[0]) {
        let body = &raw[1..raw.len() - 1];
        if !body.bytes().any(|b| b == b'\\' || b == bytes[0]) {
            return Cow::Borrowed(body);
        }
    }
    Cow::Owned(decode_lua_string(raw))
}

/// Both quote candidates differ only in ASCII escaping. Count those bytes
/// without building either string, retaining the historical double-quote tie.
fn quote_shape(value: &str) -> (usize, char) {
    let mut double = value.len() + 2;
    let mut single = double;
    for b in value.bytes() {
        double += match b {
            b'"' | b'\\' | 8 | 12 | b'\n' | b'\r' | b'\t' => 1,
            0..=31 => 5,
            _ => 0,
        };
        single += usize::from(matches!(b, b'\\' | b'\'' | b'\n' | b'\r'));
    }
    if double <= single {
        (double, '"')
    } else {
        (single, '\'')
    }
}

/// Return the shortest safe Lua long-bracket delimiter for this exact value.
/// Lua normalizes CR/CRLF in long strings and removes one opening newline, so
/// those inputs must keep quoted syntax. Internal LF bytes are preserved.
fn shorter_long_bracket_shape(
    value: &str,
    quoted_len: usize,
    extra_len: usize,
) -> Option<(usize, usize)> {
    let mut long_len = value.len() + 4;
    if long_len + extra_len >= quoted_len || value.starts_with('\n') || value.contains('\r') {
        return None;
    }
    let mut level = 0usize;
    loop {
        let close = format!("]{}]", "=".repeat(level));
        // The chosen delimiter must not appear inside the payload, and its
        // prefix must not straddle the payload/closing-delimiter boundary.
        // For example, `[[x]]]` closes at the last payload `]` plus the first
        // delimiter `]`, losing that payload byte. At level 1 the analogous
        // unsafe suffix is `]=`, at level 2 `]==`, and so on.
        let close_prefix = &close[..close.len() - 1];
        if !value.contains(&close) && !value.ends_with(close_prefix) {
            return Some((long_len, level));
        }
        level += 1;
        long_len += 2;
        if long_len + extra_len >= quoted_len {
            return None;
        }
    }
}

/// Compact printer byte length and boundary characters for the size model.
/// This is the same quote/long-bracket selection as qstr, not a second escaping policy.
pub(crate) fn qstr_shape(raw: &str) -> (usize, char, char) {
    qstr_shape_with_long_overhead(raw, 0)
}

pub(crate) fn qstr_bracket_key_shape(raw: &str) -> (usize, char, char) {
    qstr_shape_with_long_overhead(raw, 1)
}

fn qstr_shape_with_long_overhead(raw: &str, long_overhead: usize) -> (usize, char, char) {
    let value = decoded_quote_value(raw);
    let (quoted_len, quote) = quote_shape(&value);
    if let Some((long_len, _)) = shorter_long_bracket_shape(&value, quoted_len, long_overhead) {
        return (long_len, '[', ']');
    }
    (quoted_len, quote, quote)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn n(s: &str) -> f64 {
        num_val(s)
    }

    #[test]
    fn num_val_basic() {
        assert_eq!(n("0"), 0.0);
        assert_eq!(n("1.5"), 1.5);
        assert_eq!(n("1e-3"), 0.001);
        assert_eq!(n("0xFF"), 255.0);
        assert_eq!(n("0x10"), 16.0);
        assert_eq!(n("0xA.8p2"), 42.0);
        assert_eq!(n("-5"), -5.0);
        assert!(n("0xzz").is_nan());
    }

    #[test]
    fn normalize_basic() {
        assert_eq!(normalize_num_literal("0"), "0");
        assert_eq!(normalize_num_literal("1"), "1");
        assert_eq!(normalize_num_literal("1.5"), "1.5");
        assert_eq!(normalize_num_literal("0.5"), ".5");
        assert_eq!(normalize_num_literal("1e5"), "1e5");
        assert_eq!(normalize_num_literal("1e-5"), "1e-5");
        assert_eq!(normalize_num_literal("0.0000396"), "3.96e-5");
        assert_eq!(normalize_num_literal("0.0005"), "5e-4");
        assert_eq!(normalize_num_literal("0.000621371"), ".000621371");
        assert_eq!(normalize_num_literal("0xFF"), "255");
        assert_eq!(normalize_num_literal("1e21"), "1e21");
        assert_eq!(normalize_num_literal("0.0"), "0");
        assert_eq!(normalize_num_literal("4290772991"), "4290772991");
    }

    #[test]
    fn decode_string_basic() {
        assert_eq!(decode_lua_string("\"abc\""), "abc");
        assert_eq!(decode_lua_string("\"a\\\"b\""), "a\"b");
        assert_eq!(decode_lua_string("'abc'"), "abc");
        assert_eq!(decode_lua_string("[==[abc]==]"), "abc");
        assert_eq!(decode_lua_string("[=[x]=]"), "x");
        assert_eq!(decode_lua_string("[==[x]]==]"), "x]");
        assert_eq!(decode_lua_string("\"a\\n b\""), "a\n b");
        // 不正な長い括弧は TS 同様 ""（panic しない）
        assert_eq!(decode_lua_string("[abc"), "");
        assert_eq!(decode_lua_string("[["), "");
        // 単独サロゲートを含む JSON 文字列は panic せず fallback（パリティ対象外）
        assert_eq!(decode_lua_string("\"\\ud800\""), "\\ud800");
        assert_eq!(decode_lua_string("\"\\ud800\\u0000\""), "\\ud800\\u0000");
        assert_eq!(decode_lua_string("\"\\udc00\""), "\\udc00");
    }

    #[test]
    fn hex_num_val_strict() {
        // 妥当な hex（小数 p 表記含む）は TS と同じ値を返す
        assert_eq!(normalize_num_literal("0x1p-1074"), "5e-324");
        assert_eq!(normalize_num_literal("0xAp-2"), "2.5");
        assert_eq!(normalize_num_literal("0x1.8p1"), "3");
        // The exact decimal spelling is longer; retaining the valid source
        // literal is the non-expanding canonical choice.
        assert_eq!(
            normalize_num_literal("0x1.fffffffffffffp1023"),
            "0x1.fffffffffffffp1023"
        );
        // TS の正規表現が一致せず NaN になる入力は "0/0"（寛容に値を返してはならない）
        assert_eq!(normalize_num_literal("0x1p"), "0/0"); // 指数なし 'p'
        assert_eq!(normalize_num_literal("0x1p1.5"), "0/0"); // 小数指数
        assert_eq!(normalize_num_literal("0x1pp2"), "0/0"); // 指数に 'p'
        assert_eq!(normalize_num_literal("0x1.5.2"), "0/0"); // '.' が 2 つ
        assert_eq!(normalize_num_literal("0x1."), "0/0"); // 末尾ドット
        assert_eq!(normalize_num_literal("0x"), "0/0"); // 桁なし
        assert_eq!(normalize_num_literal("0x1g"), "0/0"); // hex 以外の文字
    }

    #[test]
    fn qstr_basic() {
        assert_eq!(qstr("\"abc\""), "\"abc\"");
        assert_eq!(qstr("\"a\\\"b\""), "'a\"b'");
        assert_eq!(qstr("'it\\'s'"), "\"it's\"");
        // 4 バイト文字は両候補にそのまま現れ、byte/UTF-16 長の比較結果は一致する
        assert_eq!(qstr("\"😀\""), "\"😀\"");
        assert_eq!(qstr("\"a😀b\""), "\"a😀b\"");
        assert_eq!(decode_lua_string("\"\\x41\""), "A");
        assert_eq!(decode_lua_string("\"\\65\""), "A");
    }

    #[test]
    fn qstr_control_and_separators() {
        // JS JSON.stringify は NUL/制御文字を \uXXXX にエスケープするが、U+2028/2029 は
        // 生のまま残す（ES2019 以降）。これが byte/UTF-16 長の比較に入り、一方の候補が
        // 選ばれる。以下は dist の実 TS（decodeLuaString + qstr）の実測を逐語的に
        // 固定した期待値。
        // NUL/制御: シングルクォート形（\uXXXX を使わない）が最短
        assert_eq!(qstr("\"\u{0}\""), "'\u{0}'");
        assert_eq!(qstr("\"\u{1b}\""), "'\u{1b}'");
        assert_eq!(qstr("\"\u{1}a\u{2}\""), "'\u{1}a\u{2}'");
        // タブ: JSON 形は \t の 4 バイト、シングル形は 3 バイト → シングル
        assert_eq!(qstr("\"\t\""), "'\t'");
        // U+2028/2029: どちらも 3 バイト（utf-8）+ クォート 2 = 5 バイトで同長 → JSON 形
        assert_eq!(qstr("\"\u{2028}\""), "\"\u{2028}\"");
        assert_eq!(qstr("\"\u{2029}\""), "\"\u{2029}\"");
    }

    // Keep a structurally independent candidate builder here so a shared
    // length-model error cannot validate itself.
    fn assert_quote_parity(raw: &str) {
        let value = decode_lua_string(raw);
        let double = json_stringify(&value);
        let single = format!(
            "'{}'",
            value
                .replace('\\', "\\\\")
                .replace('\'', "\\'")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
        );
        let mut expected = if double.len() <= single.len() {
            double
        } else {
            single
        };
        if !value.contains('\r') && !value.starts_with('\n') {
            for level in 0..=value.len() {
                let equals = "=".repeat(level);
                let close = format!("]{equals}]");
                let close_prefix = &close[..close.len() - 1];
                if value.contains(&close) || value.ends_with(close_prefix) {
                    continue;
                }
                let long = format!("[{equals}[{value}]{equals}]");
                if long.len() < expected.len() {
                    expected = long;
                }
                break;
            }
        }
        let actual = qstr(raw);
        assert_eq!(actual, expected, "raw string: {raw:?}");
        let (length, first, last) = qstr_shape(raw);
        assert_eq!(length, expected.len(), "raw string: {raw:?}");
        assert_eq!(Some(first), expected.chars().next());
        assert_eq!(Some(last), expected.chars().next_back());
    }

    #[test]
    fn qstr_uses_long_brackets_only_when_strictly_shorter_and_safe() {
        let plain = r#"a\\b\\c\\d\\e"#;
        assert_eq!(qstr(&quote_lua(plain)), format!("[[{plain}]]"));

        let nested = r#"\\a\\b\\c\\d]]\\e\\f\\g\\h"#;
        assert_eq!(qstr(&quote_lua(nested)), format!("[=[{nested}]=]"));

        for value in ["\na\\b\\c\\d\\e", "a\\b\r\\c\\d\\e"] {
            let output = qstr(&quote_lua(value));
            assert!(!output.starts_with('['), "{output:?}");
            assert_eq!(decode_lua_string(&output), value);
        }
    }

    #[test]
    fn quote_shape_matches_legacy_ascii_pairs_and_unicode() {
        for a in 0u8..=127 {
            for b in 0u8..=127 {
                let value = format!("{}{}", char::from(a), char::from(b));
                assert_quote_parity(&quote_lua(&value));
                assert_quote_parity(&format!("[=[{value}]=]"));
            }
        }
        for value in [
            "雪😀",
            "é\u{2028}\u{2029}",
            "'😀\"",
            "\u{80}\u{7ff}\u{ffff}",
            "a\0b\t\u{7}\u{b}\u{1b}",
            "\r\n",
            "",
            "'\"\\",
        ] {
            assert_quote_parity(&quote_lua(value));
            assert_quote_parity(&format!("[==[\r\n{value}]==]"));
        }
    }

    #[test]
    fn quote_borrowing_preserves_escape_and_malformed_boundaries() {
        for raw in ["\"plain\"", "'plain'", "\"\"", "'😀'", "\"it's\""] {
            assert!(matches!(decoded_quote_value(raw), Cow::Borrowed(_)));
            assert_quote_parity(raw);
        }
        for raw in [
            r#""a\"b""#,
            r#"'it\'s'"#,
            r#""\x41\065\0""#,
            r#""\z  a""#,
            "'a\\\r\nb'",
            r#""\2559""#,
            r#""\256""#,
            r#""\q""#,
            r#""a"ignored""#,
            "'a'ignored'",
            "[=[\nabc]=]",
            "[==[\rabc]==]",
            "[=[unterminated",
            "[[",
            "[abc",
            "",
            "x",
        ] {
            assert!(matches!(decoded_quote_value(raw), Cow::Owned(_)));
            assert_quote_parity(raw);
        }
    }

    #[test]
    fn quote_shape_matches_large_generated_payloads() {
        let mut state = 0x9e37_79b9u32;
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!#$%&()*+,-./:;<=>?@[]^_`{|}~";
        let mut value = String::new();
        for index in 0..65_536 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            value.push(char::from(alphabet[state as usize % alphabet.len()]));
            if matches!(index, 31 | 255 | 4095 | 65535) {
                assert_quote_parity(&quote_lua(&value));
                assert_quote_parity(&format!("[==[{value}]==]"));
            }
        }
        for fragment in ["'\"\\\n\r\t", "雪😀\u{1b}", "'''\"", "\"\"\"'"] {
            assert_quote_parity(&quote_lua(&fragment.repeat(4096)));
        }
    }

    #[test]
    fn integer_exponent_fast_path_preserves_old_selection() {
        let mut values = vec![
            -0.0,
            0.0,
            100_000.0,
            1_000_000.0,
            -100_000.0,
            -1_000_000.0,
            9_007_199_254_740_991.0,
            9_007_199_254_740_992.0,
            f64::MAX,
        ];
        for exponent in 0..=308 {
            let value = 10f64.powi(exponent);
            for value in [value - 1.0, value, value + 1.0, -value] {
                if is_integer(value) {
                    values.push(value);
                }
            }
        }
        values.extend((-4096..=4096).map(f64::from));
        for value in values {
            let expected = if value == 0.0 {
                if value.is_sign_negative() {
                    "-0".into()
                } else {
                    "0".into()
                }
            } else {
                let decimal = es_to_string(value);
                let exponent = compact_exponent_text(&es_to_exponential(value));
                if matches!(value, 100_000.0 | 1_000_000.0) && exponent.len() < decimal.len() {
                    exponent
                } else {
                    decimal
                }
            };
            assert_eq!(short_num(value), expected, "{value}");
        }
    }

    #[test]
    fn shared_fraction_digits_match_independent_legacy_candidates() {
        let mut state = 0x243f_6a88_85a3_08d3u64;
        let mut checked = 0;
        for _ in 0..32_768 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let value = f64::from_bits(state);
            if !value.is_finite() || is_integer(value) {
                continue;
            }
            let mut candidates = [
                clean_num_text(&es_to_string(value)),
                clean_num_text(&es_to_exponential(value)),
            ];
            candidates.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
            let expected = candidates
                .into_iter()
                .find(|candidate| js_decimal(candidate) == Some(value))
                .unwrap_or_else(|| es_to_string(value));
            assert_eq!(short_num(value), expected, "bits={state:016x}");
            checked += 1;
        }
        assert!(checked > 15_000);
    }
}

#[cfg(test)]
mod small_integer_format_tests {
    use super::*;
    fn previous(v: f64) -> String {
        if v == 0.0 {
            return if v.is_sign_negative() {
                "-0".into()
            } else {
                "0".into()
            };
        }
        let decimal = es_to_string(v);
        if matches!(v, 100_000.0 | 1_000_000.0) {
            let exponent = compact_exponent_text(&es_to_exponential(v));
            if exponent.len() < decimal.len() {
                return exponent;
            }
        }
        decimal
    }
    #[test]
    fn integer_shortcut_matches_independent_float_digit_formatting() {
        for value in -8192..=8192 {
            let v = value as f64;
            assert_eq!(short_num(v), previous(v));
        }
        for center in [
            i32::MIN as f64,
            i32::MAX as f64,
            100_000.0,
            1_000_000.0,
            -100_000.0,
            -1_000_000.0,
            9_007_199_254_740_992.0,
        ] {
            for delta in -4..=4 {
                let v = center + delta as f64;
                assert_eq!(short_num(v), previous(v));
            }
        }
        let mut state = 0x243f_6a88u32;
        for _ in 0..65_536 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let v = (state as i32) as f64;
            assert_eq!(short_num(v), previous(v), "{v}");
        }
        assert_eq!(short_num(-0.0), "-0");
        assert_eq!(short_num(0.0), "0");
        assert_eq!(short_num(f64::NAN), "0/0");
        assert_eq!(short_num(f64::INFINITY), "1/0");
        assert_eq!(short_num(f64::NEG_INFINITY), "-1/0");
    }
}
