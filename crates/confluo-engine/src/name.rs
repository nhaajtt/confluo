//! Turning what a server says into a file name that is safe to create.

use std::path::{Path, PathBuf};

use percent_encoding::percent_decode_str;
use url::Url;

const MAX_NAME_BYTES: usize = 200;
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
    "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// File name from a `Content-Disposition` header value.
pub fn from_content_disposition(value: &str) -> Option<String> {
    let mut plain = None;
    for part in value.split(';').skip(1) {
        let Some((key, val)) = part.split_once('=') else { continue };
        let key = key.trim().to_ascii_lowercase();
        let val = val.trim();
        if key == "filename*" {
            // RFC 5987: charset'lang'percent-encoded
            let mut it = val.splitn(3, '\'');
            if let (Some(charset), Some(_), Some(enc)) = (it.next(), it.next(), it.next())
                && charset.eq_ignore_ascii_case("utf-8")
                && let Ok(s) = percent_decode_str(enc).decode_utf8()
            {
                return Some(s.into_owned());
            }
        } else if key == "filename" {
            plain = Some(val.trim_matches('"').replace("\\\"", "\""));
        }
    }
    plain.filter(|s| !s.is_empty())
}

/// File name from the last path segment of a URL.
pub fn from_url(url: &Url) -> Option<String> {
    let seg = url.path_segments()?.rev().find(|s| !s.is_empty())?;
    let s = percent_decode_str(seg).decode_utf8_lossy().into_owned();
    (!s.is_empty()).then_some(s)
}

/// Makes a name safe on Windows, macOS and Linux. It can never contain a path
/// separator, so a hostile server cannot write outside the download folder.
pub fn sanitize(raw: &str) -> String {
    let mut s: String = raw
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        // Invisible characters that reorder or hide text: "exe.pdf" must not be
        // able to pass for a file that really ends in ".exe".
        .filter(|c| !matches!(*c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}'))
        .collect();
    s = s.trim().trim_end_matches(['.', ' ']).trim_start_matches('.').trim().to_string();
    if s.is_empty() {
        return "download".to_string();
    }
    let stem = s.split('.').next().unwrap_or("").trim_end_matches([' ', '.']).to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        s.insert(0, '_');
    }
    if s.len() > MAX_NAME_BYTES {
        let ext = Path::new(&s)
            .extension()
            .and_then(|e| e.to_str())
            .filter(|e| e.len() <= 16)
            .map(|e| format!(".{e}"))
            .unwrap_or_default();
        let mut cut = MAX_NAME_BYTES - ext.len();
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.truncate(s.trim_end_matches(['.', ' ']).len());
        s.push_str(&ext);
    }
    s
}

/// `name (n).ext` for the first `n` where `taken` says the path is free.
pub fn unique(dir: &Path, name: &str, taken: impl Fn(&Path) -> bool) -> String {
    if !taken(&dir.join(name)) {
        return name.to_string();
    }
    let p = Path::new(name);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    let ext = p.extension().and_then(|s| s.to_str()).map(|e| format!(".{e}")).unwrap_or_default();
    for n in 1..10_000 {
        let candidate = format!("{stem} ({n}){ext}");
        if !taken(&dir.join(&candidate)) {
            return candidate;
        }
    }
    format!("{stem} ({}){ext}", std::process::id())
}

pub fn part_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.confluo"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_and_reserved_names_are_defused() {
        // A right-to-left override would show "invoice\u{202e}fdp.exe" as "invoiceexe.pdf".
        assert_eq!(sanitize("invoice\u{202e}fdp.exe"), "invoicefdp.exe");
        assert_eq!(sanitize("zero\u{200b}width.txt"), "zerowidth.txt");
        assert_eq!(sanitize("CON .txt"), "_CON .txt");
        assert_eq!(sanitize("nul"), "_nul");
        let long = format!("{}. .txt", "a".repeat(300));
        let cut = sanitize(&long);
        assert!(cut.len() <= 200 && cut.ends_with(".txt") && !cut.contains(" .txt"), "{cut}");
    }

    #[test]
    fn disposition() {
        assert_eq!(from_content_disposition("attachment; filename=\"a b.zip\"").as_deref(), Some("a b.zip"));
        assert_eq!(
            from_content_disposition("attachment; filename=x.bin; filename*=UTF-8''%E2%82%AC%20rates.pdf")
                .as_deref(),
            Some("€ rates.pdf")
        );
        assert_eq!(from_content_disposition("inline"), None);
    }

    #[test]
    fn hostile_names_stay_inside_the_folder() {
        for raw in ["../../etc/passwd", "..\\..\\Windows\\win.ini", "C:\\x\\y.exe", "/abs/olute", "a/../../b"]
        {
            let s = sanitize(raw);
            assert!(!s.contains(['/', '\\', ':']), "{s}");
            assert!(!s.starts_with('.'), "{s}");
        }
        assert_eq!(sanitize("CON.txt"), "_CON.txt");
        assert_eq!(sanitize("  .. "), "download");
        assert_eq!(sanitize("report.pdf. . "), "report.pdf");
        let long = format!("{}.tar", "é".repeat(300));
        let s = sanitize(&long);
        assert!(s.len() <= MAX_NAME_BYTES && s.ends_with(".tar"));
    }

    #[test]
    fn unique_names() {
        let dir = Path::new("d");
        let taken = |p: &Path| matches!(p.file_name().and_then(|s| s.to_str()), Some("a.zip" | "a (1).zip"));
        assert_eq!(unique(dir, "a.zip", taken), "a (2).zip");
        assert_eq!(unique(dir, "b.zip", taken), "b.zip");
    }

    #[test]
    fn url_names() {
        let u = Url::parse("https://h/x/My%20File.iso?sig=1").unwrap();
        assert_eq!(from_url(&u).as_deref(), Some("My File.iso"));
        assert_eq!(from_url(&Url::parse("https://h/").unwrap()), None);
    }
}
