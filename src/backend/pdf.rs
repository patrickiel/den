//! PDF documents through PDFium, Google's PDF library (Chrome's): its build
//! from `bblanchon/pdfium-binaries`, downloaded into den's data folder the
//! first time a PDF opens, and bound at run time.
//!
//! PDFium is not made for many threads, so one thread of den's has it and
//! every open document: tabs send it requests (open, draw a page, search)
//! and await the replies.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Mutex, mpsc},
};

use futures::channel::oneshot;
use pdfium_render::prelude::*;

use super::http::{self, Cancel, Progress};

/// The PDFium build, the one `pdfium-render`'s `pdfium_latest` binds.
const VERSION: &str = "7881";

/// The archive for this platform, and its SHA-256.
const ARCHIVE: (&str, &str) = if cfg!(windows) {
    (
        "pdfium-win-x64.tgz",
        "73cc0de638ac2095e7445bf56a38200a5b7c7ca0e9f4ba144598f2457377ac08",
    )
} else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    (
        "pdfium-mac-arm64.tgz",
        "52e94ca5aa8847934330daf3f8150c190682c5ca93831468794f8b90d4392e40",
    )
} else {
    (
        "pdfium-mac-x64.tgz",
        "6dedf83990e0e3d6b7c93c9e7589c5a126b0ae14b7464d76120cff7a26afb18b",
    )
};

/// About how much the download is, for the offer to download it.
pub const SIZE: &str = "3.6 MB";

/// The widest a page is drawn, in pixels; zoomed in further it is scaled up.
pub const MAX_WIDTH: u32 = 4096;

/// Where this PDFium build goes; other builds' folders go on install.
fn dir() -> PathBuf {
    crate::settings::download_dir().join("pdfium").join(VERSION)
}

/// The library, once downloaded.
fn library() -> Option<PathBuf> {
    let lib = if cfg!(windows) { "bin" } else { "lib" };
    let path = Pdfium::pdfium_platform_library_name_at_path(&dir().join(lib));
    path.is_file().then_some(path)
}

pub fn installed() -> bool {
    library().is_some()
}

/// Download and unpack PDFium, unless it is there already. Resumable.
pub fn install(cancel: &Cancel, progress: Progress) -> Result<(), String> {
    if installed() {
        return Ok(());
    }
    let (name, sha256) = ARCHIVE;
    let root = dir().parent().map(Path::to_path_buf).unwrap_or_default();
    let archive = root.join("downloads").join(name);
    let url = format!(
        "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F{VERSION}/{name}"
    );
    http::download(&url, &archive, Some(sha256), "PDFium", cancel, progress)?;
    let tmp = root.join(format!("{VERSION}.tmp"));
    super::process::unpack(&archive, &tmp, "PDFium")?;
    let dest = dir();
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&tmp, &dest).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&archive);
    // Other builds, from earlier dens, go.
    for entry in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        let name = entry.file_name();
        if name != VERSION && name != "downloads" {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
    installed()
        .then_some(())
        .ok_or_else(|| "The PDFium download has no library".to_string())
}

/// An open document, by its number on PDFium's thread.
pub type DocId = u64;

/// A drawn page: its pixels in BGRA rows, as GPUI takes them.
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// A match of a search: its page, and its boxes on it (a match across
/// lines has one per line) in points from the page's top left, as
/// `[left, top, width, height]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Match {
    pub page: usize,
    pub boxes: Vec<[f32; 4]>,
}

/// An opened document: its number and its pages' sizes in points.
pub type Opened = Result<(DocId, Vec<(f32, f32)>), String>;

enum Request {
    Open(PathBuf, oneshot::Sender<Opened>),
    Render(DocId, usize, u32, oneshot::Sender<Option<Bitmap>>),
    Search(DocId, String, bool, oneshot::Sender<Vec<Match>>),
    Close(DocId),
}

/// The way to PDFium's thread, once it runs.
static THREAD: Mutex<Option<mpsc::Sender<Request>>> = Mutex::new(None);

/// Send `request` to PDFium's thread, started now if it is not running.
fn send(request: Request) -> Result<(), String> {
    let mut thread = THREAD.lock().unwrap_or_else(|e| e.into_inner());
    if thread.is_none() {
        *thread = Some(start()?);
    }
    let sender = thread.as_ref().ok_or("PDFium is not running")?;
    sender
        .send(request)
        .map_err(|_| "PDFium stopped".to_string())
}

