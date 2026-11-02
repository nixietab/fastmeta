# fastmeta
Fast, consistent website metadata retrieval and extraction


## Install

```sh
git clone https://github.com/nixietab/fastmeta
cd fastmeta
cargo install --path .
```

## Usage

```sh
fastmeta [OPTIONS] <url>...
```

A bare host is treated as `https://<host>`. Multiple URLs are fetched concurrently and printed in argument order, one JSON object per line.

| Option          | Description                |
| --------------- | -------------------------- |
| `-p`, `--pretty` | Pretty-print the JSON      |
| `-h`, `--help`   | Print help                 |
| `-V`, `--version`| Print the version          |

## Output

Each URL produces one JSON object. Fields that are absent from the page are omitted; `url` and `status` are always present.

| Field         | Description                                                                 |
| ------------- | --------------------------------------------------------------------------- |
| url         | Final URL after redirects                                                   |
| status      | HTTP status of the final response                                           |
| title       | First `<title>`, with whitespace collapsed and entities decoded             |
| description | Content of `<meta name="description">`                                      |
| canonical   | `href` of `<link rel="canonical">`                                          |
| lang        | `lang` attribute of `<html>`                                                |
| charset     | From the `Content-Type` header, otherwise from `<meta charset>`             |
| favicon     | `href` of the best icon link, as written in the page (may be relative)      |
| meta        | Other `<meta name>` tags, keyed by lowercase name                           |
| og          | Open Graph tags, keyed without the `og:` prefix                             |
| twitter     | Twitter card tags, keyed without the `twitter:` prefix                      |

### Example

```sh
fastmeta --pretty subte.cc
```

```json
{
  "url": "https://subte.cc",
  "status": 200,
  "title": "subte.cc - Dynamic DNS for everyone",
  "description": "subte.cc - Dynamic DNS for everyone. Free as in freedom and beer.",
  "lang": "en",
  "charset": "utf-8",
  "favicon": "./favicon.svg",
  "meta": {
    "viewport": "width=device-width, initial-scale=1.0",
    "author": "subte.cc",
    "keywords": "subte.cc, dynamic dns, ddns, free dns, open source, privacy",
    "description": "subte.cc - Dynamic DNS for everyone. Free as in freedom and beer."
  },
  "og": {
    "type": "website",
    "title": "subte.cc",
    "image": "https://subte.cc/assets/img/opengraph.png",
    "image:width": "1200",
    "image:height": "630",
    "image:type": "image/png",
    "description": "Dynamic DNS for everyone. Free as in freedom and beer."
  },
  "twitter": {
    "card": "summary_large_image"
  }
}
```

Errors are written to stderr. The exit code is non-zero if any URL could not be fetched.

## Library

```rust
let fetcher = fastmeta::Fetcher::with_defaults()?;
let metadata = fetcher.fetch("https://example.com").await?;
println!("{:?}", metadata.title);
```

## License

GNU General Public License v2.0.
