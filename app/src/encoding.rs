#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Eol {
    Crlf,
    Lf,
    Cr,
}

impl Eol {
    pub const DEFAULT: Eol = if cfg!(windows) { Eol::Crlf } else { Eol::Lf };

    fn as_str(self) -> &'static str {
        match self {
            Eol::Crlf => "\r\n",
            Eol::Lf => "\n",
            Eol::Cr => "\r",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Eol::Crlf => "Windows (CRLF)",
            Eol::Lf => "Unix (LF)",
            Eol::Cr => "Macintosh (CR)",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    /// Not valid UTF-8: read and written back byte-for-byte as Latin-1, so an
    /// unedited legacy file survives a save unchanged.
    Ansi,
}

impl Encoding {
    pub fn label(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Utf8Bom => "UTF-8 with BOM",
            Encoding::Utf16Le => "UTF-16 LE",
            Encoding::Utf16Be => "UTF-16 BE",
            Encoding::Ansi => "ANSI",
        }
    }
}

/// Returns the text with `\n` line endings, plus what to restore on save.
pub fn decode(bytes: &[u8]) -> (String, Eol, Encoding) {
    let utf16 = |rest: &[u8], unit: fn([u8; 2]) -> u16| {
        let units: Vec<u16> = rest.chunks_exact(2).map(|c| unit([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    };
    let (raw, encoding) = if let Some(rest) = bytes.strip_prefix(b"\xEF\xBB\xBF") {
        (
            String::from_utf8_lossy(rest).into_owned(),
            Encoding::Utf8Bom,
        )
    } else if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        (utf16(rest, u16::from_le_bytes), Encoding::Utf16Le)
    } else if let Some(rest) = bytes.strip_prefix(b"\xFE\xFF") {
        (utf16(rest, u16::from_be_bytes), Encoding::Utf16Be)
    } else {
        match std::str::from_utf8(bytes) {
            Ok(s) => (s.to_owned(), Encoding::Utf8),
            Err(_) => (bytes.iter().map(|&b| b as char).collect(), Encoding::Ansi),
        }
    };
    let eol = match raw.find(['\r', '\n']) {
        Some(i) if raw[i..].starts_with("\r\n") => Eol::Crlf,
        Some(i) if raw.as_bytes()[i] == b'\r' => Eol::Cr,
        Some(_) => Eol::Lf,
        None => Eol::DEFAULT,
    };
    (raw.replace("\r\n", "\n").replace('\r', "\n"), eol, encoding)
}

pub fn encode(text: &str, eol: Eol, encoding: Encoding) -> Vec<u8> {
    let text = text.replace('\n', eol.as_str());
    match encoding {
        Encoding::Utf8 => text.into_bytes(),
        Encoding::Utf8Bom => [b"\xEF\xBB\xBF".as_slice(), text.as_bytes()].concat(),
        Encoding::Utf16Le => [0xFF, 0xFE]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
            .collect(),
        Encoding::Utf16Be => [0xFE, 0xFF]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_be_bytes))
            .collect(),
        // ponytail: Latin-1 stands in for the system code page; chars past U+00FF become '?'.
        Encoding::Ansi => text
            .chars()
            .map(|c| u8::try_from(c).unwrap_or(b'?'))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::{Encoding, Eol, decode, encode};

    #[test]
    fn round_trips_encodings_and_line_endings() {
        for (bytes, eol, encoding) in [
            (b"a\r\nb\r\n".to_vec(), Eol::Crlf, Encoding::Utf8),
            (b"a\nb".to_vec(), Eol::Lf, Encoding::Utf8),
            (b"a\rb".to_vec(), Eol::Cr, Encoding::Utf8),
            (
                b"\xEF\xBB\xBFh\xC3\xA9\n".to_vec(),
                Eol::Lf,
                Encoding::Utf8Bom,
            ),
            (
                b"\xFF\xFEh\0i\0\r\0\n\0".to_vec(),
                Eol::Crlf,
                Encoding::Utf16Le,
            ),
            (b"\xFE\xFF\0h\0i".to_vec(), Eol::DEFAULT, Encoding::Utf16Be),
            (b"caf\xE9\r\n".to_vec(), Eol::Crlf, Encoding::Ansi),
        ] {
            let (text, got_eol, got_encoding) = decode(&bytes);
            assert!(!text.contains('\r'));
            assert_eq!((got_eol, got_encoding), (eol, encoding));
            assert_eq!(encode(&text, got_eol, got_encoding), bytes);
        }
    }
}