/// Bind PDFium on a thread of its own, which then serves the requests.
fn start() -> Result<mpsc::Sender<Request>, String> {
    let library = library().ok_or("PDFium is not installed")?;
    let (tx, rx) = mpsc::channel::<Request>();
    let (bound_tx, bound_rx) = mpsc::channel::<Result<(), String>>();
    std::thread::Builder::new()
        .name("pdfium".into())
        .spawn(move || {
            let pdfium = match Pdfium::bind_to_library(&library) {
                Ok(bindings) => Pdfium::new(bindings),
                Err(err) => {
                    _ = bound_tx.send(Err(format!("Cannot load PDFium: {err}")));
                    return;
                }
            };
            _ = bound_tx.send(Ok(()));
            serve(&pdfium, rx);
        })
        .map_err(|e| e.to_string())?;
    bound_rx
        .recv()
        .map_err(|_| "PDFium did not start".to_string())??;
    Ok(tx)
}

/// Answer the requests until den quits.
fn serve(pdfium: &Pdfium, requests: mpsc::Receiver<Request>) {
    let mut docs: HashMap<DocId, PdfDocument<'_>> = HashMap::new();
    let mut next: DocId = 0;
    for request in requests {
        match request {
            Request::Open(path, reply) => {
                let opened = pdfium
                    .load_pdf_from_file(&path, None)
                    .map_err(|err| match err {
                        PdfiumError::PdfiumLibraryInternalError(
                            PdfiumInternalError::PasswordError,
                        ) => "This PDF is protected by a password.".to_string(),
                        err => format!("Cannot open this PDF: {err}"),
                    })
                    .and_then(|doc| {
                        let sizes = doc
                            .pages()
                            .page_sizes()
                            .map_err(|err| format!("Cannot read this PDF: {err}"))?
                            .iter()
                            .map(|rect| (rect.width().value, rect.height().value))
                            .collect();
                        Ok((doc, sizes))
                    });
                _ = reply.send(opened.map(|(doc, sizes)| {
                    next += 1;
                    docs.insert(next, doc);
                    (next, sizes)
                }));
            }
            Request::Render(id, page, width, reply) => {
                let bitmap = docs.get(&id).and_then(|doc| render(doc, page, width));
                _ = reply.send(bitmap);
            }
            Request::Search(id, query, match_case, reply) => {
                let matches = docs
                    .get(&id)
                    .map(|doc| search(doc, &query, match_case))
                    .unwrap_or_default();
                _ = reply.send(matches);
            }
            Request::Close(id) => {
                docs.remove(&id);
            }
        }
    }
}

fn render(doc: &PdfDocument, page: usize, width: u32) -> Option<Bitmap> {
    let page = doc.pages().get(page.try_into().ok()?).ok()?;
    let config = PdfRenderConfig::new()
        .set_target_width(width.clamp(1, MAX_WIDTH) as i32)
        .set_format(PdfBitmapFormat::BGRA)
        .render_form_data(true);
    let bitmap = page.render_with_config(&config).ok()?;
    let (width, height) = (bitmap.width() as u32, bitmap.height() as u32);
    let raw = bitmap.as_raw_bytes();
    let row = width as usize * 4;
    let stride = raw.len() / height.max(1) as usize;
    // Rows padded past their pixels lose the padding.
    let bgra = if stride == row {
        raw
    } else {
        raw.chunks(stride)
            .flat_map(|line| &line[..row])
            .copied()
            .collect()
    };
    Some(Bitmap {
        width,
        height,
        bgra,
    })
}

fn search(doc: &PdfDocument, query: &str, match_case: bool) -> Vec<Match> {
    let options = PdfSearchOptions::new().match_case(match_case);
    let mut matches = Vec::new();
    for (ix, page) in doc.pages().iter().enumerate() {
        let height = page.height().value;
        let Ok(text) = page.text() else { continue };
        let Ok(found) = text.search(query, &options) else {
            continue;
        };
        for segments in found.iter(PdfSearchDirection::SearchForward) {
            let boxes = segments
                .iter()
                .map(|segment| {
                    let rect = segment.bounds();
                    [
                        rect.left().value,
                        height - rect.top().value,
                        rect.width().value,
                        rect.height().value,
                    ]
                })
                .collect();
            matches.push(Match { page: ix, boxes });
        }
    }
    matches
}

/// Open the PDF at `path`.
pub async fn open(path: PathBuf) -> Opened {
    let (tx, rx) = oneshot::channel();
    send(Request::Open(path, tx))?;
    rx.await.map_err(|_| "PDFium stopped".to_string())?
}

/// Draw page `page` (from 0) `width` pixels wide.
pub async fn render_page(doc: DocId, page: usize, width: u32) -> Option<Bitmap> {
    let (tx, rx) = oneshot::channel();
    send(Request::Render(doc, page, width, tx)).ok()?;
    rx.await.ok().flatten()
}

/// Every match of `query` in the document, page by page.
pub async fn find(doc: DocId, query: String, match_case: bool) -> Vec<Match> {
    let (tx, rx) = oneshot::channel();
    if send(Request::Search(doc, query, match_case, tx)).is_err() {
        return Vec::new();
    }
    rx.await.unwrap_or_default()
}

pub fn close(doc: DocId) {
    _ = send(Request::Close(doc));
}
