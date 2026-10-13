use std::pin::Pin;
use std::task::{Context, Poll};

use hyper::body::{Body, Frame, SizeHint};

use super::super::flow::Request;

pub(super) struct ResponseBody {
    inner: super::Body,
    request: Request,
}

impl ResponseBody {
    pub(super) fn new(inner: super::Body, mut request: Request) -> Self {
        if inner.is_end_stream() {
            request.complete();
        }
        Self { inner, request }
    }
}

impl Body for ResponseBody {
    type Data = <super::Body as Body>::Data;
    type Error = <super::Body as Body>::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let result = Pin::new(&mut self.inner).poll_frame(cx);
        match &result {
            Poll::Ready(Some(Err(error))) => self
                .request
                .errors()
                .observe(error.as_ref(), super::super::flow::Side::Upstream),
            Poll::Ready(None) => self.request.complete(),
            Poll::Ready(Some(Ok(_))) if self.inner.is_end_stream() => self.request.complete(),
            _ => {}
        }
        result
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}
