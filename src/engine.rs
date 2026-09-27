//! Playback and rendering engine built on GStreamer Editing Services.
//!
//! `VtEngine` is a `GtkMediaStream`, so widgets drive playback through the usual media stream
//! API. It has two modes:
//!
//! - Trimmer: a single clip covering the whole source video. Timestamps are microseconds of the
//!   source video, so trimming widgets work in source time whatever the speed.
//! - Editor: any number of segments from one or more videos on a GES layer. Timestamps are
//!   microseconds of the edited timeline. Overlapping segments crossfade automatically.
//!
//! The preview and the editor's export render the same timeline, so what plays is what exports.

use std::path::Path;

use gtk::{gdk, gio, glib, prelude::*, subclass::prelude::*};

/// A segment of the edited timeline, for drawing and hit-testing.
#[derive(Debug, Clone)]
pub struct SegmentInfo {
    /// Timeline position in microseconds.
    pub start: i64,
    /// Timeline duration in microseconds.
    pub duration: i64,
    pub speed: f64,
    /// Index of the source video, stable while in the Editor.
    pub source: usize,
    pub name: String,
    /// Where the segment starts in its source, in source µs.
    pub inpoint: i64,
    /// How far the start can move left before running out of source, in timeline µs.
    pub extend_left: i64,
    /// How far the end can move right before running out of source, in timeline µs.
    pub extend_right: i64,
}

impl SegmentInfo {
    pub fn end(&self) -> i64 {
        self.start + self.duration
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentEdge {
    Start,
    End,
}

/// Frame rate and size of an export; `None` keeps the source's.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ExportSettings {
    /// Frames per second as numerator and denominator.
    pub fps: Option<(i32, i32)>,
    pub size: Option<(i32, i32)>,
}

impl ExportSettings {
    /// FFmpeg video filters that apply the settings, letterboxing to keep the aspect ratio.
    pub fn ffmpeg_filters(&self) -> Vec<String> {
        let mut filters = Vec::new();
        if let Some((width, height)) = self.size {
            filters.push(format!(
                "scale={width}:{height}:force_original_aspect_ratio=decrease,\
pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,setsar=1"
            ));
        }
        if let Some((numerator, denominator)) = self.fps {
            filters.push(format!("fps={numerator}/{denominator}"));
        }
        filters
    }
}

#[derive(Debug, Clone)]
pub enum RenderEvent {
    /// Fraction of the timeline rendered so far.
    Progress(f64),
    Done,
    Failed(String),
}

mod imp {
    use super::*;
    use crate::config::G_LOG_DOMAIN;
    use ges::prelude::*;
    use glib::{debug, subclass::Signal, warn};
    use gst_controller::prelude::*;
    use std::{
        cell::{Cell, OnceCell, RefCell},
        sync::OnceLock,
    };

    /// How often the playback position is refreshed while playing.
    const POSITION_POLL: std::time::Duration = std::time::Duration::from_millis(33);
    /// How often the export progress is reported.
    const RENDER_POLL: std::time::Duration = std::time::Duration::from_millis(250);

    /// Length of the music fade-out, in timeline nanoseconds.
    const MUSIC_FADE_NS: u64 = crate::music_track::FADE_OUT_DURATION as u64 * 1000;
    /// Length of the fade to black at the end of the edit, in nanoseconds.
    const END_FADE_NS: u64 = 1_000_000_000;
    /// The black clip must stick out past the last segment: GES rejects a clip lying entirely
    /// within another. The final frames are black anyway.
    const END_FADE_OVERHANG_NS: u64 = 40_000_000;

    struct Source {
        name: String,
        asset: ges::UriClipAsset,
        has_audio: bool,
    }

    /// A segment as saved in the undo history.
    #[derive(Clone)]
    struct SegmentState {
        source: usize,
        start: u64,
        inpoint: u64,
        duration: u64,
        speed: f64,
    }

    const MAX_UNDO: usize = 100;

    struct Segment {
        clip: ges::Clip,
        source: usize,
        speed: f64,
    }

    type RenderCallback = Box<dyn Fn(RenderEvent)>;

    struct Render {
        callback: RenderCallback,
        path: std::path::PathBuf,
        poll: Option<glib::SourceId>,
        /// The video track's caps before the export settings were applied.
        preview_caps: Option<gst::Caps>,
    }

    #[derive(Default)]
    pub struct VtEngine {
        timeline: OnceCell<ges::Timeline>,
        video_layer: OnceCell<ges::Layer>,
        music_layer: OnceCell<ges::Layer>,
        pipeline: OnceCell<ges::Pipeline>,
        pub(super) paintable: OnceCell<gdk::Paintable>,
        volume: OnceCell<gst::Element>,
        bus_watch: RefCell<Option<gst::bus::BusWatchGuard>>,
        poll: RefCell<Option<glib::SourceId>>,

        sources: RefCell<Vec<Source>>,
        /// In Trimmer mode, the single segment covering the first source.
        segments: RefCell<Vec<Segment>>,
        editor: Cell<bool>,
        has_video: Cell<bool>,
        /// Speed of the Trimmer clip.
        speed: Cell<f64>,
        prepared: Cell<bool>,
        video_muted: Cell<bool>,
        /// Volume of the videos' own audio, 1 being unchanged.
        video_volume: Cell<f64>,
        end_fade: Cell<bool>,
        end_fade_clip: RefCell<Option<ges::Clip>>,

        seek_in_flight: Cell<bool>,
        seek_started: Cell<Option<std::time::Instant>>,
        pending_seek: Cell<Option<(i64, bool)>>,
        pub(super) next_seek_fast: Cell<bool>,

        music_asset: RefCell<Option<ges::UriClipAsset>>,
        music_clip: RefCell<Option<ges::Clip>>,

        render: RefCell<Option<Render>>,
        commit_scheduled: Cell<bool>,
        undo: RefCell<Vec<Vec<SegmentState>>>,
        redo: RefCell<Vec<Vec<SegmentState>>>,
        /// Video buffers reaching the sink, for the debug log.
        frames_to_sink: std::sync::Arc<std::sync::atomic::AtomicU32>,
        frames_logged_at: Cell<Option<std::time::Instant>>,
        frames_painted: Cell<u32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for VtEngine {
        const NAME: &'static str = "VtEngine";
        type Type = super::VtEngine;
        type ParentType = gtk::MediaStream;
    }

    impl ObjectImpl for VtEngine {
        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<[Signal; 1]> = OnceLock::new();
            SIGNALS.get_or_init(|| [Signal::builder("timeline-changed").build()])
        }

        fn constructed(&self) {
            self.parent_constructed();
            self.speed.set(1.);
            self.video_volume.set(1.);

            if let Err(err) = self.build_pipeline() {
                warn!("could not build the playback pipeline: {err}");
                self.obj()
                    .set_error(glib::Error::new(gio::IOErrorEnum::Failed, &err));
            }
        }

        fn dispose(&self) {
            self.stop_polling();
            self.bus_watch.take();
            if let Some(pipeline) = self.pipeline.get() {
                let _ = pipeline.set_state(gst::State::Null);
            }
        }
    }

