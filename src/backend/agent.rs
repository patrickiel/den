//! Agent notifications, as in den: Claude Code gets a per-terminal hooks file
//! (`--settings`) whose hooks curl a localhost listener here when a turn is
//! done or it waits for input. Codex is told to send OSC 9 notifications,
//! which the terminal reads itself.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::OnceLock,
};

use futures::channel::mpsc;

/// What a CLI reported for the terminal `pane`.
#[derive(Clone, Debug)]
pub struct AgentEvent {
    pub pane: u64,
    /// `start`, `prompt`, `done` or `input`.
    pub kind: String,
    /// The hook's JSON.
    pub body: String,
}

/// The listener's port, once bound.
static PORT: OnceLock<u16> = OnceLock::new();

const MAX_BODY: usize = 64 * 1024;

/// Bind the listener and serve it on a thread; the events come out of the
/// returned channel. `None` when it could not bind (no hooks then).
pub fn start() -> Option<mpsc::UnboundedReceiver<AgentEvent>> {
    let listener = TcpListener::bind("127.0.0.1:0").ok()?;
    let port = listener.local_addr().ok()?.port();
    PORT.set(port).ok()?;
    let (tx, rx) = mpsc::unbounded();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            if let Some(event) = read_request(stream)
                && tx.unbounded_send(event).is_err()
            {
                return;
            }
        }
    });
    Some(rx)
}

/// `POST /<agent>/<pane>/<kind>` with the CLI's JSON as the body; always
/// answered with 204.
fn read_request(mut stream: TcpStream) -> Option<AgentEvent> {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut len = 0;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            len = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; len.min(MAX_BODY)];
    reader.read_exact(&mut body).ok()?;
    let _ = stream.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n");
    parse_path(line.split_whitespace().nth(1)?, String::from_utf8_lossy(&body).into())
}

fn parse_path(path: &str, body: String) -> Option<AgentEvent> {
    let mut parts = path.trim_start_matches('/').split('/');
    let (_agent, pane, kind) = (parts.next()?, parts.next()?.parse().ok()?, parts.next()?);
    Some(AgentEvent { pane, kind: kind.into(), body })
}

fn hooks_dir() -> PathBuf {
    crate::settings::data_dir().join("hooks")
}

/// Write terminal `pane`'s Claude Code hooks file; its path with forward
/// slashes (shell-safe), or `None` while the listener is down.
pub fn claude_hooks(pane: u64) -> Option<String> {
    let port = *PORT.get()?;
    // `curl.exe`, not `curl`: in PowerShell that is an alias of Invoke-WebRequest.
    let curl = if cfg!(windows) { "curl.exe" } else { "curl" };
    let hook = |kind: &str| {
        let url = format!("http://127.0.0.1:{port}/claude/{pane}/{kind}");
        serde_json::json!([{ "type": "command", "command": format!("{curl} -s -m 2 -d @- {url}") }])
    };
    let settings = serde_json::json!({
        "hooks": {
            // These two only name the conversation, so a restart can resume it.
            "SessionStart": [{ "hooks": hook("start") }],
            "UserPromptSubmit": [{ "hooks": hook("prompt") }],
            "Stop": [{ "hooks": hook("done") }],
            "Notification": [{ "matcher": "permission_prompt|elicitation_dialog", "hooks": hook("input") }],
        }
    });
    let dir = hooks_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let file = dir.join(format!("{pane}.json"));
    std::fs::write(&file, settings.to_string()).ok()?;
    Some(file.to_string_lossy().replace('\\', "/"))
}

/// The start command with what makes a known CLI report back to `pane`;
/// anything else unchanged. With `wrapped`, a bare `claude` is left to the
/// shell's `claude` wrapper, which adds the hooks to every run, typed or not.
pub fn inject(pane: u64, command: &str, wrapped: bool) -> String {
    let command = command.trim();
    let (head, rest) = command.split_once(char::is_whitespace).unwrap_or((command, ""));
    let extra = match program_of(head).as_str() {
        "claude" if !(wrapped && head == "claude") => match claude_hooks(pane) {
            Some(file) => format!("--settings \"{file}\""),
            None => return command.into(),
        },
        // Its "auto" method picks a bell for a terminal it does not know; OSC 9 carries the text.
        "codex" => "-c tui.notifications=true -c tui.notification_method=osc9".into(),
        _ => return command.into(),
    };
    format!("{head} {extra} {rest}").trim_end().into()
}

