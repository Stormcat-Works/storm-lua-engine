//! Bounded Lua-pattern counting for LB directives. This is not a Lua source parser.
#[derive(Clone)]
struct Capture {
    start: usize,
    end: Option<usize>,
    position: bool,
}
struct Matcher<'a> {
    text: &'a [u8],
    pattern: &'a [u8],
    remaining: usize,
}
fn class(c: u8, p: u8) -> bool {
    let yes = match p.to_ascii_lowercase() {
        b'a' => c.is_ascii_alphabetic(),
        b'c' => c.is_ascii_control(),
        b'd' => c.is_ascii_digit(),
        b'g' => c.is_ascii_graphic(),
        b'l' => c.is_ascii_lowercase(),
        b'p' => c.is_ascii_punctuation(),
        b's' => c.is_ascii_whitespace(),
        b'u' => c.is_ascii_uppercase(),
        b'w' => c.is_ascii_alphanumeric(),
        b'x' => c.is_ascii_hexdigit(),
        b'z' => c == 0,
        _ => return c == p,
    };
    if p.is_ascii_uppercase() {
        !yes
    } else {
        yes
    }
}
impl Matcher<'_> {
    fn spend(&mut self) -> Result<(), String> {
        if self.remaining == 0 {
            return Err("LB pattern work budget exceeded".into());
        }
        self.remaining -= 1;
        Ok(())
    }
    fn set_end(&self, p: usize) -> Result<usize, String> {
        let mut i = p + 1;
        if self.pattern.get(i) == Some(&b'^') {
            i += 1;
        }
        if self.pattern.get(i) == Some(&b']') {
            i += 1;
        }
        while i < self.pattern.len() {
            if self.pattern[i] == b']' {
                return Ok(i + 1);
            }
            if self.pattern[i] == b'%' {
                i += 1;
            }
            i += 1;
        }
        Err("unclosed LB pattern character set".into())
    }
    fn atom_end(&self, p: usize) -> Result<usize, String> {
        match self.pattern.get(p) {
            Some(b'%') => {
                if p + 1 < self.pattern.len() {
                    Ok(p + 2)
                } else {
                    Err("unfinished LB pattern escape".into())
                }
            }
            Some(b'[') => self.set_end(p),
            Some(_) => Ok(p + 1),
            None => Err("missing pattern atom".into()),
        }
    }
    fn in_set(&self, c: u8, p: usize, end: usize) -> bool {
        let mut i = p + 1;
        let negate = self.pattern.get(i) == Some(&b'^');
        if negate {
            i += 1;
        }
        let mut found = false;
        while i + 1 < end {
            if self.pattern[i] == b'%' && i + 1 < end - 1 {
                found |= class(c, self.pattern[i + 1]);
                i += 2;
            } else if i + 2 < end - 1 && self.pattern[i + 1] == b'-' {
                found |= (self.pattern[i]..=self.pattern[i + 2]).contains(&c);
                i += 3;
            } else {
                found |= c == self.pattern[i];
                i += 1;
            }
        }
        found != negate
    }
    fn single(&self, c: u8, p: usize, end: usize) -> bool {
        match self.pattern[p] {
            b'.' => true,
            b'%' => class(c, self.pattern[p + 1]),
            b'[' => self.in_set(c, p, end),
            v => v == c,
        }
    }
    fn run(
        &mut self,
        mut s: usize,
        mut p: usize,
        mut captures: Vec<Capture>,
        depth: usize,
    ) -> Result<Option<usize>, String> {
        if depth > 128 {
            return Err("LB pattern nesting exceeds 128".into());
        }
        loop {
            self.spend()?;
            if p == self.pattern.len() {
                if captures.iter().any(|c| c.end.is_none() && !c.position) {
                    return Err("unclosed LB pattern capture".into());
                }
                return Ok(Some(s));
            }
            let c = self.pattern[p];
            if c == b'(' {
                if captures.len() >= 32 {
                    return Err("too many LB pattern captures".into());
                }
                let position = self.pattern.get(p + 1) == Some(&b')');
                captures.push(Capture {
                    start: s,
                    end: position.then_some(s),
                    position,
                });
                return self.run(s, p + if position { 2 } else { 1 }, captures, depth + 1);
            }
            if c == b')' {
                let Some(index) = captures.iter().rposition(|c| c.end.is_none()) else {
                    return Err("unmatched LB pattern capture".into());
                };
                captures[index].end = Some(s);
                p += 1;
                continue;
            }
            if c == b'$' && p + 1 == self.pattern.len() {
                return Ok((s == self.text.len()).then_some(s));
            }
            if c == b'%' {
                match self.pattern.get(p + 1).copied() {
                    Some(b'b') => {
                        let (Some(&open), Some(&close)) =
                            (self.pattern.get(p + 2), self.pattern.get(p + 3))
                        else {
                            return Err("balanced pattern needs two bytes".into());
                        };
                        if self.text.get(s) != Some(&open) {
                            return Ok(None);
                        }
                        let mut balance = 1;
                        let mut at = s + 1;
                        while at < self.text.len() {
                            self.spend()?;
                            if self.text[at] == close {
                                balance -= 1;
                                if balance == 0 {
                                    break;
                                }
                            } else if self.text[at] == open {
                                balance += 1;
                            }
                            at += 1;
                        }
                        if balance != 0 {
                            return Ok(None);
                        }
                        s = at + 1;
                        p += 4;
                        continue;
                    }
                    Some(b'f') => {
                        if self.pattern.get(p + 2) != Some(&b'[') {
                            return Err("frontier needs a character set".into());
                        }
                        let end = self.set_end(p + 2)?;
                        let previous = if s == 0 { 0 } else { self.text[s - 1] };
                        let current = self.text.get(s).copied().unwrap_or(0);
                        if self.in_set(previous, p + 2, end) || !self.in_set(current, p + 2, end) {
                            return Ok(None);
                        }
                        p = end;
                        continue;
                    }
                    Some(n @ b'1'..=b'9') => {
                        let Some(cap) = captures.get((n - b'1') as usize) else {
                            return Err("invalid LB pattern capture index".into());
                        };
                        let end = cap.end.ok_or("unfinished referenced capture")?;
                        if cap.position {
                            return Err("position capture cannot be a backreference".into());
                        }
                        let data = &self.text[cap.start..end];
                        if !self.text[s..].starts_with(data) {
                            return Ok(None);
                        }
                        s += data.len();
                        p += 2;
                        continue;
                    }
                    _ => {}
                }
            }
            let end = self.atom_end(p)?;
            let first = s < self.text.len() && self.single(self.text[s], p, end);
            match self.pattern.get(end).copied() {
                Some(b'?') => {
                    if first {
                        if let Some(result) =
                            self.run(s + 1, end + 1, captures.clone(), depth + 1)?
                        {
                            return Ok(Some(result));
                        }
                    }
                    p = end + 1;
                }
                Some(kind @ (b'*' | b'+' | b'-')) => {
                    if kind == b'+' && !first {
                        return Ok(None);
                    }
                    let minimum = s + usize::from(kind == b'+');
                    if kind == b'-' {
                        let mut at = s;
                        loop {
                            if let Some(result) =
                                self.run(at, end + 1, captures.clone(), depth + 1)?
                            {
                                return Ok(Some(result));
                            }
                            if at == self.text.len() || !self.single(self.text[at], p, end) {
                                break;
                            }
                            self.spend()?;
                            at += 1;
                        }
                        return Ok(None);
                    }
                    let mut limit = minimum;
                    while limit < self.text.len() && self.single(self.text[limit], p, end) {
                        self.spend()?;
                        limit += 1;
                    }
                    for at in (minimum..=limit).rev() {
                        if let Some(result) = self.run(at, end + 1, captures.clone(), depth + 1)? {
                            return Ok(Some(result));
                        }
                    }
                    return Ok(None);
                }
                _ => {
                    if !first {
                        return Ok(None);
                    }
                    s += 1;
                    p = end;
                }
            }
        }
    }
}
/// Non-overlapping replacement count, including Lua's empty-match progression rule.
pub(crate) fn count(text: &[u8], pattern: &str) -> Result<usize, String> {
    if pattern.len() > 1024 {
        return Err("LB pattern exceeds 1024 bytes".into());
    }
    let anchored = pattern.starts_with('^');
    let pattern = if anchored {
        &pattern.as_bytes()[1..]
    } else {
        pattern.as_bytes()
    };
    let mut matcher = Matcher {
        text,
        pattern,
        remaining: 4_000_000,
    };
    let mut at = 0;
    let mut count = 0;
    let mut last_end = None;
    loop {
        if let Some(end) = matcher.run(at, 0, vec![], 0)? {
            if last_end != Some(end) {
                count += 1;
                last_end = Some(end);
                if end > at {
                    at = end;
                    if anchored {
                        break;
                    }
                    continue;
                }
            }
        }
        if anchored || at >= text.len() {
            break;
        }
        at += 1;
    }
    Ok(count)
}
#[cfg(test)]
mod tests {
    use super::count;
    #[test]
    fn classes_quantifiers_captures_and_frontiers() {
        assert_eq!(
            count(b"foo food (a(b)c) xx xx", "%f[%a]foo%f[%A]").ok(),
            Some(1)
        );
        assert_eq!(count(b"(a(b)c)(d)", "%b()").ok(), Some(2));
        assert_eq!(count(b"abc abc def xyz", "(%a+)%s+%1").ok(), Some(1));
        assert_eq!(count(b"<a><b>", "<.->").ok(), Some(2));
        assert_eq!(count(b"a12 b9", "%a%d+").ok(), Some(2));
        assert_eq!(count(b"abc", "()").ok(), Some(4));
        assert_eq!(count(b"aaa", "a*").ok(), Some(1));
        assert!(count(b"a", "[").is_err());
        assert!(count(b"a", "(a").is_err());
    }
}
