//! Audio files, played: Symphonia reads and decodes them (MP3, AAC/M4A,
//! FLAC, WAV, AIFF, Ogg Vorbis, ALAC, CAF; Opus, in Ogg or WebM, through
//! libopus), and cpal plays them through the system's default output.
//!
//! Each player has a thread of its own, which owns the file and, while it
//! plays, the output stream: it decodes ahead into a short queue, turned to
//! the output's rate and channels, which the output drains.

use std::{
    collections::VecDeque,
    fs::File,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};
use symphonia::core::{
    audio::GenericAudioBufferRef,
    codecs::{
        CodecParameters,
        audio::{AudioDecoder, AudioDecoderOptions, well_known::CODEC_ID_MP3},
        registry::CodecRegistry,
    },
    errors::Error,
    formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType, probe::Hint},
    io::{MediaSourceStream, MediaSourceStreamOptions},
    meta::MetadataOptions,
    packet::Packet,
    units::{Time, TimeBase, Timestamp},
};
use symphonia::default::formats::MpaReader;

/// The file extensions an audio tab opens.
pub const EXTENSIONS: &[&str] = &[
    "mp3", "m4a", "m4b", "aac", "flac", "wav", "wave", "aif", "aiff", "aifc", "ogg", "oga", "opus",
    "webm", "weba", "mka", "caf",
];

/// How far ahead of the output the thread decodes, in seconds.
const AHEAD: f64 = 0.5;

/// What the file is, once read.
#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    /// The codec's name (`opus`, `aac`).
    pub codec: String,
    pub rate: u32,
    pub channels: usize,
    /// None when the container does not say.
    pub duration: Option<f64>,
}

/// A player's state, as the tab shows it.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Loading,
    Ready(Info),
    Failed(String),
}

enum Command {
    Play,
    Pause,
    /// To this many seconds from the start.
    Seek(f64),
}

/// What the player's thread and the output share.
struct Shared {
    status: Mutex<Status>,
    /// Decoded samples, at the output's rate and channels, interleaved.
    queue: Mutex<VecDeque<f32>>,
    /// Where the queue starts, in seconds: the last seek's.
    base: Mutex<f64>,
    /// Frames the output has played since `base`.
    played: AtomicU64,
    /// The output's rate, for `played` in seconds.
    out_rate: AtomicU64,
    playing: AtomicBool,
    /// The decoder is through the file: once the queue is empty, it ended.
    decoded: AtomicBool,
    /// Set by the output's error callback (the device went away).
    output_error: Mutex<Option<String>>,
}

/// One audio file, played on its own thread; stopped when dropped.
pub struct Player {
    commands: mpsc::Sender<Command>,
    shared: Arc<Shared>,
}

impl Player {
    pub fn open(path: &Path) -> Self {
        let shared = Arc::new(Shared {
            status: Mutex::new(Status::Loading),
            queue: Mutex::new(VecDeque::new()),
            base: Mutex::new(0.),
            played: AtomicU64::new(0),
            out_rate: AtomicU64::new(1),
            playing: AtomicBool::new(false),
            decoded: AtomicBool::new(false),
            output_error: Mutex::new(None),
        });
        let (commands, rx) = mpsc::channel();
        let path = path.to_path_buf();
        let thread_shared = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("den-audio".into())
            .spawn(move || run(&path, &thread_shared, rx));
        if let Err(err) = spawned {
            *shared.status.lock().unwrap() = Status::Failed(err.to_string());
        }
        Self { commands, shared }
    }

