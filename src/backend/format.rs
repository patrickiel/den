//! Format Document by the formatters on this machine: the project's own
//! Prettier (as `prettier --write` would), rustfmt, Ruff (else Black), gofmt,
//! shfmt, clang-format, StyLua, google-java-format, and PSScriptAnalyzer for
//! PowerShell as in den. Each reads the text on stdin and writes the result to
//! stdout, from the file's folder so it finds the project's config.
//!
//! What is not installed den downloads on request, as pinned releases kept in
//! `%APPDATA%\den\tools`: dprint with a plugin per language (Prettier itself
//! for the web languages, Ruff, gofumpt, shfmt, clang-format, TOML,
//! Dockerfile, PHP, SQL, XML, CMake), and StyLua, google-java-format and
//! PSScriptAnalyzer where dprint has none. The plugins read no project config.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use super::{
    http::{self, Cancel, Progress},
    process,
};

/// A formatter and the program it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tool {
    /// The Prettier program to run (the project's, else one on PATH).
    Prettier(PathBuf),
    Rustfmt(PathBuf),
    Ruff(PathBuf),
    Black(PathBuf),
    Gofmt(PathBuf),
    Shfmt(PathBuf),
    ClangFormat(PathBuf),
    Stylua(PathBuf),
    JavaFormat(PathBuf),
    /// PSScriptAnalyzer: den's download of the module, else the installed one.
    PowerShell(Option<PathBuf>),
    /// A downloaded dprint plugin: dprint, the plugin, and the file name the
    /// text goes by (dprint picks the language by it).
    Dprint { exe: PathBuf, plugin: &'static Kit, name: String },
}

impl Tool {
    pub fn name(&self) -> &'static str {
        match self {
            Tool::Prettier(_) => "Prettier",
            Tool::Rustfmt(_) => "rustfmt",
            Tool::Ruff(_) => "Ruff",
            Tool::Black(_) => "Black",
            Tool::Gofmt(_) => "gofmt",
            Tool::Shfmt(_) => "shfmt",
            Tool::ClangFormat(_) => "clang-format",
            Tool::Stylua(_) => "StyLua",
            Tool::JavaFormat(_) => "google-java-format",
            Tool::PowerShell(_) => "PSScriptAnalyzer",
            Tool::Dprint { plugin, .. } => plugin.name,
        }
    }
}

const PRETTIER: &[&str] = &[
    "js", "mjs", "cjs", "jsx", "ts", "mts", "cts", "tsx", "json", "jsonc", "json5", "css", "scss", "less", "html", "htm", "vue", "svelte", "md",
    "markdown", "mdx", "yaml", "yml", "graphql", "gql", "hbs", "handlebars",
];
const C_FAMILY: &[&str] = &["c", "h", "cc", "cpp", "cxx", "hpp", "hh", "hxx", "m", "mm"];
const SHELL: &[&str] = &["sh", "bash", "zsh"];
const XML: &[&str] = &["xml", "svg", "xsd", "xsl", "xslt", "wsdl"];
const POWERSHELL_FILES: &[&str] = &["ps1", "psm1", "psd1"];

fn extension(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase()
}

