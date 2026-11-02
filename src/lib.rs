use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper::{Request, Uri};
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use tokio::time::Instant;

pub mod entities;
pub mod parser;

pub use parser::Metadata;

/// Hard cap on compressed wire bytes read per response.
const WIRE_CAP: usize = 256 * 1024;
/// Safety cap on decompressed bytes (zip-bomb guard; pages we care about
/// find `</head>` long before this).
const INFLATE_CAP: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Config {
    pub max_bytes: usize,
    /// Deadline for the whole `fetch` call, including every redirect hop.
    pub total_timeout: Duration,
    pub connect_timeout: Duration,
    pub max_redirects: usize,
    pub user_agent: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_bytes: WIRE_CAP,
            total_timeout: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(5),
            max_redirects: 10,
            user_agent: "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
                         (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36"
                .to_string(),
        }
    }
}

type HttpsClient = Client<hyper_rustls::HttpsConnector<HttpConnector>, Empty<Bytes>>;

pub struct Fetcher {
    client: HttpsClient,
    cfg: Config,
}

impl Fetcher {
    pub fn new(cfg: Config) -> anyhow::Result<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();

        let mut http = HttpConnector::new();
        http.set_connect_timeout(Some(cfg.connect_timeout));
        http.enforce_http(false);
        http.set_nodelay(true);

        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .wrap_connector(http);

        let client = Client::builder(TokioExecutor::new())
            .pool_idle_timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(4)
            .build(https);

        Ok(Self { client, cfg })
    }

    pub fn with_defaults() -> anyhow::Result<Self> {
        Self::new(Config::default())
    }

    pub async fn fetch(&self, url: &str) -> anyhow::Result<Metadata> {
        let deadline = Instant::now() + self.cfg.total_timeout;
        let mut current = url.to_string();
        let mut redirects = 0usize;

        loop {
            match self.fetch_once(&current, deadline).await? {
                Outcome::Redirect(loc) => {
                    if redirects >= self.cfg.max_redirects {
                        anyhow::bail!("too many redirects");
                    }
                    redirects += 1;
                    current = resolve_url(&current, &loc)?;
                }
                Outcome::Done(mut md) => {
                    md.url = current;
                    return Ok(*md);
                }
            }
        }
    }

    async fn fetch_once(&self, url: &str, deadline: Instant) -> anyhow::Result<Outcome> {
        let uri: Uri = url
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid url: {e}"))?;

        let req = Request::builder()
            .method("GET")
            .uri(&uri)
            .header("user-agent", &self.cfg.user_agent)
            .header(
                "accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .header("accept-encoding", "gzip")
            .header("accept-language", "en-US,en;q=0.9")
            .body(Empty::<Bytes>::new())?;

        let resp = tokio::time::timeout_at(deadline, self.client.request(req))
            .await
            .map_err(|_| anyhow::anyhow!("request timeout"))??;

        let status = resp.status();

        // Redirects.
        if status.is_redirection() {
            if let Some(loc) = resp.headers().get(hyper::header::LOCATION) {
                if let Ok(loc) = loc.to_str() {
                    return Ok(Outcome::Redirect(loc.to_string()));
                }
            }
        }

        let encoding = resp
            .headers()
            .get(hyper::header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();

        let content_type = resp
            .headers()
            .get(hyper::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);

        let is_gzip = match encoding.as_str() {
            "" | "identity" => false,
            "gzip" | "x-gzip" => true,
            other => anyhow::bail!("unsupported content-encoding: {other}"),
        };

        let max = self.cfg.max_bytes;
        // Gzip counts decoded bytes; identity counts wire bytes.
        let body_cap = if is_gzip { INFLATE_CAP } else { max };

        let (body, head_end) = match tokio::time::timeout_at(deadline, async {
            let mut resp_body = resp.into_body();
            let mut gz = GzInflate::new();
            let mut scanner = parser::HeadScanner::default();
            let mut body: Vec<u8> = Vec::with_capacity(16 * 1024);
            let mut head_end: Option<usize> = None;
            let mut wire = 0usize;
            let mut scratch = [0u8; 32 * 1024];

            'read: while let Some(frame) = resp_body.frame().await {
                let frame = frame?;
                let Some(data) = frame.data_ref() else {
                    continue;
                };
                wire += data.len();

                if is_gzip {
                    // Truncated streams are tolerated: whatever decoded so far
                    // stays in `body`.
                    if let Err(e) = gz.feed(data, &mut body, &mut scratch, INFLATE_CAP) {
                        // The head may already be decoded; fail only if nothing was produced.
                        if body.is_empty() {
                            return Err(e);
                        }
                        break 'read;
                    }
                } else {
                    body.extend_from_slice(data);
                }

                let capped = body.len() >= body_cap;
                if capped {
                    body.truncate(body_cap);
                }
                if let Some(p) = scanner.find_end(&body) {
                    head_end = Some(p);
                    break 'read;
                }
                if capped || wire >= max {
                    break 'read;
                }
            }
            Ok::<_, anyhow::Error>((body, head_end))
        })
        .await
        {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return Err(e),
            Err(_) => anyhow::bail!("body read timeout"),
        };

        let head = &body[..head_end.unwrap_or(body.len())];
        let mut md = parser::parse(head, content_type.as_deref());
        md.status = status.as_u16();
        Ok(Outcome::Done(Box::new(md)))
    }
}

