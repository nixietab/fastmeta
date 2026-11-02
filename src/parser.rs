use std::collections::HashMap;

use serde::Serialize;

use crate::entities::decode_entities;

#[derive(Debug, Default, Serialize, Clone)]
pub struct Metadata {
    pub url: String,
    pub status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charset: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub favicon: Option<String>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub meta: HashMap<String, String>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub og: HashMap<String, String>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub twitter: HashMap<String, String>,
}

fn is_ws(b: u8) -> bool {
    b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' || b == b'\x0c'
}

/// Bytes that terminate a tag name.
fn is_name_end(b: u8) -> bool {
    is_ws(b) || b == b'/' || b == b'>'
}

// Tokenizer that only locates tag boundaries, skipping comments, quoted
// attribute values and script/style bodies.
enum Kind<'a> {
    Start {
        name: &'a [u8],
        /// Bytes between the tag name and the closing `>`.
        attrs: &'a [u8],
        /// Content of raw-text elements (script/style/title); empty otherwise.
        text: &'a [u8],
    },
    End {
        name: &'a [u8],
    },
    Other,
}

enum Next<'a> {
    Done,
    /// The construct starting at this offset is not complete yet.
    Partial(usize),
    Tag {
        lt: usize,
        next: usize,
        kind: Kind<'a>,
    },
}

fn is_raw_text(name: &[u8]) -> bool {
    name.eq_ignore_ascii_case(b"script")
        || name.eq_ignore_ascii_case(b"style")
        || name.eq_ignore_ascii_case(b"title")
}

fn is_head_end(kind: &Kind) -> bool {
    match kind {
        Kind::End { name } => name.eq_ignore_ascii_case(b"head"),
        Kind::Start { name, .. } => name.eq_ignore_ascii_case(b"body"),
        Kind::Other => false,
    }
}

/// Index of the `>` that closes a tag. Quotes only open a value after `=`.
fn find_tag_end(buf: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    let mut after_eq = false;
    while i < buf.len() {
        let b = buf[i];
        match b {
            b'>' => return Some(i),
            b'=' => after_eq = true,
            b'"' | b'\'' if after_eq => {
                let close = memchr::memchr(b, &buf[i + 1..])?;
                i += 1 + close;
                after_eq = false;
            }
            _ if is_ws(b) => {}
            _ => after_eq = false,
        }
        i += 1;
    }
    None
}

/// Finds `</name` (ASCII case-insensitive) followed by a name terminator.
/// Returns `None` if absent or if the buffer ends before it can be decided.
fn find_close_tag(buf: &[u8], from: usize, name: &[u8]) -> Option<usize> {
    let mut start = from;
    while let Some(rel) = memchr::memmem::find(&buf[start..], b"</") {
        let pos = start + rel;
        let after = pos + 2 + name.len();
        let cand = buf.get(pos + 2..after)?;
        if cand.eq_ignore_ascii_case(name) {
            let &b = buf.get(after)?;
            if is_name_end(b) {
                return Some(pos);
            }
        }
        start = pos + 1;
    }
    None
}

fn skip_to_gt(buf: &[u8], lt: usize) -> Next<'_> {
    match memchr::memchr(b'>', &buf[lt + 1..]) {
        Some(r) => Next::Tag {
            lt,
            next: lt + 1 + r + 1,
            kind: Kind::Other,
        },
        None => Next::Partial(lt),
    }
}

