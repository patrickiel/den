//! Local AI for commit messages, as in den: on first use a pinned llama.cpp
//! build and a small GGUF model are downloaded into `%APPDATA%\den\ai`;
//! `llama-server` then runs on a private localhost port while it is needed
//! (stopped after a few idle minutes and on exit), and serves OpenAI-style
//! chat requests with token streaming and cancellation.

use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};

const SERVER_EXE: &str = if cfg!(windows) { "llama-server.exe" } else { "llama-server" };
/// The llama.cpp release the runtime is pinned to; bump with the hashes below.
const LLAMA_TAG: &str = "b11222";
const DEFAULT_MODEL_URL: &str = "https://huggingface.co/Qwen/Qwen2.5-Coder-1.5B-Instruct-GGUF/resolve/main/qwen2.5-coder-1.5b-instruct-q4_k_m.gguf";
const DEFAULT_MODEL_SHA256: &str = "cc324af070c2ecbfd324a30884d2f951a7ff756aba85cb811a6ec436933bb046";
const IDLE_KILL: Duration = Duration::from_secs(5 * 60);
/// Generous: a large model read from a slow disk takes minutes the first time.
const START_TIMEOUT: Duration = Duration::from_secs(300);
pub const CANCELLED: &str = "cancelled";

/// The release asset for this platform and its SHA-256. The GPU builds also
/// carry the CPU backends, so one download serves both.
fn runtime_asset() -> Option<(&'static str, &'static str)> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => ("llama-b11222-bin-win-vulkan-x64.zip", "e115f6c2158dd0f4fb99449b9c6a31a346b006853470826360190204c5c9c03e"),
        ("windows", "aarch64") => ("llama-b11222-bin-win-cpu-arm64.zip", "ba5a0f2522a3f0d27862b4641e84f46ed5cdde4fbda4fa7816026b5744cbd660"),
        ("macos", "aarch64") => ("llama-b11222-bin-macos-arm64.tar.gz", "869b73f760042ac660e9453ff5e5cbb157725f2f8e01ec473006ffcf387a8410"),
        ("macos", "x86_64") => ("llama-b11222-bin-macos-x64.tar.gz", "babd362dbd845c41cc52f5b8dd6b1304a9b9fc9a7dc75bbbb5a85d2b669b4478"),
        ("linux", "x86_64") => ("llama-b11222-bin-ubuntu-vulkan-x64.tar.gz", "b4ea118522b8a5eda9a599eecfb288e8f4cd5f660de6847cfcb7dee2f800fe03"),
        ("linux", "aarch64") => ("llama-b11222-bin-ubuntu-vulkan-arm64.tar.gz", "070941a0da53f369b05a7b7afd4485cc03988edb7c282693fdd224550c628f08"),
        _ => return None,
    })
}

/// A ready-made model: a single-file Q4_K_M GGUF, downloaded once.
pub struct ModelPreset {
    pub name: &'static str,
    /// Empty for the shipped default.
    pub url: &'static str,
    pub size: &'static str,
    /// Where it runs well.
    pub fits: &'static str,
    /// The context that still fits in that memory next to the weights.
    pub context: u32,
}

