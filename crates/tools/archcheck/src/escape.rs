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

/// Unicode format characters (general category Cf, as of Unicode 15.0), which
/// are invisible and can hide or reorder text: the soft hyphen, bidi controls,
/// zero-width characters, interlinear annotation marks and the tag block.
/// U+E0000 is unassigned but escaped with the rest of the tag block.
pub(crate) fn is_format(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E007F}'
    )
}
