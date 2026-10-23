//! A PDF file tab: den draws the pages itself, as PDFium renders them
//! (`backend::pdf`, downloaded on the first PDF).
//!
//! The pages scroll as one column, as wide as the pane at 100%, zoomed from
//! there. With Whole Pages, one page shows at a time, all of it, and the
//! wheel and the keys turn the pages. Two Pages puts them side by side, in
//! either. Find (Ctrl+F) highlights every match and goes from one to the
//! next.
//!
//! Only the pages in sight are drawn, at the pane's pixels, and drawn again
//! when their width changes; pages far from sight let their pictures go.

use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use futures::StreamExt as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Escape, Input, InputEvent, InputState, Search},
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::backend::pdf::{self as backend, DocId, Match};

/// The key context of a PDF tab, where Ctrl+F finds in it.
const CONTEXT: &str = "PdfView";
/// The space around the pages, and between them.
const PAD: f32 = 16.;
const GAP: f32 = 12.;
/// How far the wheel goes before Whole Pages turns a page, and how long it
/// then waits (a trackpad's swipe goes on after the page turned).
const TURN: f32 = 40.;
const TURN_PAUSE: Duration = Duration::from_millis(300);
/// The pages kept drawn on each side of those in sight.
const KEEP: usize = 2;
const ZOOMS: (f32, f32) = (0.25, 5.);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-f", Search, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-f", Search, Some(CONTEXT)),
        KeyBinding::new("escape", Escape, Some(CONTEXT)),
    ]);
}

/// Whether PDFium is there, for every PDF tab.
enum Library {
    Missing,
    /// Downloading: bytes done, of how many (0 when not known).
    Downloading(u64, u64),
    Failed(SharedString),
    Ready,
}

struct Pdfium(Library);

impl Global for Pdfium {}

impl Pdfium {
    fn get(cx: &mut App) -> &Library {
        if !cx.has_global::<Self>() {
            let library = if backend::installed() {
                Library::Ready
            } else {
                Library::Missing
            };
            cx.set_global(Self(library));
        }
        &cx.global::<Self>().0
    }

    fn set(library: Library, cx: &mut App) {
        cx.set_global(Self(library));
    }

    /// Download PDFium, for every PDF tab waiting for it.
    fn download(cx: &mut App) {
        if matches!(Self::get(cx), Library::Downloading(..) | Library::Ready) {
            return;
        }
        Self::set(Library::Downloading(0, 0), cx);
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        cx.background_spawn(async move {
            let progress = tx.clone();
            let result = backend::install(
                &crate::backend::http::Cancel::default(),
                &mut |_, done, total| {
                    _ = progress.unbounded_send(Library::Downloading(done, total))
                },
            );
            _ = tx.unbounded_send(match result {
                Ok(()) => Library::Ready,
                Err(err) => Library::Failed(format!("Could not download PDFium: {err}").into()),
            });
        })
        .detach();
        cx.spawn(async move |cx| {
            while let Some(library) = rx.next().await {
                let done = !matches!(library, Library::Downloading(..));
                cx.update(|cx| Self::set(library, cx));
                if done {
                    break;
                }
            }
        })
        .detach();
    }
}

pub struct PdfView {
    path: PathBuf,
    focus_handle: FocusHandle,
    doc: Option<DocId>,
    /// The pages' sizes, in points.
    pages: Vec<(f32, f32)>,
    error: Option<SharedString>,
    /// The pages drawn, each with the width in pixels it was drawn at.
    images: HashMap<usize, (u32, Arc<RenderImage>)>,
    /// Pages being drawn, at the width asked for.
    pending: HashSet<(usize, u32)>,
    /// One whole page at a time, the wheel turning the pages.
    whole_pages: bool,
    /// Pages two side by side (the first and second, the third and fourth…).
    two_pages: bool,
    /// The page showing (with Whole Pages, the first of those showing), or
    /// at the top of the pane.
    page: usize,
    /// Over the pane's width, when the pages scroll.
    zoom: f32,
    scroll: ScrollHandle,
    /// The pane's size, from the last paint.
    viewport: Rc<Cell<Size<Pixels>>>,
    /// The column as last laid out: points to pixels, and each page's top.
    scale: f32,
    tops: Vec<f32>,
    /// Where to scroll the column once it is laid out (see `scroll_to`).
    scroll_to: Option<(usize, f32)>,
    /// The wheel's travel towards a page turn, and when the last one was.
    wheel: (f32, Option<Instant>),
    find: Option<Find>,
    _subscriptions: Vec<Subscription>,
}