/// den's catalogue, smallest first.
pub const MODEL_PRESETS: &[ModelPreset] = &[
    ModelPreset { name: "Qwen2.5-Coder 1.5B", url: "", size: "1.1 GB", fits: "any PC", context: 8192 },
    ModelPreset {
        name: "Qwen3.5 2B",
        url: "https://huggingface.co/unsloth/Qwen3.5-2B-GGUF/resolve/main/Qwen3.5-2B-Q4_K_M.gguf",
        size: "1.3 GB",
        fits: "any PC",
        context: 16384,
    },
    ModelPreset {
        name: "Qwen3.5 4B",
        url: "https://huggingface.co/unsloth/Qwen3.5-4B-GGUF/resolve/main/Qwen3.5-4B-Q4_K_M.gguf",
        size: "2.7 GB",
        fits: "4 GB GPU or 8 GB RAM",
        context: 16384,
    },
    ModelPreset {
        name: "Qwen3.5 9B",
        url: "https://huggingface.co/unsloth/Qwen3.5-9B-GGUF/resolve/main/Qwen3.5-9B-Q4_K_M.gguf",
        size: "5.7 GB",
        fits: "8 GB GPU or 16 GB RAM",
        context: 32768,
    },
    ModelPreset {
        name: "Qwen3.6 27B",
        url: "https://huggingface.co/unsloth/Qwen3.6-27B-GGUF/resolve/main/Qwen3.6-27B-Q4_K_M.gguf",
        size: "16.8 GB",
        fits: "24 GB GPU",
        context: 16384,
    },
    ModelPreset {
        name: "Qwen3.6 35B-A3B",
        url: "https://huggingface.co/unsloth/Qwen3.6-35B-A3B-GGUF/resolve/main/Qwen3.6-35B-A3B-UD-Q4_K_M.gguf",
        size: "22.1 GB",
        fits: "32 GB RAM",
        context: 32768,
    },
];

/// The shipped commit message style, for repositories without their own
/// `.den/commit-style.md`.
pub const DEFAULT_COMMIT_STYLE: &str = "Write ONE commit message for the diff below.

Format: <type>(<scope>): <summary>
- type: feat | fix | refactor | perf | docs | test | chore | build | ci
- summary: ≤ 50 chars, imperative, lowercase, no period
- Add a body (≤ 2 lines) only if the \"why\" isn't obvious
- Describe what the change does, not how the code looks
- Don't invent context not in the diff
- Output only the message, nothing else

DIFF:
{{diff}}";

/// The AI settings a generation runs with (from Settings).
#[derive(Clone, Debug, PartialEq)]
pub struct AiConfig {
    /// Empty for the default model, a download URL, or a local .gguf path.
    pub model: String,
    pub context: u32,
    pub threads: u32,
    pub gpu: bool,
    /// The global commit style.
    pub style: String,
    /// Derive a repository's style from its history on first use.
    pub derive_style: bool,
}

fn ai_dir() -> PathBuf {
    crate::settings::data_dir().join("ai")
}

fn runtime_dir() -> PathBuf {
    ai_dir().join("runtime").join(LLAMA_TAG)
}

/// The model file for `model`, the URL to fetch it from, and its checksum
/// when known.
fn model_spec(model: &str) -> (PathBuf, Option<String>, Option<&'static str>) {
    let models = ai_dir().join("models");
    let model = model.trim();
    if model.is_empty() {
        let name = DEFAULT_MODEL_URL.rsplit('/').next().unwrap_or("model.gguf");
        return (models.join(name), Some(DEFAULT_MODEL_URL.into()), Some(DEFAULT_MODEL_SHA256));
    }
    if model.starts_with("http://") || model.starts_with("https://") {
        let clean = model.split(['?', '#']).next().unwrap_or(model);
        let name: String = clean
            .rsplit('/')
            .next()
            .filter(|n| !n.is_empty())
            .unwrap_or("model.gguf")
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' })
            .collect();
        return (models.join(name), Some(model.into()), None);
    }
    (PathBuf::from(model), None, None)
}

fn model_path(model: &str) -> Option<PathBuf> {
    Some(model_spec(model).0)
}

/// What is installed.
pub struct Status {
    pub runtime: bool,
    pub model: bool,
}

pub fn status(config: &AiConfig) -> Status {
    Status { runtime: runtime_dir().join(SERVER_EXE).is_file(), model: model_spec(&config.model).0.is_file() }
}

