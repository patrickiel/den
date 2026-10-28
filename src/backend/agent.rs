//! Agent notifications, as in den: Claude Code gets a per-terminal hooks file
//! (`--settings`) whose hooks curl a localhost listener here when a turn is
//! done or it waits for input. Codex is told to send OSC 9 notifications,
//! which the terminal reads itself. Its `notify` callback names the session
//! for that terminal, so it can resume the same conversation.

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
    pub agent: String,
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
    parse_path(
        line.split_whitespace().nth(1)?,
        String::from_utf8_lossy(&body).into(),
    )
}

fn parse_path(path: &str, body: String) -> Option<AgentEvent> {
    let mut parts = path.trim_start_matches('/').split('/');
    let (agent, pane, kind) = (parts.next()?, parts.next()?.parse().ok()?, parts.next()?);
    Some(AgentEvent {
        agent: agent.into(),
        pane,
        kind: kind.into(),
        body,
    })
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

/// Codex appends its notification JSON as the last argument. Sending it
/// directly with curl avoids a script and keeps each tab's session separate.
pub fn codex_notify(pane: u64) -> Option<String> {
    Some(codex_notify_at(pane, *PORT.get()?))
}

fn codex_notify_at(pane: u64, port: u16) -> String {
    let curl = if cfg!(windows) { "curl.exe" } else { "curl" };
    serde_json::json!([
        curl,
        "-s",
        "-m",
        "2",
        "-o",
        if cfg!(windows) { "NUL" } else { "/dev/null" },
        format!("http://127.0.0.1:{port}/codex/{pane}/done"),
        "--data-binary"
    ])
    .to_string()
}

/// The start command with what makes a known CLI report back to `pane`;
/// anything else unchanged. With `wrapped`, bare agent commands are left to
/// the shell's wrappers, which add the hooks to every run, typed or not.
pub fn inject(pane: u64, command: &str, wrapped: bool) -> String {
    let command = command.trim();
    let head = command_words(command).first().copied().unwrap_or("");
    let rest = command[head.len()..].trim_start();
    let extra = match program_of(head.trim_matches(['\'', '"'])).as_str() {
        "claude" if !(wrapped && head == "claude") => match claude_hooks(pane) {
            Some(file) => format!("--settings \"{file}\""),
            None => return command.into(),
        },
        // Its "auto" method picks a bell for a terminal it does not know; OSC 9 carries the text.
        "codex" if !(wrapped && head == "codex" && PORT.get().is_some()) => {
            let mut extra = "-c tui.notifications=true -c tui.notification_method=osc9".to_string();
            if let Some(notify) = codex_notify(pane) {
                extra.push_str(&format!(" -c 'notify={notify}'"));
            }
            extra
        }
        _ => return command.into(),
    };
    format!("{head} {extra} {rest}").trim_end().into()
}

/// `claude` for `C:\…\claude.exe`.
pub fn program_of(head: &str) -> String {
    let name = head
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(head)
        .to_lowercase();
    let name = name
        .trim_end_matches(".exe")
        .trim_end_matches(".cmd")
        .trim_end_matches(".ps1");
    name.to_string()
}

/// The PowerShell agent wrappers: runs typed or started by a preset report
/// to `pane`.
pub fn powershell_wrapper(pane: u64) -> Option<String> {
    let mut script = String::new();
    if let Some(file) = claude_hooks(pane) {
        script.push_str(&format!(
            "function global:claude {{ $c = Get-Command claude -CommandType Application,ExternalScript -ErrorAction SilentlyContinue | Select-Object -First 1; if ($c) {{ & $c --settings '{file}' @args }} }}\n"
        ));
    }
    if let Some(notify) = codex_notify(pane) {
        script.push_str(&format!(
            "function global:codex {{ $c = Get-Command codex -CommandType Application,ExternalScript -ErrorAction SilentlyContinue | Select-Object -First 1; if ($c) {{ & $c -c tui.notifications=true -c tui.notification_method=osc9 -c 'notify={notify}' @args }} }}\n"
        ));
    }
    (!script.is_empty()).then_some(script)
}

/// Drop terminal `pane`'s hooks file once it is gone.
pub fn forget(pane: u64) {
    let _ = std::fs::remove_file(hooks_dir().join(format!("{pane}.json")));
}

/// A string field of a hook's JSON.
pub fn hook_field(body: &str, field: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body).ok()?[field]
        .as_str()
        .map(str::to_string)
}