    impl MediaStreamImpl for VtEngine {
        fn play(&self) -> bool {
            if self.render.borrow().is_some() {
                return false;
            }
            let obj = self.obj();
            if obj.is_ended() || obj.timestamp() >= self.total_duration() {
                self.do_seek(0, false);
            }
            let result = self.pipeline().set_state(gst::State::Playing);
            debug!("play: {result:?}");
            if result.is_err() {
                return false;
            }
            self.start_polling();
            true
        }

        fn pause(&self) {
            if self.render.borrow().is_some() {
                return;
            }
            let _ = self.pipeline().set_state(gst::State::Paused);
            self.stop_polling();
            if !self.seek_in_flight.get() {
                self.obj().update(self.position());
            }
        }

        fn seek(&self, timestamp: i64) {
            let fast = self.next_seek_fast.take();
            self.expire_stuck_seek();
            if self.seek_in_flight.get() {
                // Only the latest target matters; it is sought once the current seek lands.
                self.pending_seek.set(Some((timestamp, fast)));
                return;
            }
            self.do_seek(timestamp, fast);
        }

        fn update_audio(&self, muted: bool, volume: f64) {
            if let Some(element) = self.volume.get() {
                element.set_property("mute", muted);
                element.set_property("volume", volume);
            }
        }
    }

    impl VtEngine {
        fn pipeline(&self) -> &ges::Pipeline {
            self.pipeline.get().unwrap()
        }

        fn timeline(&self) -> &ges::Timeline {
            self.timeline.get().unwrap()
        }

        fn build_pipeline(&self) -> Result<(), String> {
            let timeline = ges::Timeline::new_audio_video();
            timeline.set_auto_transition(true);
            let video_layer = timeline.append_layer();
            let music_layer = timeline.append_layer();

            let pipeline = ges::Pipeline::new();
            pipeline
                .set_timeline(&timeline)
                .map_err(|err| err.to_string())?;

            let video_sink = gst::ElementFactory::make("gtk4paintablesink")
                .build()
                .map_err(|err| format!("gtk4paintablesink is missing: {err}"))?;
            let paintable = video_sink.property::<gdk::Paintable>("paintable");
            if let Some(pad) = video_sink.static_pad("sink") {
                let frames = self.frames_to_sink.clone();
                pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                    frames.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    gst::PadProbeReturn::Ok
                });
            }
            pipeline.set_video_sink(Some(&video_sink));

            // QVE_AUDIO_SINK replaces the output, e.g. with a file sink to check the audio.
            let output =
                std::env::var("QVE_AUDIO_SINK").unwrap_or_else(|_| "autoaudiosink".to_owned());
            let audio_sink = gst::parse::bin_from_description(
                &format!("volume name=volume ! audioconvert ! audioresample ! {output}"),
                true,
            )
            .map_err(|err| err.to_string())?;
            let volume = audio_sink.by_name("volume").unwrap();
            pipeline.set_audio_sink(Some(&audio_sink));

            let bus = pipeline.bus().unwrap();
            let watch = bus
                .add_watch_local({
                    let obj = self.obj().downgrade();
                    move |_, message| {
                        if let Some(obj) = obj.upgrade() {
                            obj.imp().on_bus_message(message);
                        }
                        glib::ControlFlow::Continue
                    }
                })
                .map_err(|err| err.to_string())?;

            self.bus_watch.replace(Some(watch));
            self.timeline.set(timeline).unwrap();
            self.video_layer.set(video_layer).unwrap();
            self.music_layer.set(music_layer).unwrap();
            self.pipeline.set(pipeline).unwrap();
            paintable.connect_invalidate_contents({
                let obj = self.obj().downgrade();
                move |_| {
                    if let Some(obj) = obj.upgrade() {
                        let imp = obj.imp();
                        imp.frames_painted.set(imp.frames_painted.get() + 1);
                    }
                }
            });
            self.paintable.set(paintable).unwrap();
            self.volume.set(volume).unwrap();
            Ok(())
        }

        fn load_source(&self, file: &gio::File) -> Result<usize, glib::Error> {
            let asset = ges::UriClipAsset::request_sync(&file.uri())?;
            let info = asset.info();
            let has_audio = !info.audio_streams().is_empty();
            let has_video = !info.video_streams().is_empty();

            let mut sources = self.sources.borrow_mut();
            if sources.is_empty() {
                self.has_video.set(has_video);
                if let Some(video) = info.video_streams().first() {
                    // Render at the first video's size instead of GES's 1280×720 default.
                    let caps = gst::Caps::builder("video/x-raw")
                        .field("width", video.width() as i32)
                        .field("height", video.height() as i32)
                        .field("framerate", video.framerate())
                        .build();
                    for track in self.timeline().tracks() {
                        if track.track_type() == ges::TrackType::VIDEO {
                            track.set_restriction_caps(&caps);
                        }
                    }
                }
            }

            let name = file
                .basename()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            sources.push(Source {
                name,
                asset,
                has_audio,
            });
            Ok(sources.len() - 1)
        }

        /// Adds a clip of the whole source at `start` (ns).
        fn add_source_clip(&self, source: usize, start: u64) -> Result<Segment, glib::Error> {
            let asset = self.sources.borrow()[source].asset.clone();
            let duration = asset.duration().unwrap_or(gst::ClockTime::ZERO);
            let clip = self
                .video_layer
                .get()
                .unwrap()
                .add_asset(
                    &asset,
                    gst::ClockTime::from_nseconds(start),
                    gst::ClockTime::ZERO,
                    duration,
                    ges::TrackType::UNKNOWN,
                )
                .map_err(|err| glib::Error::new(gio::IOErrorEnum::Failed, &err.to_string()))?;
            self.apply_audio_settings(&clip);
            Ok(Segment {
                clip,
                source,
                speed: 1.,
            })
        }

        pub(super) fn open(&self, file: &gio::File) {
            let obj = self.obj();
            let result = self
                .load_source(file)
                .and_then(|source| self.add_source_clip(source, 0));
            match result {
                Ok(segment) => self.segments.replace(vec![segment]),
                Err(err) => {
                    warn!("could not open {}: {err}", file.uri());
                    obj.set_error(err);
                    return;
                }
            };

            self.timeline().commit_sync();
            let _ = self.pipeline().set_state(gst::State::Paused);
        }

