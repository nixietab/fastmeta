use std::env;
use std::io::{self, Write};
use std::process::ExitCode;
use std::sync::Arc;

use tokio::task::JoinSet;

const USAGE: &str = "\
fastmeta - fetch HTML pages and extract their metadata as JSON

Usage:
  fastmeta [OPTIONS] <url>...

Arguments:
  <url>...  One or more URLs. A bare host is treated as https://<host>.

Options:
  -p, --pretty   Pretty-print the JSON output
  -h, --help     Print this help
  -V, --version  Print the version

Output:
  One JSON object per input URL, in argument order. Errors go to stderr.
  The process exits non-zero if any URL could not be fetched.
";

fn normalize_url(input: &str) -> String {
    if input.contains("://") {
        input.to_string()
    } else {
        format!("https://{input}")
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let mut pretty = false;
    let mut urls: Vec<String> = Vec::new();
    for a in env::args().skip(1) {
        match a.as_str() {
            "--pretty" | "-p" => pretty = true,
            "--help" | "-h" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--version" | "-V" => {
                println!("fastmeta {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            _ => urls.push(normalize_url(&a)),
        }
    }

    if urls.is_empty() {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    }

    let fetcher = match fastmeta::Fetcher::with_defaults() {
        Ok(f) => Arc::new(f),
        Err(e) => {
            eprintln!("init error: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Fetch all URLs concurrently; print in argument order.
    let mut set: JoinSet<(usize, String, Result<fastmeta::Metadata, anyhow::Error>)> =
        JoinSet::new();
    for (i, url) in urls.iter().enumerate() {
        let f = Arc::clone(&fetcher);
        let url = url.clone();
        set.spawn(async move {
            let res = f.fetch(&url).await;
            (i, url, res)
        });
    }

    let mut results: Vec<Option<(String, Result<fastmeta::Metadata, anyhow::Error>)>> =
        Vec::with_capacity(urls.len());
    results.resize_with(urls.len(), || None);

    let mut any_err = false;
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((i, url, res)) => results[i] = Some((url, res)),
            Err(e) => {
                eprintln!("task error: {e}");
                any_err = true;
            }
        }
    }

    let stdout = io::stdout();
    let mut out = stdout.lock();

    for slot in results.into_iter().flatten() {
        let (url, res) = slot;
        match res {
            Ok(md) => {
                let s = if pretty {
                    serde_json::to_string_pretty(&md)
                } else {
                    serde_json::to_string(&md)
                };
                match s {
                    Ok(s) => {
                        if let Err(e) = writeln!(out, "{s}") {
                            if e.kind() == io::ErrorKind::BrokenPipe {
                                return ExitCode::SUCCESS;
                            }
                            eprintln!("write error: {e}");
                            any_err = true;
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("serialize error: {e}");
                        any_err = true;
                    }
                }
            }
            Err(e) => {
                eprintln!("error fetching {url}: {e}");
                any_err = true;
            }
        }
    }

    if any_err {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_url;

    #[test]
    fn adds_https_to_bare_hosts() {
        assert_eq!(normalize_url("example.com"), "https://example.com");
        assert_eq!(
            normalize_url("example.com/path?q=1"),
            "https://example.com/path?q=1"
        );
    }

    #[test]
    fn keeps_explicit_schemes() {
        assert_eq!(normalize_url("http://example.com"), "http://example.com");
        assert_eq!(
            normalize_url("https://example.com"),
            "https://example.com"
        );
    }
}