/// `claude` for `C:\…\claude.exe`.
pub fn program_of(head: &str) -> String {
    let name = head.rsplit(['\\', '/']).next().unwrap_or(head).to_lowercase();
    let name = name.trim_end_matches(".exe").trim_end_matches(".cmd").trim_end_matches(".ps1");
    name.to_string()
}

/// The PowerShell `claude` wrapper: every `claude` in the terminal, typed or
/// run by a preset, reports to `pane`.
pub fn powershell_wrapper(pane: u64) -> Option<String> {
    let file = claude_hooks(pane)?;
    Some(format!(
        "function global:claude {{ $c = Get-Command claude -CommandType Application,ExternalScript -ErrorAction SilentlyContinue | Select-Object -First 1; if ($c) {{ & $c --settings '{file}' @args }} }}\n"
    ))
}

/// Drop terminal `pane`'s hooks file once it is gone.
pub fn forget(pane: u64) {
    let _ = std::fs::remove_file(hooks_dir().join(format!("{pane}.json")));
}

/// A string field of a hook's JSON.
pub fn hook_field(body: &str, field: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body).ok()?[field].as_str().map(str::to_string)
}

/// The command that brings a Claude Code conversation back, as den restarts
/// it: `--resume <id>` with the conversation known, plain `claude` for a new
/// or cleared one (nothing to resume yet), `--continue` when unknown. The
/// preset's own flags stay.
pub fn claude_resume(program: Option<&str>, session: Option<&str>, fresh: bool) -> String {
    let base: Vec<&str> = program
        .filter(|p| program_of(p.split_whitespace().next().unwrap_or("")) == "claude")
        .unwrap_or("claude")
        .split_whitespace()
        .collect();
    // Drop an earlier resume: "--continue", "-c", "--resume <id>", "-r <id>".
    let mut words = Vec::new();
    let mut skip = false;
    for word in base {
        if skip {
            skip = false;
            continue;
        }
        match word {
            "--continue" | "-c" => {}
            "--resume" | "-r" => skip = true,
            other => words.push(other),
        }
    }
    let base = words.join(" ");
    match session {
        _ if fresh => base,
        Some(id) if !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') => format!("{base} --resume {id}"),
        _ => format!("{base} --continue"),
    }
}

/// What to show for a hook: the message a Notification hook carries, or the
/// last thing the agent said for a finished turn.
pub fn message(body: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    ["message", "last_assistant_message"]
        .iter()
        .find_map(|key| json[*key].as_str())
        .map(|text| text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().chars().take(200).collect())
        .filter(|text: &String| !text.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{inject, message, parse_path, program_of};

    #[test]
    fn parses_the_request_path() {
        let event = parse_path("/claude/42/done", "{}".into()).unwrap();
        assert_eq!((event.pane, event.kind.as_str()), (42, "done"));
        assert!(parse_path("/claude/x/done", String::new()).is_none());
    }

    #[test]
    fn names_programs() {
        assert_eq!(program_of(r"C:\bin\Claude.EXE"), "claude");
        assert_eq!(program_of("codex.cmd"), "codex");
    }

    #[test]
    fn leaves_others_and_wrapped_claude_alone() {
        assert_eq!(inject(1, "pnpm dev", false), "pnpm dev");
        assert_eq!(inject(1, "claude --continue", true), "claude --continue");
        assert_eq!(inject(1, "codex", false), "codex -c tui.notifications=true -c tui.notification_method=osc9");
    }

    #[test]
    fn resume_commands() {
        use super::claude_resume;
        assert_eq!(claude_resume(Some("claude"), Some("abc-123"), false), "claude --resume abc-123");
        assert_eq!(claude_resume(Some("claude --model opus --continue"), Some("x1"), false), "claude --model opus --resume x1");
        assert_eq!(claude_resume(None, None, true), "claude");
        assert_eq!(claude_resume(Some("claude --resume old"), None, false), "claude --continue");
        assert_eq!(claude_resume(Some("claude"), Some("bad id;rm"), false), "claude --continue");
    }

    #[test]
    fn takes_the_message() {
        assert_eq!(message(r#"{"message":"Claude needs your permission"}"#).as_deref(), Some("Claude needs your permission"));
        assert_eq!(message("{}"), None);
    }
}