struct Find {
    open: bool,
    query: Entity<InputState>,
    match_case: bool,
    matches: Vec<Match>,
    current: Option<usize>,
    _search: Option<Task<()>>,
    _subscription: Subscription,
}

impl PdfView {
    pub fn new(path: PathBuf, cx: &mut Context<Self>) -> Self {
        Pdfium::get(cx);
        let _subscriptions = vec![
            cx.observe_global::<Pdfium>(|this, cx| {
                if matches!(Pdfium::get(cx), Library::Ready) && this.doc.is_none() {
                    this.open(cx);
                }
                cx.notify();
            }),
            // The document closes, and its pages' pictures go, with the tab.
            cx.on_release(|this, cx| this.close(cx)),
        ];
        let mut this = Self {
            path,
            focus_handle: cx.focus_handle(),
            doc: None,
            pages: Vec::new(),
            error: None,
            images: HashMap::new(),
            pending: HashSet::new(),
            whole_pages: crate::settings::AppState::get(cx).pdf_whole_pages,
            two_pages: crate::settings::AppState::get(cx).pdf_two_pages,
            page: 0,
            zoom: 1.,
            scroll: ScrollHandle::new(),
            viewport: Rc::default(),
            scale: 1.,
            tops: Vec::new(),
            scroll_to: None,
            wheel: (0., None),
            find: None,
            _subscriptions,
        };
        if matches!(Pdfium::get(cx), Library::Ready) {
            this.open(cx);
        }
        this
    }