    pub fn status(&self) -> Status {
        self.shared.status.lock().unwrap().clone()
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    /// Where playback is, in seconds.
    pub fn position(&self) -> f64 {
        position(&self.shared)
    }

    pub fn play(&self) {
        // Shown as playing at once, before the thread gets to it.
        self.shared.playing.store(true, Ordering::Relaxed);
        _ = self.commands.send(Command::Play);
    }

    pub fn pause(&self) {
        self.shared.playing.store(false, Ordering::Relaxed);
        _ = self.commands.send(Command::Pause);
    }

    pub fn seek(&self, seconds: f64) {
        _ = self.commands.send(Command::Seek(seconds.max(0.)));
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // The thread hangs up on seeing the channel closed.
        self.shared.playing.store(false, Ordering::Relaxed);
    }
}

fn position(shared: &Shared) -> f64 {
    let base = *shared.base.lock().unwrap();
    let rate = shared.out_rate.load(Ordering::Relaxed).max(1);
    base + shared.played.load(Ordering::Relaxed) as f64 / rate as f64
}

/// Symphonia's decoders, and libopus for Opus, which it has none for.
fn codecs() -> CodecRegistry {
    let mut codecs = CodecRegistry::new();
    symphonia::default::register_enabled_codecs(&mut codecs);
    codecs.register_audio_decoder::<symphonia_adapter_libopus::OpusDecoder>();
    codecs
}

/// The file's first audio track, read, with a decoder for it.
struct Source {
    path: PathBuf,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track: u32,
    time_base: Option<TimeBase>,
    info: Info,
    /// The packet a seek stopped at, decoded next.
    pending: Option<Packet>,
}

fn open(path: &Path) -> Result<Source, String> {
    let format = read_format(path)?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or("The file has no audio")?;
    let Some(CodecParameters::Audio(params)) = &track.codec_params else {
        return Err("The file has no audio".into());
    };
    let decoder = codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| format!("Cannot decode this audio ({e})"))?;
    let rate = params.sample_rate.unwrap_or(0);
    let duration = match (track.time_base, track.duration) {
        (Some(base), Some(duration)) => base.calc_duration(duration).map(seconds),
        _ => None,
    }
    .or_else(|| {
        let frames = track.num_frames?;
        (rate > 0).then(|| frames as f64 / rate as f64)
    })
    // WebM states it for the file, not the track.
    .or_else(|| {
        let media = format.media_info();
        Some(seconds(media.time_base?.calc_duration(media.duration?)?))
    });
    let info = Info {
        codec: decoder.codec_info().short_name.to_string(),
        rate,
        channels: params.channels.as_ref().map_or(0, |c| c.count()),
        duration,
    };
    let (track, time_base) = (track.id, track.time_base);
    Ok(Source {
        path: path.to_path_buf(),
        format,
        decoder,
        track,
        time_base,
        info,
        pending: None,
    })
}

/// How long each of `peaks`' values covers, in seconds.
pub const PEAK_SPAN: f64 = 0.02;

/// The loudest sample of each `PEAK_SPAN` of the file, over its channels,
/// for the waveform: the whole file decoded, a second or two for an hour of
/// speech. Stops early, with what it has, once `cancel` is set.
pub fn peaks(path: &Path, cancel: &AtomicBool) -> Result<Vec<f32>, String> {
    let mut source = open(path)?;
    let mut peaks = Vec::new();
    let (mut loudest, mut filled, mut span) = (0f32, 0, 0);
    let mut samples: Vec<f32> = Vec::new();
    while !cancel.load(Ordering::Relaxed) {
        let packet = match source.format.next_packet() {
            Ok(Some(packet)) => packet,
            Err(Error::ResetRequired) => {
                source.decoder.reset();
                continue;
            }
            Ok(None) | Err(_) => break,
        };
        if packet.track_id != source.track {
            continue;
        }
        let decoded = match source.decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(Error::DecodeError(_)) => continue,
            Err(_) => break,
        };
        let channels = decoded.spec().channels().count().max(1);
        if span == 0 {
            span = ((decoded.spec().rate() as f64 * PEAK_SPAN) as usize).max(1);
        }
        samples.clear();
        decoded.copy_to_vec_interleaved(&mut samples);
        for frame in samples.chunks_exact(channels) {
            loudest = frame.iter().fold(loudest, |m, s| m.max(s.abs()));
            filled += 1;
            if filled == span {
                peaks.push(loudest);
                (loudest, filled) = (0., 0);
            }
        }
    }
    if filled > 0 {
        peaks.push(loudest);
    }
    Ok(peaks)
}

