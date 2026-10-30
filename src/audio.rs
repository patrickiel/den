//! An audio file tab: play, pause and seek, as den plays it
//! (`backend::audio`). It plays on while the tab is out of sight, and stops
//! when it closes.
//!
//! The waveform is the seek bar: it seeks where it is clicked or dragged
//! to. Space (or K) plays and pauses, Left and Right go back and on 5
//! seconds, Home and End to the start and the end.

use std::{
    cell::Cell,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::backend::audio::{self as backend, Info, Player, Status};

/// How often the time shown moves on while playing.
const TICK: Duration = Duration::from_millis(100);
/// How far the buttons and the arrow keys go back and on, in seconds.
const JUMP: f64 = 10.;
const STEP: f64 = 5.;
/// The waveform's bars: their width, and the room from one to the next.
const BAR: f32 = 2.;
const BAR_STEP: f32 = 3.;
/// The tallest the waveform gets, however tall the pane.
const WAVE_HEIGHT: f32 = 220.;

pub struct AudioView {
    path: PathBuf,
    player: Player,
    focus_handle: FocusHandle,
    /// The waveform's peaks (`backend::audio::peaks`), scaled to 0..1, once
    /// read; flat until then.
    peaks: Option<Arc<Vec<f32>>>,
    /// Stops reading the peaks once the tab goes or reads the file again.
    peaks_cancel: Arc<AtomicBool>,
    /// While the waveform is dragged: where it would seek to, in seconds.
    scrub: Option<f64>,
    /// Where the mouse is over the waveform, in seconds.
    hover: Option<f64>,
    /// The waveform, as last painted, for where a click or drag falls on it.
    wave: Rc<Cell<Bounds<Pixels>>>,
    /// Moving the time on while loading or playing.
    ticking: Option<Task<()>>,
    _peaks: Option<Task<()>>,
}

impl AudioView {
    pub fn new(path: PathBuf, cx: &mut Context<Self>) -> Self {
        let player = Player::open(&path);
        let mut this = Self {
            path,
            player,
            focus_handle: cx.focus_handle(),
            peaks: None,
            peaks_cancel: Arc::default(),
            scrub: None,
            hover: None,
            wave: Rc::default(),
            ticking: None,
            _peaks: None,
        };
        this.read_peaks(cx);
        this.tick(cx);
        cx.on_release(|this, _| this.peaks_cancel.store(true, Ordering::Relaxed))
            .detach();
        this
    }

    /// Read the file again (it changed on disk), at the same place, paused.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let at = self.player.position();
        self.player = Player::open(&self.path);
        self.player.seek(at);
        self.read_peaks(cx);
        self.tick(cx);
        cx.notify();
    }

    /// Decode the whole file for its waveform, in the background.
    fn read_peaks(&mut self, cx: &mut Context<Self>) {
        self.peaks_cancel.store(true, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.peaks_cancel = cancel.clone();
        self.peaks = None;
        let path = self.path.clone();
        self._peaks = Some(cx.spawn(async move |this, cx| {
            let peaks = cx
                .background_spawn(async move { backend::peaks(&path, &cancel) })
                .await;
            let Ok(mut peaks) = peaks else { return };
            let loudest = peaks.iter().copied().fold(0f32, f32::max);
            if loudest > 0. {
                peaks.iter_mut().for_each(|p| *p /= loudest);
            }
            _ = this.update(cx, |this, cx| {
                this.peaks = Some(Arc::new(peaks));
                cx.notify();
            });
        }));
    }

    fn info(&self) -> Option<Info> {
        match self.player.status() {
            Status::Ready(info) => Some(info),
            _ => None,
        }
    }

    /// How long the file plays: as it says, else (a constant bit rate MP3
    /// without a header that says) as long as its waveform, once read.
    fn duration(&self, info: &Info) -> Option<f64> {
        info.duration
            .or_else(|| {
                let peaks = self.peaks.as_ref()?;
                Some(peaks.len() as f64 * backend::PEAK_SPAN)
            })
            .filter(|d| *d > 0.)
    }

    /// Redraw now and then while the file loads or plays, until it stops.
    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.ticking.is_some() {
            return;
        }
        self.ticking = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let going = this.update(cx, |this, cx| {
                    cx.notify();
                    let going = this.player.is_playing() || this.player.status() == Status::Loading;
                    if !going {
                        this.ticking = None;
                    }
                    going
                });
                if !matches!(going, Ok(true)) {
                    break;
                }
            }
        }));
    }

    fn toggle(&mut self, cx: &mut Context<Self>) {
        if self.info().is_none() {
            return;
        }
        if self.player.is_playing() {
            self.player.pause();
        } else {
            self.player.play();
            self.tick(cx);
        }
        cx.notify();
    }

    fn seek(&mut self, to: f64, cx: &mut Context<Self>) {
        let Some(info) = self.info() else { return };
        let Some(end) = self.duration(&info) else {
            return;
        };
        self.player.seek(to.clamp(0., end));
        // The time shown follows once the thread has seeked.
        self.tick(cx);
        cx.notify();
    }

    fn jump(&mut self, by: f64, cx: &mut Context<Self>) {
        let at = self.player.position();
        self.seek(at + by, cx);
    }

    fn key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.modified() {
            return;
        }
        match keystroke.key.as_str() {
            "space" | "k" => self.toggle(cx),
            "left" => self.jump(-STEP, cx),
            "right" => self.jump(STEP, cx),
            "home" => self.seek(0., cx),
            "end" => self.seek(f64::MAX, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    /// Where `x` falls on the waveform, in seconds.
    fn time_at(&self, x: Pixels, duration: f64) -> f64 {
        let wave = self.wave.get();
        let width = f32::from(wave.size.width).max(1.);
        let fraction = (f32::from(x - wave.origin.x) / width).clamp(0., 1.);
        fraction as f64 * duration
    }

    /// The waveform, played part in the accent colour, and a line where the
    /// mouse is; flat until its peaks are read.
    fn render_wave(&self, info: &Info, at: f64, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let duration = self.duration(info);
        let played = duration.map_or(0., |d| (at / d).clamp(0., 1.)) as f32;
        let hover = self
            .hover
            .zip(duration)
            .map(|(hover, d)| (hover / d).clamp(0., 1.) as f32);
        let peaks = self.peaks.clone().unwrap_or_default();
        let colors = (
            theme.primary,
            theme.muted_foreground.opacity(0.45),
            theme.foreground.opacity(0.6),
        );
        let wave = self.wave.clone();
        let this = cx.weak_entity();
        div()
            .id("audio-wave")
            .w_full()
            .h_full()
            .max_h(px(WAVE_HEIGHT))
            .when(duration.is_some(), |this| this.cursor_pointer())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    if let Some(duration) = duration {
                        this.scrub = Some(this.time_at(event.position.x, duration));
                        cx.notify();
                    }
                }),
            )
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                if let Some(duration) = duration {
                    this.hover = Some(this.time_at(event.position.x, duration));
                    cx.notify();
                }
            }))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !hovered {
                    this.hover = None;
                    cx.notify();
                }
            }))
            .child(
                canvas(
                    move |bounds, _, _| wave.set(bounds),
                    move |bounds, _, window, _| {
                        paint_wave(bounds, &peaks, played, hover, colors, window);
                        // A drag goes on outside the waveform: the window
                        // keeps the mouse while it's down.
                        window.on_mouse_event({
                            let this = this.clone();
                            move |event: &MouseMoveEvent, phase, _, cx| {
                                if phase != DispatchPhase::Capture {
                                    return;
                                }
                                _ = this.update(cx, |this, cx| {
                                    let Some(duration) = duration else { return };
                                    if this.scrub.is_some()
                                        && event.pressed_button == Some(MouseButton::Left)
                                    {
                                        this.scrub = Some(this.time_at(event.position.x, duration));
                                        cx.notify();
                                    }
                                });
                            }
                        });
                        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture && event.button == MouseButton::Left
                            {
                                _ = this.update(cx, |this, cx| {
                                    if let Some(to) = this.scrub.take() {
                                        this.seek(to, cx);
                                    }
                                });
                            }
                        });
                    },
                )
                .size_full(),
            )
    }
}