    fn open(&mut self, cx: &mut Context<Self>) {
        let path = self.path.clone();
        cx.spawn(async move |this, cx| {
            let opened = backend::open(path).await;
            _ = this.update(cx, |this, cx| {
                match opened {
                    Ok((doc, pages)) => {
                        this.doc = Some(doc);
                        this.page = this.page.min(pages.len().saturating_sub(1));
                        this.pages = pages;
                        this.error = None;
                        // Find again in what is there now.
                        if this.find.as_ref().is_some_and(|find| find.open) {
                            this.search(false, cx);
                        }
                    }
                    Err(err) => this.error = Some(err.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Read the file again (it changed on disk), staying on the page.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.close(cx);
        if matches!(Pdfium::get(cx), Library::Ready) {
            self.open(cx);
        }
    }

    fn close(&mut self, cx: &mut App) {
        if let Some(doc) = self.doc.take() {
            backend::close(doc);
        }
        self.pending.clear();
        for (_, (_, image)) in self.images.drain() {
            cx.drop_image(image, None);
        }
    }

    /// Draw `page` `width` wide (in pixels on screen), unless it is drawn
    /// about that wide already or on its way.
    fn draw(&mut self, page: usize, width: f32, window: &Window, cx: &mut Context<Self>) {
        let Some(doc) = self.doc else { return };
        let px = ((width * window.scale_factor()).round() as u32).clamp(1, backend::MAX_WIDTH);
        let close = |drawn: u32| drawn.abs_diff(px) * 50 <= px;
        if self
            .images
            .get(&page)
            .is_some_and(|(drawn, _)| close(*drawn))
            || self.pending.iter().any(|(p, w)| *p == page && close(*w))
        {
            return;
        }
        self.pending.insert((page, px));
        cx.spawn(async move |this, cx| {
            let bitmap = backend::render_page(doc, page, px).await;
            _ = this.update(cx, |this, cx| {
                this.pending.remove(&(page, px));
                let Some(bitmap) = bitmap.filter(|_| this.doc == Some(doc)) else {
                    return;
                };
                let Some(buffer) =
                    image::RgbaImage::from_raw(bitmap.width, bitmap.height, bitmap.bgra)
                else {
                    return;
                };
                let image = Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(
                    buffer
                )]));
                if let Some((_, old)) = this.images.insert(page, (px, image)) {
                    cx.drop_image(old, None);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Let the pictures of pages far from `shown` go.
    fn forget(&mut self, shown: std::ops::Range<usize>, window: &mut Window) {
        let keep = shown.start.saturating_sub(KEEP)..shown.end + KEEP;
        let far: Vec<usize> = self
            .images
            .keys()
            .copied()
            .filter(|page| !keep.contains(page))
            .collect();
        for page in far {
            if let Some((_, image)) = self.images.remove(&page) {
                _ = window.drop_image(image);
            }
        }
    }

    fn set_whole_pages(&mut self, whole: bool, cx: &mut Context<Self>) {
        let page = self.page;
        self.whole_pages = whole;
        crate::settings::AppState::update(cx, |state| state.pdf_whole_pages = whole);
        self.go_to(page, cx);
    }

    fn set_two_pages(&mut self, two: bool, cx: &mut Context<Self>) {
        let page = self.page;
        self.two_pages = two;
        crate::settings::AppState::update(cx, |state| state.pdf_two_pages = two);
        self.go_to(page, cx);
    }

    /// The pages side by side with `page`: itself, or the two of a spread.
    fn row_of(&self, page: usize) -> std::ops::Range<usize> {
        let n = if self.two_pages { 2 } else { 1 };
        let start = page - page % n;
        start..(start + n).min(self.pages.len())
    }

    /// A row's width (its pages' together) and height, in points.
    fn row_size(&self, row: std::ops::Range<usize>) -> (f32, f32) {
        row.map(|ix| self.pages[ix])
            .fold((0., 0.), |(w, h), (pw, ph)| (w + pw, h.max(ph)))
    }

    /// Points to pixels for `row` to fit the pane, all of it.
    fn fit(&self, row: std::ops::Range<usize>) -> f32 {
        let area = self.viewport.get();
        let gaps = GAP * row.len().saturating_sub(1) as f32;
        let (w, h) = self.row_size(row);
        ((area.width.as_f32() - 2. * PAD - gaps) / w.max(1.))
            .min((area.height.as_f32() - 2. * PAD) / h.max(1.))
            .max(0.05)
    }

    fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        let page = self.page;
        self.zoom = zoom.clamp(ZOOMS.0, ZOOMS.1);
        // The page at the top stays there.
        self.go_to(page, cx);
    }

    /// Show `page` (from 0): with Whole Pages alone (or with the page beside
    /// it), or else at the top of the pane.
    fn go_to(&mut self, page: usize, cx: &mut Context<Self>) {
        let page = self
            .row_of(page.min(self.pages.len().saturating_sub(1)))
            .start;
        self.page = page;
        if !self.whole_pages {
            self.scroll_to(page, 0., cx);
        }
        cx.notify();
    }

    /// Scroll the column so `y` points down `page` is a third down the pane;
    /// to the page's top with no `y`. Done when the column is next laid out,
    /// at the zoom it then has.
    fn scroll_to(&mut self, page: usize, y: f32, cx: &mut Context<Self>) {
        self.scroll_to = Some((page, y));
        cx.notify();
    }

    /// Scroll as `scroll_to` asked, once the column is laid out.
    fn settle_scroll(&mut self) {
        let Some((page, y)) = self.scroll_to.take() else {
            return;
        };
        let Some(top) = self.tops.get(page).copied() else {
            return;
        };
        let target = if y > 0. {
            top + y * self.scale - self.viewport.get().height.as_f32() / 3.
        } else {
            top - PAD / 2.
        };
        let x = self.scroll.offset().x;
        self.scroll.set_offset(point(x, px(-target.max(0.))));
    }

    /// The next page (or two), or the one before.
    fn turn(&mut self, forward: bool, cx: &mut Context<Self>) {
        let row = self.row_of(self.page);
        let page = if forward {
            row.end
        } else {
            row.start.saturating_sub(row.len().max(1))
        };
        if page < self.pages.len() {
            self.go_to(page, cx);
        }
    }

    /// With Whole Pages, the wheel turns a page once it went far enough.
    fn wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let dy = event.delta.pixel_delta(px(20.)).y.as_f32();
        if self.wheel.1.is_some_and(|at| at.elapsed() < TURN_PAUSE) {
            self.wheel.0 = 0.;
            return;
        }
        self.wheel.0 += dy;
        if self.wheel.0.abs() >= TURN {
            let forward = self.wheel.0 < 0.;
            self.wheel = (0., Some(Instant::now()));
            self.turn(forward, cx);
        }
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let typing = self
            .find
            .as_ref()
            .is_some_and(|find| find.query.read(cx).focus_handle(cx).is_focused(window));
        let keystroke = &event.keystroke;
        if typing || keystroke.modifiers.modified() {
            return;
        }
        let last = self.pages.len().saturating_sub(1);
        let step = px(40.);
        match (keystroke.key.as_str(), self.whole_pages) {
            ("pagedown" | "space", _) | ("down" | "right", true) => self.turn(true, cx),
            ("pageup", _) | ("up" | "left", true) => self.turn(false, cx),
            ("home", _) => self.go_to(0, cx),
            ("end", _) => self.go_to(last, cx),
            ("down", false) | ("up", false) => {
                let mut offset = self.scroll.offset();
                offset.y += if keystroke.key == "down" { -step } else { step };
                self.scroll.set_offset(offset);
                cx.notify();
            }
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl PdfView {
    /// Open Find, or select its query again.
    fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let find = self.find.get_or_insert_with(|| {
            let query = cx.new(|cx| InputState::new(window, cx).placeholder("Find"));
            let _subscription =
                cx.subscribe(&query, |this, _, event: &InputEvent, cx| match event {
                    InputEvent::Change => this.search(true, cx),
                    InputEvent::PressEnter { shift, .. } => this.step(!shift, cx),
                    _ => {}
                });
            Find {
                open: false,
                query,
                match_case: false,
                matches: Vec::new(),
                current: None,
                _search: None,
                _subscription,
            }
        });
        let reopened = !find.open;
        find.open = true;
        find.query.update(cx, |query, cx| {
            query.focus(window, cx);
            query.select_all(window, cx);
        });
        if reopened {
            self.search(true, cx);
        }
        cx.notify();
    }

    fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(find) = self.find.as_mut().filter(|find| find.open) else {
            return cx.propagate();
        };
        find.open = false;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    /// Find the query in every page. `reveal` (a new query) goes to the first
    /// match from the page showing.
    fn search(&mut self, reveal: bool, cx: &mut Context<Self>) {
        let (Some(find), Some(doc)) = (self.find.as_mut(), self.doc) else {
            return;
        };
        let query = find.query.read(cx).value().to_string();
        if query.is_empty() {
            find.matches.clear();
            find.current = None;
            find._search = None;
            return cx.notify();
        }
        let match_case = find.match_case;
        find._search = Some(cx.spawn(async move |this, cx| {
            let matches = backend::find(doc, query, match_case).await;
            _ = this.update(cx, |this, cx| {
                let page = this.page;
                let Some(find) = this.find.as_mut() else {
                    return;
                };
                let current = find.current.and_then(|ix| find.matches.get(ix)).cloned();
                // The current match stays current while it is found again.
                find.current = match current.filter(|_| !reveal) {
                    Some(m) => matches.iter().position(|found| *found == m),
                    None => None,
                }
                .or_else(|| matches.iter().position(|m| m.page >= page))
                .or((!matches.is_empty()).then_some(0));
                find.matches = matches;
                if reveal {
                    this.reveal(cx);
                }
                cx.notify();
            });
        }));
    }

    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(find) = self.find.as_mut().filter(|find| !find.matches.is_empty()) else {
            return;
        };
        let count = find.matches.len();
        find.current = Some(match find.current {
            Some(ix) if forward => (ix + 1) % count,
            Some(ix) => (ix + count - 1) % count,
            None => 0,
        });
        self.reveal(cx);
    }

    /// Show the current match: its page, and on it the match.
    fn reveal(&mut self, cx: &mut Context<Self>) {
        let Some(m) = self
            .find
            .as_ref()
            .and_then(|find| find.matches.get(find.current?))
            .cloned()
        else {
            return;
        };
        if self.whole_pages {
            self.go_to(m.page, cx);
        } else {
            self.page = m.page;
            let y = m.boxes.first().map_or(0., |b| b[1]).max(0.01);
            self.scroll_to(m.page, y, cx);
        }
    }
}

impl PdfView {
    /// A page `size` big (points times `scale`), with the matches on it.
    fn render_page(&self, page: usize, scale: f32, cx: &App) -> Div {
        let (w, h) = self.pages[page];
        let theme = cx.theme();
        let others = theme.yellow.opacity(0.35);
        let current = theme.yellow.opacity(0.75);
        let find = self.find.as_ref().filter(|find| find.open);
        let highlights = find.into_iter().flat_map(|find| {
            find.matches
                .iter()
                .enumerate()
                .filter(move |(_, m)| m.page == page)
                .flat_map(move |(ix, m)| {
                    let color = if find.current == Some(ix) {
                        current
                    } else {
                        others
                    };
                    m.boxes.iter().map(move |b| {
                        div()
                            .absolute()
                            .left(px(b[0] * scale))
                            .top(px(b[1] * scale))
                            .w(px(b[2] * scale))
                            .h(px(b[3] * scale))
                            .bg(color)
                    })
                })
        });
        div()
            .relative()
            .flex_none()
            .w(px(w * scale))
            .h(px(h * scale))
            .bg(white())
            .shadow_sm()
            .when_some(self.images.get(&page), |this, (_, image)| {
                this.child(img(ImageSource::Render(image.clone())).size_full())
            })
            .children(highlights)
    }

    /// One page (or two side by side), all of it, in the middle of the pane.
    fn render_whole_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let row = self.row_of(self.page);
        let scale = self.fit(row.clone());
        for ix in row.clone() {
            self.draw(ix, self.pages[ix].0 * scale, window, cx);
        }
        // The next pages are ready when they are turned to.
        if row.end < self.pages.len() {
            let next = self.row_of(row.end);
            let next_scale = self.fit(next.clone());
            for ix in next {
                self.draw(ix, self.pages[ix].0 * next_scale, window, cx);
            }
        }
        self.forget(row.clone(), window);
        div()
            .id("pdf-page")
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(GAP))
            .on_scroll_wheel(cx.listener(|this, event, _, cx| this.wheel(event, cx)))
            .children(row.map(|ix| self.render_page(ix, scale, cx)))
            .into_any_element()
    }

    /// The pages in a column (in rows of two with Two Pages), those in sight
    /// drawn.
    fn render_column(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let area = self.viewport.get();
        let rows: Vec<std::ops::Range<usize>> = (0..self.pages.len())
            .step_by(self.row_of(0).len().max(1))
            .map(|start| self.row_of(start))
            .collect();
        let per_row = rows.first().map_or(1, |row| row.len());
        let widest = rows
            .iter()
            .map(|row| self.row_size(row.clone()).0)
            .fold(1., f32::max);
        let gaps = GAP * (per_row - 1) as f32;
        let scale = ((area.width.as_f32() - 2. * PAD - gaps).max(50.) / widest) * self.zoom;
        // Each page's top (its row's) and left; a row is centred.
        let width = area.width.as_f32().max(widest * scale + gaps + 2. * PAD);
        let mut tops = vec![0.; self.pages.len()];
        let mut lefts = vec![0.; self.pages.len()];
        let mut y = PAD;
        for row in &rows {
            let (row_w, row_h) = self.row_size(row.clone());
            let mut x = (width - row_w * scale - GAP * (row.len() - 1) as f32) / 2.;
            for ix in row.clone() {
                tops[ix] = y;
                lefts[ix] = x;
                x += self.pages[ix].0 * scale + GAP;
            }
            y += row_h * scale + GAP;
        }
        let height = y - GAP + PAD;
        self.scale = scale;
        self.tops = tops;
        self.settle_scroll();

        let top = -self.scroll.offset().y.as_f32();
        let bottom = top + area.height.as_f32();
        let shown: Vec<usize> = (0..self.pages.len())
            .filter(|&ix| {
                let page_top = self.tops[ix];
                page_top < bottom && page_top + self.pages[ix].1 * scale > top
            })
            .collect();
        // The page counter follows the page a third down the pane.
        let mark = top + area.height.as_f32() / 3.;
        self.page = self
            .row_of(self.tops.iter().rposition(|t| *t <= mark).unwrap_or(0))
            .start;
        let range = match (shown.first(), shown.last()) {
            (Some(first), Some(last)) => *first..*last + 1,
            _ => self.row_of(self.page),
        };
        for ix in range.clone() {
            self.draw(ix, self.pages[ix].0 * scale, window, cx);
        }
        self.forget(range.clone(), window);
        let pages: Vec<AnyElement> = range
            .map(|ix| {
                self.render_page(ix, scale, cx)
                    .absolute()
                    .top(px(self.tops[ix]))
                    .left(px(lefts[ix]))
                    .into_any_element()
            })
            .collect();
        div()
            .id("pdf-column")
            .size_full()
            .overflow_scroll()
            .track_scroll(&self.scroll)
            .child(div().relative().w(px(width)).h(px(height)).children(pages))
            .into_any_element()
    }

    /// The bar over the pages: the page, the zoom and the views together in
    /// the middle, Find at the right.
    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let count = self.pages.len();
        let row = self.row_of(self.page);
        let at = if self.whole_pages && row.len() > 1 {
            format!("{}–{}", row.start + 1, row.end)
        } else {
            (self.page + 1).min(count).to_string()
        };
        let tool = |id: &'static str, icon: IconName, tooltip: &'static str| {
            Button::new(id)
                .ghost()
                .xsmall()
                .icon(Icon::new(icon))
                .tooltip(tooltip)
        };
        let toggle = |id: &'static str, icon: IconName, tooltip: &'static str, on: bool| {
            Button::new(id)
                .xsmall()
                .icon(Icon::new(icon))
                .tooltip(tooltip)
                .map(|b| if on { b.primary() } else { b.ghost() })
        };
        let separator = || div().flex_none().w(px(1.)).h_4().mx_1().bg(theme.border);
        let controls = h_flex()
            .flex_none()
            .items_center()
            .gap_0p5()
            .child(
                tool("pdf-prev", IconName::ChevronLeft, "Previous Page")
                    .disabled(self.page == 0)
                    .on_click(cx.listener(|this, _, _, cx| this.turn(false, cx))),
            )
            .child(div().px_1().child(format!("{at} / {count}")))
            .child(
                tool("pdf-next", IconName::ChevronRight, "Next Page")
                    .disabled(row.end >= count)
                    .on_click(cx.listener(|this, _, _, cx| this.turn(true, cx))),
            )
            .when(!self.whole_pages, |this| {
                this.child(separator())
                    .child(
                        tool("pdf-zoom-out", IconName::ZoomOut, "Zoom Out")
                            .disabled(self.zoom <= ZOOMS.0)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.set_zoom(this.zoom / 1.25, cx)),
                            ),
                    )
                    .child(
                        Button::new("pdf-zoom")
                            .ghost()
                            .xsmall()
                            .label(format!("{:.0}%", self.zoom * 100.))
                            .tooltip("Fit Width")
                            .on_click(cx.listener(|this, _, _, cx| this.set_zoom(1., cx))),
                    )
                    .child(
                        tool("pdf-zoom-in", IconName::ZoomIn, "Zoom In")
                            .disabled(self.zoom >= ZOOMS.1)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.set_zoom(this.zoom * 1.25, cx)),
                            ),
                    )
            })
            .child(separator())
            .child(
                toggle(
                    "pdf-whole-pages",
                    IconName::RectangleVertical,
                    "Whole Pages: all of a page at a time, the wheel turning the pages",
                    self.whole_pages,
                )
                .on_click(
                    cx.listener(|this, _, _, cx| this.set_whole_pages(!this.whole_pages, cx)),
                ),
            )
            .child(
                toggle(
                    "pdf-two-pages",
                    IconName::BookOpen,
                    "Two Pages: side by side",
                    self.two_pages,
                )
                .on_click(cx.listener(|this, _, _, cx| this.set_two_pages(!this.two_pages, cx))),
            );
        h_flex()
            .flex_none()
            .h(px(36.))
            .px_2()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .text_sm()
            // Sides alike, so the controls stay in the middle.
            .child(div().flex_1().min_w_0())
            .child(controls)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .justify_end()
                    .child(self.render_find(cx)),
            )
    }

