use std::cmp::Ordering;

/// Compare two strings the way JavaScript's default string comparison does:
/// lexicographically by UTF-16 code unit. This differs from Rust's `str: Ord`
/// (UTF-8 byte order, equivalent to code point order) exactly when one string
/// contains an astral-plane character (U+10000+, encoded as a UTF-16 surrogate
/// pair whose leading unit is in 0xD800..=0xDBFF) and the other contains a BMP
/// character at or above that leading-surrogate range (e.g. U+E000-U+FFFF).
pub fn js_cmp(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// Sort paths in place using [`js_cmp`], matching Node's `Array.prototype.sort()`
/// default ordering on strings.
pub fn sort_js(paths: &mut [String]) {
    paths.sort_by(|a, b| js_cmp(a, b));
}

#[cfg(test)]
mod tests {
    use super::js_cmp;

    #[test]
    fn js_cmp_orders_like_javascript() {
        use std::cmp::Ordering;
        let js_cmp_bytes = |a: &str, b: &str| a.cmp(b);
        // ASCII agrees with byte order.
        assert_eq!(js_cmp("a", "b"), Ordering::Less);
        assert_eq!(js_cmp("a/b", "a-b"), js_cmp_bytes("a/b", "a-b"));
        // BMP vs astral: U+FF01 (fullwidth !) is E0-block; U+1F389 (party popper) is astral.
        // JS: "！" (0xFF01) > "🎉" (0xD83C first unit), so the emoji sorts FIRST.
        // Bytes: EF BC 81 < F0 9F 8E 89, so the fullwidth char sorts first. These must differ.
        assert_eq!(js_cmp("\u{1F389}.md", "\u{FF01}.md"), Ordering::Less);
        assert_eq!("\u{1F389}.md".cmp("\u{FF01}.md"), Ordering::Greater);
        // Prefix rule: shorter string first, like JS.
        assert_eq!(js_cmp("a", "ab"), Ordering::Less);
        assert_eq!(js_cmp("ab", "ab"), Ordering::Equal);
    }
}