/// The formatter for `path`, if one is installed or downloaded; `Err` says
/// what is missing.
pub fn tool_for(path: &Path) -> Result<Tool, String> {
    let ext = extension(path);
    let native = |make: fn(PathBuf) -> Tool, exe: &str| on_path(exe).map(make);
    let found = match ext.as_str() {
        e if PRETTIER.contains(&e) => prettier(path).map(Tool::Prettier),
        "rs" => return native(Tool::Rustfmt, "rustfmt").ok_or_else(|| "rustfmt is not installed: rustup component add rustfmt".into()),
        "py" | "pyi" => native(Tool::Ruff, "ruff").or_else(|| native(Tool::Black, "black")),
        "go" => native(Tool::Gofmt, "gofmt"),
        e if SHELL.contains(&e) => native(Tool::Shfmt, "shfmt"),
        e if C_FAMILY.contains(&e) => native(Tool::ClangFormat, "clang-format"),
        "lua" => native(Tool::Stylua, "stylua").or_else(|| installed(&STYLUA).map(Tool::Stylua)),
        "java" => native(Tool::JavaFormat, "google-java-format").or_else(|| installed(&JAVA_FORMAT).map(Tool::JavaFormat)),
        e if POWERSHELL_FILES.contains(&e) => match installed(&PSSCRIPTANALYZER) {
            Some(module) => Some(Tool::PowerShell(Some(module))),
            None => psscriptanalyzer_installed().then_some(Tool::PowerShell(None)),
        },
        _ => None,
    };
    if let Some(tool) = found {
        return Ok(tool);
    }
    if let Some((plugin, name)) = plugin_for(path)
        && let (Some(exe), Some(_)) = (installed(&DPRINT), installed(plugin))
    {
        return Ok(Tool::Dprint { exe, plugin, name });
    }
    let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Err(match kit_for(path) {
        Some(kit) => format!("{} is not installed", kit.name),
        None => format!("no formatter for {file}"),
    })
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
    search_path().iter().find_map(|dir| exts.iter().map(|ext| dir.join(format!("{name}{ext}"))).find(|p| p.is_file()))
}

/// The folders of PATH. On Windows the saved user and machine PATH follow
/// den's own: den may have been started before a tool was installed, or by a
/// program with an older environment.
fn search_path() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    #[cfg(windows)]
    for (key, sub) in [
        (windows_registry::CURRENT_USER, "Environment"),
        (windows_registry::LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment"),
    ] {
        if let Ok(saved) = key.open(sub).and_then(|k| k.get_string("Path")) {
            dirs.extend(std::env::split_paths(&expand_env(&saved)));
        }
    }
    dirs
}