fn scan_next(buf: &[u8], pos: usize) -> Next<'_> {
    let Some(tail) = buf.get(pos..) else {
        return Next::Done;
    };
    let Some(rel) = memchr::memchr(b'<', tail) else {
        return Next::Done;
    };
    let lt = pos + rel;
    let rest = &buf[lt..];
    let Some(&c1) = rest.get(1) else {
        return Next::Partial(lt);
    };

    match c1 {
        b'!' => {
            if rest.len() < 4 && b"<!--".starts_with(rest) {
                return Next::Partial(lt);
            }
            if rest.starts_with(b"<!--") {
                // Search from lt+2 so the degenerate comments `<!-->` and
                // `<!--->` terminate immediately, as in browsers.
                return match memchr::memmem::find(&buf[lt + 2..], b"-->") {
                    Some(r) => Next::Tag {
                        lt,
                        next: lt + 2 + r + 3,
                        kind: Kind::Other,
                    },
                    None => Next::Partial(lt),
                };
            }
            skip_to_gt(buf, lt)
        }
        b'?' => skip_to_gt(buf, lt),
        b'/' => {
            let Some(&c2) = rest.get(2) else {
                return Next::Partial(lt);
            };
            if !c2.is_ascii_alphabetic() {
                // Bogus comment per the HTML spec.
                return skip_to_gt(buf, lt);
            }
            let Some(rel) = rest[2..].iter().position(|&b| is_name_end(b)) else {
                return Next::Partial(lt);
            };
            let name_end = lt + 2 + rel;
            let name = &buf[lt + 2..name_end];
            match find_tag_end(buf, name_end) {
                Some(gt) => Next::Tag {
                    lt,
                    next: gt + 1,
                    kind: Kind::End { name },
                },
                None => Next::Partial(lt),
            }
        }
        c if c.is_ascii_alphabetic() => {
            let Some(rel) = rest[1..].iter().position(|&b| is_name_end(b)) else {
                return Next::Partial(lt);
            };
            let name_end = lt + 1 + rel;
            let name = &buf[lt + 1..name_end];
            let Some(gt) = find_tag_end(buf, name_end) else {
                return Next::Partial(lt);
            };
            let attrs = &buf[name_end..gt];
            let mut next = gt + 1;
            let mut text: &[u8] = &[];
            if is_raw_text(name) {
                match find_close_tag(buf, gt + 1, name) {
                    Some(close) => {
                        text = &buf[gt + 1..close];
                        // Leave the end tag to be tokenized normally.
                        next = close;
                    }
                    None => return Next::Partial(lt),
                }
            }
            Next::Tag {
                lt,
                next,
                kind: Kind::Start { name, attrs, text },
            }
        }
        // `<` followed by something that can't start a tag: it's text.
        _ => Next::Tag {
            lt,
            next: lt + 1,
            kind: Kind::Other,
        },
    }
}

/// Finds the end of `<head>`: the start of `</head>` or `<body`.
/// Keeps its position between calls so a growing buffer is scanned in linear
/// time. The buffer must only grow by appending.
#[derive(Debug, Default)]
pub struct HeadScanner {
    pos: usize,
}

impl HeadScanner {
    /// Returns the byte offset where the head ends, once seen.
    pub fn find_end(&mut self, buf: &[u8]) -> Option<usize> {
        loop {
            match scan_next(buf, self.pos) {
                Next::Done => {
                    self.pos = buf.len();
                    return None;
                }
                Next::Partial(lt) => {
                    self.pos = lt;
                    return None;
                }
                Next::Tag { lt, next, kind } => {
                    if is_head_end(&kind) {
                        return Some(lt);
                    }
                    self.pos = next;
                }
            }
        }
    }
}

pub fn find_head_end(buf: &[u8]) -> Option<usize> {
    HeadScanner::default().find_end(buf)
}

