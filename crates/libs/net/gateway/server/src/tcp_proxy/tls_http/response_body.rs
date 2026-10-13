use std::pin::Pin;
use std::task::{Context, Poll};

use hyper::body::{Body, Frame, SizeHint};

use super::super::flow::Request;

pub(super) struct ResponseBody<B> {
    inner: B,
    request: Request,
}

impl<B: Body> ResponseBody<B> {
    pub(super) fn new(inner: B, mut request: Request) -> Self {
        if inner.is_end_stream() {
            request.complete();
        }
        Self { inner, request }
    }
}

impl<B: Body + Unpin> Body for ResponseBody<B> {
    type Data = B::Data;
    type Error = B::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let result = Pin::new(&mut self.inner).poll_frame(cx);
        match &result {
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
