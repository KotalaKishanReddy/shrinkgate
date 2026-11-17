//! Shared HTTP plumbing: TLS-capable upstream client, body helpers, header
//! filtering, and the SSE usage-tapping body.

use bytes::Buf;
use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame, SizeHint};
use hyper::HeaderMap;
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use serde_json::Value;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{ready, Context, Poll};

use crate::store::Usage;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type UpstreamClient = Client<HttpsConnector<HttpConnector>, BoxBody>;

pub fn make_client() -> UpstreamClient {
    let mut http = HttpConnector::new();
    http.enforce_http(false);
    http.set_connect_timeout(Some(std::time::Duration::from_secs(10)));
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_or_http()
        .enable_http1()
        .wrap_connector(http);
    Client::builder(TokioExecutor::new()).build(https)
}

/// A boxed [`Body`] that is `Send` (but not necessarily `Sync`): exactly what
/// `tokio::spawn`ed connections and the legacy client need, and loose enough
/// for tapping streams.
pub struct BoxBody<D: Buf = Bytes, E = BoxError> {
    inner: Pin<Box<dyn Body<Data = D, Error = E> + Send>>,
}

impl<D: Buf, E> BoxBody<D, E> {
    pub fn new<B>(body: B) -> Self
    where
        B: Body<Data = D, Error = E> + Send + 'static,
    {
        Self {
            inner: Box::pin(body),
        }
    }
}

impl<D: Buf, E> Body for BoxBody<D, E> {
    type Data = D;
    type Error = E;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        self.get_mut().inner.as_mut().poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.as_ref().is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.as_ref().size_hint()
    }
}

pub fn full(bytes: Bytes) -> BoxBody {
    BoxBody::new(Full::new(bytes).map_err(|e| -> BoxError { match e {} }))
}

/// Bytes stream -> boxed HTTP body.
pub fn boxed_stream<S>(s: S) -> BoxBody
where
    S: futures_util::Stream<Item = Result<Bytes, BoxError>> + Send + 'static,
{
    BoxBody::new(http_body_util::StreamBody::new(
        s.map(|r| r.map(Frame::data)),
    ))
}

/// hyper body stream (Bytes, hyper::Error) -> boxed HTTP body.
pub fn hyper_stream(
    s: impl futures_util::Stream<Item = Result<Bytes, hyper::Error>> + Send + 'static,
) -> BoxBody {
    boxed_stream(s.map(|r| r.map_err(|e| Box::new(e) as BoxError)))
}

// Headers that must not be forwarded between the two sides.
const STRIP_REQ: &[&str] = &[
    "host",
    "connection",
    "keep-alive",
    "transfer-encoding",
    "upgrade",
    "te",
    "trailer",
    "expect",
    "content-length",
    "accept-encoding",
];
const STRIP_RESP: &[&str] = &[
    "connection",
    "keep-alive",
    "transfer-encoding",
    "upgrade",
    "te",
    "trailer",
    "content-length",
];

fn is_hop(name: &hyper::header::HeaderName, list: &[&str]) -> bool {
    list.iter().any(|h| name.as_str().eq_ignore_ascii_case(h))
        || name.as_str().starts_with("x-shrinkgate-")
        || name.as_str().starts_with("proxy-")
}

fn connection_header(src: &HeaderMap, name: &hyper::header::HeaderName) -> bool {
    src.get_all("connection")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|token| token.trim().eq_ignore_ascii_case(name.as_str()))
}

/// Forwardable request headers. When `identity_ae`, Accept-Encoding is forced
/// to `identity` so usage taps see plain bytes.
pub fn forward_req_headers(src: &HeaderMap, identity_ae: bool) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (k, v) in src.iter() {
        if is_hop(k, STRIP_REQ) || connection_header(src, k) {
            continue;
        }
        out.append(k, v.clone());
    }
    if identity_ae {
        out.insert(
            hyper::header::ACCEPT_ENCODING,
            hyper::header::HeaderValue::from_static("identity"),
        );
    }
    out
}

pub fn forward_resp_headers(src: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (k, v) in src.iter() {
        if is_hop(k, STRIP_RESP) || connection_header(src, k) {
            continue;
        }
        out.append(k, v.clone());
    }
    out
}

// ---------------------------------------------------------------------------
// Tapping body: streams SSE through unchanged while extracting usage events.
// ---------------------------------------------------------------------------

