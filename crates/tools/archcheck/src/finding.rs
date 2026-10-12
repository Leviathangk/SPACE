use crate::config::Mode;

/// One reported problem. `path` is repository-relative with `/` separators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub rule: &'static str,
    pub behaviour: &'static str,
    pub path: String,
    /// The rest of the message after the path, such as `": 1200 code lines…"`.
    /// Reports render the path and this text separately, so a path is never
    /// found by searching the message.
    pub detail: String,
    pub mode: Mode,
}

impl Finding {
    /// The path followed by the detail, as one sentence.
    pub fn message(&self) -> String {
        format!("{}{}", self.path, self.detail)
    }
}
