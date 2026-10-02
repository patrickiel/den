//! The shell telling den where it is: an OSC 7 sequence on every prompt,
//! `ESC ] 7 ; file://host/path BEL`, as den has its shells send.
//!
//! For PowerShell the hook wraps the `prompt` function after the profile has
//! loaded (oh-my-posh and starship keep working); for bash it is
//! `PROMPT_COMMAND`. Other shells send nothing and keep their start folder.

use std::path::PathBuf;

/// Finds the OSC sequences that start with `start` in shell output, across
/// reads that split them.
pub struct OscScanner {
    start: &'static [u8],
    /// The tail of the last read when it ended inside a sequence.
    carry: Vec<u8>,
}

impl OscScanner {
    pub fn new(start: &'static [u8]) -> Self {
        Self { start, carry: Vec::new() }
    }

    /// The bodies of the complete sequences in `bytes`.
    pub fn scan(&mut self, bytes: &[u8]) -> Vec<String> {
        let start_seq = self.start;
        let mut data = std::mem::take(&mut self.carry);
        data.extend_from_slice(bytes);
        let mut found = Vec::new();
        let mut at = 0;
        while let Some(start) = find(&data[at..], start_seq).map(|i| i + at) {
            let body = start + start_seq.len();
            let end = data[body..].iter().position(|&b| b == 0x07 || b == 0x1b).map(|i| i + body);
            match end {
                Some(end) => {
                    found.push(String::from_utf8_lossy(&data[body..end]).into_owned());
                    at = end + 1;
                }
                None => {
                    // The sequence goes on in the next read; keep it, unless
                    // it is too long to be one.
                    if data.len() - start < 4096 {
                        self.carry = data[start..].to_vec();
                    }
                    return found;
                }
            }
        }
        // A read can end on the first bytes of the next sequence.
        let tail = data.len().saturating_sub(start_seq.len() - 1);
        if let Some(i) = data[tail..].iter().position(|&b| b == 0x1b) {
            let partial = &data[tail + i..];
            if start_seq.starts_with(partial) {
                self.carry = partial.to_vec();
            }
        }
        found
    }
}

/// Finds OSC 7 reports in shell output.
pub struct CwdScanner(OscScanner);

impl Default for CwdScanner {
    fn default() -> Self {
        Self(OscScanner::new(b"\x1b]7;"))
    }
}

impl CwdScanner {
    /// The folder of the last complete report in `bytes`, if any.
    pub fn scan(&mut self, bytes: &[u8]) -> Option<PathBuf> {
        self.0.scan(bytes).iter().filter_map(|uri| parse_uri(uri)).last()
    }
}

/// Notifications a program sends: OSC 9 (iTerm2 style, as Codex sends) and
/// OSC 777 `notify;title;body`. ConEmu's numbered OSC 9 commands are not
/// messages.
pub struct NotifyScanner {
    nine: OscScanner,
    seven: OscScanner,
}

impl Default for NotifyScanner {
    fn default() -> Self {
        Self { nine: OscScanner::new(b"\x1b]9;"), seven: OscScanner::new(b"\x1b]777;") }
    }
}

impl NotifyScanner {
    pub fn scan(&mut self, bytes: &[u8]) -> Vec<String> {
        let numbered = |text: &String| text.split(';').next().is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        let mut found: Vec<String> = self.nine.scan(bytes).into_iter().filter(|text| !numbered(text)).collect();
        for text in self.seven.scan(bytes) {
            let mut parts = text.splitn(3, ';');
            if parts.next() == Some("notify") {
                let (title, body) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                found.push(if body.is_empty() { title } else { body }.to_string());
            }
        }
        found
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// `file://HOST/C:/Users/x` → `C:\Users\x`; `file:///home/x` → `/home/x`.
pub fn parse_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path = &rest[rest.find('/')?..];
    let path = percent_decode(path);
    let bytes = path.as_bytes();
    // `/C:/…`: a Windows drive.
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' {
        return Some(PathBuf::from(path[1..].replace('/', "\\")));
    }
    Some(PathBuf::from(path))
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&text[i + 1..i + 3], 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The PowerShell hook, run after the profile: wraps `prompt` to report the
/// folder first. Works in Windows PowerShell 5.1 and in pwsh.
pub const POWERSHELL_HOOK: &str = r#"
$__denPrompt = $function:prompt
function global:prompt {
    $location = $executionContext.SessionState.Path.CurrentLocation
    if ($location.Provider.Name -eq 'FileSystem') {
        $path = $location.ProviderPath -replace '\\', '/'
        [Console]::Write([char]27 + "]7;file://$env:COMPUTERNAME/$path" + [char]7)
    }
    & $__denPrompt
}
"#;

/// The bash hook, as `PROMPT_COMMAND`.
pub const BASH_HOOK: &str = r#"printf '\e]7;file://%s%s\a' "$HOSTNAME" "$PWD""#;

/// PowerShell's `-EncodedCommand`: the script as base64 of UTF-16LE, so no
/// quoting can go wrong on the way through the command line.
pub fn encode_powershell(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64(&bytes)
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{CwdScanner, NotifyScanner, base64, encode_powershell, parse_uri};
    use std::path::PathBuf;

    #[test]
    fn windows_and_unix_uris() {
        assert_eq!(parse_uri("file://PC/C:/Users/me%20too"), Some(PathBuf::from(r"C:\Users\me too")));
        assert_eq!(parse_uri("file://host/home/me"), Some(PathBuf::from("/home/me")));
        assert_eq!(parse_uri("http://x/y"), None);
    }

    #[test]
    fn finds_the_last_report_in_a_read() {
        let mut scanner = CwdScanner::default();
        let out = b"text\x1b]7;file://PC/C:/a\x07more\x1b]7;file://PC/C:/b\x1b\\prompt> ";
        assert_eq!(scanner.scan(out), Some(PathBuf::from(r"C:\b")));
    }

    #[test]
    fn a_report_split_across_reads() {
        let mut scanner = CwdScanner::default();
        assert_eq!(scanner.scan(b"output\x1b]7;file://PC/C:/pro"), None);
        assert_eq!(scanner.scan(b"jects\x07PS> "), Some(PathBuf::from(r"C:\projects")));
        assert_eq!(scanner.scan(b"x\x1b]"), None);
        assert_eq!(scanner.scan(b"7;file://PC/D:/\x07"), Some(PathBuf::from(r"D:\")));
    }

    #[test]
    fn notifications_but_not_conemu_commands() {
        let mut scanner = NotifyScanner::default();
        let out = b"\x1b]9;4;1;50\x07\x1b]9;Agent turn complete\x07\x1b]777;notify;Title;Body\x07";
        assert_eq!(scanner.scan(out), vec!["Agent turn complete".to_string(), "Body".to_string()]);
    }

    #[test]
    fn encodes_like_powershell_expects() {
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        // "ls" as UTF-16LE.
        assert_eq!(encode_powershell("ls"), "bABzAA==");
    }
}