/// The file's container, read. An MP3 Symphonia cannot place (one inside
/// a WAV, or behind a tag whose size is wrong, where it takes stray bytes
/// for an MP1 frame) is read from where its frames start, as players find
/// them.
fn read_format(path: &Path) -> Result<Box<dyn FormatReader>, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_lowercase();
    let file = File::open(path).map_err(|e| e.to_string())?;
    let stream = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    hint.with_extension(&ext);
    let probed = symphonia::default::get_probe().probe(
        &hint,
        stream,
        FormatOptions::default(),
        MetadataOptions::default(),
    );
    let suspect = match &probed {
        Ok(format) => {
            ext == "mp3"
                && format.default_track(TrackType::Audio).and_then(|track| {
                    match &track.codec_params {
                        Some(CodecParameters::Audio(params)) => Some(params.codec),
                        _ => None,
                    }
                }) != Some(CODEC_ID_MP3)
        }
        Err(_) => true,
    };
    if suspect && let Some(start) = mpeg_start(path) {
        let window = Window::new(path, start).map_err(|e| e.to_string())?;
        let stream = MediaSourceStream::new(Box::new(window), MediaSourceStreamOptions::default());
        if let Ok(reader) = MpaReader::try_new(stream, FormatOptions::default()) {
            return Ok(Box::new(reader));
        }
    }
    probed.map_err(|e| format!("Not an audio file den can read ({e})"))
}

/// How far into a file `mpeg_start` looks, past tags and cover art.
const MPEG_SEARCH: u64 = 16 << 20;

/// Where MPEG audio starts in the file: the first frame header with two
/// more of the same kind where its length says, as ffmpeg finds it.
fn mpeg_start(path: &Path) -> Option<u64> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(MPEG_SEARCH)
        .read_to_end(&mut bytes)
        .ok()?;
    (0..bytes.len()).find_map(|at| {
        let (len, kind) = mpeg_frame(bytes.get(at..)?)?;
        let (second, second_kind) = mpeg_frame(bytes.get(at + len..)?)?;
        let (_, third_kind) = mpeg_frame(bytes.get(at + len + second..)?)?;
        (kind == second_kind && kind == third_kind).then_some(at as u64)
    })
}

/// The length of the MPEG audio frame whose header starts `bytes`, and its
/// version, layer and sample rate (the same in every frame of a stream).
fn mpeg_frame(bytes: &[u8]) -> Option<(usize, (u8, u8, u8))> {
    let &[0xFF, b1, b2, ..] = bytes else {
        return None;
    };
    if b1 & 0xE0 != 0xE0 {
        return None;
    }
    let (version, layer) = ((b1 >> 3) & 3, (b1 >> 1) & 3);
    let (bitrate, rate, padding) = (b2 >> 4, (b2 >> 2) & 3, ((b2 >> 1) & 1) as usize);
    if version == 1 || layer == 0 || bitrate == 0 || bitrate == 15 || rate == 3 {
        return None;
    }
    // Kbit/s, by version 1 or 2 (2.5 too) and layer.
    const RATES: [[u16; 15]; 5] = [
        [
            0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
        ],
        [
            0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
        ],
        [
            0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
        ],
        [
            0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256,
        ],
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
    ];
    let v1 = version == 3;
    let table = match (v1, layer) {
        (true, 3) => 0,
        (true, 2) => 1,
        (true, _) => 2,
        (false, 3) => 3,
        (false, _) => 4,
    };
    let bitrate = RATES[table][bitrate as usize] as usize * 1000;
    let base = [44_100, 48_000, 32_000][rate as usize];
    let sample_rate = match version {
        3 => base,
        2 => base / 2,
        _ => base / 4,
    };
    let len = match layer {
        3 => (12 * bitrate / sample_rate + padding) * 4,
        1 if !v1 => 72 * bitrate / sample_rate + padding,
        _ => 144 * bitrate / sample_rate + padding,
    };
    Some((len, (version, layer, rate)))
}