        fn on_bus_message(&self, message: &gst::Message) {
            if self.render.borrow().is_some() {
                match message.view() {
                    gst::MessageView::Eos(_) => self.finish_render(RenderEvent::Done),
                    gst::MessageView::Error(err) => self.finish_render(RenderEvent::Failed(
                        format!("{} ({:?})", err.error(), err.debug()),
                    )),
                    _ => {}
                }
                return;
            }

            let obj = self.obj();
            match message.view() {
                gst::MessageView::AsyncDone(_) => self.on_async_done(),
                gst::MessageView::StateChanged(change)
                    if message.src() == Some(self.pipeline().upcast_ref()) =>
                {
                    debug!("pipeline {:?} -> {:?}", change.old(), change.current());
                }
                gst::MessageView::Qos(qos) => {
                    let (_, dropped) = qos.stats();
                    let (jitter, _, _) = qos.values();
                    debug!(
                        "qos from {:?}: {dropped} dropped, jitter {} ms",
                        qos.src().map(|src| src.name()),
                        jitter / 1_000_000
                    );
                }
                gst::MessageView::Latency(_) => {
                    debug!("latency changed");
                }
                gst::MessageView::Warning(warning) => {
                    warn!(
                        "playback warning: {} ({:?})",
                        warning.error(),
                        warning.debug()
                    );
                }
                gst::MessageView::Eos(_) => {
                    let position = self.position();
                    let total = self.total_duration();
                    debug!("end of stream at {position} of {total} µs");
                    // A seek racing with a state change can end the stream early; keep playing
                    // from where the playhead is instead of stopping there.
                    const END_TOLERANCE: i64 = 500_000;
                    if total - position > END_TOLERANCE && !self.seek_in_flight.get() {
                        warn!("early end of stream at {position} µs of {total}, resuming");
                        self.do_seek(position, false);
                        let _ = self.pipeline().set_state(gst::State::Playing);
                        return;
                    }
                    let _ = self.pipeline().set_state(gst::State::Paused);
                    self.stop_polling();
                    obj.update(self.total_duration());
                    obj.stream_ended();
                }
                gst::MessageView::Error(err) => {
                    warn!(
                        "playback error from {:?}: {} ({:?})",
                        err.src().map(|src| src.path_string()),
                        err.error(),
                        err.debug()
                    );
                    self.stop_polling();
                    obj.set_error(err.error());
                }
                _ => {}
            }
        }

        fn on_async_done(&self) {
            let obj = self.obj();

            if !self.prepared.get() {
                self.prepared.set(true);
                let has_audio = self.sources.borrow().first().is_some_and(|s| s.has_audio);
                obj.stream_prepared(has_audio, self.has_video.get(), true, self.total_duration());
                obj.update(0);
                // QVE_AUTOPLAY starts playback on load, after the given milliseconds if any, to
                // check the output without a pointer.
                // "DELAY,TOGGLE" also toggles playback every TOGGLE milliseconds; "DELAY,TOGGLE,1"
                // also scrubs while paused, like dragging the playhead before playing again.
                if let Ok(value) = std::env::var("QVE_AUTOPLAY") {
                    let mut parts = value.split(',').map(|part| part.parse::<u64>().ok());
                    let delay = parts.next().flatten().unwrap_or(0);
                    let toggle = parts.next().flatten();
                    let mode = parts.next().flatten().unwrap_or(0);
                    let scrub = mode == 1;
                    // Mode 2 scrubs without pausing.
                    let scrub_while_playing = mode == 2;
                    glib::timeout_add_local_once(std::time::Duration::from_millis(delay), {
                        let obj = obj.downgrade();
                        move || {
                            let Some(obj) = obj.upgrade() else {
                                return;
                            };
                            obj.play();
                            if let Some(toggle) = toggle {
                                let obj = obj.downgrade();
                                glib::timeout_add_local(
                                    std::time::Duration::from_millis(toggle),
                                    move || {
                                        let Some(obj) = obj.upgrade() else {
                                            return glib::ControlFlow::Break;
                                        };
                                        if mode == 3 && obj.is_playing() {
                                            // Click the timeline, press play while the seek is
                                            // still running, then the exact seek on release.
                                            obj.pause();
                                            let duration = obj.duration().max(1);
                                            let target =
                                                (obj.timestamp() + duration / 3) % duration;
                                            obj.seek_fast(target);
                                            let obj = obj.downgrade();
                                            glib::timeout_add_local_once(
                                                std::time::Duration::from_millis(300),
                                                move || {
                                                    let Some(obj) = obj.upgrade() else {
                                                        return;
                                                    };
                                                    obj.play();
                                                    let obj = obj.downgrade();
                                                    glib::timeout_add_local_once(
                                                        std::time::Duration::from_millis(150),
                                                        move || {
                                                            if let Some(obj) = obj.upgrade() {
                                                                obj.seek(target);
                                                            }
                                                        },
                                                    );
                                                },
                                            );
                                        } else if scrub_while_playing && obj.is_playing() {
                                            let duration = obj.duration().max(1);
                                            let target =
                                                (obj.timestamp() + duration / 3) % duration;
                                            for step in 0..8 {
                                                obj.seek_fast(target + step * 100_000);
                                            }
                                            obj.seek(target);
                                        } else if obj.is_playing() {
                                            obj.pause();
                                            if scrub {
                                                let duration = obj.duration().max(1);
                                                let target =
                                                    (obj.timestamp() + duration / 3) % duration;
                                                for step in 0..8 {
                                                    obj.seek_fast(target + step * 100_000);
                                                }
                                                obj.seek(target);
                                                // Play right after the drag, as a user would.
                                                obj.play();
                                            }
                                        } else {
                                            obj.play();
                                        }
                                        debug!("autoplay toggle: playing {}", obj.is_playing());
                                        glib::ControlFlow::Continue
                                    },
                                );
                            }
                        }
                    });
                }
            }

            if !self.seek_in_flight.get() {
                return;
            }
            debug!("seek done");
            self.seek_in_flight.set(false);
            if let Some((timestamp, fast)) = self.pending_seek.take() {
                self.do_seek(timestamp, fast);
            } else {
                obj.seek_success();
                obj.update(self.position());
            }
        }

        fn do_seek(&self, timestamp: i64, fast: bool) {
            let flags = if fast {
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT | gst::SeekFlags::SNAP_NEAREST
            } else {
                gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE
            };
            let position = self.to_timeline(timestamp.clamp(0, self.total_duration()));

            self.seek_in_flight.set(true);
            self.seek_started.set(Some(std::time::Instant::now()));
            debug!("seek to {position} (fast {fast})");
            if let Err(err) = self.pipeline().seek_simple(flags, position) {
                debug!("seek to {position} failed: {err}");
                self.seek_in_flight.set(false);
                self.obj().seek_failed();
            }
        }

        /// Forgets a seek that never completed, so later seeks and position updates don't wait
        /// on it forever.
        fn expire_stuck_seek(&self) {
            const SEEK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
            if !self.seek_in_flight.get()
                || self
                    .seek_started
                    .get()
                    .is_none_or(|started| started.elapsed() < SEEK_TIMEOUT)
            {
                return;
            }
            warn!("a seek never completed, carrying on without it");
            self.seek_in_flight.set(false);
            if let Some((timestamp, fast)) = self.pending_seek.take() {
                self.do_seek(timestamp, fast);
            } else {
                self.obj().seek_success();
            }
        }

        /// Current playback position, in the mode's time.
        fn position(&self) -> i64 {
            self.pipeline()
                .query_position::<gst::ClockTime>()
                .map(|position| self.timeline_to_mode(position))
                .unwrap_or(0)
                .clamp(0, self.total_duration())
        }

        /// Duration in the mode's time: the source video in the Trimmer, the edit in the Editor.
        pub(super) fn total_duration(&self) -> i64 {
            if self.editor.get() {
                self.editor_end() as i64 / 1000
            } else {
                self.sources
                    .borrow()
                    .first()
                    .and_then(|source| source.asset.duration())
                    .map_or(0, |duration| duration.useconds() as i64)
            }
        }

