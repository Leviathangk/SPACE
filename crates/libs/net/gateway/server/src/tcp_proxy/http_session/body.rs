//! Guest or origin body forwarded across the session, finishing its accounting
//! attempt when the stream ends.
use std::pin::Pin;
use std::task::{Context, Poll};

use hyper::body::{Body as HttpBody, Bytes, Incoming};

use super::super::accounting::{Attempt, Outcome};

pub(super) struct ForwardingBody {
    inner: Incoming,
    attempt: Option<Attempt>,
}

impl ForwardingBody {
    /// `attempt` is finished once when the body completes or fails.
    pub(super) fn new(inner: Incoming, mut attempt: Option<Attempt>) -> Self {
        if inner.is_end_stream()
            && let Some(attempt) = &mut attempt
        {
            attempt.finish(Outcome::Success, "body_complete");
        }
        Self { inner, attempt }
    }
}

impl HttpBody for ForwardingBody {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        let result = Pin::new(&mut self.inner).poll_frame(context);
        let finished = match &result {
            Poll::Ready(Some(Err(_))) => Some((Outcome::Error, "body_error")),
            Poll::Ready(None) => Some((Outcome::Success, "body_complete")),
            Poll::Ready(Some(Ok(_))) if self.inner.is_end_stream() => {
                Some((Outcome::Success, "body_complete"))
            }
            _ => None,
        };
        if let (Some(attempt), Some((outcome, reason))) = (&mut self.attempt, finished) {
            attempt.finish(outcome, reason);
        }
        result
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.inner.size_hint()
    }
}
