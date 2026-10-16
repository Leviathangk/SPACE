//! Keep or drop a root that links to an origin as that origin was sampled.
//!
//! Work that applies a persisted intent starts its own trace and links to the
//! context of the request that caused it
//! ([`crate::trace_propagation::link_root_to_origins`]). Sampled on its own,
//! such a root would be kept or dropped independently of the request, so a
//! kept request could lose the work it caused and a dropped request could
//! keep work that links to nothing. A root that links to an origin and
//! records `origin_sampled` therefore follows the origin's sampled flag. A
//! root linked without it
//! ([`crate::trace_propagation::link_root_to_origins_without_following`]) and
//! a root with no origin keep the configured sampler.
//!
//! Every span with a parent follows it, whatever the configured sampler is, as
//! the shim's sampler does and as the native Datadog sampler gives every span
//! its trace's priority: a sampler that is not parent-based, such as
//! `traceidratio`, would otherwise drop most children of a root kept for its
//! origin. A span under a sampled parent is sampled; one under a parent that
//! was not sampled is recorded unsampled, so APM stats count it and a failure
//! can still be exported. A gRPC call made with no traced request in scope
//! takes such a parent
//! ([`crate::trace_propagation::set_unsampled_remote_parent`]), and its server
//! follows the unsampled context it propagates.

use opentelemetry::{
    Context, KeyValue,
    trace::{Link, SpanKind, TraceContextExt as _, TraceId},
};
use opentelemetry_sdk::trace::{SamplingDecision, SamplingResult, ShouldSample};

/// The field [`crate::trace_propagation::link_root_to_origins`] records on a
/// root whose sampling follows its origins. The native Datadog sampler never
/// sees links, so its rules match this instead; the OTLP sampler follows a
/// root's links only when it carries it, so a root linked with
/// [`crate::trace_propagation::link_root_to_origins_without_following`] keeps
/// the configured sampler on both.
const ORIGIN_SAMPLED_ATTRIBUTE: &str = "origin_sampled";

/// The OTLP pipeline's sampler: the one `OTEL_TRACES_SAMPLER` configures for
/// roots, except that a root that records `origin_sampled` and has a valid
/// link follows its links. A span with a parent follows the parent.
#[derive(Clone, Debug)]
pub(super) struct FollowOrigin {
    configured: Box<dyn ShouldSample>,
}

impl FollowOrigin {
    pub(super) fn from_env() -> Self {
        // `Config::default` is where the SDK reads `OTEL_TRACES_SAMPLER` and
        // its argument; taking its sampler keeps that parsing in one place.
        Self {
            configured: opentelemetry_sdk::trace::Config::default().sampler,
        }
    }
}

impl ShouldSample for FollowOrigin {
    fn should_sample(
        &self,
        parent_context: Option<&Context>,
        trace_id: TraceId,
        name: &str,
        span_kind: &SpanKind,
        attributes: &[KeyValue],
        links: &[Link],
    ) -> SamplingResult {
        let parent = parent_context
            .filter(|context| context.has_active_span())
            .map(|context| context.span().span_context().clone())
            .filter(|parent| parent.is_valid());
        // A span with a parent follows it whatever OTEL_TRACES_SAMPLER says,
        // as the native Datadog sampler and the shim do: `always_on` would
        // otherwise sample the server side of a call its caller left
        // unsampled, and `traceidratio` would drop most children of a root
        // kept for its origin.
        if let Some(parent) = parent {
            return SamplingResult {
                decision: if parent.is_sampled() {
                    SamplingDecision::RecordAndSample
                } else {
                    SamplingDecision::RecordOnly
                },
                attributes: Vec::new(),
                trace_state: parent.trace_state().clone(),
            };
        }
        let mut origins = links
            .iter()
            .map(|link| &link.span_context)
            .filter(|origin| origin.is_valid())
            .peekable();
        // Only a root that records `origin_sampled` follows its links, as
        // the native Datadog rules do; one linked without it, such as a
        // retry, keeps the configured sampler.
        let follows = attributes
            .iter()
            .any(|attribute| attribute.key.as_str() == ORIGIN_SAMPLED_ATTRIBUTE);
        if !follows || origins.peek().is_none() {
            return self.configured.should_sample(
                parent_context,
                trace_id,
                name,
                span_kind,
                attributes,
                links,
            );
        }
        let decision = if origins.any(|origin| origin.is_sampled()) {
            SamplingDecision::RecordAndSample
        } else {
            SamplingDecision::Drop
        };
        SamplingResult {
            decision,
            attributes: Vec::new(),
            trace_state: Default::default(),
        }
    }
}

/// The native Datadog sampler's rules: two that follow a root's origin,
/// ahead of the rules `DD_TRACE_SAMPLING_RULES` configures.
///
/// datadog-opentelemetry installs its own sampler, which can be neither
/// replaced nor wrapped and ignores links, but matches rules against the
/// attributes a span has when it starts. `origin_sampled` is recorded only on
/// roots that link to an origin, so other spans fall through to the
/// configured rules. Setting rules in code replaces the environment's, so
/// those are appended unchanged. Remote Configuration sampling rules, when
/// an operator sends them, replace the whole list, these two included, and a
/// keep rule is still subject to `DD_TRACE_RATE_LIMIT`.
#[cfg(feature = "datadog")]
pub(super) fn datadog_rules(
    configured: &[datadog_opentelemetry::configuration::SamplingRuleConfig],
) -> Vec<datadog_opentelemetry::configuration::SamplingRuleConfig> {
    use datadog_opentelemetry::configuration::SamplingRuleConfig;
    let follow = |sampled: bool| SamplingRuleConfig {
        sample_rate: if sampled { 1.0 } else { 0.0 },
        service: None,
        name: None,
        resource: None,
        tags: std::collections::HashMap::from([(
            ORIGIN_SAMPLED_ATTRIBUTE.to_string(),
            sampled.to_string(),
        )]),
    };
    let mut rules = vec![follow(true), follow(false)];
    rules.extend(configured.iter().cloned());
    rules
}

#[cfg(test)]
#[path = "origin_sampling_tests.rs"]
mod tests;
