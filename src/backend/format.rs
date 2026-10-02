//! Format Document by the formatters on this machine: the project's own
//! Prettier (as `prettier --write` would), rustfmt, Ruff (else Black), gofmt,
//! shfmt, clang-format, and PSScriptAnalyzer for PowerShell as in den. Each
//! reads the text on stdin and writes the result to stdout, from the file's
//! folder so it finds the project's config.

use std::{
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// A formatter and what it needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tool {
    /// The Prettier program to run (the project's, else one on PATH).
    Prettier(PathBuf),
    Rustfmt,
    Ruff,
    Black,
    Gofmt,
    Shfmt,
    ClangFormat,
    PowerShell,
}

impl Tool {
    pub fn name(&self) -> &'static str {
        match self {
            Tool::Prettier(_) => "Prettier",
            Tool::Rustfmt => "rustfmt",
            Tool::Ruff => "Ruff",
            Tool::Black => "Black",
            Tool::Gofmt => "gofmt",
            Tool::Shfmt => "shfmt",
            Tool::ClangFormat => "clang-format",
            Tool::PowerShell => "PSScriptAnalyzer",
        }
    }
}

const PRETTIER: &[&str] = &[
    "js", "mjs", "cjs", "jsx", "ts", "mts", "cts", "tsx", "json", "jsonc", "json5", "css", "scss", "less", "html", "htm", "vue", "svelte", "md",
    "markdown", "mdx", "yaml", "yml", "graphql", "gql", "hbs", "handlebars",
];

/// The formatter for `path`, if one is installed; `Err` names what to install.
pub fn tool_for(path: &Path) -> Result<Tool, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let need = |tool: Tool, exe: &str, install: &str| if on_path(exe).is_some() { Ok(tool) } else { Err(format!("{exe} is not installed: {install}")) };
    match ext.as_str() {
        e if PRETTIER.contains(&e) => prettier(path).map(Tool::Prettier).ok_or_else(|| "Prettier is not installed: add it to the project (pnpm add -D prettier)".into()),
        "rs" => need(Tool::Rustfmt, "rustfmt", "rustup component add rustfmt"),
        "py" | "pyi" => need(Tool::Ruff, "ruff", "pip install ruff").or_else(|e| need(Tool::Black, "black", "").map_err(|_| e)),
        "go" => need(Tool::Gofmt, "gofmt", "install Go"),
        "sh" | "bash" => need(Tool::Shfmt, "shfmt", "see github.com/mvdan/sh"),
        "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" => need(Tool::ClangFormat, "clang-format", "install LLVM"),
        "ps1" | "psm1" | "psd1" => Ok(Tool::PowerShell),
        _ => Err(format!("no formatter for .{ext} files")),
    }
}

/// The project's Prettier (`node_modules/.bin` in the file's folder or one
/// above it), else one on PATH.
fn prettier(path: &Path) -> Option<PathBuf> {
    let exe = if cfg!(windows) { "prettier.cmd" } else { "prettier" };
    path.ancestors()
        .skip(1)
        .map(|dir| dir.join("node_modules").join(".bin").join(exe))
        .find(|candidate| candidate.is_file())
        .or_else(|| on_path("prettier"))
}

/// `name` on PATH, with the Windows program extensions tried.
fn on_path(name: &str) -> Option<PathBuf> {
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat", ""] } else { &[""] };
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| exts.iter().map(|ext| dir.join(format!("{name}{ext}"))).find(|p| p.is_file()))
}

/// The edition in the nearest `Cargo.toml`, for rustfmt (which reading stdin
/// assumes 2015).
fn rust_edition(path: &Path) -> String {
    path.ancestors()
        .skip(1)
        .find_map(|dir| std::fs::read_to_string(dir.join("Cargo.toml")).ok())
        .and_then(|toml| {
            toml.lines().find_map(|line| {
                let (key, value) = line.split_once('=')?;
                (key.trim() == "edition").then(|| value.trim().trim_matches('"').to_string())
            })
        })
        .unwrap_or_else(|| "2021".into())
}