        /// The Trimmer works in source time, which the speed scales onto the timeline.
        fn time_scale(&self) -> f64 {
            if self.editor.get() {
                1.
            } else {
                self.speed.get()
            }
        }

        fn to_timeline(&self, timestamp: i64) -> gst::ClockTime {
            let nanoseconds = timestamp.max(0) as f64 * 1000. / self.time_scale();
            gst::ClockTime::from_nseconds(nanoseconds as u64)
        }

        fn timeline_to_mode(&self, position: gst::ClockTime) -> i64 {
            (position.nseconds() as f64 / 1000. * self.time_scale()) as i64
        }

        fn start_polling(&self) {
            if self.poll.borrow().is_some() {
                return;
            }
            let source = glib::timeout_add_local(POSITION_POLL, {
                let obj = self.obj().downgrade();
                move || {
                    let Some(obj) = obj.upgrade() else {
                        return glib::ControlFlow::Break;
                    };
                    let imp = obj.imp();
                    imp.expire_stuck_seek();
                    if !imp.seek_in_flight.get() {
                        obj.update(imp.position());
                    }
                    let logged_at = imp.frames_logged_at.get();
                    if logged_at.is_none_or(|at| at.elapsed().as_secs() >= 1) {
                        let frames = imp
                            .frames_to_sink
                            .swap(0, std::sync::atomic::Ordering::Relaxed);
                        let painted = imp.frames_painted.replace(0);
                        debug!("last second: {frames} video frames to sink, {painted} painted");
                        imp.frames_logged_at.set(Some(std::time::Instant::now()));
                    }
                    glib::ControlFlow::Continue
                }
            });
            self.poll.replace(Some(source));
        }

        fn stop_polling(&self) {
            if let Some(source) = self.poll.take() {
                source.remove();
            }
        }

        /// Sets the speed of `clip` so it plays `span` ns of source.
        fn apply_speed(&self, clip: &ges::Clip, span: u64, speed: f64, has_audio: bool) {
            for effect in clip.top_effects() {
                let _ = clip.remove(&effect);
            }
            // Keep the clip valid at every step: at rate 1 it can't play more than the span.
            let shortest = (span as f64 / speed.max(1.)) as u64;
            clip.set_duration(gst::ClockTime::from_nseconds(shortest));
            if speed != 1. {
                let mut descriptions = vec![format!("videorate rate={speed}")];
                if has_audio {
                    // Changes the tempo without changing the pitch. scaletempo ships with the
                    // base plugins everywhere, unlike SoundTouch's pitch.
                    descriptions.push(format!("scaletempo rate={speed}"));
                }
                for description in descriptions {
                    match ges::Effect::new(&description) {
                        Ok(effect) => {
                            if let Err(err) = clip.add_top_effect(&effect, -1) {
                                warn!("could not add {description}: {err}");
                            } else if description.starts_with("videorate") {
                                fix_rate_segments(&effect, speed);
                            }
                        }
                        Err(err) => warn!("could not create {description}: {err}"),
                    }
                }
            }
            clip.set_duration(gst::ClockTime::from_nseconds((span as f64 / speed) as u64));
        }

        /// Trimmer: speed of the whole clip.
        pub(super) fn set_speed(&self, speed: f64) {
            if self.editor.get() {
                return;
            }
            {
                let mut segments = self.segments.borrow_mut();
                let Some(segment) = segments.first_mut() else {
                    self.speed.set(speed);
                    return;
                };
                let sources = self.sources.borrow();
                let source = &sources[segment.source];
                let span = source.asset.duration().map_or(0, |d| d.nseconds());
                self.apply_speed(&segment.clip, span, speed, source.has_audio);
                segment.speed = speed;
            }
            self.speed.set(speed);
            // The playhead keeps its source position; the deferred seek maps it at the new speed.
            self.request_commit();
        }

        pub(super) fn set_video_audio_muted(&self, muted: bool) {
            self.video_muted.set(muted);
            for segment in self.segments.borrow().iter() {
                self.apply_audio_settings(&segment.clip);
            }
        }

        pub(super) fn set_video_volume(&self, volume: f64) {
            self.video_volume.set(volume);
            for segment in self.segments.borrow().iter() {
                self.apply_audio_settings(&segment.clip);
            }
            self.timeline().commit();
        }

        /// Applies the mute and volume of the videos' own audio to a clip.
        fn apply_audio_settings(&self, clip: &ges::Clip) {
            // Clips without audio have neither property.
            let _ = clip.set_child_property("mute", self.video_muted.get().to_value());
            let _ = clip.set_child_property("volume", self.video_volume.get().to_value());
        }

        /// Length of all the added videos at normal speed, in µs.
        pub(super) fn sources_duration(&self) -> i64 {
            let sources = self.sources.borrow();
            self.used_sources()
                .into_iter()
                .filter_map(|index| sources[index].asset.duration())
                .map(|duration| duration.useconds() as i64)
                .sum()
        }

        /// Sources that still have a segment in the edit; undoing an addition keeps its source.
        fn used_sources(&self) -> Vec<usize> {
            let mut used: Vec<usize> = self
                .segments
                .borrow()
                .iter()
                .map(|segment| segment.source)
                .collect();
            used.sort_unstable();
            used.dedup();
            used
        }

        pub(super) fn source_path(&self, index: usize) -> Option<std::path::PathBuf> {
            let sources = self.sources.borrow();
            let uri = sources.get(index)?.asset.id();
            gio::File::for_uri(&uri).path()
        }

        pub(super) fn source_has_audio(&self, index: usize) -> bool {
            self.sources
                .borrow()
                .get(index)
                .is_some_and(|source| source.has_audio)
        }

        // Editor.

        pub(super) fn is_editor(&self) -> bool {
            self.editor.get()
        }

        pub(super) fn source_count(&self) -> usize {
            self.used_sources()
                .len()
                .max(self.sources.borrow().len().min(1))
        }

        /// Switches to the Editor. The Trimmer clip, at its speed, becomes the first segment.
        pub(super) fn enter_editor(&self) {
            if self.editor.get() {
                return;
            }
            let position = self.to_timeline(self.obj().timestamp()).useconds() as i64;
            self.editor.set(true);
            self.edited();
            self.obj().seek(position);
        }

        /// Switches back to the Trimmer with the whole first video at normal speed.
        pub(super) fn enter_trimmer(&self) {
            if !self.editor.get() {
                return;
            }
            let layer = self.video_layer.get().unwrap();
            for segment in self.segments.take() {
                let _ = layer.remove_clip(&segment.clip);
            }
            self.remove_end_fade_clip();
            self.undo.borrow_mut().clear();
            self.redo.borrow_mut().clear();
            self.sources.borrow_mut().truncate(1);
            self.editor.set(false);
            self.speed.set(1.);
            match self.add_source_clip(0, 0) {
                Ok(segment) => self.segments.replace(vec![segment]),
                Err(err) => {
                    warn!("could not restore the Trimmer clip: {err}");
                    return;
                }
            };
            self.timeline().commit_sync();
            self.obj().seek(0);
            self.obj().emit_by_name::<()>("timeline-changed", &[]);
        }

