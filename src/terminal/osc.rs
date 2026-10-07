//! The shell telling den where it is: an OSC 7 sequence on every prompt,
//! `ESC ] 7 ; file://host/path BEL`, as den has its shells send.
//!
//! For PowerShell the hook wraps the `prompt` function after the profile has
//! loaded (oh-my-posh and starship keep working); for bash it is
//! `PROMPT_COMMAND`. Other shells send nothing and keep their start folder.

use std::path::PathBuf;

/// Finds the numbered OSC sequences (`ESC ] <number> ; <body> BEL`, or
/// `ESC \` for BEL) in shell output, across reads that split them.
#[derive(Default)]
pub struct OscScanner {
    /// The tail of the last read when it ended inside a sequence.
    carry: Vec<u8>,
}

impl OscScanner {
    /// The complete sequences in `bytes`, as `(number, body)`.
    pub fn scan(&mut self, bytes: &[u8]) -> Vec<(u32, String)> {
        let joined;
        let data: &[u8] = if self.carry.is_empty() {
            bytes
        } else {
            joined = [self.carry.as_slice(), bytes].concat();
            &joined
        };
        self.carry.clear();
        let mut found = Vec::new();
        let mut at = 0;
        while let Some(start) = find(&data[at..], b"\x1b]").map(|i| i + at) {
            let head = start + 2;
            let digits = data[head..].iter().take_while(|b| b.is_ascii_digit()).count();
            match data.get(head + digits) {
                // The read ends inside the number: the rest comes later.
                None => return self.keep(data, start, found),
                Some(b';') if digits > 0 => {}
                // Not a numbered sequence (a title set by name, say).
                _ => {
                    at = head;
                    continue;
                }
            }
            let body = head + digits + 1;
            let Some(end) = data[body..].iter().position(|&b| b == 0x07 || b == 0x1b).map(|i| i + body) else {
                return self.keep(data, start, found);
            };
            let number = std::str::from_utf8(&data[head..head + digits]).ok().and_then(|n| n.parse().ok()).unwrap_or(u32::MAX);
            found.push((number, String::from_utf8_lossy(&data[body..end]).into_owned()));
            at = end + 1;
        }
        // A read can end on the first byte of the next sequence.
        if data.last() == Some(&0x1b) {
            self.carry.push(0x1b);
        }
        found
    }

    /// Keep the data from `start` on for the next read, unless it is too long
    /// to be a sequence; what was found before it.
    fn keep(&mut self, data: &[u8], start: usize, found: Vec<(u32, String)>) -> Vec<(u32, String)> {
        if data.len() - start < 4096 {
            self.carry = data[start..].to_vec();
        }
        found
    }
}

/// OSC 7: the folder the shell is in.
pub fn cwd_of(number: u32, body: &str) -> Option<PathBuf> {
    (number == 7).then(|| parse_uri(body)).flatten()
}

/// A notification a program sent: OSC 9 (iTerm2 style, as Codex sends) and
/// OSC 777 `notify;title;body`. ConEmu's numbered OSC 9 commands are not
/// messages.
pub fn notification_of(number: u32, body: &str) -> Option<String> {
    match number {
        9 => {
            let numbered = body.split(';').next().is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
            (!numbered).then(|| body.to_string())
        }
        777 => {
            let mut parts = body.splitn(3, ';');
            (parts.next() == Some("notify")).then(|| {
                let (title, body) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                if body.is_empty() { title } else { body }.to_string()
            })
        }
        _ => None,
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
    use super::{OscScanner, base64, cwd_of, encode_powershell, notification_of, parse_uri};
    use std::path::PathBuf;

    fn last_cwd(scanner: &mut OscScanner, bytes: &[u8]) -> Option<PathBuf> {
        scanner.scan(bytes).into_iter().filter_map(|(number, body)| cwd_of(number, &body)).next_back()
    }

    #[test]
    fn windows_and_unix_uris() {
        assert_eq!(parse_uri("file://PC/C:/Users/me%20too"), Some(PathBuf::from(r"C:\Users\me too")));
        assert_eq!(parse_uri("file://host/home/me"), Some(PathBuf::from("/home/me")));
        assert_eq!(parse_uri("http://x/y"), None);
    }

    #[test]
    fn finds_the_last_report_in_a_read() {
        let mut scanner = OscScanner::default();
        let out = b"text\x1b]7;file://PC/C:/a\x07more\x1b]7;file://PC/C:/b\x1b\\prompt> ";
        assert_eq!(last_cwd(&mut scanner, out), Some(PathBuf::from(r"C:\b")));
    }

    #[test]
    fn a_report_split_across_reads() {
        let mut scanner = OscScanner::default();
        assert_eq!(last_cwd(&mut scanner, b"output\x1b]7;file://PC/C:/pro"), None);
        assert_eq!(last_cwd(&mut scanner, b"jects\x07PS> "), Some(PathBuf::from(r"C:\projects")));
        assert_eq!(last_cwd(&mut scanner, b"x\x1b]"), None);
        assert_eq!(last_cwd(&mut scanner, b"7;file://PC/D:/\x07"), Some(PathBuf::from(r"D:\")));
        assert_eq!(last_cwd(&mut scanner, b"y\x1b"), None);
        assert_eq!(last_cwd(&mut scanner, b"]7;file://PC/E:/\x07"), Some(PathBuf::from(r"E:\")));
    }

    #[test]
    fn notifications_but_not_conemu_commands_or_titles() {
        let mut scanner = OscScanner::default();
        let out = b"\x1b]9;4;1;50\x07\x1b]0;a title\x07\x1b]9;Agent turn complete\x07\x1b]777;notify;Title;Body\x07\x1b]P1\x07";
        let found: Vec<String> = scanner.scan(out).into_iter().filter_map(|(number, body)| notification_of(number, &body)).collect();
        assert_eq!(found, vec!["Agent turn complete".to_string(), "Body".to_string()]);
    }

    #[test]
    fn encodes_like_powershell_expects() {
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        // "ls" as UTF-16LE.
        assert_eq!(encode_powershell("ls"), "bABzAA==");
    }
}
