use crate::secrets::{RequestSecrets, StreamScrubber};
use bytes::Bytes;
use http_body_util::Full;
use hyper::{
    body::{Body, Frame, Incoming},
    client::conn::http1::SendRequest,
};
use std::{
    future::Future,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::time::{Instant, Sleep};

pub(crate) const RESPONSE_LIMIT: usize = 64 * 1024 * 1024;
const CHUNK: usize = 16 * 1024;
const IDLE: Duration = Duration::from_secs(60);
pub(crate) type Driver = Pin<Box<dyn Future<Output = Result<(), hyper::Error>> + Send>>;

pub(crate) struct RedactedBody {
    upstream: Option<Incoming>,
    driver: Option<Driver>,
    sender: Option<SendRequest<Full<Bytes>>>,
    scrubber: Option<StreamScrubber>,
    input: Bytes,
    received: usize,
    emitted: usize,
    idle: Pin<Box<Sleep>>,
    done: bool,
}

impl RedactedBody {
    pub(crate) fn new(
        upstream: Incoming,
        driver: Driver,
        sender: SendRequest<Full<Bytes>>,
        secrets: Arc<RequestSecrets>,
    ) -> Self {
        Self {
            upstream: Some(upstream),
            driver: Some(driver),
            sender: Some(sender),
            scrubber: Some(StreamScrubber::new(secrets)),
            input: Bytes::new(),
            received: 0,
            emitted: 0,
            idle: Box::pin(tokio::time::sleep(IDLE)),
            done: false,
        }
    }

    fn close(&mut self) {
        self.done = true;
        self.driver.take();
        self.sender.take();
        self.upstream.take();
        self.scrubber.take();
        self.input = Bytes::new();
    }

    fn rejected(&mut self) -> Poll<Option<io::Result<Frame<Bytes>>>> {
        self.close();
        Poll::Ready(Some(Err(io::Error::other(
            "HTTPS response stream rejected",
        ))))
    }
}

impl Body for RedactedBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<io::Result<Frame<Bytes>>>> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        if this.idle.as_mut().poll(cx).is_ready() {
            return this.rejected();
        }
        if let Some(driver) = &mut this.driver {
            match driver.as_mut().poll(cx) {
                Poll::Ready(Err(_)) => return this.rejected(),
                Poll::Ready(Ok(())) => {
                    this.driver = None;
                }
                Poll::Pending => {}
            }
        }
        for _ in 0..8 {
            if !this.input.is_empty() {
                let chunk = this.input.split_to(CHUNK.min(this.input.len()));
                match this
                    .scrubber
                    .as_mut()
                    .unwrap()
                    .push(&chunk, RESPONSE_LIMIT - this.emitted)
                {
                    Ok(output) if output.is_empty() => continue,
                    Ok(output) => {
                        this.emitted += output.len();
                        return Poll::Ready(Some(Ok(Frame::data(Bytes::from(output)))));
                    }
                    Err(_) => return this.rejected(),
                }
            }
            match Pin::new(this.upstream.as_mut().unwrap()).poll_frame(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Err(_))) => return this.rejected(),
                Poll::Ready(Some(Ok(frame))) => {
                    let Ok(data) = frame.into_data() else {
                        return this.rejected();
                    };
                    if data.len() > RESPONSE_LIMIT - this.received {
                        return this.rejected();
                    }
                    this.received += data.len();
                    if !data.is_empty() {
                        this.idle.as_mut().reset(Instant::now() + IDLE);
                    }
                    this.input = data;
                }
                Poll::Ready(None) => {
                    let final_bytes = this
                        .scrubber
                        .take()
                        .unwrap()
                        .finish(RESPONSE_LIMIT - this.emitted);
                    this.close();
                    return match final_bytes {
                        Ok(output) if output.is_empty() => Poll::Ready(None),
                        Ok(output) => Poll::Ready(Some(Ok(Frame::data(Bytes::from(output))))),
                        Err(_) => this.rejected(),
                    };
                }
            }
        }
        cx.waker().wake_by_ref();
        Poll::Pending
    }

    fn is_end_stream(&self) -> bool {
        self.done
    }
}