type OnEnd = Box<dyn FnOnce(Option<Usage>, bool) + Send>;

pub struct TapBody {
    inner: BoxBody,
    buf: Vec<u8>,
    usage: Arc<Mutex<Option<Usage>>>,
    on_end: Option<OnEnd>,
    skip_line: bool,
}

impl TapBody {
    pub fn new(inner: BoxBody, usage: Arc<Mutex<Option<Usage>>>, on_end: OnEnd) -> Self {
        Self {
            inner,
            buf: Vec::new(),
            usage,
            on_end: Some(on_end),
            skip_line: false,
        }
    }

    fn scan(&mut self, data: &[u8]) {
        // Process each byte once; never drain/memmove the remainder of a chunk.
        for part in data.split_inclusive(|b| *b == b'\n') {
            let ended = part.last() == Some(&b'\n');
            if !self.skip_line {
                if self.buf.len() + part.len() > (1 << 20) {
                    self.buf.clear();
                    self.skip_line = true;
                } else {
                    self.buf.extend_from_slice(part);
                }
            }
            if ended {
                if !self.skip_line {
                    if let Some(rest) = self.buf.strip_prefix(b"data:") {
                        if let Ok(v) = serde_json::from_slice::<Value>(rest) {
                            if let Ok(mut cell) = self.usage.lock() {
                                cell.get_or_insert_with(Usage::default).merge_json(&v);
                            }
                        }
                    }
                }
                self.buf.clear();
                self.skip_line = false;
            }
        }
    }

    fn finish(&mut self, complete: bool) {
        if let Some(on_end) = self.on_end.take() {
            let usage = self
                .usage
                .lock()
                .ok()
                .and_then(|mut c| c.take())
                .filter(|u| u.input_tokens.is_some() || u.output_tokens.is_some());
            on_end(usage, complete);
        }
    }
}

impl Body for TapBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match ready!(Pin::new(&mut self.inner).poll_frame(cx)) {
            Some(Ok(frame)) => {
                if let Some(data) = frame.data_ref() {
                    self.scan(data);
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Some(Err(e)) => {
                self.finish(false);
                Poll::Ready(Some(Err(e)))
            }
            None => {
                self.finish(true);
                Poll::Ready(None)
            }
        }
    }
}

impl Drop for TapBody {
    fn drop(&mut self) {
        self.finish(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn connection_nominated_headers_are_removed() {
        let mut h = HeaderMap::new();
        h.insert("connection", "x-private, keep-alive".parse().unwrap());
        h.insert("x-private", "secret".parse().unwrap());
        h.insert("authorization", "Bearer test".parse().unwrap());
        assert!(!forward_req_headers(&h, true).contains_key("x-private"));
        assert!(!forward_resp_headers(&h).contains_key("x-private"));
        assert_eq!(
            forward_req_headers(&h, true)["authorization"],
            "Bearer test"
        );
    }
    #[test]
    fn fragmented_sse_and_drop_report_once() {
        let usage = Arc::new(Mutex::new(None));
        let reports = Arc::new(Mutex::new(Vec::new()));
        let output = reports.clone();
        let mut tap = TapBody::new(
            full(Bytes::new()),
            usage.clone(),
            Box::new(move |u, complete| {
                output
                    .lock()
                    .unwrap()
                    .push((u.unwrap().input_tokens, complete));
            }),
        );
        tap.scan(b"event: usage\r\ndata: {\"usage\":{\"input_");
        tap.scan(b"tokens\":42}}\r\n\r\ndata: [DONE]\n\n");
        tap.finish(true);
        drop(tap);
        assert_eq!(*reports.lock().unwrap(), vec![(Some(42), true)]);
    }
    #[test]
    fn oversized_sse_line_is_skipped_without_losing_next_event() {
        let usage = Arc::new(Mutex::new(None));
        let mut tap = TapBody::new(full(Bytes::new()), usage.clone(), Box::new(|_, _| {}));
        tap.scan(&vec![b'x'; (1 << 20) + 10]);
        assert!(tap.buf.len() <= 1 << 20);
        tap.scan(b"\ndata: {\"usage\":{\"input_tokens\":7}}\n");
        assert_eq!(
            usage.lock().unwrap().as_ref().unwrap().input_tokens,
            Some(7)
        );
    }
}
