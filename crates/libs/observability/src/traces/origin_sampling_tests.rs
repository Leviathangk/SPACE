use super::{FollowOrigin, ORIGIN_SAMPLED_ATTRIBUTE};
use opentelemetry::{
    Context, KeyValue,
    trace::{
        Link, Span as _, SpanContext, SpanId, SpanKind, TraceContextExt as _, TraceFlags, TraceId,
        TraceState, Tracer as _, TracerProvider as _,
    },
};
use opentelemetry_sdk::trace::{
    Sampler, SamplingDecision, SamplingResult, SdkTracerProvider, ShouldSample as _,
};

/// A configured sampler that samples nothing, so every span this test sees
/// sampled was sampled by following its parent or origin.
fn never() -> FollowOrigin {
    FollowOrigin {
        configured: Box::new(Sampler::TraceIdRatioBased(0.0)),
    }
}

fn remote(flags: TraceFlags, trace_state: TraceState) -> SpanContext {
    SpanContext::new(
        TraceId::from(0x0af7651916cd43dd8448eb211c80319c),
        SpanId::from(0xb7ad6b7169203331),
        flags,
        true,
        trace_state,
    )
}

fn child_of(parent: SpanContext) -> SamplingResult {
    let trace_id = parent.trace_id();
    never().should_sample(
        Some(&Context::new().with_remote_span_context(parent)),
        trace_id,
        "child",
        &SpanKind::Internal,
        &[],
        &[],
    )
}

fn trace_state() -> TraceState {
    TraceState::from_key_value([("dd", "s:1")]).unwrap()
}

#[test]
fn a_child_of_a_sampled_parent_is_sampled_whatever_the_configured_sampler() {
    let result = child_of(remote(TraceFlags::SAMPLED, trace_state()));
    assert_eq!(result.decision, SamplingDecision::RecordAndSample);
    assert_eq!(result.trace_state, trace_state());
}

#[test]
fn a_child_of_an_unsampled_parent_is_recorded_unsampled() {
    let result = child_of(remote(TraceFlags::default(), trace_state()));
    assert_eq!(result.decision, SamplingDecision::RecordOnly);
    assert_eq!(result.trace_state, trace_state());
}

/// The case a ratio sampler broke: the root is kept for its origin, and the
/// work under it must stay with it.
#[test]
fn a_root_kept_for_its_sampled_origin_keeps_its_children() {
    let provider = SdkTracerProvider::builder().with_sampler(never()).build();
    let tracer = provider.tracer("origin_sampling_tests");
    let root = tracer
        .span_builder("root")
        .with_links(vec![Link::with_context(remote(
            TraceFlags::SAMPLED,
            TraceState::default(),
        ))])
        .with_attributes([KeyValue::new(ORIGIN_SAMPLED_ATTRIBUTE, true)])
        .start(&tracer);
    let root = Context::new().with_span(root);
    assert!(root.span().span_context().is_sampled(), "root");
    let child = tracer.start_with_context("child", &root);
    assert!(child.span_context().is_sampled(), "child");
}
