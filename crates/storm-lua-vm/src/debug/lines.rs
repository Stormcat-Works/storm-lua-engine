//! Lua 5.3のバイトコードダンプから、全関数の行情報を読み取ります。
//!
//! `debug.getinfo`の`activelines`は関数1つぶんしか返さず、入れ子の関数の行を含みません。
//! ここではコンパイル結果のダンプ（非strip）を走査し、入れ子を含む全関数の行情報を集めます。
//! 形式が想定と違う場合は、空の一覧ではなくエラーを返します。
use std::collections::BTreeSet;

const MALFORMED: &str = "unexpected Lua bytecode layout";
const MAX_DEPTH: usize = 256;

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    int: usize,
    size_t: usize,
    instruction: usize,
    integer: usize,
    number: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], &'static str> {
        let end = self.at.checked_add(length).ok_or(MALFORMED)?;
        let slice = self.bytes.get(self.at..end).ok_or(MALFORMED)?;
        self.at = end;
        Ok(slice)
    }
    fn byte(&mut self) -> Result<u8, &'static str> {
        Ok(self.take(1)?[0])
    }
    /// ダンプはVMと同じエンディアン・同じ幅で書かれます（同一プロセス内でのみ使用）。
    fn unsigned(&mut self, width: usize) -> Result<usize, &'static str> {
        let raw = self.take(width)?;
        let mut buffer = [0_u8; 8];
        if width > 8 {
            return Err(MALFORMED);
        }
        if cfg!(target_endian = "little") {
            buffer[..width].copy_from_slice(raw);
            usize::try_from(u64::from_le_bytes(buffer)).map_err(|_| MALFORMED)
        } else {
            buffer[8 - width..].copy_from_slice(raw);
            usize::try_from(u64::from_be_bytes(buffer)).map_err(|_| MALFORMED)
        }
    }
    fn count(&mut self) -> Result<usize, &'static str> {
        let width = self.int;
        self.unsigned(width)
    }
    fn skip_string(&mut self) -> Result<(), &'static str> {
        let mut size = usize::from(self.byte()?);
        if size == 0xFF {
            let width = self.size_t;
            size = self.unsigned(width)?;
        }
        if size > 0 {
            self.take(size - 1)?;
        }
        Ok(())
    }
    fn function(&mut self, lines: &mut BTreeSet<u32>, depth: usize) -> Result<(), &'static str> {
        if depth > MAX_DEPTH {
            return Err(MALFORMED);
        }
        self.skip_string()?; // source
        self.take(self.int * 2)?; // linedefined, lastlinedefined
        self.take(3)?; // numparams, is_vararg, maxstacksize
        let code = self.count()?;
        self.take(code.checked_mul(self.instruction).ok_or(MALFORMED)?)?;
        for _ in 0..self.count()? {
            match self.byte()? {
                0 => {}
                1 => {
                    self.byte()?;
                }
                3 => {
                    self.take(self.number)?;
                }
                0x13 => {
                    self.take(self.integer)?;
                }
                4 | 0x14 => self.skip_string()?,
                _ => return Err(MALFORMED),
            }
        }
        let upvalues = self.count()?;
        self.take(upvalues.checked_mul(2).ok_or(MALFORMED)?)?;
        for _ in 0..self.count()? {
            self.function(lines, depth + 1)?;
        }
        let line_count = self.count()?;
        // 非stripのダンプは、命令1つにつき行を1つ持ちます。
        if line_count != code {
            return Err(MALFORMED);
        }
        for _ in 0..line_count {
            let line = self.count()?;
            if let Some(line) = u32::try_from(line).ok().filter(|line| *line > 0) {
                lines.insert(line);
            }
        }
        for _ in 0..self.count()? {
            self.skip_string()?;
            self.take(self.int * 2)?;
        }
        for _ in 0..self.count()? {
            self.skip_string()?;
        }
        Ok(())
    }
}

/// 非stripのLua 5.3ダンプに含まれる、全関数の命令の行を昇順で返します。
pub(super) fn active_lines(dump: &[u8]) -> Result<Vec<u32>, &'static str> {
    let mut reader = Reader {
        bytes: dump,
        at: 0,
        int: 0,
        size_t: 0,
        instruction: 0,
        integer: 0,
        number: 0,
    };
    // 署名、バージョン0x53、公式フォーマット0、変換検査用の6バイト。
    if reader.take(4)? != b"\x1bLua" || reader.take(2)? != [0x53, 0] {
        return Err(MALFORMED);
    }
    if reader.take(6)? != b"\x19\x93\r\n\x1a\n" {
        return Err(MALFORMED);
    }
    reader.int = usize::from(reader.byte()?);
    reader.size_t = usize::from(reader.byte()?);
    reader.instruction = usize::from(reader.byte()?);
    reader.integer = usize::from(reader.byte()?);
    reader.number = usize::from(reader.byte()?);
    if !(matches!(reader.int, 4 | 8) && matches!(reader.size_t, 4 | 8))
        || reader.instruction != 4
        || reader.integer != 8
        || reader.number != 8
    {
        return Err(MALFORMED);
    }
    reader.take(reader.integer + reader.number)?; // 0x5678 と 370.5 の検査値
    reader.byte()?; // メイン関数のupvalue数
    let mut lines = BTreeSet::new();
    reader.function(&mut lines, 0)?;
    if reader.at != dump.len() {
        return Err(MALFORMED);
    }
    Ok(lines.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dump(source: &str) -> mlua::Result<Vec<u8>> {
        Ok(mlua::Lua::new().load(source).into_function()?.dump(false))
    }

    #[test]
    fn nested_functions_constants_and_long_strings_are_walked() -> mlua::Result<()> {
        let long = "x".repeat(300);
        let source = format!(
            "local a = 1\n\n-- comment\nlocal function f(n)\n  local s = '{long}'\n  return function()\n    return n + 1.5, s, nil, true\n  end\nend\nreturn f(a)\n"
        );
        assert_eq!(
            active_lines(&dump(&source)?),
            // 関数を作る命令は、その関数のendの行に付きます（4行目と6行目には命令がありません）。
            Ok(vec![1, 5, 7, 8, 9, 10])
        );
        Ok(())
    }

    #[test]
    fn truncated_stripped_or_foreign_dumps_are_rejected_not_reported_as_empty() -> mlua::Result<()>
    {
        let good = dump("local a = 1\nreturn a")?;
        assert!(active_lines(&good).is_ok());
        for cut in [0, 3, 12, good.len() / 2, good.len() - 1] {
            assert_eq!(active_lines(&good[..cut]), Err(MALFORMED), "{cut}");
        }
        let mut trailing = good.clone();
        trailing.push(0);
        assert_eq!(active_lines(&trailing), Err(MALFORMED));
        let mut version = good.clone();
        version[4] = 0x54;
        assert_eq!(active_lines(&version), Err(MALFORMED));
        let stripped = mlua::Lua::new()
            .load("local a = 1\nreturn a")
            .into_function()?
            .dump(true);
        assert_eq!(active_lines(&stripped), Err(MALFORMED));
        Ok(())
    }
}
