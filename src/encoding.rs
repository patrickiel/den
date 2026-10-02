//! Text encodings a file tab reads and saves in, as den has them (named as
//! in VS Code): the four Unicode ones by name, the rest by WHATWG label,
//! decoded and encoded by encoding_rs. Also the indentation a file uses.

use std::path::Path;

/// Id (as stored), name in the menu, name in the status bar.
pub const ENCODINGS: &[(&str, &str, &str)] = &[
    ("utf8", "UTF-8", "UTF-8"),
    ("utf8bom", "UTF-8 with BOM", "UTF-8 with BOM"),
    ("utf16le", "UTF-16 LE", "UTF-16 LE"),
    ("utf16be", "UTF-16 BE", "UTF-16 BE"),
    ("windows-1252", "Western (Windows 1252)", "Windows 1252"),
    ("iso-8859-3", "Western (ISO 8859-3)", "ISO 8859-3"),
    ("iso-8859-15", "Western (ISO 8859-15)", "ISO 8859-15"),
    ("macintosh", "Western (Mac Roman)", "Mac Roman"),
    ("windows-1256", "Arabic (Windows 1256)", "Windows 1256"),
    ("iso-8859-6", "Arabic (ISO 8859-6)", "ISO 8859-6"),
    ("windows-1257", "Baltic (Windows 1257)", "Windows 1257"),
    ("iso-8859-4", "Baltic (ISO 8859-4)", "ISO 8859-4"),
    ("iso-8859-14", "Celtic (ISO 8859-14)", "ISO 8859-14"),
    ("windows-1250", "Central European (Windows 1250)", "Windows 1250"),
    ("iso-8859-2", "Central European (ISO 8859-2)", "ISO 8859-2"),
    ("windows-1251", "Cyrillic (Windows 1251)", "Windows 1251"),
    ("ibm866", "Cyrillic (CP 866)", "CP 866"),
    ("iso-8859-5", "Cyrillic (ISO 8859-5)", "ISO 8859-5"),
    ("koi8-r", "Cyrillic (KOI8-R)", "KOI8-R"),
    ("koi8-u", "Cyrillic (KOI8-U)", "KOI8-U"),
    ("iso-8859-13", "Estonian (ISO 8859-13)", "ISO 8859-13"),
    ("windows-1253", "Greek (Windows 1253)", "Windows 1253"),
    ("iso-8859-7", "Greek (ISO 8859-7)", "ISO 8859-7"),
    ("windows-1255", "Hebrew (Windows 1255)", "Windows 1255"),
    ("iso-8859-8", "Hebrew (ISO 8859-8)", "ISO 8859-8"),
    ("iso-8859-10", "Nordic (ISO 8859-10)", "ISO 8859-10"),
    ("iso-8859-16", "Romanian (ISO 8859-16)", "ISO 8859-16"),
    ("windows-1254", "Turkish (Windows 1254)", "Windows 1254"),
    ("windows-1258", "Vietnamese (Windows 1258)", "Windows 1258"),
    ("windows-874", "Thai (Windows 874)", "Windows 874"),
    ("gbk", "Simplified Chinese (GBK)", "GBK"),
    ("gb18030", "Simplified Chinese (GB 18030)", "GB 18030"),
    ("big5", "Traditional Chinese (Big5)", "Big5"),
    ("shift_jis", "Japanese (Shift JIS)", "Shift JIS"),
    ("euc-jp", "Japanese (EUC-JP)", "EUC-JP"),
    ("euc-kr", "Korean (EUC-KR)", "EUC-KR"),
];

/// The status bar's name for `id`.
pub fn short_name(id: &str) -> &str {
    ENCODINGS.iter().find(|(e, ..)| *e == id).map_or(id, |(_, _, short)| short)
}

/// A file read for a tab is limited to this many bytes.
const MAX_BYTES: u64 = 50 * 1024 * 1024;

fn lookup(label: &str) -> Result<&'static encoding_rs::Encoding, String> {
    encoding_rs::Encoding::for_label(label.as_bytes()).ok_or_else(|| format!("unknown encoding {label}"))
}