/// Whether a finished turn left background agents or workflows running, which
/// report back with another turn. Shells and monitors don't count: a dev
/// server never ends. Without `background_tasks` (an older CLI), `false`.
pub fn background_pending(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|json| json["background_tasks"].as_array().cloned())
        .is_some_and(|tasks| {
            tasks
                .iter()
                .any(|task| task["agent_type"].is_string() || task["name"].is_string())
        })
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
        Some(id)
            if !id.is_empty()
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') =>
        {
            format!("{base} --resume {id}")
        }
        _ => format!("{base} --continue"),
    }
}

/// Shell words with their original quotes and escapes, so a preset's flag
/// values survive rebuilding its resume command on Unix and PowerShell.
fn command_words(command: &str) -> Vec<&str> {
    let (mut start, mut quote, mut escaped) = (None, None, false);
    let mut words = Vec::new();
    for (at, c) in command.char_indices() {
        if escaped {
            escaped = false;
        } else if c.is_whitespace() && quote.is_none() {
            if let Some(start) = start.take() {
                words.push(&command[start..at]);
            }
            continue;
        } else if Some(c) == quote {
            quote = None;
        } else if quote.is_none() && matches!(c, '\'' | '"') {
            quote = Some(c);
        } else if quote != Some('\'') && matches!(c, '\\' | '`') {
            escaped = true;
        }
        start.get_or_insert(at);
    }
    if let Some(start) = start {
        words.push(&command[start..]);
    }
    words
}

/// A Codex conversation comes back by ID. Old saves without an ID fall back
/// to the current folder's last session; a fresh layout starts a new one.
/// Initial prompts and old resume/fork selectors must not be replayed.
pub fn codex_resume(program: Option<&str>, session: Option<&str>, fresh: bool) -> String {
    let base = command_words(program.filter(|p| is_codex(p)).unwrap_or("codex"));
    let mut words = vec![base.first().copied().unwrap_or("codex")];
    let (mut value, mut selector, mut last, mut old_session) = (false, false, false, None);
    let mut skip = false;
    for word in base.into_iter().skip(1) {
        if skip {
            skip = false;
        } else if value {
            words.push(word);
            value = false;
        } else if matches!(word, "resume" | "fork") {
            selector = true;
        } else if selector && matches!(word, "--last" | "--all" | "--include-non-interactive") {
            // Selection flags belong to the old resume, not this one.
            last |= word == "--last";
        } else if !fresh && matches!(word, "-i" | "--image") {
            skip = true;
        } else if !fresh
            && (word.starts_with("--image=") || matches!(word, "--ephemeral" | "--worktree"))
        {
            // A resumed turn must not replay its original inputs or create a worktree.
        } else if word.starts_with('-') && word != "--" {
            words.push(word);
            value = codex_value_flag(word);
        } else if selector && !last && old_session.is_none() {
            old_session = Some(word);
        } else if fresh && !selector {
            words.push(word);
        }
    }
    let base = words.join(" ");
    match session {
        _ if fresh => base,
        Some(id) if valid_session_id(id) => format!("{base} resume {id}"),
        None if old_session.is_some() => format!("{base} resume {}", old_session.unwrap()),
        _ => format!("{base} resume --last"),
    }
}

