//! PII redaction for OCR text, window titles and transcripts.
//!
//! Regexes are compiled once. Numeric identifiers are found as digit tokens
//! (digits optionally separated by single spaces/dashes, optional trailing X),
//! then classified by structure, so `电话13812345678号` and `ID:11010119900307891X。`
//! are caught even without surrounding spaces (the old `\b`-anchored patterns
//! needed ASCII word boundaries).

use regex::Regex;
use std::sync::OnceLock;

struct Patterns {
    numeric: Regex,
    email: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        numeric: Regex::new(r"[0-9]+(?:[- ][0-9]+)*[Xx]?").expect("numeric regex"),
        email: Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+").expect("email regex"),
    })
}

fn all_digits(s: &str) -> bool { !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) }

/// Does this run of digit groups look like a sensitive identifier?
fn classify_groups(groups: &[&str]) -> bool {
    match groups {
        // Chinese ID: 17 digits + [0-9X]; Chinese mobile: 1[3-9] + 9 digits
        [g] => {
            let b = g.as_bytes();
            let (body, last) = match b.split_last() { Some((l, body)) => (body, *l), None => return false };
            if b.len() == 18 && body.iter().all(|c| c.is_ascii_digit()) && (last.is_ascii_digit() || last == b'X' || last == b'x') {
                return true;
            }
            b.len() == 11 && all_digits(g) && b[0] == b'1' && (b'3'..=b'9').contains(&b[1])
        }
        // SSN 123-45-6789
        [a, b, c] => a.len() == 3 && b.len() == 2 && c.len() == 4 && all_digits(a) && all_digits(b) && all_digits(c),
        // Card 1234 5678 9012 3456
        [a, b, c, d] => [a, b, c, d].iter().all(|g| g.len() == 4 && all_digits(g)),
        _ => false,
    }
}

/// Redact sensitive sub-sequences inside one digit token, keeping the rest.
fn redact_token(token: &str) -> String {
    // Split into groups while remembering the separator that followed each group.
    let mut groups: Vec<&str> = Vec::new();
    let mut seps: Vec<char> = Vec::new();
    let mut start = 0;
    for (i, c) in token.char_indices() {
        if c == '-' || c == ' ' {
            groups.push(&token[start..i]);
            seps.push(c);
            start = i + c.len_utf8();
        }
    }
    groups.push(&token[start..]);
    seps.push('\0');

    let n = groups.len();
    let mut redacted = vec![false; n];
    for size in [4usize, 3, 1] {
        if n < size { continue; }
        for i in 0..=n - size {
            if redacted[i..i + size].iter().any(|r| *r) { continue; }
            if classify_groups(&groups[i..i + size]) {
                for r in &mut redacted[i..i + size] { *r = true; }
            }
        }
    }
    if !redacted.iter().any(|r| *r) { return token.to_string(); }

    let mut out = String::with_capacity(token.len());
    let mut i = 0;
    while i < n {
        if redacted[i] {
            out.push_str("***");
            while i < n && redacted[i] { i += 1; }
            if i < n { out.push(seps[i - 1]); }
        } else {
            out.push_str(groups[i]);
            if seps[i] != '\0' { out.push(seps[i]); }
            i += 1;
        }
    }
    out
}

/// Replace PII patterns in text with `***`.
pub fn sanitize(text: &str) -> String {
    if text.is_empty() { return String::new(); }
    let p = patterns();
    let has_digit = text.bytes().any(|b| b.is_ascii_digit());
    let step1 = if has_digit {
        p.numeric.replace_all(text, |caps: &regex::Captures| redact_token(&caps[0]))
    } else {
        std::borrow::Cow::Borrowed(text)
    };
    if step1.contains('@') {
        p.email.replace_all(&step1, "***").into_owned()
    } else {
        step1.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_credit_card() {
        assert_eq!(sanitize("card 1234 5678 9012 3456 ok"), "card *** ok");
        assert_eq!(sanitize("card 1234-5678-9012-3456 ok"), "card *** ok");
    }

    #[test]
    fn test_ssn() {
        assert_eq!(sanitize("ssn 123-45-6789 end"), "ssn *** end");
    }

    #[test]
    fn test_chinese_id() {
        assert_eq!(sanitize("id 11010119900307891X end"), "id *** end");
        assert_eq!(sanitize("id 110101199003078910 end"), "id *** end");
        assert_eq!(sanitize("身份证号：11010119900307891X。"), "身份证号：***。");
    }

    #[test]
    fn test_chinese_phone() {
        assert_eq!(sanitize("call 13812345678 now"), "call *** now");
        assert_eq!(sanitize("电话13812345678号"), "电话***号");
        assert_eq!(sanitize("2026-04-09 13812345678"), "2026-04-09 ***");
    }

    #[test]
    fn test_email() {
        assert_eq!(sanitize("mail user@example.com ok"), "mail *** ok");
        assert_eq!(sanitize("mail first.last+tag@sub.example.co.uk ok"), "mail *** ok");
    }

    #[test]
    fn test_no_pii() {
        assert_eq!(sanitize("hello world"), "hello world");
        assert_eq!(sanitize("version 1.2.3 build 2026-04-09 room 1234"), "version 1.2.3 build 2026-04-09 room 1234");
        assert_eq!(sanitize("meeting at 10:30, 2 attendees"), "meeting at 10:30, 2 attendees");
    }
}
