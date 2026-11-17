//! Shared HTTP plumbing: TLS-capable upstream client, body helpers, header
//! filtering, and the SSE usage-tapping body.

use bytes::Bytes;
use bytes::Buf;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame, SizeHint};
use hyper::HeaderMap;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use hyper_rustls::HttpsConnector;
use futures_util::StreamExt;
use serde_json::Value;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{ready, Context, Poll};

use crate::store::Usage;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type UpstreamClient = Client<HttpsConnector<HttpConnector>, BoxBody>;

pub fn make_client() -> UpstreamClient {
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_or_http()
        .enable_http1()
        .build();
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
        Self { inner: Box::pin(body) }
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

/// Forwardable request headers. When `identity_ae`, Accept-Encoding is forced
/// to `identity` so usage taps see plain bytes.
pub fn forward_req_headers(src: &HeaderMap, identity_ae: bool) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (k, v) in src.iter() {
        if is_hop(k, STRIP_REQ) {
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
        if is_hop(k, STRIP_RESP) {
            continue;
        }
        out.append(k, v.clone());
    }
    out
}

// ---------------------------------------------------------------------------
// Tapping body: streams SSE through unchanged while extracting usage events.
// ---------------------------------------------------------------------------

pub struct TapBody {
    inner: BoxBody,
    buf: Vec<u8>,
    usage: Arc<Mutex<Option<Usage>>>,
    on_end: Option<Box<dyn FnOnce(Option<Usage>) + Send>>,
}

impl TapBody {
    pub fn new(
        inner: BoxBody,
        usage: Arc<Mutex<Option<Usage>>>,
        on_end: Box<dyn FnOnce(Option<Usage>) + Send>,
    ) -> Self {
        Self {
            inner,
            buf: Vec::new(),
            usage,
            on_end: Some(on_end),
        }
    }

    fn scan(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
        loop {
            let Some(pos) = self.buf.iter().position(|b| *b == b'\n') else {
                if self.buf.len() > (1 << 20) {
                    self.buf.clear();
                }
                break;
            };
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop(); // \n
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if let Some(rest) = line.strip_prefix(b"data:".as_slice()) {
                let rest = String::from_utf8_lossy(rest).trim().to_string();
                if rest.is_empty() || rest == "[DONE]" {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<Value>(&rest) {
                    if let Ok(mut cell) = self.usage.lock() {
                        let u = cell.get_or_insert_with(Usage::default);
                        u.merge_json(&v);
                    }
                }
            }
        }
    }

    fn finish(&mut self) {
        if let Some(on_end) = self.on_end.take() {
            let usage = self
                .usage
                .lock()
                .ok()
                .and_then(|mut c| c.take())
                .filter(|u| u.input_tokens.is_some() || u.output_tokens.is_some());
            on_end(usage);
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
                self.finish();
                Poll::Ready(Some(Err(e)))
            }
            None => {
                self.finish();
                Poll::Ready(None)
            }
        }
    }
}