/// A file from `start` on, as a source of its own.
struct Window {
    file: File,
    start: u64,
    len: u64,
}

impl Window {
    fn new(path: &Path, start: u64) -> std::io::Result<Self> {
        use std::io::Seek as _;
        let mut file = File::open(path)?;
        let len = file.metadata()?.len().saturating_sub(start);
        file.seek(std::io::SeekFrom::Start(start))?;
        Ok(Self { file, start, len })
    }
}

impl std::io::Read for Window {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.file.read(buf)
    }
}

impl std::io::Seek for Window {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        use std::io::SeekFrom;
        let to = match pos {
            SeekFrom::Start(n) => SeekFrom::Start(self.start + n),
            other => other,
        };
        let at = self.file.seek(to)?;
        if at < self.start {
            self.file.seek(SeekFrom::Start(self.start))?;
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek before the start",
            ));
        }
        Ok(at - self.start)
    }
}

impl symphonia::core::io::MediaSource for Window {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.len)
    }
}

fn seconds(time: Time) -> f64 {
    let (secs, nanos) = time.parts();
    secs as f64 + nanos as f64 / 1e9
}

/// The player's thread: read the file, then follow the commands, decoding
/// ahead while playing.
fn run(path: &Path, shared: &Arc<Shared>, commands: mpsc::Receiver<Command>) {
    let mut source = match open(path) {
        Ok(source) => source,
        Err(err) => {
            *shared.status.lock().unwrap() = Status::Failed(err);
            return;
        }
    };
    *shared.status.lock().unwrap() = Status::Ready(source.info.clone());
    let mut output: Option<Output> = None;
    let mut convert = Convert::default();
    let mut samples: Vec<f32> = Vec::new();
    loop {
        let playing = shared.playing.load(Ordering::Relaxed);
        let ended = playing
            && shared.decoded.load(Ordering::Relaxed)
            && shared.queue.lock().unwrap().is_empty();
        if ended {
            shared.playing.store(false, Ordering::Relaxed);
            output = None;
        }
        if let Some(err) = shared.output_error.lock().unwrap().take() {
            shared.playing.store(false, Ordering::Relaxed);
            output = None;
            *shared.status.lock().unwrap() = Status::Failed(err);
        }
        let wanted = match &output {
            Some(out) if playing => {
                shared.queue.lock().unwrap().len()
                    < (AHEAD * out.rate as f64) as usize * out.channels
            }
            _ => false,
        };
        let command = if wanted && !shared.decoded.load(Ordering::Relaxed) {
            match commands.try_recv() {
                Ok(command) => Some(command),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        } else {
            match commands.recv_timeout(Duration::from_millis(20)) {
                Ok(command) => Some(command),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        };
        match command {
            Some(Command::Play) => {
                // Played to the end: from the start again.
                if shared.decoded.load(Ordering::Relaxed) && shared.queue.lock().unwrap().is_empty()
                {
                    seek(&mut source, shared, &mut convert, 0.);
                }
                if output.is_none() {
                    match Output::start(shared) {
                        Ok(out) => output = Some(out),
                        Err(err) => {
                            shared.playing.store(false, Ordering::Relaxed);
                            *shared.status.lock().unwrap() = Status::Failed(err);
                        }
                    }
                }
                if let Some(out) = &output {
                    _ = out.stream.play();
                }
                shared.playing.store(output.is_some(), Ordering::Relaxed);
            }
            Some(Command::Pause) => {
                shared.playing.store(false, Ordering::Relaxed);
                // Kept for the resume, as the queue's samples are at its rate.
                if let Some(out) = &output {
                    _ = out.stream.pause();
                }
            }
            Some(Command::Seek(to)) => seek(&mut source, shared, &mut convert, to),
            None => {}
        }
        if let (Some(out), None) = (&output, &command) {
            decode_one(&mut source, shared, out, &mut convert, &mut samples);
        }
    }
}

/// Go to `to` seconds: the queue is dropped, and decoding goes on from
/// the packet `to` is in. Some readers seek past where they are asked to
/// (Symphonia's WebM, to the end of a cluster, and keep the frames they read
/// through later seeks), so on landing past `to` the file is read afresh and
/// the seek starts further back, until it lands before `to`; the packets
/// from there to `to` are passed over.
fn seek(source: &mut Source, shared: &Shared, convert: &mut Convert, to: f64) {
    let to = match source.info.duration {
        Some(duration) => to.min(duration),
        None => to,
    };
    source.pending = None;
    let mut back = 0.;
    let landed = loop {
        let from = (to - back).max(0.);
        let seeked = source.format.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time: Time::from_nanos((from * 1e9) as i64),
                track_id: Some(source.track),
            },
        );
        let at = seeked.ok().map(|s| source.time_of(s.actual_ts));
        match at {
            Some(at) if at <= to + 0.001 => break Some(at),
            _ if from == 0. => break at,
            _ => {
                back = if back == 0. { 5. } else { back * 2. };
                if let Ok(fresh) = open(&source.path) {
                    *source = fresh;
                }
            }
        }
    };
    let landed = landed.and_then(|_| skip_to(source, to));
    source.decoder.reset();
    *convert = Convert::default();
    let mut queue = shared.queue.lock().unwrap();
    queue.clear();
    *shared.base.lock().unwrap() = landed.unwrap_or(to);
    shared.played.store(0, Ordering::Relaxed);
    // Nowhere to seek to: past the end.
    shared.decoded.store(landed.is_none(), Ordering::Relaxed);
}

