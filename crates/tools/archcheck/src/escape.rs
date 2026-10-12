//! Escaping for text that comes from the checked tree.
//!
//! On a pull request, paths, workspace member names and TOML keys are chosen
//! by its author, and every line archcheck prints may land in a GitHub Actions
//! log. The runner reads a line as a workflow command when it starts with
//! `::` after leading whitespace, or when it contains `##[` anywhere, so such
//! text must never start a line or contain that prefix.

/// `value` as one line that GitHub Actions cannot read as a workflow command.
///
/// Control characters (including CR, LF and DEL) and line or paragraph
/// separators are escaped as `\n`, `\u{7f}` and the like, so `value` cannot
/// start a new line; the `[` of every `##[` is escaped; and a leading `::` has
/// its first `:` escaped. Every diagnostic and every human-format line goes
/// through this function.
pub fn log_line(value: &str) -> String {
    let out = escape_where(value, |c| c.is_control() || is_line_separator(c));
    // The runner trims Unicode whitespace, as `trim_start` does.
    let rest = out.trim_start();
    match rest.strip_prefix(':') {
        Some(after) if after.starts_with(':') => {
            let indent: String = out.chars().take_while(|c| c.is_whitespace()).collect();
            format!("{indent}\\u{{3a}}{after}")
        }
        _ => out,
    }
}

/// Escapes every character for which `escape` holds, and the `[` of every
/// `##[`. The escapes themselves contain no `#` or `[`, so the result never
/// contains `##[`.
pub(crate) fn escape_where(value: &str, escape: impl Fn(char) -> bool) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if escape(c) {
            out.extend(c.escape_default());
        } else if c == '[' && out.ends_with("##") {
            out.extend(c.escape_unicode());
        } else {
            out.push(c);
        }
    }
    out
}

/// U+2028 and U+2029, which some viewers render as line breaks.
pub(crate) fn is_line_separator(c: char) -> bool {
    matches!(c, '\u{2028}' | '\u{2029}')
}