/// `text` with its `%NAME%` references replaced by the variables' values, as
/// Windows expands registry strings; unknown names stay.
#[cfg_attr(not(windows), allow(dead_code))]
fn expand_env(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%').filter(|&end| end > 0).and_then(|end| Some((end, std::env::var(&after[..end]).ok()?))) {
            Some((end, value)) => {
                out.push_str(&value);
                rest = &after[end + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Whether PSScriptAnalyzer is in a folder PowerShell loads modules from.
fn psscriptanalyzer_installed() -> bool {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PSModulePath").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    // Each PowerShell adds the user's folders itself, so den's environment may lack them.
    #[cfg(windows)]
    {
        let documents = windows_registry::CURRENT_USER
            .open(r"Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders")
            .and_then(|k| k.get_string("Personal"))
            .map(|d| PathBuf::from(expand_env(&d)))
            .ok()
            .or_else(|| std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join("Documents")));
        let programs = std::env::var_os("ProgramFiles").map(PathBuf::from);
        for base in documents.into_iter().chain(programs) {
            dirs.push(base.join("WindowsPowerShell").join("Modules"));
            dirs.push(base.join("PowerShell").join("Modules"));
        }
    }
    dirs.iter().any(|dir| dir.join("PSScriptAnalyzer").is_dir())
}

// -- Downloads ---------------------------------------------------------------

/// Something den can download: a pinned release, checked against its SHA-256
/// and kept in `%APPDATA%\den\tools\<dir>`.
#[derive(Debug, PartialEq, Eq)]
pub struct Kit {
    pub name: &'static str,
    /// What it formats, for Settings.
    pub formats: &'static str,
    /// The file den runs (or imports, or hands dprint), once downloaded.
    file: &'static str,
    /// `<tool>-<version>`; any other folder goes on install.
    dir: &'static str,
    url: &'static str,
    sha256: &'static str,
    kind: Kind,
    pub size: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// The program itself.
    Program,
    /// A zip to unpack (a nupkg is one).
    Zip,
    /// A dprint plugin: a Wasm module, or a process plugin's manifest (whose
    /// program dprint fetches into its cache, checked by the manifest).
    Plugin,
}

/// dprint, which runs the plugins. The x64 build everywhere: the Prettier
/// plugin has no Arm one, and Windows on Arm runs x64 programs.
static DPRINT: Kit = Kit {
    name: "dprint",
    formats: "runs the plugins",
    file: "dprint.exe",
    dir: "dprint-0.59.0",
    url: "https://github.com/dprint/dprint/releases/download/0.59.0/dprint-x86_64-pc-windows-msvc.zip",
    sha256: "f75040d7288d3cf0a271b025dedcab589edec6b363c481790ff53850dadae6f5",
    kind: Kind::Zip,
    size: "9 MB",
};

const fn plugin(name: &'static str, formats: &'static str, file: &'static str, dir: &'static str, url: &'static str, sha256: &'static str, size: &'static str) -> Kit {
    Kit { name, formats, file, dir, url, sha256, kind: Kind::Plugin, size }
}

static PRETTIER_PLUGIN: Kit = plugin(
    "Prettier",
    "JS, TS, CSS, HTML, Vue, Svelte, Markdown, YAML",
    "prettier.json",
    "plugin-prettier-0.72.0",
    "https://plugins.dprint.dev/prettier-0.72.0.json",
    "6e7af2cb2182a5a11358d3adde9ae0ef3b94b7d2e5dcfdd252e3f7c3916541fe",
    "20 MB",
);
static RUFF: Kit = plugin(
    "Ruff",
    "Python",
    "ruff.wasm",
    "plugin-ruff-0.9.1",
    "https://plugins.dprint.dev/ruff-0.9.1.wasm",
    "a5b1afada43eb1e3913bfe3363f1e93e0a201482eef5b2ed538f12b065f73137",
    "13 MB",
);
static GOFUMPT: Kit = plugin(
    "gofumpt",
    "Go",
    "gofumpt.wasm",
    "plugin-gofumpt-0.0.18",
    "https://plugins.dprint.dev/jakebailey/gofumpt-v0.0.18.wasm",
    "21a4d37fab5de6d8c7fb1d7912ed4ccedfa41b425a67c7b0e21735bb0292e88a",
    "1 MB",
);
static SHFMT: Kit = plugin(
    "shfmt",
    "Shell",
    "sh.wasm",
    "plugin-sh-0.1.2",
    "https://plugins.dprint.dev/kachick/sh-0.1.2.wasm",
    "8564c9082c81ff4cf6c9090cfa20021003d7d402254877ebf5d7b0f487437476",
    "3 MB",
);
static CLANG_FORMAT: Kit = plugin(
    "clang-format",
    "C, C++, Objective-C",
    "clang-format.wasm",
    "plugin-clang-format-0.1.0",
    "https://plugins.dprint.dev/sargunv/dprint-clang-format-0.1.0.wasm",
    "fcf4fe85f4527454ed9bed4fcdfa2dd994e0721c91a0a57329a2d1ed112803a2",
    "4 MB",
);
static TOML: Kit = plugin(
    "TOML",
    "TOML",
    "toml.wasm",
    "plugin-toml-0.8.0",
    "https://plugins.dprint.dev/toml-0.8.0.wasm",
    "69cb40cba5e8a53560ccea7fdced073a55d04eb05492f0d9b877f916b976945d",
    "1 MB",
);
static DOCKERFILE: Kit = plugin(
    "Dockerfile",
    "Dockerfile",
    "dockerfile.wasm",
    "plugin-dockerfile-0.6.0",
    "https://plugins.dprint.dev/dockerfile-0.6.0.wasm",
    "412f8b9329fd2ad7fbf21cd5ef0619a891cd47a39aa9fe900708f33108e1bd42",
    "1 MB",
);
static MAGO: Kit = plugin(
    "Mago",
    "PHP",
    "mago.wasm",
    "plugin-mago-0.29.0",
    "https://plugins.dprint.dev/mago-0.29.0.wasm",
    "d3565d94cd504a171fc0c4454f1bb54ad79bb55681f33e692940bed6b6a736c3",
    "2 MB",
);
static SQL: Kit = plugin(
    "lax-sql",
    "SQL",
    "sql.wasm",
    "plugin-sql-0.3.0",
    "https://plugins.dprint.dev/bartlomieju/lax-sql-0.3.0.wasm",
    "c21281b3031d20182f34fed537d0695a4ffa7e705374c00ecb18d6194908da27",
    "1 MB",
);
static MARKUP: Kit = plugin(
    "markup_fmt",
    "XML, SVG",
    "markup.wasm",
    "plugin-markup-0.27.5",
    "https://plugins.dprint.dev/g-plane/markup_fmt-v0.27.5.wasm",
    "64baa23c20ff51becba26f0203bd2ff193983a4315e7f722df705541bb9d0f76",
    "2 MB",
);
static CMAKE: Kit = plugin(
    "cmakefmt",
    "CMake",
    "cmake.wasm",
    "plugin-cmake-0.1.0",
    "https://plugins.dprint.dev/sargunv/dprint-cmakefmt-0.1.0.wasm",
    "8d0415e5b06f0cd1fb3c3fca1aa90aee1f45aadae239fb5b92c56014dd56c195",
    "1 MB",
);

/// No Arm build: Windows on Arm runs the x64 one.
static STYLUA: Kit = Kit {
    name: "StyLua",
    formats: "Lua",
    file: "stylua.exe",
    dir: "stylua-2.5.2",
    url: "https://github.com/JohnnyMorganz/StyLua/releases/download/v2.5.2/stylua-windows-x86_64.zip",
    sha256: "e77d0ea1226b8b389b43f702240091249a96eea25857281f90ea24d0eb9eb969",
    kind: Kind::Zip,
    size: "3 MB",
};

/// The native build, which needs no Java.
static JAVA_FORMAT: Kit = Kit {
    name: "google-java-format",
    formats: "Java",
    file: "google-java-format.exe",
    dir: "google-java-format-1.37.0",
    url: "https://github.com/google/google-java-format/releases/download/v1.37.0/google-java-format_windows-x86-64.exe",
    sha256: "48260bed87f6830bae44a7a27f66ce98f7d9fb245f500021bd7ae16c9e2daa06",
    kind: Kind::Program,
    size: "33 MB",
};

/// The module from the PowerShell Gallery, imported by its manifest.
static PSSCRIPTANALYZER: Kit = Kit {
    name: "PSScriptAnalyzer",
    formats: "PowerShell",
    file: "PSScriptAnalyzer.psd1",
    dir: "PSScriptAnalyzer-1.25.0",
    url: "https://www.powershellgallery.com/api/v2/package/PSScriptAnalyzer/1.25.0",
    sha256: "14e634c828eb98efb9f40b2918ba90f139ed5eccdf663a2a747736d996995d60",
    kind: Kind::Zip,
    size: "15 MB",
};

impl Kit {
    pub fn version(&self) -> &'static str {
        self.dir.rsplit_once('-').map_or("", |(_, version)| version)
    }

    pub fn is_installed(&self) -> bool {
        installed(self).is_some()
    }

    /// The download it takes to use this: its own, and dprint's for a plugin
    /// while dprint is missing.
    pub fn download_size(&self) -> String {
        if self.kind == Kind::Plugin && !DPRINT.is_installed() { format!("{} + dprint {}", self.size, DPRINT.size) } else { self.size.to_string() }
    }
}

/// Everything den can download, for Settings.
pub fn kits() -> &'static [&'static Kit] {
    static KITS: [&Kit; 15] = [
        &PRETTIER_PLUGIN,
        &RUFF,
        &GOFUMPT,
        &SHFMT,
        &CLANG_FORMAT,
        &TOML,
        &DOCKERFILE,
        &MAGO,
        &SQL,
        &MARKUP,
        &CMAKE,
        &STYLUA,
        &JAVA_FORMAT,
        &PSSCRIPTANALYZER,
        &DPRINT,
    ];
    if cfg!(windows) { &KITS } else { &[] }
}