/// Read a file: in `encoding`, else by its BOM (UTF-8, UTF-16 LE/BE), else
/// UTF-8, else Windows-1252 (which decodes any bytes and so saves them back
/// unchanged). The text and the encoding it was read in.
pub fn read(path: &Path, encoding: Option<&str>) -> Result<(String, String), String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_BYTES {
        return Err(format!("The file is larger than {} MB.", MAX_BYTES / 1024 / 1024));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let encoding = match encoding {
        Some(e) => e.to_string(),
        None => match bytes.as_slice() {
            [0xEF, 0xBB, 0xBF, ..] => "utf8bom".into(),
            [0xFF, 0xFE, ..] => "utf16le".into(),
            [0xFE, 0xFF, ..] => "utf16be".into(),
            _ if bytes.iter().take(8000).any(|&b| b == 0) => return Err("This is a binary file.".into()),
            _ if std::str::from_utf8(&bytes).is_ok() => "utf8".into(),
            _ => "windows-1252".into(),
        },
    };
    let text = match encoding.as_str() {
        "utf8" | "utf8bom" => encoding_rs::UTF_8.decode_with_bom_removal(&bytes).0,
        "utf16le" => encoding_rs::UTF_16LE.decode_with_bom_removal(&bytes).0,
        "utf16be" => encoding_rs::UTF_16BE.decode_with_bom_removal(&bytes).0,
        label => lookup(label)?.decode_without_bom_handling(&bytes).0,
    };
    Ok((text.into_owned(), encoding))
}

/// Write `text` in `encoding`; UTF-16 gets a BOM, as in VS Code. Fails,
/// writing nothing, when the encoding cannot represent the text.
pub fn write(path: &Path, text: &str, encoding: &str) -> Result<(), String> {
    let bytes: Vec<u8> = match encoding {
        "utf8" => text.as_bytes().to_vec(),
        "utf8bom" => [&[0xEF, 0xBB, 0xBF][..], text.as_bytes()].concat(),
        "utf16le" => [0xFEFFu16].into_iter().chain(text.encode_utf16()).flat_map(u16::to_le_bytes).collect(),
        "utf16be" => [0xFEFFu16].into_iter().chain(text.encode_utf16()).flat_map(u16::to_be_bytes).collect(),
        label => {
            let enc = lookup(label)?;
            let (bytes, _, unmappable) = enc.encode(text);
            if unmappable {
                return Err(format!("the text has characters that {} cannot represent", enc.name()));
            }
            bytes.into_owned()
        }
    };
    std::fs::write(path, bytes).map_err(|e| e.to_string())
}

/// The indentation `text` uses: tabs, or spaces and how many per level.
/// `None` when it has no indented lines to tell.
pub fn detect_indent(text: &str) -> Option<(bool, usize)> {
    let (mut tabs, mut spaced) = (0usize, 0usize);
    let mut widths = [0usize; 9];
    let mut previous = 0usize;
    for line in text.lines().take(5000) {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with('\t') {
            tabs += 1;
            continue;
        }
        let indent = line.len() - line.trim_start_matches(' ').len();
        if indent > 0 {
            spaced += 1;
        }
        // The step from the line before is the indent unit.
        let step = indent.abs_diff(previous);
        if (2..=8).contains(&step) {
            widths[step] += 1;
        }
        previous = indent;
    }
    if tabs == 0 && spaced == 0 {
        return None;
    }
    if tabs > spaced {
        return Some((true, 4));
    }
    let width = (2..=8).max_by_key(|&w| (widths[w], usize::from(w == 4))).filter(|&w| widths[w] > 0).unwrap_or(4);
    Some((false, width))
}

#[cfg(test)]
mod tests {
    use super::{detect_indent, read, write};

    #[test]
    fn indentation() {
        assert_eq!(detect_indent("a {\n  b\n  c {\n    d\n  }\n}\n"), Some((false, 2)));
        assert_eq!(detect_indent("fn x() {\n    y();\n}\n"), Some((false, 4)));
        assert_eq!(detect_indent("a\n\tb\n\tc\n"), Some((true, 4)));
        assert_eq!(detect_indent("plain\ntext\n"), None);
    }

    #[test]
    fn round_trips_in_other_encodings() {
        let dir = std::env::temp_dir().join(format!("den-enc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.txt");
        write(&path, "Grüße", "windows-1252").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"Gr\xfc\xdfe");
        assert_eq!(read(&path, None).unwrap(), ("Grüße".to_string(), "windows-1252".to_string()));
        write(&path, "hé", "utf16le").unwrap();
        assert_eq!(read(&path, None).unwrap(), ("hé".to_string(), "utf16le".to_string()));
        assert!(write(&path, "日本", "windows-1252").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
