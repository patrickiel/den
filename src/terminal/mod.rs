//! Terminal tabs: a shell (or a program in one, such as Claude Code) on a
//! pseudo console.
//!
//! portable-pty opens the ConPTY and spawns the shell; a thread reads its
//! output into alacritty_terminal's parser, which keeps the screen grid;
//! `element.rs` paints the grid cell by cell, `keys.rs` turns keystrokes into
//! the bytes a terminal sends, and `links.rs` finds URLs and file paths for
//! Ctrl+click.

pub mod colors;
mod element;
mod glyphs;
mod keys;
mod links;
mod osc;

pub use osc::encode_powershell;

use std::{
    io::{Read, Write},
    ops::Range,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use alacritty_terminal::{
    Term,
    event::{Event as TermEvent, EventListener, WindowSize},
    grid::{Dimensions, Scroll},
    index::{Column, Line},
    selection::{Selection, SelectionType},
    term::{Config, TermMode, cell::Flags},
    vte::ansi::Processor,
};
use futures::{StreamExt as _, channel::mpsc};
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::{prelude::FluentBuilder as _, *};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    backend::agent,
    pane::{AlertKind, Pane, PaneEvent},
    settings::Settings,
};
use element::TerminalElement;
use links::Link;

pub const TERMINAL: &str = "Terminal";
const CONTEXT: &str = "Terminal";
/// Space between the panel's edge and the grid.
const PADDING: Pixels = px(4.);

/// Bytes for the shell, from a key the app binds elsewhere (Ctrl+W, Ctrl+S,
/// Tab): bound again in the terminal's context so they reach the shell.
#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = terminal, no_json)]
pub struct SendText(pub String);

actions!(terminal, [Copy, Paste, ScrollPageUp, ScrollPageDown]);

/// The terminals by their hook id, for the agent events that name one.
#[derive(Default)]
struct Terminals(std::collections::HashMap<u64, WeakEntity<TerminalPanel>>);

impl Global for Terminals {}

static NEXT_HOOK_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub fn init(cx: &mut App) {
    cx.set_global(Terminals::default());
    // Agent hooks report here; each event goes to the terminal it names.
    if let Some(mut events) = agent::start() {
        cx.spawn(async move |cx| {
            while let Some(event) = events.next().await {
                _ = cx.update(|cx| {
                    let terminal = cx.global::<Terminals>().0.get(&event.pane).and_then(WeakEntity::upgrade);
                    if let Some(terminal) = terminal {
                        terminal.update(cx, |terminal, cx| terminal.agent_event(&event, cx));
                    }
                });
            }
        })
        .detach();
    }
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("ctrl-w", SendText("\x17".into()), context),
        KeyBinding::new("ctrl-s", SendText("\x13".into()), context),
        KeyBinding::new("tab", SendText("\t".into()), context),
        KeyBinding::new("shift-tab", SendText("\x1b[Z".into()), context),
        KeyBinding::new("ctrl-shift-c", Copy, context),
        KeyBinding::new("ctrl-shift-v", Paste, context),
        KeyBinding::new("shift-pageup", ScrollPageUp, context),
        KeyBinding::new("shift-pagedown", ScrollPageDown, context),
    ]);
}

/// What a terminal is started with.
pub struct Launch {
    pub cwd: PathBuf,
    /// The program the tab is (`claude`), `None` for a plain shell.
    pub program: Option<String>,
    /// What the shell runs first (`claude --continue` when a session comes back).
    pub command: Option<String>,
    /// The text a restored shell showed last time, for its scrollback.
    pub history: Option<String>,
    /// A coding agent, for default groups (Agents rather than Terminals).
    pub agent: bool,
}

/// The program a terminal runs, by its preset's name when there is one.
fn program_label(program: &str, cx: &App) -> String {
    if let Some(preset) = Settings::get(cx).presets.iter().find(|p| p.command.trim() == program) {
        return preset.name.clone();
    }
    match program {
        "claude" => "Claude Code".into(),
        other => other.split_whitespace().next().unwrap_or(other).to_string(),
    }
}

/// The shell from Settings; else `pwsh` when it is installed, else Windows
/// PowerShell (`$SHELL` elsewhere). `TERM_SHELL` overrides the automatic choice,
/// as in den.
fn shell(setting: &str) -> String {
    if !setting.trim().is_empty() {
        return setting.trim().to_string();
    }
    if let Ok(shell) = std::env::var("TERM_SHELL") {
        return shell;
    }
    if cfg!(windows) {
        let on_path = |exe: &str| {
            std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(exe).is_file()))
        };
        if on_path("pwsh.exe") { "pwsh.exe".into() } else { "powershell.exe".into() }
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())
    }
}

