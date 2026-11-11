//! A deliberately small HTTP/1.1 client: one request at a time per connection,
//! every connection pinned to a network interface.
//!
//! HTTP/2 is not negotiated on purpose. It multiplexes all streams over a single
//! TCP connection, which defeats both per-interface routing and parallel TCP flows.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use http_body_util::Empty;
use hyper::body::Incoming;
use hyper::client::conn::http1::SendRequest;
use hyper::header::{self, HeaderName, HeaderValue};
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use url::{Host, Url};
use weft_net::BindSpec;

use crate::error::{Error, Result};

const MAX_REDIRECTS: usize = 10;

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

fn tls_config() -> Arc<ClientConfig> {
    static CFG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    CFG.get_or_init(|| {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
        let mut cfg = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("default TLS versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        Arc::new(cfg)
    })
    .clone()
}

/// Per-request settings shared by every connection of a download.
#[derive(Debug, Clone)]
pub struct RequestOptions {
    pub user_agent: String,
    /// Extra headers (Referer, Cookie, Authorization, ...).
    pub headers: Vec<(String, String)>,
    pub connect_timeout: Duration,
    /// Time allowed between sending a request and receiving response headers.
    pub header_timeout: Duration,
}

impl Default for RequestOptions {
    fn default() -> Self {
        Self {
            user_agent: concat!("Weft/", env!("CARGO_PKG_VERSION")).to_string(),
            headers: Vec::new(),
            connect_timeout: Duration::from_secs(10),
            header_timeout: Duration::from_secs(20),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Origin {
    https: bool,
    host: String,
    port: u16,
}

impl Origin {
    fn of(url: &Url) -> Result<Self> {
        let https = match url.scheme() {
            "https" => true,
            "http" => false,
            other => return Err(Error::InvalidUrl(format!("unsupported scheme {other}"))),
        };
        let host = url.host_str().ok_or_else(|| Error::InvalidUrl("missing host".into()))?.to_string();
        let port = url.port_or_known_default().unwrap_or(if https { 443 } else { 80 });
        Ok(Self { https, host, port })
    }
}

/// Resolves a URL's host with the system resolver.
pub async fn resolve(url: &Url) -> Result<Vec<SocketAddr>> {
    let port = url.port_or_known_default().unwrap_or(80);
    match url.host() {
        Some(Host::Ipv4(ip)) => Ok(vec![SocketAddr::new(IpAddr::V4(ip), port)]),
        Some(Host::Ipv6(ip)) => Ok(vec![SocketAddr::new(IpAddr::V6(ip), port)]),
        Some(Host::Domain(name)) => {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((name, port)).await?.collect();
            if addrs.is_empty() {
                return Err(Error::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("no address for {name}"),
                )));
            }
            Ok(addrs)
        }
        None => Err(Error::InvalidUrl("missing host".into())),
    }
}

/// One keep-alive connection to one origin.
pub struct Conn {
    origin: Origin,
    sender: SendRequest<Empty<Bytes>>,
    task: tokio::task::JoinHandle<()>,
    /// Requests already sent on this connection.
    used: u32,
    pub remote: SocketAddr,
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Conn {
    /// Opens a connection (TCP and, for https, TLS) without sending a request.
    pub async fn open(bind: Option<&BindSpec>, url: &Url, opts: &RequestOptions) -> Result<Self> {
        let origin = Origin::of(url)?;
        let addrs = resolve(url).await?;
        let (tcp, remote) = weft_net::connect_any(bind, &addrs, opts.connect_timeout).await?;
        let io: Box<dyn Io> = if origin.https {
            let name = ServerName::try_from(origin.host.trim_matches(['[', ']']).to_string())
                .map_err(|e| Error::Tls(e.to_string()))?;
            let tls = TlsConnector::from(tls_config());
            let stream = tokio::time::timeout(opts.connect_timeout, tls.connect(name, tcp))
                .await
                .map_err(|_| Error::Timeout("TLS handshake"))?
                .map_err(|e| Error::Tls(e.to_string()))?;
            Box::new(stream)
        } else {
            Box::new(tcp)
        };
        let (sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(io)).await?;
        let task = tokio::spawn(async move {
            let _ = conn.await;
        });
        Ok(Self { origin, sender, task, used: 0, remote })
    }

    fn alive(&self) -> bool {
        !self.sender.is_closed() && !self.task.is_finished()
    }
}

/// What to ask for.
#[derive(Debug, Clone, Default)]
pub struct Ask<'a> {
    /// `(first byte, last byte inclusive)`; `None` end means "to the end".
    pub range: Option<(u64, Option<u64>)>,
    /// Validator for `If-Range` (a strong ETag or a Last-Modified date).
    pub if_range: Option<&'a str>,
}

fn build(
    url: &Url,
    opts: &RequestOptions,
    ask: &Ask<'_>,
    send_secrets: bool,
) -> Result<Request<Empty<Bytes>>> {
    let origin = Origin::of(url)?;
    let mut target = url.path().to_string();
    if let Some(q) = url.query() {
        target.push('?');
        target.push_str(q);
    }
    let default_port = if origin.https { 443 } else { 80 };
    let host = if origin.port == default_port {
        origin.host.clone()
    } else {
        format!("{}:{}", origin.host, origin.port)
    };
    let bad = |what: &str| Error::InvalidUrl(format!("bad {what}"));
    let mut req = Request::builder().method("GET").uri(target).body(Empty::new()).map_err(|_| bad("path"))?;
    let h = req.headers_mut();
    h.insert(header::HOST, HeaderValue::from_str(&host).map_err(|_| bad("host"))?);
    h.insert(header::ACCEPT, HeaderValue::from_static("*/*"));
    // Byte offsets only mean something on the unencoded representation.
    h.insert(header::ACCEPT_ENCODING, HeaderValue::from_static("identity"));
    if let Ok(v) = HeaderValue::from_str(&opts.user_agent) {
        h.insert(header::USER_AGENT, v);
    }
    if !url.username().is_empty() && send_secrets {
        let raw = format!("{}:{}", url.username(), url.password().unwrap_or(""));
        let v = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(raw));
        if let Ok(v) = HeaderValue::from_str(&v) {
            h.insert(header::AUTHORIZATION, v);
        }
    }
    for (name, value) in &opts.headers {
        let Ok(name) = HeaderName::from_bytes(name.as_bytes()) else { continue };
        let secret = name == header::COOKIE || name == header::AUTHORIZATION;
        if name == header::HOST
            || name == header::RANGE
            || name == header::ACCEPT_ENCODING
            || (secret && !send_secrets)
        {
            continue;
        }
        if let Ok(v) = HeaderValue::from_str(value) {
            h.insert(name, v);
        }
    }
    if let Some((start, end)) = ask.range {
        let v = match end {
            Some(end) => format!("bytes={start}-{end}"),
            None => format!("bytes={start}-"),
        };
        h.insert(header::RANGE, HeaderValue::from_str(&v).map_err(|_| bad("range"))?);
        if let Some(v) = ask.if_range.and_then(|v| HeaderValue::from_str(v).ok()) {
            h.insert(header::IF_RANGE, v);
        }
    }
    Ok(req)
}

/// A response plus the URL that finally produced it.
pub struct Fetched {
    pub url: Url,
    pub response: Response<Incoming>,
}

/// Sends a GET, following redirects. `slot` holds the connection to reuse and is
/// left holding the connection the response body is arriving on.
///
/// Cookies and credentials are only sent to the host they were given for.
pub async fn get(
    slot: &mut Option<Conn>,
    bind: Option<&BindSpec>,
    url: &Url,
    opts: &RequestOptions,
    ask: &Ask<'_>,
) -> Result<Fetched> {
    let first_host = url.host_str().map(str::to_owned);
    let mut url = url.clone();
    for _ in 0..=MAX_REDIRECTS {
        let origin = Origin::of(&url)?;
        let send_secrets = url.host_str().map(str::to_owned) == first_host;
        let mut attempt = 0;
        let response = loop {
            attempt += 1;
            let reused = match slot {
                Some(c) if c.origin == origin && c.alive() => c.used > 0,
                _ => {
                    *slot = Some(Conn::open(bind, &url, opts).await?);
                    false
                }
            };
            let conn = slot.as_mut().expect("connection just ensured");
            let req = build(&url, opts, ask, send_secrets)?;
            let sent = tokio::time::timeout(opts.header_timeout, async {
                conn.sender.ready().await?;
                conn.sender.send_request(req).await
            })
            .await;
            match sent {
                Ok(Ok(r)) => {
                    conn.used += 1;
                    break r;
                }
                // A kept-alive connection the server closed while idle: retry once on a fresh one.
                Ok(Err(_)) if reused && attempt == 1 => *slot = None,
                Ok(Err(e)) => {
                    *slot = None;
                    return Err(e.into());
                }
                Err(_) => {
                    *slot = None;
                    return Err(Error::Timeout("response headers"));
                }
            }
        };
        if response.status().is_redirection()
            && let Some(loc) = response.headers().get(header::LOCATION).and_then(|v| v.to_str().ok())
        {
            let next = url.join(loc).map_err(|e| Error::InvalidUrl(e.to_string()))?;
            // The redirect body is not read, so this connection cannot be reused.
            *slot = None;
            url = next;
            continue;
        }
        return Ok(Fetched { url, response });
    }
    Err(Error::TooManyRedirects)
}

/// Parses `Retry-After` given in seconds.
pub fn retry_after(resp: &Response<Incoming>) -> Option<Duration> {
    let v = resp.headers().get(header::RETRY_AFTER)?.to_str().ok()?;
    v.trim().parse::<u64>().ok().map(|s| Duration::from_secs(s.min(120)))
}

/// Turns a non-success status into an error.
pub fn check_status(resp: &Response<Incoming>) -> Result<()> {
    let code = resp.status().as_u16();
    if (200..300).contains(&code) {
        Ok(())
    } else {
        Err(Error::Status { code, retry_after: retry_after(resp) })
    }
}

/// `Content-Range: bytes a-b/total` as `(a, b, total)`; total is `None` for `*`.
pub fn content_range(resp: &Response<Incoming>) -> Option<(u64, u64, Option<u64>)> {
    let v = resp.headers().get(header::CONTENT_RANGE)?.to_str().ok()?;
    let rest = v.trim().strip_prefix("bytes")?.trim_start_matches([' ', '=']);
    let (span, total) = rest.split_once('/')?;
    let (a, b) = span.trim().split_once('-')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?, total.trim().parse().ok()))
}

pub fn content_length(resp: &Response<Incoming>) -> Option<u64> {
    resp.headers().get(header::CONTENT_LENGTH)?.to_str().ok()?.trim().parse().ok()
}

pub fn header_str<'a>(resp: &'a Response<Incoming>, name: &str) -> Option<&'a str> {
    resp.headers().get(name)?.to_str().ok()
}
