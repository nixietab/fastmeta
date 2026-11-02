pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'&' {
            if let Some((decoded, consumed)) = try_decode(&bytes[i..]) {
                out.push(decoded);
                i += consumed;
                continue;
            }
        }
        let c = s[i..].chars().next().unwrap();
        out.push(c);
        i += c.len_utf8();
    }
    out
}

fn try_decode(b: &[u8]) -> Option<(char, usize)> {
    // b[0] == b'&'
    let semi = b.iter().take(34).position(|&c| c == b';')?;
    let entity = std::str::from_utf8(&b[1..semi]).ok()?;

    let c = if let Some(hex) = entity.strip_prefix("#x").or_else(|| entity.strip_prefix("#X")) {
        // `from_str_radix` would accept a leading '+', so validate digits.
        if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        numeric_char(u32::from_str_radix(hex, 16).ok()?)
    } else if let Some(dec) = entity.strip_prefix('#') {
        if dec.is_empty() || !dec.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        numeric_char(dec.parse::<u32>().ok()?)
    } else {
        named_entity(entity)?
    };

    Some((c, semi + 1))
}

/// Browsers map numeric references in 0x80-0x9F through Windows-1252.
const WIN1252: [char; 32] = [
    '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž', '\u{8F}',
    '\u{90}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '•', '–', '—', '˜', '™', 'š', '›',
    'œ', '\u{9D}', 'ž', 'Ÿ',
];

fn numeric_char(n: u32) -> char {
    match n {
        0 => '\u{FFFD}',
        0x80..=0x9F => WIN1252[(n - 0x80) as usize],
        // Surrogates and out-of-range values become U+FFFD.
        _ => char::from_u32(n).unwrap_or('\u{FFFD}'),
    }
}

fn named_entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{00A0}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "hellip" => '…',
        "mdash" => '—',
        "ndash" => '–',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "ldquo" => '\u{201C}',
        "rdquo" => '\u{201D}',
        "bull" => '•',
        "middot" => '·',
        "deg" => '°',
        "plusmn" => '±',
        "times" => '×',
        "divide" => '÷',
        "frac12" => '½',
        "frac14" => '¼',
        "frac34" => '¾',
        "sup2" => '²',
        "sup3" => '³',
        "euro" => '€',
        "pound" => '£',
        "yen" => '¥',
        "cent" => '¢',
        "sect" => '§',
        "para" => '¶',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::decode_entities;

    #[test]
    fn named_and_numeric() {
        assert_eq!(decode_entities("a &amp; b &#65; &#x42;"), "a & b A B");
    }

    #[test]
    fn rejects_signs_and_empty() {
        assert_eq!(decode_entities("&#+65;"), "&#+65;");
        assert_eq!(decode_entities("&#x+41;"), "&#x+41;");
        assert_eq!(decode_entities("&#;"), "&#;");
    }

    #[test]
    fn nul_surrogate_and_win1252() {
        assert_eq!(decode_entities("&#0;"), "\u{FFFD}");
        assert_eq!(decode_entities("&#xD800;"), "\u{FFFD}");
        assert_eq!(decode_entities("&#150;"), "–");
        assert_eq!(decode_entities("&#x80;"), "€");
    }

    #[test]
    fn unknown_entity_passes_through() {
        assert_eq!(decode_entities("&bogus; x"), "&bogus; x");
    }
}