/// The dprint plugin for `path`, and the file name its text goes by: dprint
/// picks the language by the name, and the Prettier plugin knows no `.htm`,
/// say.
fn plugin_for(path: &Path) -> Option<(&'static Kit, String)> {
    let file = path.file_name()?.to_str()?.to_lowercase();
    let ext = extension(path);
    if file == "dockerfile" || file == "containerfile" || file.starts_with("dockerfile.") || ext == "dockerfile" {
        return Some((&DOCKERFILE, "Dockerfile".into()));
    }
    if file == "cmakelists.txt" {
        return Some((&CMAKE, "CMakeLists.cmake".into()));
    }
    let (kit, ext) = match ext.as_str() {
        "htm" => (&PRETTIER_PLUGIN, "html"),
        "markdown" => (&PRETTIER_PLUGIN, "md"),
        "jsonc" | "json5" => (&PRETTIER_PLUGIN, "json"),
        "hbs" | "handlebars" => return None,
        e if PRETTIER.contains(&e) => (&PRETTIER_PLUGIN, e),
        e @ ("py" | "pyi") => (&RUFF, e),
        "go" => (&GOFUMPT, "go"),
        e if SHELL.contains(&e) => (&SHFMT, e),
        e if C_FAMILY.contains(&e) => (&CLANG_FORMAT, e),
        "toml" => (&TOML, "toml"),
        "php" => (&MAGO, "php"),
        "sql" => (&SQL, "sql"),
        e if XML.contains(&e) => (&MARKUP, e),
        "cmake" => (&CMAKE, "cmake"),
        _ => return None,
    };
    Some((kit, format!("file.{ext}")))
}

