//! The door the browser extension knocks on.
//!
//! A tiny HTTP service on the loopback interface. What can reach it, and what it lets in:
//!
//! * Only this machine: it listens on `127.0.0.1`, never on a LAN address.
//! * Not web pages: a request carrying a web `Origin` is refused, and the `Host`
//!   header must name the loopback address (this defeats DNS rebinding).
//! * Not other programs or other extensions: every request that does anything
//!   needs the pairing token, which the user copies from the app into the extension.
//! * Even a paired extension can only *offer* an `http`/`https` link. Whether an
//!   offer starts by itself or waits for a click is the app's decision.

use std::convert::Infallible;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::header::{self, HeaderValue};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

/// Ports tried in order. Fixed, so the extension knows where to look.
pub const PORTS: [u16; 3] = [47610, 47611, 47612];
const MAX_BODY: usize = 64 * 1024;

/// A link the browser hands over, with what is needed to fetch it as the browser would.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Offer {
    pub url: String,
    pub file_name: Option<String>,
    pub referrer: Option<String>,
    pub cookie: Option<String>,
    pub user_agent: Option<String>,
}

/// Decides what happens to an offer. Returns the id of the new download.
pub type OnOffer = dyn Fn(Offer) -> Result<u64, String> + Send + Sync;

/// A running bridge. Dropping it stops the service.
pub struct Bridge {
    pub port: u16,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Reads the pairing token from `dir`, creating a random one on first use.
pub fn load_or_create_token(dir: &Path) -> io::Result<String> {
    let file = dir.join("bridge-token");
    if let Ok(t) = std::fs::read_to_string(&file) {
        let t = t.trim().to_string();
        if t.len() >= 32 && t.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(t);
        }
    }
    let mut raw = [0u8; 24];
    SystemRandom::new().fill(&mut raw).map_err(|_| io::Error::other("no system randomness"))?;
    let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::create_dir_all(dir)?;
    std::fs::write(&file, &token)?;
    Ok(token)
}

fn same(a: &[u8], b: &[u8]) -> bool {
    // Constant time, so the token cannot be guessed one character at a time.
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn extension_origin(origin: &str) -> bool {
    ["chrome-extension://", "moz-extension://"].iter().any(|p| {
        origin
            .strip_prefix(p)
            .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
    })
}

fn reply(status: StatusCode, body: serde_json::Value, origin: Option<&str>) -> Response<Full<Bytes>> {
    let mut r = Response::new(Full::new(Bytes::from(body.to_string())));
    *r.status_mut() = status;
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Some(v) = origin.and_then(|o| HeaderValue::from_str(o).ok()) {
        h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
        h.insert(header::VARY, HeaderValue::from_static("Origin"));
    }
    r
}

fn fail(status: StatusCode, why: &str, origin: Option<&str>) -> Response<Full<Bytes>> {
    reply(status, serde_json::json!({ "error": why }), origin)
}

async fn handle(
    req: Request<Incoming>,
    port: u16,
    token: Arc<str>,
    on_offer: Arc<OnOffer>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let header =
        |name: header::HeaderName| req.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_owned);
    let host = header(header::HOST).unwrap_or_default();
    if host != format!("127.0.0.1:{port}") && host != format!("localhost:{port}") {
        return Ok(fail(StatusCode::FORBIDDEN, "wrong host", None));
    }
    let origin = header(header::ORIGIN);
    if origin.as_deref().is_some_and(|o| !extension_origin(o)) {
        return Ok(fail(StatusCode::FORBIDDEN, "origin not allowed", None));
    }
    let origin = origin.as_deref();
    let paired = header(header::AUTHORIZATION)
        .and_then(|v| v.strip_prefix("Bearer ").map(str::to_owned))
        .is_some_and(|t| same(t.trim().as_bytes(), token.as_bytes()));

    match (req.method().clone(), req.uri().path()) {
        (Method::OPTIONS, _) => {
            let mut r = reply(StatusCode::NO_CONTENT, serde_json::json!({}), origin);
            let h = r.headers_mut();
            h.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("GET, POST"));
            h.insert(
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                HeaderValue::from_static("authorization, content-type"),
            );
            h.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("600"));
            Ok(r)
        }
        (Method::GET, "/v1/ping") => Ok(reply(
            StatusCode::OK,
            serde_json::json!({ "app": "weft", "version": env!("CARGO_PKG_VERSION"), "paired": paired }),
            origin,
        )),
        (Method::POST, "/v1/offer") => {
            if !paired {
                return Ok(fail(StatusCode::UNAUTHORIZED, "not paired", origin));
            }
            let json = header(header::CONTENT_TYPE)
                .is_some_and(|v| v.to_ascii_lowercase().starts_with("application/json"));
            if !json {
                return Ok(fail(StatusCode::UNSUPPORTED_MEDIA_TYPE, "json only", origin));
            }
            let Ok(body) = Limited::new(req.into_body(), MAX_BODY).collect().await else {
                return Ok(fail(StatusCode::PAYLOAD_TOO_LARGE, "request too large", origin));
            };
            let Ok(offer) = serde_json::from_slice::<Offer>(&body.to_bytes()) else {
                return Ok(fail(StatusCode::BAD_REQUEST, "not a valid offer", origin));
            };
            let scheme_ok =
                ["http://", "https://"].iter().any(|s| offer.url.to_ascii_lowercase().starts_with(s));
            if !scheme_ok {
                return Ok(fail(StatusCode::BAD_REQUEST, "only http and https links", origin));
            }
            match on_offer(offer) {
                Ok(id) => Ok(reply(StatusCode::OK, serde_json::json!({ "id": id }), origin)),
                Err(e) => Ok(fail(StatusCode::BAD_REQUEST, &e, origin)),
            }
        }
        _ => Ok(fail(StatusCode::NOT_FOUND, "no such endpoint", origin)),
    }
}