        /// Appends a whole video at the end of the edit.
        pub(super) fn add_video(&self, file: &gio::File) -> Result<(), glib::Error> {
            self.enter_editor();
            let before = self.snapshot();
            let source = self.load_source(file)?;
            let segment = self.add_source_clip(source, self.segments_end())?;
            self.segments.borrow_mut().push(segment);
            self.commit_edit(before);
            Ok(())
        }

        pub(super) fn segments(&self) -> Vec<SegmentInfo> {
            let sources = self.sources.borrow();
            self.segments
                .borrow()
                .iter()
                .map(|segment| {
                    let source = &sources[segment.source];
                    let inpoint = segment.clip.inpoint().useconds() as f64;
                    let duration = segment.clip.duration().useconds() as f64;
                    let source_duration =
                        source.asset.duration().map_or(0., |d| d.useconds() as f64);
                    let used_end = inpoint + duration * segment.speed;
                    SegmentInfo {
                        start: segment.clip.start().useconds() as i64,
                        duration: duration as i64,
                        speed: segment.speed,
                        source: segment.source,
                        name: source.name.clone(),
                        inpoint: inpoint as i64,
                        extend_left: (inpoint / segment.speed) as i64,
                        extend_right: ((source_duration - used_end).max(0.) / segment.speed) as i64,
                    }
                })
                .collect()
        }

        /// End of the last segment, in nanoseconds.
        fn segments_end(&self) -> u64 {
            self.segments
                .borrow()
                .iter()
                .map(|segment| (segment.clip.start() + segment.clip.duration()).nseconds())
                .max()
                .unwrap_or(0)
        }

        /// End of the edit including the fade to black, in nanoseconds.
        fn editor_end(&self) -> u64 {
            let fade_end = self
                .end_fade_clip
                .borrow()
                .as_ref()
                .map_or(0, |clip| (clip.start() + clip.duration()).nseconds());
            self.segments_end().max(fade_end)
        }

        /// Splits the segment under `position` (µs). Returns whether a segment was split.
        pub(super) fn split(&self, position: i64) -> bool {
            let before = self.snapshot();
            let position = position.max(0) as u64 * 1000;
            {
                let mut segments = self.segments.borrow_mut();
                let Some(index) = segments.iter().position(|segment| {
                    let start = segment.clip.start().nseconds();
                    start < position && position < start + segment.clip.duration().nseconds()
                }) else {
                    return false;
                };
                let segment = &segments[index];
                match segment.clip.split(position) {
                    Ok(clip) => {
                        self.apply_audio_settings(&clip);
                        let new = Segment {
                            clip,
                            source: segment.source,
                            speed: segment.speed,
                        };
                        segments.insert(index + 1, new);
                    }
                    Err(err) => {
                        warn!("could not split: {err}");
                        return false;
                    }
                }
            }
            self.commit_edit(before);
            true
        }

        /// Removes a segment and closes the gap it leaves.
        pub(super) fn remove_segment(&self, index: usize) {
            let before = self.snapshot();
            {
                let mut segments = self.segments.borrow_mut();
                if index >= segments.len() {
                    return;
                }
                let removed = segments.remove(index);
                let start = removed.clip.start();
                let duration = removed.clip.duration();
                let _ = self.video_layer.get().unwrap().remove_clip(&removed.clip);
                // Shift from left to right so moved segments never pile onto each other.
                let mut later: Vec<&Segment> = segments
                    .iter()
                    .filter(|segment| segment.clip.start() > start)
                    .collect();
                later.sort_by_key(|segment| segment.clip.start());
                for segment in later {
                    let shifted = segment.clip.start().saturating_sub(duration).max(start);
                    segment.clip.set_start(shifted);
                }
            }
            self.commit_edit(before);
        }

        /// Moves a segment to `start` (µs). Returns false if that position is not allowed
        /// (a segment would lie entirely within another, or three would overlap).
        pub(super) fn move_segment(&self, index: usize, start: i64) -> bool {
            let before = self.snapshot();
            let moved = {
                let segments = self.segments.borrow();
                let Some(segment) = segments.get(index) else {
                    return false;
                };
                segment
                    .clip
                    .set_start(gst::ClockTime::from_useconds(start.max(0) as u64))
            };
            if moved {
                self.commit_edit(before);
            }
            moved
        }

        /// Moves a segment edge to `position` (µs), trimming the source.
        pub(super) fn trim_segment(&self, index: usize, edge: SegmentEdge, position: i64) -> bool {
            let before = self.snapshot();
            let trimmed = {
                let segments = self.segments.borrow();
                let Some(segment) = segments.get(index) else {
                    return false;
                };
                let edge = match edge {
                    SegmentEdge::Start => ges::Edge::Start,
                    SegmentEdge::End => ges::Edge::End,
                };
                ges::prelude::TimelineElementExt::edit(
                    &segment.clip,
                    &[],
                    -1,
                    ges::EditMode::Trim,
                    edge,
                    position.max(0) as u64 * 1000,
                )
            };
            if trimmed {
                self.commit_edit(before);
            }
            trimmed
        }

        /// Changes a segment's speed, keeping the source it plays. The given edge stays put.
        pub(super) fn set_segment_speed(&self, index: usize, speed: f64, fixed: SegmentEdge) {
            let before = self.snapshot();
            {
                let mut segments = self.segments.borrow_mut();
                let Some(segment) = segments.get_mut(index) else {
                    return;
                };
                let clip = segment.clip.clone();
                let start = clip.start().nseconds();
                let end = start + clip.duration().nseconds();
                let span = (clip.duration().nseconds() as f64 * segment.speed) as u64;
                let has_audio = self.sources.borrow()[segment.source].has_audio;
                self.apply_speed(&clip, span, speed, has_audio);
                segment.speed = speed;
                if fixed == SegmentEdge::End {
                    let duration = clip.duration().nseconds();
                    clip.set_start(gst::ClockTime::from_nseconds(end.saturating_sub(duration)));
                }
            }
            self.commit_edit(before);
        }

        fn snapshot(&self) -> Vec<SegmentState> {
            self.segments
                .borrow()
                .iter()
                .map(|segment| SegmentState {
                    source: segment.source,
                    start: segment.clip.start().nseconds(),
                    inpoint: segment.clip.inpoint().nseconds(),
                    duration: segment.clip.duration().nseconds(),
                    speed: segment.speed,
                })
                .collect()
        }

        /// Records the state before an edit that succeeded, and applies it.
        fn commit_edit(&self, before: Vec<SegmentState>) {
            let mut undo = self.undo.borrow_mut();
            undo.push(before);
            if undo.len() > MAX_UNDO {
                undo.remove(0);
            }
            drop(undo);
            self.redo.borrow_mut().clear();
            self.edited();
        }

        pub(super) fn undo(&self) -> bool {
            let Some(state) = self.undo.borrow_mut().pop() else {
                return false;
            };
            self.redo.borrow_mut().push(self.snapshot());
            self.restore(&state);
            true
        }

        pub(super) fn redo(&self) -> bool {
            let Some(state) = self.redo.borrow_mut().pop() else {
                return false;
            };
            self.undo.borrow_mut().push(self.snapshot());
            self.restore(&state);
            true
        }