/// What den can download for `path`, when nothing installed formats it.
pub fn kit_for(path: &Path) -> Option<&'static Kit> {
    if !cfg!(windows) {
        return None;
    }
    if let Some((kit, _)) = plugin_for(path) {
        return Some(kit);
    }
    match extension(path).as_str() {
        "lua" => Some(&STYLUA),
        "java" => Some(&JAVA_FORMAT),
        e if POWERSHELL_FILES.contains(&e) => Some(&PSSCRIPTANALYZER),
        _ => None,
    }
}

fn tools_dir() -> PathBuf {
    crate::settings::data_dir().join("tools")
}

/// dprint's cache (compiled plugins, the Prettier program), den's own.
fn dprint_cache() -> PathBuf {
    tools_dir().join("dprint-cache")
}

/// The kit's file, once downloaded.
fn installed(kit: &Kit) -> Option<PathBuf> {
    super::find_file(&tools_dir().join(kit.dir), kit.file, 4)
}

/// Download and unpack `kit` (with dprint for a plugin), unless it is there
/// already. Resumable.
pub fn install(kit: &'static Kit, cancel: &Cancel, progress: Progress) -> Result<(), String> {
    if kit.kind == Kind::Plugin {
        install(&DPRINT, cancel, progress)?;
    }
    if installed(kit).is_some() {
        return Ok(());
    }
    let root = tools_dir();
    let dest = root.join(kit.dir);
    if kit.kind == Kind::Zip {
        let archive = root.join("downloads").join(format!("{}.zip", kit.dir));
        http::download(kit.url, &archive, Some(kit.sha256), kit.name, cancel, progress)?;
        let tmp = root.join(format!("{}.tmp", kit.dir));
        process::unpack(&archive, &tmp, kit.name)?;
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::rename(&tmp, &dest).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(&archive);
    } else {
        let file = dest.join(kit.file);
        let part = root.join("downloads").join(kit.file);
        http::download(kit.url, &part, Some(kit.sha256), kit.name, cancel, progress)?;
        std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
        std::fs::rename(&part, &file).map_err(|e| e.to_string())?;
        if kit.kind == Kind::Plugin {
            // A first run compiles the plugin (and fetches a process plugin's
            // program); dprint caches that by where the plugin is.
            progress(kit.name, 0, 0);
            if let Err(err) = warm_up(kit, &file) {
                let _ = remove(kit);
                return Err(err);
            }
        }
    }
    // Other versions, and what earlier dens downloaded, go.
    let keep: Vec<&str> = kits().iter().map(|k| k.dir).chain(["downloads", "dprint-cache", "dprint-config"]).collect();
    for entry in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        if !keep.contains(&entry.file_name().to_string_lossy().as_ref()) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
    installed(kit).map(|_| ()).ok_or_else(|| format!("The {} download has no {}", kit.name, kit.file))
}

