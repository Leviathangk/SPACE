package main

import (
	"context"
	"encoding/json"
	"io"
	"os"
	"path/filepath"
	"testing"

	taskAPI "github.com/containerd/containerd/api/runtime/task/v3"
	"go.opentelemetry.io/otel/attribute"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.opentelemetry.io/otel/sdk/trace/tracetest"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	pb "github.com/perplexityai/SPACE/shim/proto/sandbox/manager/v1"
)

// recordTraces makes every span the shim starts land in the returned recorder
// until the test ends.
func recordTraces(t *testing.T) *tracetest.SpanRecorder {
	t.Helper()
	tracerOnce.Do(func() {})
	recorder := tracetest.NewSpanRecorder()
	provider := sdktrace.NewTracerProvider(sdktrace.WithSpanProcessor(recorder), sdktrace.WithSampler(sdktrace.AlwaysSample()))
	previous := activeTracer.Swap(&activeTracePipeline{&otlpTracePipeline{tracer: provider.Tracer("test"), provider: provider}})
	t.Cleanup(func() {
		if previous == nil {
			previous = &activeTracePipeline{newDisabledTracePipeline()}
		}
		activeTracer.Store(previous)
		if err := provider.Shutdown(context.Background()); err != nil {
			t.Error(err)
		}
	})
	return recorder
}

func writeBundle(t *testing.T, annotations map[string]string) string {
	t.Helper()
	bundle := t.TempDir()
	raw, err := json.Marshal(map[string]any{"annotations": annotations})
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(bundle, "config.json"), raw, 0600); err != nil {
		t.Fatal(err)
	}
	return bundle
}

// Kubelet retries a failed pod sandbox with the Pod's annotations unchanged,
// so every attempt sees the same traceparent. Each attempt must be its own
// trace, linked to that context, and a task's teardown links to the attempt
// that created it.
func TestEveryCreateAttemptRootsATraceLinkedToThePodContext(t *testing.T) {
	recorder := recordTraces(t)
	const podTrace = "69f2ccfd00000000e88861448bd50939"
	const podSpan = "ea1dc2f49db4d7a9"
	const sandboxID = "01900000-0000-7000-8000-000000000002"
	annotations := map[string]string{
		"space.pplx.net/traceparent": "00-" + podTrace + "-" + podSpan + "-01",
		sandboxIDAnnotation:          sandboxID,
		sandboxOwnerAnnotation:       "review@example.test",
	}

	// Two attempts whose restore fails, as a crash-looping create does.
	annotations[sandboxCheckpointAnnotation] = "latest"
	failing := &spaceTaskService{tasks: map[string]*task{}, sandboxManagerClientFactory: func() (pb.SandboxManagerClient, io.Closer, error) {
		return failingRestoreClient{}, fakeConn{}, nil
	}}
	for range 2 {
		bundle := writeBundle(t, annotations)
		if _, err := failing.Create(context.Background(), &taskAPI.CreateTaskRequest{ID: "attempt", Bundle: bundle}); status.Code(err) != codes.Unavailable {
			t.Fatalf("failing attempt: %v", err)
		}
	}

	// Then one that succeeds, and its task's teardown.
	delete(annotations, sandboxCheckpointAnnotation)
	client := &activationPolicyClient{installedSeconds: 30}
	succeeding := &spaceTaskService{tasks: map[string]*task{}, sandboxManagerClientFactory: func() (pb.SandboxManagerClient, io.Closer, error) {
		return client, client, nil
	}}
	bundle := writeBundle(t, annotations)
	if _, err := succeeding.Create(context.Background(), &taskAPI.CreateTaskRequest{ID: "attempt", Bundle: bundle}); err != nil {
		t.Fatalf("succeeding attempt: %v", err)
	}
	// A detached task tears down without calling the manager; its span is
	// what this test reads.
	succeeding.tasks["attempt"].sandboxDetached = true
	if _, err := succeeding.Delete(context.Background(), &taskAPI.DeleteRequest{ID: "attempt"}); err != nil {
		t.Fatalf("Delete: %v", err)
	}

	var creates []sdktrace.ReadOnlySpan
	var teardown sdktrace.ReadOnlySpan
	for _, span := range recorder.Ended() {
		switch span.Name() {
		case "shim.create":
			creates = append(creates, span)
		case "shim.delete":
			teardown = span
		}
	}
	if len(creates) != 3 || teardown == nil {
		t.Fatalf("got %d shim.create spans and teardown %v, want 3 and one", len(creates), teardown != nil)
	}
	traces := map[string]bool{}
	for i, create := range creates {
		traceID := create.SpanContext().TraceID().String()
		if create.Parent().IsValid() || traceID == podTrace || traces[traceID] {
			t.Fatalf("attempt %d is not a root of its own trace: trace %s parent %v", i, traceID, create.Parent())
		}
		traces[traceID] = true
		links := create.Links()
		if len(links) != 1 || links[0].SpanContext.TraceID().String() != podTrace || links[0].SpanContext.SpanID().String() != podSpan {
			t.Fatalf("attempt %d does not link to the Pod's context: %v", i, links)
		}
		if !hasAttribute(create.Attributes(), "sandbox_id", sandboxID) {
			t.Fatalf("attempt %d does not name its sandbox: %v", i, create.Attributes())
		}
	}
	created := creates[2].SpanContext()
	if links := teardown.Links(); len(links) != 1 || links[0].SpanContext.SpanID() != created.SpanID() || links[0].SpanContext.TraceID() != created.TraceID() {
		t.Fatalf("teardown does not link to the create that made its task: %v", teardown.Links())
	}
}

func hasAttribute(attributes []attribute.KeyValue, key, value string) bool {
	for _, attr := range attributes {
		if string(attr.Key) == key && attr.Value.AsString() == value {
			return true
		}
	}
	return false
}