        /// Rebuilds the segments from a snapshot.
        fn restore(&self, state: &[SegmentState]) {
            let layer = self.video_layer.get().unwrap();
            for segment in self.segments.take() {
                let _ = layer.remove_clip(&segment.clip);
            }
            let mut segments = Vec::new();
            for saved in state {
                let (asset, has_audio) = {
                    let sources = self.sources.borrow();
                    let source = &sources[saved.source];
                    (source.asset.clone(), source.has_audio)
                };
                let span = (saved.duration as f64 * saved.speed) as u64;
                // Never longer than the final clip, so it can't cover a neighbour on the way.
                let initial = span.min(saved.duration);
                let clip = match layer.add_asset(
                    &asset,
                    gst::ClockTime::from_nseconds(saved.start),
                    gst::ClockTime::from_nseconds(saved.inpoint),
                    gst::ClockTime::from_nseconds(initial),
                    ges::TrackType::UNKNOWN,
                ) {
                    Ok(clip) => clip,
                    Err(err) => {
                        warn!("could not restore a segment: {err}");
                        continue;
                    }
                };
                self.apply_audio_settings(&clip);
                self.apply_speed(&clip, span, saved.speed, has_audio);
                segments.push(Segment {
                    clip,
                    source: saved.source,
                    speed: saved.speed,
                });
            }
            self.segments.replace(segments);
            self.edited();
        }

        pub(super) fn can_undo(&self) -> bool {
            !self.undo.borrow().is_empty()
        }

        pub(super) fn can_redo(&self) -> bool {
            !self.redo.borrow().is_empty()
        }

        pub(super) fn set_end_fade(&self, enabled: bool) {
            self.end_fade.set(enabled);
            if self.editor.get() {
                self.edited();
            }
        }

        fn remove_end_fade_clip(&self) {
            if let Some(clip) = self.end_fade_clip.take() {
                let _ = self.video_layer.get().unwrap().remove_clip(&clip);
            }
        }

        /// Fades the end of the edit to black and silence by overlapping a black, silent clip
        /// with the last segment: the automatic transition does the crossfade.
        fn refresh_end_fade(&self) {
            self.remove_end_fade_clip();
            if !self.end_fade.get() || !self.editor.get() {
                return;
            }
            let segments = self.segments.borrow();
            let Some(last) = segments
                .iter()
                .max_by_key(|segment| (segment.clip.start() + segment.clip.duration()).nseconds())
            else {
                return;
            };
            let end = (last.clip.start() + last.clip.duration()).nseconds();
            // Never reach past the last segment's start, or three clips could overlap.
            let fade = END_FADE_NS.min(last.clip.duration().nseconds() / 2);
            let asset = match ges::Asset::request::<ges::TestClip>(None) {
                Ok(asset) => asset,
                Err(_) => {
                    warn!("could not create the fade-out clip");
                    return;
                }
            };
            match self.video_layer.get().unwrap().add_asset(
                &asset,
                gst::ClockTime::from_nseconds(end - fade),
                gst::ClockTime::ZERO,
                gst::ClockTime::from_nseconds(fade + END_FADE_OVERHANG_NS),
                ges::TrackType::UNKNOWN,
            ) {
                Ok(clip) => {
                    if let Some(test_clip) = clip.downcast_ref::<ges::TestClip>() {
                        test_clip.set_vpattern(ges::VideoTestPattern::Black);
                        // A muted test clip has no audio for the transition to fade into.
                        test_clip.set_volume(0.);
                    }
                    self.end_fade_clip.replace(Some(clip));
                }
                Err(err) => warn!("could not add the fade-out clip: {err}"),
            }
        }

        /// Commits an edit and tells the widgets.
        fn edited(&self) {
            self.refresh_end_fade();
            // Widgets read the clips directly, so they update before the commit lands.
            self.obj().emit_by_name::<()>("timeline-changed", &[]);
            self.request_commit();
        }

        /// Commits pending timeline changes once the main loop is idle, then seeks in place.
        ///
        /// A synchronous commit blocks the interface for up to a few hundred milliseconds
        /// while GES rebuilds the composition, and a drag or an edit that also moves the music
        /// would commit several times. The seek shows the frame at the playhead after the edit,
        /// and makes audio added while paused part of what plays.
        fn request_commit(&self) {
            if self.commit_scheduled.replace(true) {
                return;
            }
            glib::idle_add_local_once({
                let obj = self.obj().downgrade();
                move || {
                    let Some(obj) = obj.upgrade() else {
                        return;
                    };
                    let imp = obj.imp();
                    imp.commit_scheduled.set(false);
                    imp.timeline().commit();
                    obj.seek(obj.timestamp().min(imp.total_duration()));
                }
            });
        }

        // Music.

        pub(super) fn set_music(&self, uri: Option<&str>) {
            if let Some(clip) = self.music_clip.take() {
                let _ = self.music_layer.get().unwrap().remove_clip(&clip);
            }
            let asset = uri.and_then(|uri| match ges::UriClipAsset::request_sync(uri) {
                Ok(asset) => Some(asset),
                Err(err) => {
                    warn!("could not load the music {uri}: {err}");
                    None
                }
            });
            debug!(
                "music: {} (duration {:?})",
                uri.unwrap_or("removed"),
                asset.as_ref().and_then(|asset| asset.duration())
            );
            self.music_asset.replace(asset);
            self.apply_audio_change();
        }

        /// Commits a change to the audio and seeks in place. Without the seek, a composition
        /// rebuilt while paused keeps playing the old audio: music added then stays silent.
        fn apply_audio_change(&self) {
            self.request_commit();
        }