enum Outcome {
    Redirect(String),
    Done(Box<Metadata>),
}

fn resolve_url(base: &str, loc: &str) -> anyhow::Result<String> {
    let u = url::Url::parse(base)?.join(loc)?;
    anyhow::ensure!(
        matches!(u.scheme(), "http" | "https"),
        "redirect to unsupported scheme: {}",
        u.scheme()
    );
    Ok(u.to_string())
}

/// Total length of a gzip member header if `h` contains enough bytes to
/// determine it. Returns `None` when more bytes are needed.
fn gzip_header_len(h: &[u8]) -> Option<usize> {
    if h.len() < 10 {
        return None;
    }
    if h[0] != 0x1f || h[1] != 0x8b || h[2] != 8 {
        // Enough bytes to know it's invalid.
        return Some(usize::MAX);
    }
    let flg = h[3];
    let mut n = 10usize;
    if flg & 4 != 0 {
        // FEXTRA
        if h.len() < n + 2 {
            return None;
        }
        n += 2 + u16::from_le_bytes([h[n], h[n + 1]]) as usize;
    }
    if flg & 8 != 0 {
        // FNAME: zero-terminated
        let rest = h.get(n..)?;
        n += rest.iter().position(|&b| b == 0)? + 1;
    }
    if flg & 16 != 0 {
        // FCOMMENT: zero-terminated
        let rest = h.get(n..)?;
        n += rest.iter().position(|&b| b == 0)? + 1;
    }
    if flg & 2 != 0 {
        // FHCRC
        if h.len() < n + 2 {
            return None;
        }
        n += 2;
    }
    (h.len() >= n).then_some(n)
}

struct GzInflate {
    dec: flate2::Decompress,
    pending: Vec<u8>,
    in_stream: bool,
    trailer_left: usize,
}

impl GzInflate {
    fn new() -> Self {
        Self {
            dec: flate2::Decompress::new(false),
            pending: Vec::with_capacity(64),
            in_stream: false,
            trailer_left: 0,
        }
    }