/// Bars across `bounds`, each as tall as the loudest peak under it, mirrored
/// about the middle: those before `played` (0..1) in the first colour, the
/// rest in the second, and a line at `hover` in the third.
fn paint_wave(
    bounds: Bounds<Pixels>,
    peaks: &[f32],
    played: f32,
    hover: Option<f32>,
    (played_color, rest_color, hover_color): (Hsla, Hsla, Hsla),
    window: &mut Window,
) {
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    let bars = (width / BAR_STEP).floor().max(1.) as usize;
    let middle = bounds.top() + px(height / 2.);
    for bar in 0..bars {
        let peak = match peaks.len() {
            0 => 0.,
            len => {
                let from = bar * len / bars;
                let to = ((bar + 1) * len / bars).max(from + 1).min(len);
                peaks[from..to].iter().copied().fold(0., f32::max)
            }
        };
        let tall = (peak * height).max(BAR);
        let x = bounds.left() + px(bar as f32 * BAR_STEP);
        let color = if (bar as f32 + 0.5) / bars as f32 <= played {
            played_color
        } else {
            rest_color
        };
        window.paint_quad(quad(
            Bounds::new(point(x, middle - px(tall / 2.)), size(px(BAR), px(tall))),
            px(BAR / 2.),
            color,
            px(0.),
            transparent_black(),
            BorderStyle::default(),
        ));
    }
    if let Some(hover) = hover {
        let x = bounds.left() + px(width * hover);
        window.paint_quad(fill(
            Bounds::new(point(x, bounds.top()), size(px(1.), bounds.size.height)),
            hover_color,
        ));
    }
}

/// `seconds` as a clock: 4:05, or 1:02:05 from an hour.
fn clock(seconds: f64) -> String {
    let total = seconds.max(0.) as u64;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    match h {
        0 => format!("{m}:{s:02}"),
        h => format!("{h}:{m:02}:{s:02}"),
    }
}