        /// Places the music. `offset` is where it starts, in the mode's time; with `fade_end`
        /// (in the mode's time too) it fades out during the last second before it. `volume` is
        /// 1 for unchanged.
        pub(super) fn update_music(&self, offset: i64, fade_end: Option<i64>, volume: f64) {
            let Some(asset) = self.music_asset.borrow().clone() else {
                return;
            };
            let music_duration = asset.duration().unwrap_or(gst::ClockTime::ZERO).nseconds();

            // The music keeps its own speed, so only its placement scales with the video speed.
            let start = offset as f64 * 1000. / self.time_scale();
            let (start, inpoint) = if start < 0. {
                (0, (-start) as u64)
            } else {
                (start as u64, 0)
            };
            let layer = self.music_layer.get().unwrap();
            // The music never outlasts the video, so the preview and the export end with it.
            let video_end = self.editor_end();
            if inpoint >= music_duration || start >= video_end {
                if let Some(clip) = self.music_clip.take() {
                    let _ = layer.remove_clip(&clip);
                }
                self.apply_audio_change();
                return;
            }
            let duration = (music_duration - inpoint).min(video_end - start);
            debug!(
                "music: placed at {start} ns, in-point {inpoint} ns, duration {duration} ns, \
fade end {fade_end:?}, editor {}",
                self.editor.get()
            );

            let clip = self.music_clip.borrow().clone();
            let clip = match clip {
                Some(clip) => {
                    // Shrink first so the in-point change never overruns the music.
                    clip.set_duration(gst::ClockTime::from_nseconds(
                        duration.min(clip.duration().nseconds()),
                    ));
                    clip.set_inpoint(gst::ClockTime::from_nseconds(inpoint));
                    clip.set_duration(gst::ClockTime::from_nseconds(duration));
                    clip.set_start(gst::ClockTime::from_nseconds(start));
                    clip
                }
                None => match layer.add_asset(
                    &asset,
                    gst::ClockTime::from_nseconds(start),
                    gst::ClockTime::from_nseconds(inpoint),
                    gst::ClockTime::from_nseconds(duration),
                    ges::TrackType::AUDIO,
                ) {
                    Ok(clip) => {
                        self.music_clip.replace(Some(clip.clone()));
                        clip
                    }
                    Err(err) => {
                        warn!("could not add the music clip: {err}");
                        return;
                    }
                },
            };

            for element in clip.children(false) {
                let Some(source) = element.downcast_ref::<ges::TrackElement>() else {
                    continue;
                };
                if source.track_type() != ges::TrackType::AUDIO {
                    continue;
                }
                let _ = source.remove_control_binding("volume");
                let _ = source
                    .upcast_ref::<ges::TimelineElement>()
                    .set_child_property("volume", volume.to_value());
                let Some(fade_end) = fade_end else {
                    continue;
                };

                // Control points are in the source's internal time; the music has no speed
                // effects, so that is its in-point plus the time since the clip start.
                let fade_end = (fade_end as f64 * 1000. / self.time_scale()) as u64;
                let to_internal = |timeline: u64| inpoint + timeline.saturating_sub(start);
                let control = gst_controller::InterpolationControlSource::new();
                control.set_mode(gst_controller::InterpolationMode::Linear);
                control.set(
                    gst::ClockTime::from_nseconds(to_internal(
                        fade_end.saturating_sub(MUSIC_FADE_NS),
                    )),
                    volume,
                );
                control.set(gst::ClockTime::from_nseconds(to_internal(fade_end)), 0.);
                if !source.set_control_source(&control, "volume", "direct-absolute") {
                    warn!("could not fade the music out");
                }
            }

            self.apply_audio_change();
        }

        // Export.

        pub(super) fn render(
            &self,
            path: &Path,
            settings: ExportSettings,
            callback: RenderCallback,
        ) -> Result<(), String> {
            if self.render.borrow().is_some() {
                return Err("an export is already running".to_owned());
            }
            let obj = self.obj();
            if obj.is_playing() {
                obj.pause();
            }

            let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("mp4");
            let profile = encoding_profile(extension);
            let uri = gio::File::for_path(path).uri();

            let pipeline = self.pipeline();
            let _ = pipeline.set_state(gst::State::Null);
            let preview_caps = self.apply_export_settings(settings);
            // Seeks can't land while rendering; the preview seeks again afterwards.
            self.seek_in_flight.set(false);
            self.pending_seek.set(None);
            pipeline
                .set_render_settings(&uri, &profile)
                .map_err(|err| format!("unsupported output format: {err}"))?;
            pipeline
                .set_mode(ges::PipelineFlags::RENDER)
                .map_err(|err| err.to_string())?;

            let poll = glib::timeout_add_local(RENDER_POLL, {
                let obj = obj.downgrade();
                move || {
                    let Some(obj) = obj.upgrade() else {
                        return glib::ControlFlow::Break;
                    };
                    let imp = obj.imp();
                    let total = imp.editor_end().max(1) as f64;
                    if let Some(position) = imp.pipeline().query_position::<gst::ClockTime>() {
                        if let Some(render) = imp.render.borrow().as_ref() {
                            (render.callback)(RenderEvent::Progress(
                                (position.nseconds() as f64 / total).clamp(0., 1.),
                            ));
                        }
                    }
                    glib::ControlFlow::Continue
                }
            });
            self.render.replace(Some(Render {
                callback,
                path: path.to_owned(),
                poll: Some(poll),
                preview_caps,
            }));

            if pipeline.set_state(gst::State::Playing).is_err() {
                self.finish_render(RenderEvent::Failed("could not start the export".to_owned()));
            }
            Ok(())
        }

        fn video_track(&self) -> Option<ges::Track> {
            self.timeline()
                .tracks()
                .into_iter()
                .find(|track| track.track_type() == ges::TrackType::VIDEO)
        }

        /// Sets the export frame rate and size on the video track. Returns the previous caps,
        /// or `None` when nothing changed.
        fn apply_export_settings(&self, settings: ExportSettings) -> Option<gst::Caps> {
            if settings == ExportSettings::default() {
                return None;
            }
            let track = self.video_track()?;
            let previous = track.restriction_caps()?;
            let mut caps = previous.copy();
            {
                let caps = caps.make_mut();
                let structure = caps.structure_mut(0)?;
                if let Some((width, height)) = settings.size {
                    structure.set("width", width);
                    structure.set("height", height);
                }
                if let Some((numerator, denominator)) = settings.fps {
                    structure.set("framerate", gst::Fraction::new(numerator, denominator));
                }
            }
            debug!("export caps: {caps}");
            track.set_restriction_caps(&caps);
            self.timeline().commit_sync();
            Some(previous)
        }

        pub(super) fn cancel_render(&self) {
            if self.render.borrow().is_some() {
                self.finish_render(RenderEvent::Failed("cancelled".to_owned()));
            }
        }