pub fn parse(head: &[u8], content_type: Option<&str>) -> Metadata {
    let mut md = Metadata {
        charset: content_type.and_then(extract_charset),
        ..Metadata::default()
    };

    // Best favicon candidate seen so far: (priority, href). Higher wins.
    let mut favicon: Option<(u8, String)> = None;

    let mut pos = 0usize;
    while let Next::Tag { next, kind, .. } = scan_next(head, pos) {
        pos = next;
        if is_head_end(&kind) {
            break;
        }
        let Kind::Start { name, attrs: raw, text } = kind else {
            continue;
        };

        if name.eq_ignore_ascii_case(b"title") {
            // First <title> wins (matches browsers).
            if md.title.is_none() {
                let s = String::from_utf8_lossy(text);
                md.title = Some(decode_entities(&collapse_ws(&s)));
            }
        } else if name.eq_ignore_ascii_case(b"meta") {
            let attrs = parse_attrs(raw);
            if let Some(cs) = attrs.get("charset") {
                md.charset.get_or_insert_with(|| cs.trim().to_string());
            }
            if let (Some(eq), Some(c)) = (attrs.get("http-equiv"), attrs.get("content")) {
                if eq.eq_ignore_ascii_case("content-type") && md.charset.is_none() {
                    md.charset = extract_charset(c);
                }
            }

            let key_attr = attrs.get("name").or_else(|| attrs.get("property"));
            if let (Some(n), Some(c)) = (key_attr, attrs.get("content")) {
                let key = n.trim().to_ascii_lowercase();
                let val = decode_entities(c);
                // First occurrence of a key wins.
                if let Some(rest) = key.strip_prefix("og:") {
                    md.og.entry(rest.to_string()).or_insert(val);
                } else if let Some(rest) = key.strip_prefix("twitter:") {
                    md.twitter.entry(rest.to_string()).or_insert(val);
                } else {
                    if key == "description" && md.description.is_none() {
                        md.description = Some(val.clone());
                    }
                    md.meta.entry(key).or_insert(val);
                }
            }
        } else if name.eq_ignore_ascii_case(b"link") {
            let attrs = parse_attrs(raw);
            if let (Some(rel), Some(href)) = (attrs.get("rel"), attrs.get("href")) {
                let href = href.trim();
                if !href.is_empty() {
                    if rel.eq_ignore_ascii_case("canonical") && md.canonical.is_none() {
                        md.canonical = Some(decode_entities(href));
                    }
                    if let Some(priority) = favicon_priority(
                        rel,
                        attrs.get("type").map(String::as_str),
                        attrs.get("sizes").map(String::as_str),
                    ) {
                        if favicon.as_ref().map_or(true, |(best, _)| priority > *best) {
                            favicon = Some((priority, decode_entities(href)));
                        }
                    }
                }
            }
        } else if name.eq_ignore_ascii_case(b"html") {
            let attrs = parse_attrs(raw);
            if let Some(lang) = attrs.get("lang") {
                md.lang = Some(lang.trim().to_string());
            }
        }
    }

    md.favicon = favicon.map(|(_, href)| href);
    md
}

/// Returns a score for a `<link>` tag that can serve as a favicon, or `None`
/// for unrelated rel values. SVG icons and larger bitmaps win over the
/// ubiquitous 16px `favicon.ico`.
fn favicon_priority(rel: &str, content_type: Option<&str>, sizes: Option<&str>) -> Option<u8> {
    let mut priority = 0u8;
    let mut is_icon = false;

    for token in rel.split_ascii_whitespace() {
        if token.eq_ignore_ascii_case("icon") {
            priority = priority.max(80);
            is_icon = true;
        } else if token.eq_ignore_ascii_case("apple-touch-icon") {
            priority = priority.max(60);
            is_icon = true;
        }
    }
    if !is_icon {
        return None;
    }

    let is_svg = content_type.map_or(false, |t| {
        t.split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("image/svg+xml")
    });
    if is_svg {
        priority += 20;
    }

    if let Some(sizes) = sizes {
        let large = sizes
            .split_ascii_whitespace()
            .filter_map(|s| s.split_once(|c| c == 'x' || c == 'X'))
            .filter_map(|(w, _)| w.parse::<u32>().ok())
            .any(|w| w >= 32);
        if large {
            priority += 10;
        } else if sizes.eq_ignore_ascii_case("any") {
            priority += 5;
        }
    }

    Some(priority)
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for w in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    out
}