/// `pwsh` for `C:\…\pwsh.exe`.
fn exe_name(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

/// Events the parser raises while it runs, handled after each batch of output.
#[derive(Clone, Default)]
struct Listener(Arc<Mutex<Vec<TermEvent>>>);

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        if let Ok(mut events) = self.0.lock() {
            events.push(event);
        }
    }
}

struct GridSize {
    columns: usize,
    lines: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.lines
    }

    fn screen_lines(&self) -> usize {
        self.lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

/// The live half of a terminal; absent when the shell could not start.
struct Pty {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
}

pub struct TerminalPanel {
    focus_handle: FocusHandle,
    cwd: PathBuf,
    /// The program run in the shell (`claude`), or `None` for a plain shell.
    program: Option<String>,
    agent: bool,
    shell_name: String,
    term: Term<Listener>,
    parser: Processor,
    events: Listener,
    pty: Option<Pty>,
    error: Option<SharedString>,
    title: Option<SharedString>,
    exited: bool,
    columns: usize,
    lines: usize,
    cell: Size<Pixels>,
    /// Where the grid starts in the window, from the last layout.
    origin: Point<Pixels>,
    selecting: bool,
    /// The link under the pointer while Ctrl is held: its screen row and columns.
    hover_link: Option<(usize, Range<usize>)>,
    cwd_scanner: osc::CwdScanner,
    notify_scanner: osc::NotifyScanner,
    /// Names this terminal to the agent hooks.
    hook_id: u64,
    /// The program it was started with still runs (until the shell's prompt
    /// comes back).
    program_running: bool,
    /// The Claude Code conversation running here, from its hooks: `None`
    /// while none runs, `Some(None)` for a new one not yet named.
    claude_session: Option<Option<String>>,
    /// The last alert, so a burst (a bell and a hook) alerts once.
    last_alert: Option<std::time::Instant>,
    /// Wheel movement not yet sent as a page to a full-screen program.
    wheel_lines: i32,
    _reader: Option<Task<()>>,
}

impl TerminalPanel {
    /// `command` is what the shell runs first (`claude --continue` when a
    /// session comes back), `program` what the tab is (`claude`).
    pub fn new(launch: Launch, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Launch { cwd, program, command, history, agent } = launch;
        let (columns, lines) = (100, 30);
        let events = Listener::default();
        let settings = Settings::get(cx).clone();
        let config = Config {
            scrolling_history: settings.scrollback,
            ..Config::default()
        };
        let term = Term::new(config, &GridSize { columns, lines }, events.clone());
        let focus_handle = cx.focus_handle();
        let shell = shell(&settings.shell);
        let program_is_set = program.is_some();
        let mut this = Self {
            focus_handle,
            cwd,
            program,
            agent,
            shell_name: exe_name(&shell),
            term,
            parser: Processor::new(),
            events,
            pty: None,
            error: None,
            title: None,
            exited: false,
            columns,
            lines,
            cell: size(px(8.), px(17.)),
            origin: Point::default(),
            selecting: false,
            hover_link: None,
            cwd_scanner: osc::CwdScanner::default(),
            notify_scanner: osc::NotifyScanner::default(),
            hook_id: NEXT_HOOK_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            last_alert: None,
            claude_session: None,
            program_running: program_is_set,
            wheel_lines: 0,
            _reader: None,
        };
        if let Some(history) = history.filter(|h| !h.trim().is_empty()) {
            // Then enough new lines to push it into the scrollback, where the
            // console's first screen clear does not reach.
            let text = format!("{}\r\n{}", history.trim_end().replace('\n', "\r\n"), "\r\n".repeat(lines));
            this.parser.advance(&mut this.term, text.as_bytes());
        }
        let weak = cx.weak_entity();
        cx.global_mut::<Terminals>().0.insert(this.hook_id, weak);
        if let Err(err) = this.spawn(&shell, command, columns, lines, cx) {
            this.error = Some(format!("Could not start the shell: {err}").into());
        }
        let focus = this.focus_handle.clone();
        cx.on_focus(&focus, window, |_, _, cx| cx.notify()).detach();
        cx.on_blur(&focus, window, |_, _, cx| cx.notify()).detach();
        this
    }

    fn spawn(&mut self, shell: &str, command: Option<String>, columns: usize, lines: usize, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let pair = native_pty_system().openpty(PtySize {
            rows: lines as u16,
            cols: columns as u16,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut cmd = CommandBuilder::new(shell);
        let lower = shell.to_lowercase();
        let powershell = lower.contains("powershell") || lower.contains("pwsh");
        if powershell {
            // The prompt hook, the `claude` wrapper that adds the agent hooks,
            // and the program, if any, after the profile loads.
            let mut script = osc::POWERSHELL_HOOK.to_string();
            script.push_str(&agent::powershell_wrapper(self.hook_id).unwrap_or_default());
            if let Some(command) = &command {
                script.push_str(&agent::inject(self.hook_id, command, true));
                script.push('\n');
            }
            cmd.args(["-NoLogo", "-NoExit", "-EncodedCommand", &osc::encode_powershell(&script)]);
        } else {
            cmd.env("PROMPT_COMMAND", osc::BASH_HOOK);
            if let Some(command) = &command {
                let command = agent::inject(self.hook_id, command, false);
                cmd.args(["-c", &format!("{command}; exec {shell}")]);
            }
        }
        cmd.cwd(&self.cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        // A den started from a Claude Code session must not hand that session's
        // markers on: Claude Code in a tab would take itself for a child session.
        for (key, _) in std::env::vars_os() {
            let key = key.to_string_lossy();
            if key.starts_with("CLAUDE_CODE") || key == "CLAUDECODE" || key.starts_with("GPUI_") {
                cmd.env_remove(key.as_ref());
            }
        }
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        // Blocking reads on a thread of their own; the view parses on its side.
        let (tx, mut rx) = mpsc::unbounded::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.unbounded_send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        self._reader = Some(cx.spawn(async move |this, cx| {
            while let Some(mut bytes) = rx.next().await {
                // Take whatever else has arrived, so a burst draws once.
                while let Ok(more) = rx.try_recv() {
                    bytes.extend(more);
                }
                if this.update(cx, |this, cx| this.feed(&bytes, cx)).is_err() {
                    return;
                }
            }
            _ = this.update(cx, |this, cx| {
                this.exited = true;
                cx.notify();
            });
        }));
        self.pty = Some(Pty {
            master: pair.master,
            writer,
            child,
        });
        Ok(())
    }

    fn feed(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        if let Some(cwd) = self.cwd_scanner.scan(bytes) {
            self.cwd = cwd;
            // The shell's prompt is back: whatever program or Claude Code ran
            // has ended.
            self.claude_session = None;
            if self.program_running {
                self.program_running = false;
                cx.emit(PaneEvent::Changed);
            }
        }
        for message in self.notify_scanner.scan(bytes) {
            let codex = self.program.as_deref().map(|p| agent::program_of(p.split_whitespace().next().unwrap_or(p))).as_deref() == Some("codex");
            let kind = match message.to_lowercase() {
                _ if !codex => AlertKind::Attention,
                text if text.contains("approv") || text.contains("permission") || text.contains("waiting") => AlertKind::Input,
                _ => AlertKind::Done,
            };
            let message = (!message.trim().eq_ignore_ascii_case("agent turn complete")).then_some(message);
            self.alert(kind, message, cx);
        }
        self.parser.advance(&mut self.term, bytes);
        let events: Vec<TermEvent> = self.events.0.lock().map(|mut e| std::mem::take(&mut *e)).unwrap_or_default();
        for event in events {
            match event {
                TermEvent::PtyWrite(text) => self.write(text.as_bytes()),
                TermEvent::Title(title) => {
                    self.title = Some(title.into());
                    cx.emit(PaneEvent::Changed);
                }
                TermEvent::ResetTitle => {
                    self.title = None;
                    cx.emit(PaneEvent::Changed);
                }
                TermEvent::Bell => self.alert(AlertKind::Attention, None, cx),
                TermEvent::ClipboardStore(_, text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
                TermEvent::ClipboardLoad(_, format) => {
                    let text = cx.read_from_clipboard().and_then(|item| item.text()).unwrap_or_default();
                    self.write(format(&text).as_bytes());
                }
                TermEvent::TextAreaSizeRequest(format) => {
                    let size = WindowSize {
                        num_lines: self.lines as u16,
                        num_cols: self.columns as u16,
                        cell_width: self.cell.width.as_f32() as u16,
                        cell_height: self.cell.height.as_f32() as u16,
                    };
                    self.write(format(size).as_bytes());
                }
                TermEvent::ColorRequest(index, format) => {
                    let rgb = colors::palette_rgb(index);
                    self.write(format(rgb).as_bytes());
                }
                _ => {}
            }
        }
        cx.notify();
    }

    /// Tell the workspace the program wants the user, at most once in two seconds.
    fn alert(&mut self, kind: AlertKind, message: Option<String>, cx: &mut Context<Self>) {
        let now = std::time::Instant::now();
        if self.last_alert.is_some_and(|last| now.duration_since(last).as_secs_f32() < 2.) {
            return;
        }
        self.last_alert = Some(now);
        cx.emit(PaneEvent::Alert { kind, message });
    }

    /// A hook of the agent running here reported.
    fn agent_event(&mut self, event: &agent::AgentEvent, cx: &mut Context<Self>) {
        let session = agent::hook_field(&event.body, "session_id");
        match event.kind.as_str() {
            "start" => {
                // A new or cleared conversation has no transcript to resume yet.
                let source = agent::hook_field(&event.body, "source").unwrap_or_default();
                self.claude_session = Some(if source == "startup" || source == "clear" { None } else { session });
                return;
            }
            _ if session.is_some() => self.claude_session = Some(session),
            _ => {}
        }
        let kind = match event.kind.as_str() {
            "done" => AlertKind::Done,
            "input" => AlertKind::Input,
            _ => return,
        };
        // A hook says more than the bell that may come with it.
        self.last_alert = None;
        self.alert(kind, agent::message(&event.body), cx);
    }

    /// The title the program set, without the spinner or status glyph it
    /// leads with while it works.
    pub fn title_text(&self) -> Option<String> {
        let title = self.title.as_ref()?;
        let text = title.trim_start_matches(|c: char| !c.is_alphanumeric()).trim();
        (!text.is_empty()).then(|| text.to_string())
    }

    fn write(&mut self, bytes: &[u8]) {
        if let Some(pty) = &mut self.pty {
            _ = pty.writer.write_all(bytes);
            _ = pty.writer.flush();
        }
    }

    /// Typing goes to the shell, drops the selection and brings the view back
    /// to the bottom.
    fn input(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        if self.term.grid().display_offset() != 0 {
            self.term.scroll_display(Scroll::Bottom);
        }
        if self.term.selection.take().is_some() {
            cx.notify();
        }
        self.write(bytes);
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else { return };
        // Line ends as a terminal sends them, wrapped for programs that ask.
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        if self.term.mode().contains(TermMode::BRACKETED_PASTE) {
            self.input(format!("\x1b[200~{text}\x1b[201~").as_bytes(), cx);
        } else {
            self.input(text.as_bytes(), cx);
        }
    }

    /// Copy the selection, if there is one; `false` when there was nothing.
    fn copy(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(text) = self.term.selection_to_string().filter(|text| !text.is_empty()) else {
            return false;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.term.selection = None;
        cx.notify();
        true
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        // Ctrl+C copies while there is a selection, as in Windows Terminal.
        if keystroke.key == "c" && keystroke.modifiers.control && !keystroke.modifiers.shift && !keystroke.modifiers.alt && self.copy(cx) {
            cx.stop_propagation();
            return;
        }
        let app_cursor = self.term.mode().contains(TermMode::APP_CURSOR);
        if let Some(bytes) = keys::key_bytes(keystroke, app_cursor) {
            self.input(&bytes, cx);
            cx.stop_propagation();
        }
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let lines = match event.delta {
            ScrollDelta::Lines(delta) => delta.y * 3.,
            ScrollDelta::Pixels(delta) => delta.y.as_f32() / self.cell.height.as_f32().max(1.),
        };
        let lines = lines.round() as i32;
        if lines == 0 {
            return;
        }
        let mode = *self.term.mode();
        if mode.intersects(TermMode::MOUSE_MODE) {
            // The program asked for the mouse: real wheel events (SGR or X10).
            let (_, _, row, col) = self.cell_at(event.position);
            let button = if lines > 0 { 64 } else { 65 };
            for _ in 0..lines.unsigned_abs().min(10) {
                let report = if mode.contains(TermMode::SGR_MOUSE) {
                    format!("\x1b[<{button};{};{}M", col + 1, row + 1)
                } else {
                    let byte = |v: usize| char::from_u32(32 + v.min(222) as u32).unwrap_or(' ');
                    format!("\x1b[M{}{}{}", byte(button as usize), byte(col + 1), byte(row + 1))
                };
                self.write(report.as_bytes());
            }
        } else if mode.contains(TermMode::ALT_SCREEN) {
            // A full-screen program that scrolls itself (Claude Code): pages,
            // not arrow keys, which would walk its prompt history.
            self.wheel_lines += lines;
            const PER_PAGE: i32 = 5;
            while self.wheel_lines.abs() >= PER_PAGE {
                let up = self.wheel_lines > 0;
                self.write(if up { b"\x1b[5~" } else { b"\x1b[6~" });
                self.wheel_lines -= if up { PER_PAGE } else { -PER_PAGE };
            }
        } else {
            self.term.scroll_display(Scroll::Delta(lines));
            cx.notify();
        }
    }

    // -- Mouse ---------------------------------------------------------------

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
        if event.modifiers.control {
            if let Some((_, _, link)) = self.link_at(event.position) {
                self.open_link(link, cx);
            }
            return;
        }
        let (point, side, _, _) = self.cell_at(event.position);
        let kind = match event.click_count {
            1 => SelectionType::Simple,
            2 => SelectionType::Semantic,
            _ => SelectionType::Lines,
        };
        self.term.selection = Some(Selection::new(kind, point, side));
        self.selecting = true;
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting && event.pressed_button == Some(MouseButton::Left) {
            let (point, side, _, _) = self.cell_at(event.position);
            if let Some(selection) = &mut self.term.selection {
                selection.update(point, side);
            }
            cx.notify();
        }
        let hover = event
            .modifiers
            .control
            .then(|| self.link_at(event.position))
            .flatten()
            .map(|(row, range, _)| (row, range));
        if hover != self.hover_link {
            self.hover_link = hover;
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.selecting = false;
        // A click without a drag selects nothing.
        if self.term.selection_to_string().is_none_or(|text| text.is_empty()) {
            self.term.selection = None;
            cx.notify();
        }
    }

    /// The characters of a screen row, one per column.
    fn row_text(&self, row: usize) -> Vec<char> {
        let offset = self.term.grid().display_offset() as i32;
        let line = &self.term.grid()[Line(row as i32 - offset)];
        (0..self.columns)
            .map(|col| {
                let cell = &line[Column(col)];
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    ' '
                } else {
                    cell.c
                }
            })
            .collect()
    }

    fn link_at(&self, position: Point<Pixels>) -> Option<(usize, Range<usize>, Link)> {
        let (_, _, row, col) = self.cell_at(position);
        let text = self.row_text(row);
        links::find(&text, col, &self.cwd, |path| path.is_file()).map(|(range, link)| (row, range, link))
    }

    fn open_link(&mut self, link: Link, cx: &mut Context<Self>) {
        match link {
            Link::Url(url) => cx.open_url(&url),
            Link::File { path, line, column } => cx.emit(PaneEvent::OpenFile { path, line, column }),
        }
    }

    /// Fit the grid (and the pseudo console) to the space the view was given.
    fn fit(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let font_size = px(Settings::get(cx).editor_font_size);
        let font = font(crate::settings::mono_font(cx));
        let text = window.text_system();
        let font_id = text.resolve_font(&font);
        let width = text.advance(font_id, font_size, 'm').map(|s| s.width).unwrap_or(font_size * 0.6);
        self.cell = size(width, (font_size * 1.2).round());
        self.origin = point(bounds.origin.x + PADDING, bounds.origin.y + PADDING / 2.);
        let columns = ((bounds.size.width - PADDING * 2.) / self.cell.width).floor().max(2.) as usize;
        let lines = ((bounds.size.height - PADDING) / self.cell.height).floor().max(1.) as usize;
        if (columns, lines) == (self.columns, self.lines) {
            return;
        }
        self.columns = columns;
        self.lines = lines;
        self.term.resize(GridSize { columns, lines });
        if let Some(pty) = &self.pty {
            _ = pty.master.resize(PtySize {
                rows: lines as u16,
                cols: columns as u16,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
        cx.notify();
    }

    /// The last `max` lines of output as plain text, trailing blank lines dropped.
    fn scrollback(&self, max: usize) -> String {
        let grid = self.term.grid();
        let start = alacritty_terminal::index::Point::new(Line(-(grid.history_size() as i32)), Column(0));
        let end = alacritty_terminal::index::Point::new(Line(self.lines as i32 - 1), Column(self.columns.saturating_sub(1)));
        let text = self.term.bounds_to_string(start, end);
        let lines: Vec<&str> = text.trim_end().lines().collect();
        lines[lines.len().saturating_sub(max)..].join("\n")
    }

    pub fn program(&self) -> Option<&str> {
        self.program.as_deref()
    }

    pub fn is_agent(&self) -> bool {
        self.agent
    }

    fn tab_label(&self, cx: &App) -> SharedString {
        if let Some(title) = &self.title {
            let title = title.trim();
            if !title.is_empty() {
                // PowerShell names its window after its executable; show the
                // shell's name instead of the path.
                if title.to_lowercase().ends_with(".exe") {
                    return exe_name(title).into();
                }
                return title.to_string().into();
            }
        }
        match &self.program {
            Some(program) => program_label(program, cx).into(),
            None => self.shell_name.clone().into(),
        }
    }
}

impl Drop for TerminalPanel {
    fn drop(&mut self) {
        if let Some(pty) = &mut self.pty {
            _ = pty.child.kill();
        }
        agent::forget(self.hook_id);
    }
}

impl EventEmitter<PaneEvent> for TerminalPanel {}

impl Focusable for TerminalPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Pane for TerminalPanel {
    fn kind(&self) -> &'static str {
        TERMINAL
    }

    fn icon(&self, _: &App) -> IconName {
        if self.agent { IconName::Bot } else { IconName::SquareTerminal }
    }

    /// The mark of the program running here, as den shows it: its preset's
    /// icon, else a known CLI's logo; the terminal icon otherwise.
    fn icon_element(&self, cx: &App) -> Option<AnyElement> {
        let program = if self.program_running {
            self.program.clone()?
        } else if self.claude_session.is_some() || self.title.as_ref().is_some_and(|t| t.contains("Claude Code")) {
            "claude".to_string()
        } else {
            return None;
        };
        let settings = Settings::get(cx);
        if let Some(preset) = settings.presets.iter().find(|p| !p.browser && p.command.trim() == program.trim()) {
            return Some(crate::preset_icon::render(preset, preset.icon.as_deref(), 16., cx));
        }
        crate::preset_icon::has_program_logo(&program).then(|| {
            let preset = crate::settings::Preset {
                name: program.clone(),
                command: program.clone(),
                agent: true,
                browser: false,
                pinned: false,
                color: None,
                icon: None,
            };
            crate::preset_icon::render(&preset, None, 16., cx)
        })
    }

    fn label(&self, cx: &App) -> SharedString {
        self.tab_label(cx)
    }

    fn dump(&self, _: &App) -> Value {
        json!({
            "cwd": self.cwd.to_string_lossy(),
            "program": self.program,
            "agent": self.agent,
            // A shell brings its last screens back; a program redraws itself.
            "scrollback": if self.program.is_none() && self.claude_session.is_none() { self.scrollback(500) } else { String::new() },
            // The Claude Code conversation to bring back, as den does.
            "resume": self.claude_session.as_ref().map(|session| agent::claude_resume(self.program.as_deref(), session.as_deref(), session.is_none())),
        })
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);
        let theme = cx.theme();
        let link_cursor = self.hover_link.is_some();

        v_flex()
            .id("terminal")
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(colors::palette().background.unwrap_or(theme.background))
            .text_color(theme.foreground)
            .cursor(if link_cursor { CursorStyle::PointingHand } else { CursorStyle::IBeam })
            .on_key_down(cx.listener(Self::on_key_down))
            .on_action(cx.listener(|this, action: &SendText, _, cx| this.input(action.0.as_bytes(), cx)))
            .on_action(cx.listener(|this, _: &Copy, _, cx| {
                this.copy(cx);
            }))
            .on_action(cx.listener(|this, _: &Paste, _, cx| this.paste(cx)))
            .on_action(cx.listener(|this, _: &ScrollPageUp, _, cx| {
                this.term.scroll_display(Scroll::PageUp);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ScrollPageDown, _, cx| {
                this.term.scroll_display(Scroll::PageDown);
                cx.notify();
            }))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            // Right-click copies a selection, else pastes, as in den.
            .on_mouse_down(MouseButton::Right, cx.listener(|this, _, window, cx| {
                this.focus_handle.focus(window, cx);
                if !this.copy(cx) {
                    this.paste(cx);
                }
            }))
            .child(TerminalElement::new(cx.entity(), focused))
            .when_some(self.error.clone(), |this, error| {
                this.child(div().absolute().top_0().left_0().p_2().text_color(cx.theme().danger).child(error))
            })
            .when(self.exited, |this| {
                this.child(
                    div()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .p_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("[process exited]"),
                )
            })
    }
}