/// Download progress: what, bytes done, bytes in all (0 when unknown).
pub type Progress<'a> = &'a mut dyn FnMut(&'static str, u64, u64);

/// Download whatever is missing: the runtime, then the model. Resumable.
pub fn install(config: &AiConfig, cancel: &Cancel, progress: Progress) -> Result<(), String> {
    let runtime = runtime_dir();
    if !runtime.join(SERVER_EXE).is_file() {
        let (asset, sha) = runtime_asset().ok_or("Local AI is not available for this platform.")?;
        let url = format!("https://github.com/ggml-org/llama.cpp/releases/download/{LLAMA_TAG}/{asset}");
        let archive = ai_dir().join("downloads").join(asset);
        download(&url, &archive, Some(sha), "AI runtime", cancel, progress)?;
        progress("Unpacking", 0, 0);
        extract_runtime(&archive, &runtime)?;
        let _ = std::fs::remove_file(&archive);
    }
    let (path, url, sha) = model_spec(&config.model);
    if !path.is_file() {
        let url = url.ok_or_else(|| format!("Model file not found: {}", path.display()))?;
        download(&url, &path, sha, "AI model", cancel, progress)?;
    }
    Ok(())
}

/// Stream `url` into `dest` through `dest.part`, resuming a partial download.
fn download(url: &str, dest: &Path, sha256: Option<&str>, stage: &'static str, cancel: &Cancel, progress: Progress) -> Result<(), String> {
    if dest.is_file() {
        return Ok(());
    }
    std::fs::create_dir_all(dest.parent().ok_or("bad download path")?).map_err(|e| e.to_string())?;
    let part = dest.with_file_name(format!("{}.part", dest.file_name().unwrap_or_default().to_string_lossy()));
    let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(20)).timeout_read(Duration::from_secs(60)).build();
    let mut offset = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    let mut request = agent.get(url);
    if offset > 0 {
        request = request.set("Range", &format!("bytes={offset}-"));
    }
    let response = match request.call() {
        Ok(response) => response,
        Err(ureq::Error::Status(416, _)) => {
            // The partial file is complete or stale: start over.
            let _ = std::fs::remove_file(&part);
            offset = 0;
            agent.get(url).call().map_err(|e| format!("Download failed: {e}"))?
        }
        Err(e) => return Err(format!("Download failed: {e}")),
    };
    if response.status() != 206 {
        offset = 0; // the server ignored the range
    }
    let len: u64 = response.header("Content-Length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let total = if len > 0 { len + offset } else { 0 };
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(offset > 0)
        .truncate(offset == 0)
        .open(&part)
        .map_err(|e| e.to_string())?;
    let mut reader = response.into_reader();
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = offset;
    let mut last = Instant::now() - Duration::from_secs(1);
    loop {
        cancel.check()?;
        let n = reader.read(&mut buf).map_err(|e| format!("Download interrupted: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        if last.elapsed() >= Duration::from_millis(200) {
            last = Instant::now();
            progress(stage, done, total);
        }
    }
    file.flush().map_err(|e| e.to_string())?;
    drop(file);
    if total > 0 && done < total {
        return Err("Download interrupted; try again to resume.".into());
    }
    if let Some(want) = sha256 {
        progress("Checking", 0, 0);
        let got = sha256_file(&part, cancel)?;
        if !got.eq_ignore_ascii_case(want) {
            let _ = std::fs::remove_file(&part);
            return Err(format!("The downloaded file is corrupt (checksum mismatch): {url}"));
        }
    }
    std::fs::rename(&part, dest).map_err(|e| e.to_string())
}

fn sha256_file(path: &Path, cancel: &Cancel) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        cancel.check()?;
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Unpack with the system tar (bsdtar on Windows 10+ reads zip too), then
/// move the folder holding `llama-server` to `dest`. Older runtimes go.
fn extract_runtime(archive: &Path, dest: &Path) -> Result<(), String> {
    let root = dest.parent().ok_or("bad runtime path")?;
    let tmp = root.join(format!("{LLAMA_TAG}.tmp"));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let tar = if cfg!(windows) {
        let sysroot = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        PathBuf::from(sysroot).join("System32").join("tar.exe")
    } else {
        PathBuf::from("tar")
    };
    let mut cmd = Command::new(tar);
    cmd.arg(if cfg!(windows) { "-xf" } else { "-xzf" }).arg(archive).arg("-C").arg(&tmp);
    no_window(&mut cmd);
    let out = cmd.output().map_err(|e| format!("Cannot run tar: {e}"))?;
    if !out.status.success() {
        return Err(format!("Unpacking the AI runtime failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let bin = find_file(&tmp, SERVER_EXE, 3).ok_or("The AI runtime archive has no llama-server")?;
    let src = bin.parent().ok_or("bad archive layout")?.to_path_buf();
    let _ = std::fs::remove_dir_all(dest);
    std::fs::rename(&src, dest).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&tmp);
    for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
        if entry.file_name() != LLAMA_TAG {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
    Ok(())
}

fn find_file(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
    let candidate = dir.join(name);
    if candidate.is_file() {
        return Some(candidate);
    }
    if depth == 0 {
        return None;
    }
    std::fs::read_dir(dir).ok()?.flatten().filter(|e| e.path().is_dir()).find_map(|e| find_file(&e.path(), name, depth - 1))
}


// -- Server ------------------------------------------------------------------

#[derive(Clone, PartialEq)]
struct ServerKey {
    model: PathBuf,
    context: u32,
    threads: u32,
    gpu: bool,
}

struct Server {
    child: Child,
    port: u16,
    key: ServerKey,
    gpu: bool,
    ready: bool,
    deadline: Instant,
    log: PathBuf,
}

impl Server {
    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

static SERVER: Mutex<Option<Server>> = Mutex::new(None);
static LAST_USED: Mutex<Option<Instant>> = Mutex::new(None);
static REAPER: std::sync::Once = std::sync::Once::new();
/// Models the GPU could not run here; they start on the CPU.
static GPU_BROKEN: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

fn touch() {
    if let Ok(mut last) = LAST_USED.lock() {
        *last = Some(Instant::now());
    }
}

/// Stop the server (on exit).
pub fn stop() {
    if let Some(server) = SERVER.lock().ok().and_then(|mut s| s.take()) {
        server.kill();
    }
}

/// A thread that stops the server once nothing used it for a while.
fn start_reaper() {
    REAPER.call_once(|| {
        std::thread::spawn(|| {
            loop {
                std::thread::sleep(Duration::from_secs(30));
                let idle = LAST_USED.lock().ok().and_then(|l| *l).is_none_or(|t| t.elapsed() > IDLE_KILL);
                if idle && let Ok(mut slot) = SERVER.try_lock()
                    && let Some(server) = slot.take()
                {
                    server.kill();
                }
            }
        });
    });
}

fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

fn spawn(key: &ServerKey, gpu: bool) -> Result<Server, String> {
    let runtime = runtime_dir();
    let exe = runtime.join(SERVER_EXE);
    if !exe.is_file() {
        return Err("The AI runtime is not installed.".into());
    }
    let port = TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map_err(|e| e.to_string())?.port();
    let log = ai_dir().join("server.log");
    let _ = std::fs::create_dir_all(ai_dir());
    let log_file = File::create(&log).map_err(|e| e.to_string())?;
    let mut cmd = Command::new(&exe);
    cmd.current_dir(&runtime)
        .arg("-m")
        .arg(&key.model)
        .args(["-c", &key.context.to_string()])
        .args(["--host", "127.0.0.1", "--port", &port.to_string()])
        .args(["-np", "1", "--no-webui", "--jinja"])
        .args(["-ngl", if gpu { "99" } else { "0" }])
        .args(["-sm", "none"])
        .stdin(Stdio::null())
        .stdout(log_file.try_clone().map_err(|e| e.to_string())?)
        .stderr(log_file);
    if key.threads > 0 {
        cmd.args(["-t", &key.threads.to_string()]);
    }
    no_window(&mut cmd);
    let child = cmd.spawn().map_err(|e| format!("Cannot start llama-server: {e}"))?;
    Ok(Server { child, port, key: key.clone(), gpu, ready: false, deadline: Instant::now() + START_TIMEOUT, log })
}

fn log_tail(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(8)..].join("\n")
}

fn wait_ready(server: &mut Server, cancel: &Cancel) -> Result<(), String> {
    while !server.ready {
        cancel.check()?;
        if !server.alive() {
            return Err(format!("llama-server exited while starting:\n{}", log_tail(&server.log)));
        }
        if matches!(http(server.port, "GET", "/health", None, None), Ok((200, _))) {
            server.ready = true;
            break;
        }
        if Instant::now() > server.deadline {
            return Err(format!("llama-server did not become ready in time:\n{}", log_tail(&server.log)));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

/// The running server for `key`, started (on the GPU, else the CPU) if needed.
fn ensure_server(key: ServerKey, cancel: &Cancel) -> Result<u16, String> {
    start_reaper();
    let mut slot = SERVER.lock().map_err(|e| e.to_string())?;
    let reuse = slot.as_mut().is_some_and(|s| s.key == key && s.alive());
    if !reuse {
        if let Some(old) = slot.take() {
            old.kill();
        }
        let gpu = key.gpu && !GPU_BROKEN.lock().is_ok_and(|b| b.contains(&key.model));
        *slot = Some(spawn(&key, gpu)?);
    }
    let server = slot.as_mut().expect("just filled");
    match wait_ready(server, cancel) {
        Ok(()) => Ok(server.port),
        Err(err) if err == CANCELLED => Err(err),
        Err(err) => {
            let failed = slot.take().expect("just filled");
            let was_gpu = failed.gpu;
            failed.kill();
            if !was_gpu {
                return Err(err);
            }
            // The card cannot run this model: remember, and retry on the CPU.
            if let Ok(mut broken) = GPU_BROKEN.lock() {
                broken.push(key.model.clone());
            }
            let mut cpu = spawn(&key, false)?;
            let started = wait_ready(&mut cpu, cancel);
            let port = cpu.port;
            *slot = Some(cpu);
            started.map(|()| port).map_err(|e| if e == CANCELLED { e } else { format!("{e}\n(GPU start failed first: {err})") })
        }
    }
}

// -- Requests ----------------------------------------------------------------

/// Stops a request: polled between tokens, and the socket is shut so a read
/// blocked on prompt processing returns at once.
#[derive(Clone, Default)]
pub struct Cancel {
    flag: Arc<AtomicBool>,
    stream: Arc<Mutex<Option<TcpStream>>>,
}

impl Cancel {
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        if let Some(stream) = self.stream.lock().ok().and_then(|s| s.as_ref().and_then(|s| s.try_clone().ok())) {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    pub fn check(&self) -> Result<(), String> {
        if self.is_cancelled() { Err(CANCELLED.into()) } else { Ok(()) }
    }
}

/// One chat request; `on_token` gets each piece as it streams in.
pub fn chat(config: &AiConfig, mut body: serde_json::Value, cancel: &Cancel, mut on_token: impl FnMut(&str)) -> Result<String, String> {
    touch();
    let model = model_path(&config.model).filter(|p| p.is_file()).ok_or("The AI model is not installed.")?;
    let key = ServerKey { model, context: config.context, threads: config.threads, gpu: config.gpu };
    let port = ensure_server(key, cancel)?;
    body["stream"] = serde_json::Value::Bool(true);
    let payload = serde_json::to_vec(&body).map_err(|e| e.to_string())?;
    let or_cancelled = |e: String| if cancel.is_cancelled() { CANCELLED.to_string() } else { e };
    let (status, mut reader) = http(port, "POST", "/v1/chat/completions", Some(&payload), Some(cancel)).map_err(or_cancelled)?;
    if status != 200 {
        let mut text = String::new();
        let _ = reader.read_to_string(&mut text);
        let message = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
            .unwrap_or_else(|| text.trim().to_string());
        return Err(format!("AI server error {status}: {message}"));
    }
    let mut out = String::new();
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).map_err(|e| or_cancelled(e.to_string()))?;
        cancel.check()?;
        if n == 0 {
            break;
        }
        let Some(data) = line.trim().strip_prefix("data:") else { continue };
        let data = data.trim();
        if data == "[DONE]" {
            break;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else { continue };
        if let Some(err) = value.get("error") {
            return Err(format!("AI server error: {}", err["message"].as_str().unwrap_or("unknown")));
        }
        if let Some(piece) = value["choices"][0]["delta"]["content"].as_str()
            && !piece.is_empty()
        {
            out.push_str(piece);
            on_token(piece);
        }
    }
    touch();
    Ok(out)
}

type Body = Box<dyn BufRead + Send>;

/// A bare HTTP/1.1 request to the server (no client library needed for one
/// localhost peer).
fn http(port: u16, method: &str, path: &str, body: Option<&[u8]>, cancel: Option<&Cancel>) -> Result<(u16, Body), String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    stream.set_nodelay(true).ok();
    match cancel {
        Some(cancel) => {
            if let Ok(mut slot) = cancel.stream.lock() {
                *slot = stream.try_clone().ok();
            }
            cancel.check()?;
        }
        // Health probes must not hang.
        None => _ = stream.set_read_timeout(Some(Duration::from_secs(2))),
    }
    let body = body.unwrap_or_default();
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nAccept: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    // The server may answer (an error) and close before it has read all of a
    // big request: read its answer even when writing failed.
    let written = stream.write_all(head.as_bytes()).and_then(|()| stream.write_all(body));
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if let Err(read) = reader.read_line(&mut line) {
        return Err(match written {
            Err(write) => write.to_string(),
            Ok(()) => read.to_string(),
        });
    }
    let status: u16 = line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).ok_or_else(|| format!("bad HTTP response: {line:?}"))?;
    let (mut chunked, mut length) = (false, None);
    loop {
        line.clear();
        if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            break;
        }
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            let (name, value) = (name.trim().to_ascii_lowercase(), value.trim());
            if name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            } else if name == "content-length" {
                length = value.parse::<u64>().ok();
            }
        }
    }
    let body: Body = if chunked {
        Box::new(BufReader::new(Chunked { inner: reader, left: 0, done: false }))
    } else if let Some(n) = length {
        Box::new(BufReader::new(reader.take(n)))
    } else {
        Box::new(reader)
    };
    Ok((status, body))
}

/// A chunked HTTP body, read as plain bytes.
struct Chunked<R: BufRead> {
    inner: R,
    left: u64,
    done: bool,
}

impl<R: BufRead> Read for Chunked<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        if self.left == 0 {
            let mut line = String::new();
            self.inner.read_line(&mut line)?;
            if line.trim().is_empty() {
                line.clear();
                self.inner.read_line(&mut line)?;
            }
            let size = line.trim().split(';').next().unwrap_or("");
            if size.is_empty() {
                self.done = true;
                return Ok(0);
            }
            self.left = u64::from_str_radix(size, 16).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            if self.left == 0 {
                self.done = true;
                return Ok(0);
            }
        }
        let max = buf.len().min(self.left as usize);
        let n = self.inner.read(&mut buf[..max])?;
        if n == 0 {
            self.done = true;
            return Ok(0);
        }
        self.left -= n as u64;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::{Chunked, model_path};
    use std::io::{BufReader, Read};

    #[test]
    fn reads_chunked_bodies() {
        let raw = b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        let mut body = String::new();
        Chunked { inner: BufReader::new(&raw[..]), left: 0, done: false }.read_to_string(&mut body).unwrap();
        assert_eq!(body, "hello world");
    }

    #[test]
    fn model_files_by_url() {
        let path = model_path("https://huggingface.co/x/resolve/main/Qwen3.5-9B-Q4_K_M.gguf?download=1").unwrap();
        assert!(path.ends_with(r"models\Qwen3.5-9B-Q4_K_M.gguf") || path.ends_with("models/Qwen3.5-9B-Q4_K_M.gguf"));
    }
}