        fn finish_render(&self, event: RenderEvent) {
            let Some(mut render) = self.render.take() else {
                return;
            };
            if let Some(poll) = render.poll.take() {
                poll.remove();
            }

            let pipeline = self.pipeline();
            let _ = pipeline.set_state(gst::State::Null);
            if let (Some(caps), Some(track)) = (render.preview_caps.take(), self.video_track()) {
                track.set_restriction_caps(&caps);
                self.timeline().commit_sync();
            }
            if !matches!(event, RenderEvent::Done) {
                let _ = std::fs::remove_file(&render.path);
            }
            if let Err(err) = pipeline.set_mode(ges::PipelineFlags::FULL_PREVIEW) {
                warn!("could not return to preview: {err}");
            }
            // Go back to the playhead once the preview has prerolled; seeking before that fails.
            let obj = self.obj();
            self.seek_in_flight.set(true);
            self.pending_seek.set(Some((obj.timestamp(), false)));
            let _ = pipeline.set_state(gst::State::Paused);

            (render.callback)(event);
        }
    }

    /// Works around GES handing a speed effect's `videorate` a segment in timeline time while
    /// its buffers are in source time. After a seek into a sped-up clip the segment start is
    /// divided by the speed, so every frame falls outside it and the picture freezes while the
    /// audio plays on. Scaling the segment back by the speed makes it match the buffers again.
    fn fix_rate_segments(effect: &ges::Effect, speed: f64) {
        let Some(element) = effect.element() else {
            warn!("the speed effect has no element to fix");
            return;
        };
        let videorate = match element.downcast_ref::<gst::Bin>() {
            Some(bin) => bin.iterate_recurse().into_iter().flatten().find(|child| {
                child
                    .factory()
                    .is_some_and(|factory| factory.name() == "videorate")
            }),
            None => Some(element.clone()),
        };
        let Some(pad) = videorate.and_then(|videorate| videorate.static_pad("sink")) else {
            warn!("could not find the speed effect's videorate");
            return;
        };
        pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
            let Some(gst::PadProbeData::Event(event)) = &mut info.data else {
                return gst::PadProbeReturn::Ok;
            };
            let gst::EventView::Segment(segment_event) = event.view() else {
                return gst::PadProbeReturn::Ok;
            };
            let Ok(mut segment) = segment_event.segment().clone().downcast::<gst::ClockTime>()
            else {
                return gst::PadProbeReturn::Ok;
            };
            let scale = |time: Option<gst::ClockTime>| {
                time.map(|time| {
                    gst::ClockTime::from_nseconds((time.nseconds() as f64 * speed) as u64)
                })
            };
            segment.set_start(scale(segment.start()));
            segment.set_stop(scale(segment.stop()));
            segment.set_time(scale(segment.time()));
            segment.set_position(scale(segment.position()));
            let seqnum = event.seqnum();
            *event = gst::event::Segment::builder(&segment)
                .seqnum(seqnum)
                .build();
            gst::PadProbeReturn::Ok
        });
    }

    /// Encoding settings for an output file extension. Encoders are picked explicitly with
    /// constant-quality settings; the defaults are fixed, low bitrates.
    fn encoding_profile(extension: &str) -> gst_pbutils::EncodingContainerProfile {
        let (container, video, video_encoder, video_properties, audio) = match extension {
            "webm" => (
                gst::Caps::builder("video/webm").build(),
                gst::Caps::builder("video/x-vp9").build(),
                "vp9enc",
                gst_pbutils::ElementProperties::builder_general()
                    .field("deadline", 1i64)
                    .field("cpu-used", 4i32)
                    .field("row-mt", true)
                    // End usage 2 is "cq", constant quality.
                    .field("end-usage", 2i32)
                    .field("cq-level", 32i32)
                    .build(),
                gst::Caps::builder("audio/x-opus").build(),
            ),
            extension => (
                if extension == "mkv" {
                    gst::Caps::builder("video/x-matroska").build()
                } else {
                    gst::Caps::builder("video/quicktime")
                        .field("variant", "iso")
                        .build()
                },
                gst::Caps::builder("video/x-h264").build(),
                "x264enc",
                // Enum properties take their numeric values: pass 5 is "qual", constant quality
                // with the quantizer as the target; speed preset 4 is "faster".
                gst_pbutils::ElementProperties::builder_general()
                    .field("pass", 5i32)
                    .field("quantizer", 17u32)
                    .field("speed-preset", 4i32)
                    .build(),
                gst::Caps::builder("audio/mpeg")
                    .field("mpegversion", 4i32)
                    .build(),
            ),
        };

        // Without the preferred encoder (e.g. no x264 on stock Fedora), let encodebin pick any
        // encoder for the format with its defaults.
        let video_profile = if gst::ElementFactory::find(video_encoder).is_some() {
            gst_pbutils::EncodingVideoProfile::builder(&video)
                .preset_name(video_encoder)
                .element_properties(video_properties)
                .build()
        } else {
            warn!("{video_encoder} is missing, exporting with another encoder");
            gst_pbutils::EncodingVideoProfile::builder(&video).build()
        };

        gst_pbutils::EncodingContainerProfile::builder(&container)
            .add_profile(video_profile)
            .add_profile(gst_pbutils::EncodingAudioProfile::builder(&audio).build())
            .build()
    }
}

glib::wrapper! {
    pub struct VtEngine(ObjectSubclass<imp::VtEngine>)
        @extends gtk::MediaStream,
        @implements gdk::Paintable;
}

impl Default for VtEngine {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl VtEngine {
    /// The paintable showing the video.
    pub fn paintable(&self) -> Option<gdk::Paintable> {
        self.imp().paintable.get().cloned()
    }

    pub fn open(&self, file: &gio::File) {
        self.imp().open(file);
    }

    /// Seeks to the nearest keyframe: fast enough to follow a drag.
    pub fn seek_fast(&self, timestamp: i64) {
        self.imp().next_seek_fast.set(true);
        self.seek(timestamp);
    }

    /// Duration in the mode's time (source video in the Trimmer, edit in the Editor).
    pub fn total_duration(&self) -> i64 {
        self.imp().total_duration()
    }

    pub fn set_speed(&self, speed: f64) {
        self.imp().set_speed(speed);
    }

    pub fn set_video_audio_muted(&self, muted: bool) {
        self.imp().set_video_audio_muted(muted);
    }

    pub fn set_video_volume(&self, volume: f64) {
        self.imp().set_video_volume(volume);
    }

    pub fn source_path(&self, index: usize) -> Option<std::path::PathBuf> {
        self.imp().source_path(index)
    }

    pub fn sources_duration(&self) -> i64 {
        self.imp().sources_duration()
    }

    pub fn source_has_audio(&self, index: usize) -> bool {
        self.imp().source_has_audio(index)
    }

    pub fn is_editor(&self) -> bool {
        self.imp().is_editor()
    }

    pub fn source_count(&self) -> usize {
        self.imp().source_count()
    }

    pub fn enter_editor(&self) {
        self.imp().enter_editor();
    }

    pub fn enter_trimmer(&self) {
        self.imp().enter_trimmer();
    }

    pub fn add_video(&self, file: &gio::File) -> Result<(), glib::Error> {
        self.imp().add_video(file)
    }

    pub fn segments(&self) -> Vec<SegmentInfo> {
        self.imp().segments()
    }

    pub fn split(&self, position: i64) -> bool {
        self.imp().split(position)
    }

    pub fn remove_segment(&self, index: usize) {
        self.imp().remove_segment(index);
    }

    pub fn move_segment(&self, index: usize, start: i64) -> bool {
        self.imp().move_segment(index, start)
    }

    pub fn trim_segment(&self, index: usize, edge: SegmentEdge, position: i64) -> bool {
        self.imp().trim_segment(index, edge, position)
    }

    pub fn set_segment_speed(&self, index: usize, speed: f64, fixed: SegmentEdge) {
        self.imp().set_segment_speed(index, speed, fixed);
    }

    pub fn undo(&self) -> bool {
        self.imp().undo()
    }

    pub fn redo(&self) -> bool {
        self.imp().redo()
    }

    pub fn can_undo(&self) -> bool {
        self.imp().can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.imp().can_redo()
    }

    pub fn set_end_fade(&self, enabled: bool) {
        self.imp().set_end_fade(enabled);
    }

    pub fn set_music(&self, uri: Option<&str>) {
        self.imp().set_music(uri);
    }

    pub fn update_music(&self, offset: i64, fade_end: Option<i64>, volume: f64) {
        self.imp().update_music(offset, fade_end, volume);
    }

    /// Exports the edit to `path`, reporting through `callback` on the main thread.
    pub fn render(
        &self,
        path: &Path,
        settings: ExportSettings,
        callback: impl Fn(RenderEvent) + 'static,
    ) -> Result<(), String> {
        self.imp().render(path, settings, Box::new(callback))
    }

    pub fn cancel_render(&self) {
        self.imp().cancel_render();
    }
}