    /// Returns `Err` only on hard deflate/format errors; truncated streams
    /// are fine (whatever was decoded so far stays in `out`). Inflation stops
    /// once `out` reaches `limit`; do not feed again afterwards.
    fn feed(
        &mut self,
        mut input: &[u8],
        out: &mut Vec<u8>,
        scratch: &mut [u8],
        limit: usize,
    ) -> anyhow::Result<()> {
        use flate2::{FlushDecompress, Status};

        loop {
            // 1. Skip the 8-byte member trailer, if we're inside one.
            //    It may live in `pending` (held back from an earlier feed),
            //    in `input`, or straddle both.
            if self.trailer_left > 0 {
                if !self.pending.is_empty() {
                    let n = self.trailer_left.min(self.pending.len());
                    self.pending.drain(..n);
                    self.trailer_left -= n;
                }
                if self.trailer_left > 0 {
                    if input.is_empty() {
                        break;
                    }
                    let n = self.trailer_left.min(input.len());
                    input = &input[n..];
                    self.trailer_left -= n;
                }
                if self.trailer_left == 0 {
                    self.in_stream = false;
                    // NB: keep `pending` — bytes past the trailer belong to
                    // the next member and are parsed as its header in step 2.
                }
                continue;
            }

            // 2. Parse the member header (spans feed boundaries via `pending`).
            if !self.in_stream {
                match gzip_header_len(&self.pending) {
                    Some(hl) if hl == usize::MAX => anyhow::bail!("not a gzip stream"),
                    Some(hl) => {
                        self.pending.drain(..hl);
                        self.dec = flate2::Decompress::new(false);
                        self.in_stream = true;
                    }
                    None => {
                        if input.is_empty() {
                            break; // need more bytes next feed
                        }
                        // Copy only a bounded slice; the loop repeats until
                        // the header is complete or input runs out. Guards
                        // against absurd "headers" (corrupt streams).
                        let take = input.len().min(4096);
                        anyhow::ensure!(
                            self.pending.len() + take < 70_000,
                            "bad gzip header"
                        );
                        self.pending.extend_from_slice(&input[..take]);
                        input = &input[take..];
                        continue;
                    }
                }
            }

            // 3. Inflate: drain `pending` first (bytes that arrived attached
            //    to the header), then consume `input` directly.
            let chunk: &[u8] = if self.pending.is_empty() {
                input
            } else {
                &self.pending
            };
            if chunk.is_empty() {
                break;
            }
            let in_before = self.dec.total_in();
            let out_before = self.dec.total_out();
            let status = self
                .dec
                .decompress(chunk, scratch, FlushDecompress::None)
                .map_err(|e| anyhow::anyhow!("gzip error: {e}"))?;
            let consumed = (self.dec.total_in() - in_before) as usize;
            let produced = (self.dec.total_out() - out_before) as usize;
            out.extend_from_slice(&scratch[..produced]);

            if self.pending.is_empty() {
                input = &input[consumed..];
            } else {
                self.pending.drain(..consumed);
            }

            // Bounds work on highly compressible input.
            if out.len() >= limit {
                return Ok(());
            }

            match status {
                Status::StreamEnd => {
                    self.trailer_left = 8;
                }
                Status::Ok | Status::BufError => {
                    // No progress and no output: needs more input.
                    if consumed == 0 && produced == 0 {
                        break;
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn gzip_roundtrip_single_feed() {
        let plain: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
        let comp = gzip(&plain);
        let mut gz = GzInflate::new();
        let mut out = Vec::new();
        let mut scratch = [0u8; 4096];
        gz.feed(&comp, &mut out, &mut scratch, usize::MAX).unwrap();
        assert_eq!(out, plain);
    }

    #[test]
    fn gzip_roundtrip_byte_at_a_time() {
        let plain = b"<html><head><title>hi</title></head><body>x</body></html>";
        let comp = gzip(plain);
        let mut gz = GzInflate::new();
        let mut out = Vec::new();
        let mut scratch = [0u8; 64];
        for b in &comp {
            gz.feed(std::slice::from_ref(b), &mut out, &mut scratch, usize::MAX)
                .unwrap();
        }
        assert_eq!(out, plain);
    }

    #[test]
    fn gzip_truncated_is_tolerated() {
        let plain: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        let comp = gzip(&plain);
        let mut gz = GzInflate::new();
        let mut out = Vec::new();
        let mut scratch = [0u8; 4096];
        gz.feed(&comp[..comp.len() / 3], &mut out, &mut scratch, usize::MAX)
            .unwrap();
        assert!(!out.is_empty());
        assert!(out.len() < plain.len());
        assert!(out[..] == plain[..out.len()]);
    }

    #[test]
    fn gzip_multi_member() {
        let a = b"first member ";
        let b = b"second member";
        let mut comp = gzip(a);
        comp.extend_from_slice(&gzip(b));
        let mut gz = GzInflate::new();
        let mut out = Vec::new();
        let mut scratch = [0u8; 4096];
        gz.feed(&comp, &mut out, &mut scratch, usize::MAX).unwrap();
        assert_eq!(out, b"first member second member");
    }

    #[test]
    fn gzip_with_fname_header() {
        // Manually craft a member with FNAME set.
        let plain = b"named member";
        let mut comp = Vec::new();
        comp.extend_from_slice(b"\x1f\x8b\x08\x08"); // magic, deflate, FNAME
        comp.extend_from_slice(&[0; 4]); // mtime
        comp.extend_from_slice(&[0, 3]); // xfl, os
        comp.extend_from_slice(b"file.txt\0");
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(plain).unwrap();
        let raw = e.finish().unwrap();
        // GzEncoder already added a header; strip its 10-byte header and use ours.
        comp.extend_from_slice(&raw[10..]);
        let mut gz = GzInflate::new();
        let mut out = Vec::new();
        let mut scratch = [0u8; 4096];
        gz.feed(&comp, &mut out, &mut scratch, usize::MAX).unwrap();
        assert_eq!(out, plain);
    }

    #[test]
    fn gzip_output_limit_bounds_a_bomb() {
        // Highly compressible input must stop at the limit.
        let plain = vec![0u8; 20 * 1024 * 1024];
        let comp = gzip(&plain);
        assert!(comp.len() < 64 * 1024);

        let limit = 1_000_000usize;
        let mut gz = GzInflate::new();
        let mut out = Vec::new();
        let mut scratch = [0u8; 4096];
        gz.feed(&comp, &mut out, &mut scratch, limit).unwrap();
        assert!(out.len() >= limit);
        assert!(out.len() <= limit + scratch.len());
    }

    #[test]
    fn redirect_scheme_is_validated() {
        assert!(resolve_url("https://a.test/x", "/y").is_ok());
        assert!(resolve_url("https://a.test/x", "file:///etc/passwd").is_err());
    }
}