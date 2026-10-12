//! The rule interface and the registry every check runs from.

mod file_size;

use crate::{baseline::Baseline, context::Context, finding::Finding};

pub trait Rule {
    /// Matches the rule's `[rules.<id>]` table in `ARCHITECTURE.toml`.
    fn id(&self) -> &'static str;

    fn check(&self, ctx: &Context) -> Vec<Finding>;

    /// Fills this rule's table for `--write-baseline`. Rules without a
    /// baseline keep the default.
    fn update_baseline(&self, _ctx: &Context, _baseline: &mut Baseline) {}
}

/// Every rule, in reporting order. Register a new rule here.
pub fn registered() -> Vec<Box<dyn Rule>> {
    vec![Box::new(file_size::FileSize)]
}