/// Run dprint once with the plugin at `file`.
fn warm_up(kit: &'static Kit, file: &Path) -> Result<(), String> {
    let exe = installed(&DPRINT).ok_or("dprint is not installed")?;
    let config = write_dprint_config(kit, file)?;
    let mut cmd = Command::new(exe);
    cmd.args(["fmt", "--stdin", plugin_sample(kit), "--config"]).arg(config).env("DPRINT_CACHE_DIR", dprint_cache());
    let out = output(cmd, "")?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{} does not run: {}", kit.name, String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// A file name of the plugin's language.
fn plugin_sample(kit: &Kit) -> &'static str {
    match kit.file {
        "ruff.wasm" => "file.py",
        "gofumpt.wasm" => "file.go",
        "sh.wasm" => "file.sh",
        "clang-format.wasm" => "file.c",
        "toml.wasm" => "file.toml",
        "dockerfile.wasm" => "Dockerfile",
        "mago.wasm" => "file.php",
        "sql.wasm" => "file.sql",
        "markup.wasm" => "file.xml",
        "cmake.wasm" => "file.cmake",
        _ => "file.ts", // Prettier
    }
}

/// A dprint config with just the plugin at `file` (a process plugin's with
/// its checksum, which dprint demands).
fn write_dprint_config(kit: &Kit, file: &Path) -> Result<PathBuf, String> {
    let dir = tools_dir().join("dprint-config");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut reference = file.to_string_lossy().into_owned();
    if kit.file.ends_with(".json") {
        reference = format!("{reference}@{}", kit.sha256);
    }
    let path = dir.join(format!("{}.json", kit.dir));
    std::fs::write(&path, serde_json::json!({ "plugins": [reference] }).to_string()).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Delete den's download of `kit`; Format Document offers it again when
/// needed. A plugin's compiled copy in dprint's cache goes with it, and all
/// of the cache with dprint.
pub fn remove(kit: &Kit) -> Result<(), String> {
    let dir = tools_dir().join(kit.dir);
    if kit.kind == Kind::Plugin {
        forget_cached(&dir);
    }
    if *kit == DPRINT {
        let _ = std::fs::remove_dir_all(dprint_cache());
    }
    match std::fs::remove_dir_all(&dir) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err.to_string()),
        _ => Ok(()),
    }
}

/// Drop what dprint's cache holds for the plugin in `dir`: each entry's
/// `<id>.json` names its source, next to `<id>.cwasm` or an `<id>` folder.
fn forget_cached(dir: &Path) {
    let plugins = dprint_cache().join("plugins");
    let source = format!("local:{}", dir.to_string_lossy()).to_lowercase();
    for entry in std::fs::read_dir(&plugins).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let from = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|value| value.get("source")?.as_str().map(str::to_lowercase));
        if from.is_some_and(|from| from.starts_with(&source)) {
            let stem = path.with_extension("");
            let _ = std::fs::remove_file(stem.with_extension("cwasm"));
            // A process plugin's program can take a moment to exit after dprint.
            for _ in 0..20 {
                if std::fs::remove_dir_all(&stem).is_ok() || !stem.exists() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            let _ = std::fs::remove_file(&path);
        }
    }
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