/// What the file holds, as the tab says it: "Opus · 48 kHz · Mono".
fn describe(info: &Info) -> String {
    let codec = match info.codec.as_str() {
        "opus" => "Opus".to_string(),
        "vorbis" => "Vorbis".to_string(),
        codec if codec.starts_with("pcm") => "PCM".to_string(),
        codec if codec.starts_with("adpcm") => "ADPCM".to_string(),
        codec => codec.to_uppercase(),
    };
    let mut parts = vec![codec];
    if info.rate > 0 {
        let khz = info.rate as f64 / 1000.;
        parts.push(match khz.fract() {
            0. => format!("{khz} kHz"),
            _ => format!("{khz:.1} kHz"),
        });
    }
    parts.push(match info.channels {
        0 => return parts.join(" · "),
        1 => "Mono".to_string(),
        2 => "Stereo".to_string(),
        n => format!("{n} channels"),
    });
    parts.join(" · ")
}

impl Focusable for AudioView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AudioView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        let status = self.player.status();
        let playing = self.player.is_playing();
        let at = self.scrub.unwrap_or_else(|| self.player.position());
        let header = h_flex()
            .flex_none()
            .w_full()
            .gap_2()
            .items_center()
            .child(
                Icon::new(IconName::AudioLines)
                    .size(px(18.))
                    .text_color(if playing {
                        theme.primary
                    } else {
                        theme.muted_foreground
                    }),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_base()
                    .text_color(theme.foreground)
                    .child(name),
            )
            .when_some(self.info(), |this, info| {
                this.child(div().flex_none().ml_auto().child(describe(&info)))
            });
        let body = match &status {
            Status::Loading => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child("Opening…")
                .into_any_element(),
            Status::Failed(err) => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(div().max_w(px(420.)).text_center().child(err.clone()))
                .into_any_element(),
            Status::Ready(info) => {
                let tool = |id: &'static str, icon: IconName, tooltip: &'static str| {
                    Button::new(id)
                        .ghost()
                        .icon(Icon::new(icon))
                        .tooltip(tooltip)
                };
                let end = self.duration(info);
                let mono = crate::settings::mono_font(cx);
                let clock_text = |text: String| {
                    div()
                        .flex_1()
                        .font_family(mono.clone())
                        .text_xs()
                        .child(text)
                };
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .gap_4()
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .flex()
                            .items_center()
                            .child(self.render_wave(info, at, cx)),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .w_full()
                            .items_center()
                            .child(clock_text(clock(at)))
                            .child(
                                h_flex()
                                    .flex_none()
                                    .items_center()
                                    .gap_3()
                                    .child(
                                        tool("audio-back", IconName::Rewind, "Back 10 Seconds")
                                            .on_click(
                                                cx.listener(|this, _, _, cx| this.jump(-JUMP, cx)),
                                            ),
                                    )
                                    .child(
                                        Button::new("audio-play")
                                            .primary()
                                            .large()
                                            .icon(Icon::new(if playing {
                                                IconName::Pause
                                            } else {
                                                IconName::Play
                                            }))
                                            .tooltip(if playing { "Pause" } else { "Play" })
                                            .on_click(
                                                cx.listener(|this, _, _, cx| this.toggle(cx)),
                                            ),
                                    )
                                    .child(
                                        tool(
                                            "audio-on",
                                            IconName::FastForward,
                                            "Forward 10 Seconds",
                                        )
                                        .disabled(end.is_some_and(|end| at >= end))
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.jump(JUMP, cx)),
                                        ),
                                    ),
                            )
                            .child(
                                clock_text(end.map_or("–:––".into(), clock))
                                    .flex()
                                    .justify_end(),
                            ),
                    )
                    .into_any_element()
            }
        };
        v_flex()
            .size_full()
            .gap_4()
            .px_6()
            .py_4()
            .bg(theme.background)
            .text_sm()
            .text_color(theme.muted_foreground)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    if !this.focus_handle.contains_focused(window, cx) {
                        this.focus_handle.focus(window, cx);
                    }
                }),
            )
            .child(header)
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::{clock, describe};
    use crate::backend::audio::Info;

    #[test]
    fn clocks() {
        assert_eq!(clock(0.), "0:00");
        assert_eq!(clock(65.9), "1:05");
        assert_eq!(clock(1272.9), "21:12");
        assert_eq!(clock(3725.), "1:02:05");
    }

    #[test]
    fn describes() {
        let info = |codec: &str, rate, channels| Info {
            codec: codec.into(),
            rate,
            channels,
            duration: None,
        };
        assert_eq!(describe(&info("opus", 48_000, 1)), "Opus · 48 kHz · Mono");
        assert_eq!(describe(&info("aac", 44_100, 2)), "AAC · 44.1 kHz · Stereo");
        assert_eq!(describe(&info("pcm_s16le", 0, 6)), "PCM · 6 channels");
    }
}
