use crate::config::Mode;

/// One reported problem. `path` is repository-relative with `/` separators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub rule: &'static str,
    pub behaviour: &'static str,
    pub path: String,
    pub message: String,
    pub mode: Mode,
}