/// Panic-free attribute scanner over the *raw* (non-lowercased) tag attributes.
/// Byte-index slicing is safe because every boundary we stop at is an ASCII
/// byte (`<`, `>`, `"`, `'`, whitespace). Keys are lowercased on insertion;
/// values keep their original case. Only called for the handful of tags we
/// actually care about, so a HashMap per tag is fine.
fn parse_attrs(s: &[u8]) -> HashMap<String, String> {
    let mut map: HashMap<String, String> = HashMap::new();
    let bytes = s;
    let mut i = 0usize;

    while i < bytes.len() {
        while i < bytes.len() && (is_ws(bytes[i]) || bytes[i] == b'/') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }

        let name_start = i;
        while i < bytes.len() && !is_ws(bytes[i]) && bytes[i] != b'=' && bytes[i] != b'/' {
            i += 1;
        }
        if name_start == i {
            i += 1;
            continue;
        }
        let name = String::from_utf8_lossy(&bytes[name_start..i]).to_ascii_lowercase();

        while i < bytes.len() && is_ws(bytes[i]) {
            i += 1;
        }

        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && is_ws(bytes[i]) {
                i += 1;
            }
            if i >= bytes.len() {
                map.entry(name).or_default();
                break;
            }

            let value = if bytes[i] == b'"' || bytes[i] == b'\'' {
                let quote = bytes[i];
                i += 1;
                let vs = i;
                while i < bytes.len() && bytes[i] != quote {
                    i += 1;
                }
                let v = String::from_utf8_lossy(&bytes[vs..i]).into_owned();
                if i < bytes.len() {
                    i += 1;
                }
                v
            } else {
                let vs = i;
                while i < bytes.len() && !is_ws(bytes[i]) {
                    i += 1;
                }
                String::from_utf8_lossy(&bytes[vs..i]).into_owned()
            };

            map.entry(name).or_insert(value);
        } else {
            map.entry(name).or_default();
        }
    }

    map
}