fn valid_session_id(id: &str) -> bool {
    id.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn codex_value_flag(word: &str) -> bool {
    matches!(
        word,
        "-c" | "--config"
            | "-m"
            | "--model"
            | "-p"
            | "--profile"
            | "-s"
            | "--sandbox"
            | "-a"
            | "--ask-for-approval"
            | "-C"
            | "--cd"
            | "--add-dir"
            | "--enable"
            | "--disable"
            | "--local-provider"
            | "--remote"
            | "--remote-auth-token-env"
            | "-i"
            | "--image"
    )
}

/// Only interactive Codex commands represent a conversation tab. Commands
/// such as `codex exec` or `codex login` must still run as saved.
pub fn is_codex(program: &str) -> bool {
    let words = command_words(program);
    if words
        .first()
        .is_none_or(|head| program_of(head.trim_matches(['\'', '"'])) != "codex")
    {
        return false;
    }
    let mut value = false;
    for word in words.into_iter().skip(1) {
        if value {
            value = false;
        } else if word.starts_with('-') {
            value = codex_value_flag(word);
        } else {
            return !matches!(
                word,
                "exec"
                    | "e"
                    | "review"
                    | "login"
                    | "logout"
                    | "mcp"
                    | "mcp-server"
                    | "app-server"
                    | "completion"
                    | "sandbox"
                    | "debug"
                    | "apply"
                    | "a"
                    | "cloud"
                    | "features"
                    | "help"
            );
        }
    }
    true
}

/// A notification can only select a session by a safe ID, never by arbitrary
/// command text carried in the event payload.
pub fn codex_notified_resume(program: Option<&str>, body: &str) -> Option<String> {
    let id = hook_field(body, "thread-id").filter(|id| valid_session_id(id))?;
    Some(codex_resume(program, Some(&id), false))
}

/// Resume or clear the conversation in a known agent's command.
pub fn resume(program: &str, session: Option<&str>, fresh: bool) -> Option<String> {
    let words = command_words(program);
    match program_of(words.first()?.trim_matches(['\'', '"'])).as_str() {
        "claude" => Some(claude_resume(Some(program), session, fresh)),
        "codex" if is_codex(program) => Some(codex_resume(Some(program), session, fresh)),
        _ => None,
    }
}

/// What to show for a hook: the message a Notification hook carries, or the
/// last thing the agent said for a finished turn.
pub fn message(body: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    [
        "message",
        "last_assistant_message",
        "last-assistant-message",
    ]
    .iter()
    .find_map(|key| json[*key].as_str())
    .map(|text| {
        text.lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim()
            .chars()
            .take(200)
            .collect()
    })
    .filter(|text: &String| !text.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{
        background_pending, codex_notified_resume, codex_notify_at, codex_resume, inject, is_codex,
        message, parse_path, program_of, resume,
    };

    #[test]
    fn waits_for_background_agents_only() {
        assert!(background_pending(
            r#"{"background_tasks":[{"id":"a","type":"subagent","status":"running","agent_type":"Explore"}]}"#
        ));
        assert!(!background_pending(
            r#"{"background_tasks":[{"id":"b","type":"shell","command":"pnpm dev"}]}"#
        ));
        assert!(!background_pending(r#"{"background_tasks":[]}"#));
        assert!(!background_pending("{}"));
    }

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
        assert_eq!(
            inject(1, "codex", false),
            "codex -c tui.notifications=true -c tui.notification_method=osc9"
        );
    }

    #[test]
    fn resume_commands() {
        use super::claude_resume;
        assert_eq!(
            claude_resume(Some("claude"), Some("abc-123"), false),
            "claude --resume abc-123"
        );
        assert_eq!(
            claude_resume(Some("claude --model opus --continue"), Some("x1"), false),
            "claude --model opus --resume x1"
        );
        assert_eq!(claude_resume(None, None, true), "claude");
        assert_eq!(
            claude_resume(Some("claude --resume old"), None, false),
            "claude --continue"
        );
        assert_eq!(
            claude_resume(Some("claude"), Some("bad id;rm"), false),
            "claude --continue"
        );
    }

    #[test]
    fn codex_resumes_the_named_session_and_keeps_flags() {
        assert_eq!(
            codex_resume(Some("codex"), Some("abc-123"), false),
            "codex resume abc-123"
        );
        assert_eq!(
            codex_resume(
                Some(
                    r#"codex --model "my model" -c 'model_reasoning_effort="high"' resume --last --all"#
                ),
                Some("tab-2"),
                false
            ),
            r#"codex --model "my model" -c 'model_reasoning_effort="high"' resume tab-2"#,
        );
        assert_eq!(
            codex_resume(
                Some(r#""C:\Program Files\codex.exe" resume old --profile work"#),
                Some("new-id"),
                false
            ),
            r#""C:\Program Files\codex.exe" --profile work resume new-id"#,
        );
        assert_eq!(
            codex_resume(Some("codex resume old"), None, false),
            "codex resume old"
        );
        assert_eq!(
            codex_resume(Some("codex"), None, false),
            "codex resume --last"
        );
        assert_eq!(
            codex_resume(Some("codex resume old --model test"), None, true),
            "codex --model test"
        );
    }

    #[test]
    fn codex_resume_does_not_repeat_the_initial_turn() {
        assert_eq!(
            codex_resume(
                Some(r#"codex --model test --image "screen shot.png" --worktree "Fix the bug""#),
                Some("abc"),
                false
            ),
            "codex --model test resume abc",
        );
        assert_eq!(
            codex_resume(
                Some(r#"codex fork old --model test "Try another approach""#),
                Some("fork-id"),
                false
            ),
            "codex --model test resume fork-id",
        );
        assert_eq!(
            codex_resume(Some(r#"codex resume --last "A follow-up""#), None, false),
            "codex resume --last",
        );
        assert_eq!(
            codex_resume(Some("codex --ephemeral"), Some("abc"), false),
            "codex resume abc"
        );
    }

    #[test]
    fn codex_subcommands_are_not_conversation_tabs() {
        for command in [
            "codex exec 'hello'",
            "codex -c model=test login",
            "codex --model test review",
            "codex mcp list",
        ] {
            assert!(!is_codex(command), "{command}");
            assert_eq!(resume(command, None, false), None);
        }
        assert!(is_codex("codex -m test resume abc"));
        assert!(is_codex(r#"codex "Review the changes""#));
    }

    #[test]
    fn codex_callbacks_name_the_tab_and_reject_invalid_ids() {
        let body =
            r#"{"type":"agent-turn-complete","thread-id":"tab-1","last-assistant-message":"Done"}"#;
        let event = parse_path("/codex/42/done", body.into()).unwrap();
        assert_eq!(
            (event.agent.as_str(), event.pane, event.kind.as_str()),
            ("codex", 42, "done")
        );
        assert_eq!(
            codex_notified_resume(Some("codex --model test"), &event.body).as_deref(),
            Some("codex --model test resume tab-1")
        );
        // A Codex typed in a shell still resumes Codex, rather than the shell preset.
        assert_eq!(
            codex_notified_resume(Some("bash"), body).as_deref(),
            Some("codex resume tab-1")
        );
        assert_eq!(message(body).as_deref(), Some("Done"));
        for body in [
            r#"{"thread-id":"bad id;rm"}"#,
            r#"{"thread-id":"--all"}"#,
            "{}",
            "bad json",
        ] {
            assert_eq!(codex_notified_resume(None, body), None);
        }
        let args: Vec<String> = serde_json::from_str(&codex_notify_at(42, 1234)).unwrap();
        assert_eq!(
            &args[args.len() - 2..],
            ["http://127.0.0.1:1234/codex/42/done", "--data-binary"]
        );
        assert_ne!(codex_notify_at(42, 1234), codex_notify_at(43, 1234));
    }

    #[test]
    fn codex_notification_posts_its_json_to_the_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let received =
            std::thread::spawn(move || super::read_request(listener.accept().unwrap().0).unwrap());
        let args: Vec<String> = serde_json::from_str(&codex_notify_at(17, port)).unwrap();
        let body = r#"{"type":"agent-turn-complete","thread-id":"right-tab","last-assistant-message":"A quote: \"done\"\nand another line"}"#;
        assert!(
            std::process::Command::new(&args[0])
                .args(&args[1..])
                .arg(body)
                .status()
                .unwrap()
                .success()
        );
        let event = received.join().unwrap();
        assert_eq!((event.agent.as_str(), event.pane), ("codex", 17));
        assert_eq!(event.body, body);
        assert_eq!(
            codex_notified_resume(None, &event.body).as_deref(),
            Some("codex resume right-tab")
        );
    }

    #[test]
    fn takes_the_message() {
        assert_eq!(
            message(r#"{"message":"Claude needs your permission"}"#).as_deref(),
            Some("Claude needs your permission")
        );
        assert_eq!(message("{}"), None);
    }
}