/// Read on to the packet that plays at `to`, kept to decode next; returns
/// where it starts, or None at the end of the file.
fn skip_to(source: &mut Source, to: f64) -> Option<f64> {
    loop {
        let packet = source.format.next_packet().ok()??;
        if packet.track_id != source.track {
            continue;
        }
        let start = source.time_of(packet.pts);
        let end = start
            + source
                .time_base
                .map_or(0., |b| seconds(b.calc_duration_saturating(packet.dur)));
        if start >= to - 0.001 || end > to {
            source.pending = Some(packet);
            return Some(start);
        }
    }
}

impl Source {
    fn time_of(&self, ts: Timestamp) -> f64 {
        self.time_base
            .map_or(0., |base| seconds(base.calc_time_saturating(ts)))
    }
}

/// Decode the next packet of the track into the queue.
fn decode_one(
    source: &mut Source,
    shared: &Shared,
    out: &Output,
    convert: &mut Convert,
    samples: &mut Vec<f32>,
) {
    let next = match source.pending.take() {
        Some(packet) => Ok(Some(packet)),
        None => source.format.next_packet(),
    };
    let packet = match next {
        Ok(Some(packet)) => packet,
        Ok(None) => return shared.decoded.store(true, Ordering::Relaxed),
        // A broken file ends where it breaks.
        Err(Error::IoError(_)) => return shared.decoded.store(true, Ordering::Relaxed),
        Err(Error::ResetRequired) => return source.decoder.reset(),
        Err(_) => return shared.decoded.store(true, Ordering::Relaxed),
    };
    if packet.track_id != source.track {
        return;
    }
    let decoded: GenericAudioBufferRef = match source.decoder.decode(&packet) {
        Ok(decoded) => decoded,
        // A bad packet is skipped, as players do.
        Err(Error::DecodeError(_)) => return,
        Err(_) => return shared.decoded.store(true, Ordering::Relaxed),
    };
    let channels = decoded.spec().channels().count().max(1);
    let rate = decoded.spec().rate();
    samples.clear();
    decoded.copy_to_vec_interleaved(samples);
    let converted = convert.run(samples, channels, rate, out.channels, out.rate);
    shared.queue.lock().unwrap().extend(converted);
}

/// The system's output, playing the queue.
struct Output {
    stream: cpal::Stream,
    rate: u32,
    channels: usize,
}