fn extract_charset(ct: &str) -> Option<String> {
    let lower = ct.to_ascii_lowercase();
    let idx = lower.find("charset")?;
    let rest = ct.get(idx + 7..)?;
    let rest = rest.trim_start_matches(|c: char| c == '=' || c.is_whitespace());
    let end = rest
        .find(|c: char| c == ';' || c == '"' || c == '\'' || c.is_whitespace())
        .unwrap_or(rest.len());
    let cs = rest[..end].trim();
    if cs.is_empty() {
        None
    } else {
        Some(cs.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_no_trailing_bracket() {
        let md = parse(b"<html><head><title>Hacker News</title></head></html>", None);
        assert_eq!(md.title.as_deref(), Some("Hacker News"));
    }

    #[test]
    fn title_case_insensitive() {
        let md = parse(b"<TITLE>Hi</TITLE>", None);
        assert_eq!(md.title.as_deref(), Some("Hi"));
    }

    #[test]
    fn first_title_wins_and_whitespace_collapses() {
        let md = parse(b"<title>\n  Hello \t World\n</title><title>B</title>", None);
        assert_eq!(md.title.as_deref(), Some("Hello World"));
    }

    #[test]
    fn og_and_twitter() {
        let md = parse(
            b"<meta property=\"og:title\" content=\"T&amp;itle\">\
              <meta name=\"twitter:card\" content=\"summary\">",
            None,
        );
        assert_eq!(md.og.get("title").map(String::as_str), Some("T&itle"));
        assert_eq!(md.twitter.get("card").map(String::as_str), Some("summary"));
    }

    #[test]
    fn duplicate_keys_first_wins() {
        let md = parse(
            b"<meta property=\"og:title\" content=\"A\">\
              <meta property=\"og:title\" content=\"B\">\
              <meta name=\"description\" content=\"D1\">\
              <meta name=\"description\" content=\"D2\">",
            None,
        );
        assert_eq!(md.og.get("title").map(String::as_str), Some("A"));
        assert_eq!(md.description.as_deref(), Some("D1"));
        assert_eq!(md.meta.get("description").map(String::as_str), Some("D1"));
    }

    #[test]
    fn gt_inside_quoted_attribute() {
        let md = parse(b"<meta name=\"description\" content=\"a > b\"><title>T</title>", None);
        assert_eq!(md.description.as_deref(), Some("a > b"));
        assert_eq!(md.title.as_deref(), Some("T"));
    }

    #[test]
    fn gt_inside_comment_does_not_leak_tags() {
        let md = parse(
            b"<!-- a > b <meta name=\"x\" content=\"y\"> --><title>Z</title>",
            None,
        );
        assert!(md.meta.is_empty());
        assert_eq!(md.title.as_deref(), Some("Z"));
    }

    #[test]
    fn script_and_style_contents_are_skipped() {
        let md = parse(
            br#"<script>document.write('<meta name="x" content="y">')</script>
                <style>/* <meta name="q" content="r"> */</style><title>Z</title>"#,
            None,
        );
        assert!(md.meta.is_empty());
        assert_eq!(md.title.as_deref(), Some("Z"));
    }

    #[test]
    fn empty_tag_does_not_abort_parsing() {
        let md = parse(b"<><title>Z</title>", None);
        assert_eq!(md.title.as_deref(), Some("Z"));
    }

    #[test]
    fn parse_stops_at_body() {
        let md = parse(
            b"<title>A</title><body><meta name=\"x\" content=\"y\">",
            None,
        );
        assert_eq!(md.title.as_deref(), Some("A"));
        assert!(md.meta.is_empty());
    }

    #[test]
    fn head_end_finds_close_head_or_body() {
        let s = "<title>a</title></HEAD><body>";
        assert_eq!(find_head_end(s.as_bytes()), s.find("</HEAD"));
        let s = "<meta charset=x><BODY class=a>";
        assert_eq!(find_head_end(s.as_bytes()), s.find("<BODY"));
        assert_eq!(find_head_end(b"<title>a</title>"), None);
    }

    #[test]
    fn head_end_ignores_script_comment_and_quotes() {
        let s = "<script>var s='</head><body>';</script>\
                 <!-- </head> --><meta content=\"</head>\"></head>";
        assert_eq!(find_head_end(s.as_bytes()), s.rfind("</head>"));
    }

    #[test]
    fn head_end_does_not_match_header() {
        let s = "</header></head>";
        assert_eq!(find_head_end(s.as_bytes()), s.find("</head>"));
    }

    #[test]
    fn incremental_scanner_matches_full_scan() {
        let s: &[u8] = b"<meta content=\"a>b\"><script>x</head></script>\
                         <!-- </head> --><title>t</title></head><body>";
        let full = find_head_end(s);
        assert!(full.is_some());
        for step in 1..9 {
            let mut sc = HeadScanner::default();
            let (mut got, mut n) = (None, 0usize);
            while n < s.len() && got.is_none() {
                n = (n + step).min(s.len());
                got = sc.find_end(&s[..n]);
            }
            assert_eq!(got, full, "step {step}");
        }
    }

    #[test]
    fn canonical_and_lang() {
        let md = parse(
            b"<html lang=\"en\"><link rel=\"Canonical\" href=\"https://x/y\"></html>",
            None,
        );
        assert_eq!(md.lang.as_deref(), Some("en"));
        assert_eq!(md.canonical.as_deref(), Some("https://x/y"));
    }

    #[test]
    fn charset_from_content_type_and_meta() {
        // NB: charset value case is preserved (matches historical behavior).
        assert_eq!(
            extract_charset("text/html; charset=UTF-8").as_deref(),
            Some("UTF-8")
        );
        let md = parse(b"<meta charset=\"iso-8859-1\">", Some("text/html"));
        assert_eq!(md.charset.as_deref(), Some("iso-8859-1"));
    }

    #[test]
    fn comment_and_doctype_skipped() {
        let md = parse(
            b"<!DOCTYPE html><!-- <meta name=\"x\" content=\"y\"> --><title>Z</title>",
            None,
        );
        assert!(md.meta.is_empty());
        assert_eq!(md.title.as_deref(), Some("Z"));
    }

    #[test]
    fn favicon_prefers_svg() {
        let md = parse(
            b"<link rel=\"shortcut icon\" href=\"/favicon.ico\" sizes=\"16x16\">\
              <link rel=\"icon\" type=\"image/svg+xml\" href=\"/icon.svg\">\
              <link rel=\"apple-touch-icon\" sizes=\"180x180\" href=\"/apple.png\">",
            None,
        );
        assert_eq!(md.favicon.as_deref(), Some("/icon.svg"));
    }

    #[test]
    fn favicon_accepts_relative_href_and_ignores_other_links() {
        let md = parse(
            b"<link rel=\"stylesheet\" href=\"/style.css\">\
              <link rel=icon href=/favicon.ico>",
            None,
        );
        assert_eq!(md.favicon.as_deref(), Some("/favicon.ico"));
    }

    #[test]
    fn unquoted_and_single_quoted_attrs() {
        let md = parse(b"<meta charset=windows-1252 property='og:x' content=y>", None);
        assert_eq!(md.charset.as_deref(), Some("windows-1252"));
        assert_eq!(md.og.get("x").map(String::as_str), Some("y"));
    }
}