/// Starts the bridge on the first free port of `ports`, on the loopback interface only.
pub async fn serve(token: String, ports: &[u16], on_offer: Arc<OnOffer>) -> io::Result<Bridge> {
    let mut last = io::Error::new(io::ErrorKind::AddrInUse, "no port given");
    for &want in ports {
        let listener = match TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, want))).await {
            Ok(l) => l,
            Err(e) => {
                last = e;
                continue;
            }
        };
        let port = listener.local_addr()?.port();
        let token: Arc<str> = token.into();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, peer)) = listener.accept().await else { continue };
                if !peer.ip().is_loopback() {
                    continue;
                }
                let (token, on_offer) = (token.clone(), on_offer.clone());
                tokio::spawn(async move {
                    let svc = service_fn(move |req| handle(req, port, token.clone(), on_offer.clone()));
                    let conn = hyper::server::conn::http1::Builder::new()
                        .header_read_timeout(std::time::Duration::from_secs(5))
                        .timer(hyper_util::rt::TokioTimer::new())
                        .serve_connection(TokioIo::new(stream), svc);
                    let _ = conn.await;
                });
            }
        });
        return Ok(Bridge { port, task });
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn send(port: u16, raw: String) -> (u16, String) {
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(raw.as_bytes()).await.unwrap();
        let mut out = String::new();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), s.read_to_string(&mut out)).await;
        let code = out.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
        (code, out)
    }

    fn post(port: u16, host: &str, extra: &str, body: &str) -> String {
        format!(
            "POST /v1/offer HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .replace("{port}", &port.to_string())
    }

    #[tokio::test]
    async fn only_a_paired_extension_gets_in() {
        let dir = tempfile::tempdir().unwrap();
        let token = load_or_create_token(dir.path()).unwrap();
        assert_eq!(token.len(), 48);
        assert_eq!(load_or_create_token(dir.path()).unwrap(), token, "token must be stable");

        let seen: Arc<Mutex<Vec<Offer>>> = Arc::default();
        let sink = seen.clone();
        let bridge = serve(
            token.clone(),
            &[0],
            Arc::new(move |o| {
                sink.lock().unwrap().push(o);
                Ok(7)
            }),
        )
        .await
        .unwrap();
        let p = bridge.port;
        let host = format!("127.0.0.1:{p}");
        let auth = format!("Authorization: Bearer {token}\r\n");
        let ext = "Origin: chrome-extension://abcdefghijklmnop\r\n";
        let body = r#"{"url":"https://example.com/a.zip","file_name":"a.zip","cookie":"s=1"}"#;

        // A paired extension: accepted.
        let (code, text) = send(p, post(p, &host, &format!("{auth}{ext}"), body)).await;
        assert_eq!(code, 200, "{text}");
        assert!(
            text.contains(r#""id":7"#)
                && text.contains("access-control-allow-origin: chrome-extension://abcdefghijklmnop")
        );
        assert_eq!(seen.lock().unwrap()[0].cookie.as_deref(), Some("s=1"));

        // No token, wrong token.
        assert_eq!(send(p, post(p, &host, ext, body)).await.0, 401);
        assert_eq!(
            send(p, post(p, &host, &format!("Authorization: Bearer {}\r\n", "0".repeat(48)), body)).await.0,
            401
        );
        // A web page (even with the token) and a rebinding attempt.
        let web = "Origin: https://evil.example\r\n";
        assert_eq!(send(p, post(p, &host, &format!("{auth}{web}"), body)).await.0, 403);
        assert_eq!(send(p, post(p, "evil.example", &auth, body)).await.0, 403);
        // Things that are not links to fetch.
        for bad in [r#"{"url":"file:///C:/Windows/win.ini"}"#, r#"{"url":"javascript:alert(1)"}"#, "not json"]
        {
            assert_eq!(send(p, post(p, &host, &auth, bad)).await.0, 400, "{bad}");
        }
        // Too large.
        let big = format!(r#"{{"url":"https://example.com/","cookie":"{}"}}"#, "x".repeat(MAX_BODY));
        assert_eq!(send(p, post(p, &host, &auth, &big)).await.0, 413);
        // Not JSON content type.
        let form = post(p, &host, &auth, body).replace("application/json", "text/plain");
        assert_eq!(send(p, form).await.0, 415);
        assert_eq!(seen.lock().unwrap().len(), 1, "a refused request reached the app");

        // Ping tells an unpaired caller nothing but that the app is there.
        let ping = format!("GET /v1/ping HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
        let (code, text) = send(p, ping).await;
        assert!(code == 200 && text.contains(r#""paired":false"#) && !text.contains(&token));
    }
}