impl Output {
    fn start(shared: &Arc<Shared>) -> Result<Self, String> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("No audio output device")?;
        let config = device
            .default_output_config()
            .map_err(|e| format!("Cannot use the audio output: {e}"))?;
        let format = config.sample_format();
        let config: cpal::StreamConfig = config.into();
        let (rate, channels) = (config.sample_rate, config.channels as usize);
        shared.out_rate.store(rate as u64, Ordering::Relaxed);
        let stream = match format {
            cpal::SampleFormat::F32 => build::<f32>(&device, config, shared),
            cpal::SampleFormat::F64 => build::<f64>(&device, config, shared),
            cpal::SampleFormat::I16 => build::<i16>(&device, config, shared),
            cpal::SampleFormat::U16 => build::<u16>(&device, config, shared),
            cpal::SampleFormat::I32 => build::<i32>(&device, config, shared),
            cpal::SampleFormat::U32 => build::<u32>(&device, config, shared),
            cpal::SampleFormat::I8 => build::<i8>(&device, config, shared),
            cpal::SampleFormat::U8 => build::<u8>(&device, config, shared),
            other => {
                return Err(format!(
                    "The audio output's format ({other}) is not supported"
                ));
            }
        }
        .map_err(|e| format!("Cannot use the audio output: {e}"))?;
        Ok(Self {
            stream,
            rate,
            channels,
        })
    }
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    shared: &Arc<Shared>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let channels = (config.channels as usize).max(1);
    let (data, errors) = (shared.clone(), shared.clone());
    device.build_output_stream(
        config,
        move |out: &mut [T], _: &cpal::OutputCallbackInfo| {
            let mut queue = data.queue.lock().unwrap();
            let playing = data.playing.load(Ordering::Relaxed);
            let mut taken = 0;
            for sample in out.iter_mut() {
                let value = match playing {
                    true => queue.pop_front().inspect(|_| taken += 1).unwrap_or(0.),
                    false => 0.,
                };
                *sample = T::from_sample(value);
            }
            data.played
                .fetch_add((taken / channels) as u64, Ordering::Relaxed);
        },
        move |err| *errors.output_error.lock().unwrap() = Some(err.to_string()),
        None,
    )
}

/// Turns decoded samples to the output's channels and rate: linear
/// interpolation, carried across packets.
#[derive(Default)]
struct Convert {
    /// Where the next output frame falls, in input frames from the last
    /// packet's last (-1 is that frame, 0 this packet's first).
    at: f64,
    /// The last packet's last frame, in the output's channels.
    last: Vec<f32>,
    out: Vec<f32>,
    mapped: Vec<f32>,
}