    /// Find, at the toolbar's right: its button, or its query and matches.
    fn render_find(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(find) = self.find.as_ref().filter(|find| find.open) else {
            return Button::new("pdf-find")
                .ghost()
                .xsmall()
                .icon(Icon::new(IconName::Search))
                .tooltip(crate::ui::key_label("Find (Ctrl+F)"))
                .on_click(cx.listener(|this, _, window, cx| this.open_find(window, cx)))
                .into_any_element();
        };
        let found = !find.matches.is_empty();
        let label = match find.current {
            _ if find.query.read(cx).value().is_empty() => String::new(),
            Some(ix) if found => format!("{} of {}", ix + 1, find.matches.len()),
            _ => "No results".into(),
        };
        let step = |id: &'static str, icon: IconName, tooltip: &'static str, forward: bool| {
            Button::new(id)
                .ghost()
                .xsmall()
                .icon(Icon::new(icon))
                .tooltip(crate::ui::key_label(tooltip))
                .disabled(!found)
                .on_click(cx.listener(move |this, _, _, cx| this.step(forward, cx)))
        };
        h_flex()
            .min_w_0()
            .items_center()
            .gap_0p5()
            .child(
                div().w(px(240.)).min_w_16().child(
                    Input::new(&find.query).small().suffix(
                        Button::new("pdf-find-case")
                            .xsmall()
                            .icon(Icon::new(IconName::CaseSensitive))
                            .tooltip("Match Case")
                            .map(|b| {
                                if find.match_case {
                                    b.primary()
                                } else {
                                    b.ghost()
                                }
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(find) = this.find.as_mut() {
                                    find.match_case = !find.match_case;
                                }
                                this.search(true, cx);
                            })),
                    ),
                ),
            )
            .child(
                div()
                    .flex_none()
                    .min_w_16()
                    .px_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(step(
                "pdf-find-prev",
                IconName::ChevronUp,
                "Previous Match (Shift+Enter)",
                false,
            ))
            .child(step(
                "pdf-find-next",
                IconName::ChevronDown,
                "Next Match (Enter)",
                true,
            ))
            .child(
                Button::new("pdf-find-close")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::Close))
                    .tooltip(crate::ui::key_label("Close (Escape)"))
                    .on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx))),
            )
            .into_any_element()
    }

    /// What shows instead of the pages: PDFium to download, or why not.
    fn render_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let notice = |text: SharedString| {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .p_6()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(div().max_w(px(420.)).text_center().child(text))
        };
        let download = |label: &'static str| {
            Button::new("pdf-download")
                .primary()
                .small()
                .label(label)
                .on_click(|_, _, cx| Pdfium::download(cx))
        };
        Some(match Pdfium::get(cx) {
            Library::Missing => notice(
                format!(
                    "PDF tabs use PDFium, Google's PDF library ({}), which den downloads once into its data folder.",
                    backend::SIZE
                )
                .into(),
            )
            .child(download("Download PDFium"))
            .into_any_element(),
            Library::Downloading(done, total) => notice(if *total > 0 {
                format!("Downloading PDFium… {}%", done * 100 / total).into()
            } else {
                "Downloading PDFium…".into()
            })
            .into_any_element(),
            Library::Failed(err) => notice(err.clone())
                .child(download("Try Again"))
                .into_any_element(),
            Library::Ready => match &self.error {
                Some(err) => notice(err.clone()).into_any_element(),
                None if self.doc.is_none() => notice("Opening…".into()).into_any_element(),
                None if self.pages.is_empty() => {
                    notice("This PDF has no pages.".into()).into_any_element()
                }
                None => return None,
            },
        })
    }
}

impl Focusable for PdfView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PdfView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let notice = self.render_notice(cx);
        let ready = notice.is_none();
        let pages = match notice {
            Some(notice) => notice,
            None if self.whole_pages => self.render_whole_page(window, cx),
            None => self.render_column(window, cx),
        };
        let viewport = self.viewport.clone();
        v_flex()
            .size_full()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Search, window, cx| this.open_find(window, cx)))
            .on_action(cx.listener(|this, _: &Escape, window, cx| this.close_find(window, cx)))
            .on_key_down(cx.listener(Self::key))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    if !this.focus_handle.contains_focused(window, cx) {
                        this.focus_handle.focus(window, cx);
                    }
                }),
            )
            .when(ready, |this| this.child(self.render_toolbar(cx)))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .bg(cx.theme().muted)
                    // The pane's size, for the next layout of the pages.
                    .child(
                        canvas(
                            move |bounds, window, _| {
                                if viewport.replace(bounds.size) != bounds.size {
                                    window.refresh();
                                }
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(pages),
            )
    }
}