/// PowerShell: PSScriptAnalyzer's Invoke-Formatter (den's download when
/// DEN_PSSA names it), with the nearest PSScriptAnalyzerSettings.psd1 above
/// the file, else One True Brace Style.
const POWERSHELL: &str = r#"$ErrorActionPreference = 'Stop'
$utf8 = [Text.UTF8Encoding]::new($false)
[Console]::InputEncoding = $utf8
[Console]::OutputEncoding = $utf8
if ($env:DEN_PSSA) {
  Import-Module $env:DEN_PSSA
} elseif (-not (Get-Module -ListAvailable PSScriptAnalyzer)) {
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
        Tool::Rustfmt(exe) => {
            let mut cmd = Command::new(exe);
            cmd.args(["--edition", &rust_edition(path), "--emit", "stdout"]);
            cmd
        }
        Tool::Ruff(exe) => {
            let mut cmd = Command::new(exe);
            cmd.args(["format", "--stdin-filename", &file, "-"]);
            cmd
        }
        Tool::Black(exe) => {
            let mut cmd = Command::new(exe);
            cmd.args(["-q", "--stdin-filename", &file, "-"]);
            cmd
        }
        Tool::Gofmt(exe) => Command::new(exe),
        Tool::Shfmt(exe) => {
            let mut cmd = Command::new(exe);
            cmd.args(["--filename", &file]);
            cmd
        }
        Tool::ClangFormat(exe) => {
            let mut cmd = Command::new(exe);
            cmd.arg(format!("--assume-filename={file}"));
            cmd
        }
        Tool::Stylua(exe) => {
            let mut cmd = Command::new(exe);
            cmd.args(["--stdin-filepath", &file, "-"]);
            cmd
        }
        Tool::JavaFormat(exe) => {
            let mut cmd = Command::new(exe);
            cmd.arg("-");
            cmd
        }
        Tool::PowerShell(module) => {
            let shell = on_path("pwsh").or_else(|| on_path("powershell")).unwrap_or_else(|| "powershell".into());
            let mut cmd = Command::new(shell);
            cmd.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", &crate::terminal::encode_powershell(POWERSHELL)]);
            cmd.env("DEN_FORMAT_PATH", &file);
            if let Some(module) = module {
                cmd.env("DEN_PSSA", module);
            }
            cmd
        }
        Tool::Dprint { exe, plugin, name } => {
            let plugin_file = installed(plugin).ok_or_else(|| format!("{} is not installed", plugin.name))?;
            let config = write_dprint_config(plugin, &plugin_file)?;
            let mut cmd = Command::new(exe);
            cmd.args(["fmt", "--stdin", name, "--config"]).arg(config).env("DPRINT_CACHE_DIR", dprint_cache());
            cmd
        }
    };
    if let Some(dir) = path.parent().filter(|d| d.is_dir()) {
        cmd.current_dir(dir);
    }
    let out = output(cmd, text).map_err(|err| format!("cannot start {}: {err}", tool.name()))?;
    if out.status.success() {
        let formatted = String::from_utf8(out.stdout).map_err(|err| err.to_string())?;
        Ok(keep_line_endings(text, formatted))
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let err = if err.is_empty() { String::from_utf8_lossy(&out.stdout).trim().to_string() } else { err };
        Err(if err.is_empty() { format!("{} failed ({})", tool.name(), out.status) } else { err.lines().take(6).collect::<Vec<_>>().join("\n") })
    }
}

/// Run `cmd` without a window, `text` on its stdin.
fn output(cmd: Command, text: &str) -> Result<std::process::Output, String> {
    process::output_with_input(cmd, Some(text)).map_err(|err| err.to_string())
}

/// The formatted text with the line endings the file had (rustfmt writes
/// Windows ones on Windows, Prettier Unix ones).
fn keep_line_endings(original: &str, formatted: String) -> String {
    let unix = formatted.replace("\r\n", "\n");
    if original.contains("\r\n") { unix.replace('\n', "\r\n") } else { unix }
}

#[cfg(test)]
mod tests {
    use super::{expand_env, keep_line_endings, plugin_for, rust_edition, tool_for};
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

    #[test]
    fn registry_strings_expand_known_variables() {
        let path = std::env::var("PATH").unwrap();
        assert_eq!(expand_env("%PATH%\\bin"), format!("{path}\\bin"));
        assert_eq!(expand_env("100% %DEN_NO_SUCH_VAR% 5%"), "100% %DEN_NO_SUCH_VAR% 5%");
        assert_eq!(expand_env("%%"), "%%");
    }

    #[test]
    fn plugins_by_file_name() {
        let name = |p: &str| plugin_for(Path::new(p)).map(|(kit, name)| (kit.name, name));
        assert_eq!(name("src/App.svelte"), Some(("Prettier", "file.svelte".into())));
        assert_eq!(name("index.HTM"), Some(("Prettier", "file.html".into())));
        assert_eq!(name("Dockerfile"), Some(("Dockerfile", "Dockerfile".into())));
        assert_eq!(name("CMakeLists.txt"), Some(("cmakefmt", "CMakeLists.cmake".into())));
        assert_eq!(name("main.cpp"), Some(("clang-format", "file.cpp".into())));
        assert_eq!(name("page.hbs"), None);
    }
}