impl Convert {
    fn run(
        &mut self,
        samples: &[f32],
        in_channels: usize,
        in_rate: u32,
        out_channels: usize,
        out_rate: u32,
    ) -> &[f32] {
        // Mono to both speakers, the rest by position; to mono, the mean.
        self.mapped.clear();
        for frame in samples.chunks_exact(in_channels) {
            for c in 0..out_channels {
                self.mapped.push(match (in_channels, out_channels) {
                    (1, _) if c < 2 => frame[0],
                    (_, 1) => frame.iter().sum::<f32>() / in_channels as f32,
                    _ => frame.get(c).copied().unwrap_or(0.),
                });
            }
        }
        if in_rate == out_rate || in_rate == 0 {
            return &self.mapped;
        }
        let frames = self.mapped.len() / out_channels;
        if self.last.len() != out_channels {
            self.last = vec![0.; out_channels];
        }
        let step = in_rate as f64 / out_rate as f64;
        self.out.clear();
        while frames > 0 && self.at < (frames - 1) as f64 {
            let i = self.at.floor();
            let t = (self.at - i) as f32;
            let i = i as isize;
            for c in 0..out_channels {
                let a = match i {
                    -1 => self.last[c],
                    i => self.mapped[i as usize * out_channels + c],
                };
                let b = self.mapped[(i + 1) as usize * out_channels + c];
                self.out.push(a + (b - a) * t);
            }
            self.at += step;
        }
        if frames > 0 {
            self.at -= frames as f64;
            self.last
                .copy_from_slice(&self.mapped[(frames - 1) * out_channels..]);
        }
        &self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_mono_to_stereo_at_the_same_rate() {
        let mut convert = Convert::default();
        let out = convert.run(&[0.1, 0.2], 1, 48_000, 2, 48_000);
        assert_eq!(out, &[0.1, 0.1, 0.2, 0.2]);
    }

    #[test]
    fn resamples_to_about_the_right_length_across_packets() {
        let mut convert = Convert::default();
        let input: Vec<f32> = (0..4410).map(|i| (i as f32 / 100.).sin()).collect();
        let mut total = 0;
        for chunk in input.chunks(441) {
            total += convert.run(chunk, 1, 44_100, 1, 48_000).len();
        }
        // 0.1 s at 48 kHz, give or take a frame at the end.
        assert!((4798..=4800).contains(&total), "{total}");
    }

    #[test]
    fn finds_mp3_frames_past_a_tag_that_looks_like_one() {
        // MPEG 1 Layer III, 128 kbit/s, 44.1 kHz: 417 bytes a frame.
        let frame = |padded: bool| {
            let mut frame = vec![0xFF, 0xFB, if padded { 0x92 } else { 0x90 }, 0];
            frame.resize(if padded { 418 } else { 417 }, 0);
            frame
        };
        assert_eq!(mpeg_frame(&frame(false)).unwrap().0, 417);
        assert_eq!(mpeg_frame(&frame(true)).unwrap().0, 418);
        // A UTF-16 tag's byte order mark passes for a frame header alone.
        let mut file = b"ID3\x03\0\0\0\0\0\x10\xFF\xFEa\0b\0".to_vec();
        let start = file.len() as u64;
        for padded in [false, true, false] {
            file.extend(frame(padded));
        }
        let path = std::env::temp_dir().join(format!("den-mpeg-{}.mp3", std::process::id()));
        std::fs::write(&path, file).unwrap();
        assert_eq!(mpeg_start(&path), Some(start));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn reads_and_seeks_a_wav() {
        let (_, _, notes) = crate::sound::SOUNDS[0];
        let wav = crate::sound::render(notes, 60);
        let dir = std::env::temp_dir().join(format!("den-audio-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("chime.wav");
        std::fs::write(&path, wav).unwrap();
        let source = open(&path).map_err(|e| e.to_string()).unwrap();
        assert_eq!(source.info.rate, 44_100);
        assert_eq!(source.info.channels, 1);
        let duration = source.info.duration.unwrap();
        assert!((0.8..1.0).contains(&duration), "{duration}");
        let mut source = source;
        let shared = Shared {
            status: Mutex::new(Status::Loading),
            queue: Mutex::new(VecDeque::from([0.; 4])),
            base: Mutex::new(0.),
            played: AtomicU64::new(9),
            out_rate: AtomicU64::new(44_100),
            playing: AtomicBool::new(false),
            decoded: AtomicBool::new(true),
            output_error: Mutex::new(None),
        };
        seek(&mut source, &shared, &mut Convert::default(), 0.5);
        let at = position(&shared);
        assert!((0.49..=0.5).contains(&at), "{at}");
        assert!(shared.queue.lock().unwrap().is_empty());
        assert!(!shared.decoded.load(Ordering::Relaxed));
        // A peak per 20 ms, and not silent.
        let wave = peaks(&path, &AtomicBool::new(false)).unwrap();
        assert_eq!(wave.len(), (duration / PEAK_SPAN).ceil() as usize);
        assert!(wave.iter().any(|&p| p > 0.1));
        assert!(peaks(&path, &AtomicBool::new(true)).unwrap().is_empty());
        // Past the end: there, with nothing left to play.
        seek(&mut source, &shared, &mut Convert::default(), 60.);
        assert!((position(&shared) - duration).abs() < 0.05);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