/// PowerShell: PSScriptAnalyzer's Invoke-Formatter, with the nearest
/// PSScriptAnalyzerSettings.psd1 above the file, else One True Brace Style.
const POWERSHELL: &str = r#"$ErrorActionPreference = 'Stop'
$utf8 = [Text.UTF8Encoding]::new($false)
[Console]::InputEncoding = $utf8
[Console]::OutputEncoding = $utf8
if (-not (Get-Module -ListAvailable PSScriptAnalyzer)) {
  [Console]::Error.Write('PSScriptAnalyzer is not installed. Install it with: Install-Module PSScriptAnalyzer -Scope CurrentUser')
  exit 3
}
$text = [Console]::In.ReadToEnd()
$settings = 'CodeFormattingOTBS'
$dir = if ($env:DEN_FORMAT_PATH) { Split-Path -Parent $env:DEN_FORMAT_PATH }
while ($dir) {
  $f = Join-Path $dir 'PSScriptAnalyzerSettings.psd1'
  if (Test-Path -LiteralPath $f) { $settings = $f; break }
  $parent = Split-Path -Parent $dir
  if ($parent -eq $dir) { break }
  $dir = $parent
}
[Console]::Out.Write((Invoke-Formatter -ScriptDefinition $text -Settings $settings))"#;

/// `text` (the file at `path`) formatted by `tool`; `Err` carries the
/// formatter's message (a syntax error, say).
pub fn run(tool: &Tool, path: &Path, text: &str) -> Result<String, String> {
    let file = path.to_string_lossy().to_string();
    let mut cmd = match tool {
        Tool::Prettier(exe) => {
            let mut cmd = Command::new(exe);
            cmd.args(["--stdin-filepath", &file]);
            cmd
        }
        Tool::Rustfmt => {
            let mut cmd = Command::new("rustfmt");
            cmd.args(["--edition", &rust_edition(path), "--emit", "stdout"]);
            cmd
        }
        Tool::Ruff => {
            let mut cmd = Command::new("ruff");
            cmd.args(["format", "--stdin-filename", &file, "-"]);
            cmd
        }
        Tool::Black => {
            let mut cmd = Command::new("black");
            cmd.args(["-q", "--stdin-filename", &file, "-"]);
            cmd
        }
        Tool::Gofmt => Command::new("gofmt"),
        Tool::Shfmt => {
            let mut cmd = Command::new("shfmt");
            cmd.args(["--filename", &file]);
            cmd
        }
        Tool::ClangFormat => {
            let mut cmd = Command::new("clang-format");
            cmd.arg(format!("--assume-filename={file}"));
            cmd
        }
        Tool::PowerShell => {
            let shell = if on_path("pwsh").is_some() { "pwsh" } else { "powershell" };
            let mut cmd = Command::new(shell);
            cmd.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", &crate::terminal::encode_powershell(POWERSHELL)]);
            cmd.env("DEN_FORMAT_PATH", &file);
            cmd
        }
    };
    if let Some(dir) = path.parent().filter(|d| d.is_dir()) {
        cmd.current_dir(dir);
    }
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|err| format!("cannot start {}: {err}", tool.name()))?;
    if let Some(mut pipe) = child.stdin.take() {
        // From its own thread, so a large file cannot deadlock against a full stdout.
        let text = text.to_string();
        std::thread::spawn(move || {
            let _ = pipe.write_all(text.as_bytes());
        });
    }
    let out = child.wait_with_output().map_err(|err| err.to_string())?;
    if out.status.success() {
        let formatted = String::from_utf8(out.stdout).map_err(|err| err.to_string())?;
        Ok(keep_line_endings(text, formatted))
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let err = if err.is_empty() { String::from_utf8_lossy(&out.stdout).trim().to_string() } else { err };
        Err(if err.is_empty() { format!("{} failed ({})", tool.name(), out.status) } else { err.lines().take(6).collect::<Vec<_>>().join("\n") })
    }
}

/// The formatted text with the line endings the file had (rustfmt writes
/// Windows ones on Windows, Prettier Unix ones).
fn keep_line_endings(original: &str, formatted: String) -> String {
    let unix = formatted.replace("\r\n", "\n");
    if original.contains("\r\n") { unix.replace('\n', "\r\n") } else { unix }
}

#[cfg(test)]
mod tests {
    use super::{keep_line_endings, rust_edition, tool_for};
    use std::path::Path;

    #[test]
    fn line_endings_follow_the_file() {
        assert_eq!(keep_line_endings("a\n", "b\r\nc\r\n".into()), "b\nc\n");
        assert_eq!(keep_line_endings("a\r\n", "b\nc\n".into()), "b\r\nc\r\n");
    }

    #[test]
    fn edition_from_the_nearest_manifest() {
        let here = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("main.rs");
        assert_eq!(rust_edition(&here), "2024");
    }

    #[test]
    fn unknown_kinds_have_no_formatter() {
        assert!(tool_for(Path::new("notes.txt")).is_err());
    }
}
