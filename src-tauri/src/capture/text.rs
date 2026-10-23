//! Char-boundary-safe string truncation. `&s[..n]` panics on multi-byte text
//! (Chinese OCR output, transcripts), so nothing in the app slices by byte index.

/// Longest prefix of `s` with at most `max_bytes` bytes, ending on a char boundary.
pub fn truncate_bytes(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes { return s; }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) { end -= 1; }
    &s[..end]
}

/// First `max_chars` characters of `s`.
pub fn truncate_chars(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

/// Owned variant of [`truncate_chars`].
pub fn take_chars(s: &str, max_chars: usize) -> String {
    truncate_chars(s, max_chars).to_string()
}

/// Character count (not bytes) — for "is this text long enough" checks.
pub fn char_len(s: &str) -> usize { s.chars().count() }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_panics_on_cjk() {
        let s = "腾讯会议TencentMeeting会议纪要";
        for n in 0..=s.len() + 2 {
            let t = truncate_bytes(s, n);
            assert!(t.len() <= n.min(s.len()));
            assert!(s.starts_with(t));
        }
        assert_eq!(truncate_chars(s, 4), "腾讯会议");
        assert_eq!(truncate_chars("ab", 5), "ab");
        assert_eq!(take_chars("日本語テキスト", 3), "日本語");
    }
}
