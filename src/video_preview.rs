use std::{path::PathBuf, time::Duration};

use glib::subclass::prelude::*;
use gtk::{gio, glib};

mod imp {
    use super::*;
    use crate::{
        config::G_LOG_DOMAIN,
        editor_timeline::VtEditorTimeline,
        engine::VtEngine,
        knob::VtKnob,
        music_track::{VtMusicTrack, PEAK_INTERVAL},
        original_audio_track::VtOriginalAudioTrack,
        timeline::VtTimeline,
    };
    use gettextrs::gettext;
    use glib::{debug, subclass::Signal, warn, Properties};
    use gtk::{glib, prelude::*, subclass::prelude::*, CompositeTemplate};
    use std::{
        cell::{Cell, OnceCell, RefCell},
        marker::PhantomData,
        path::Path,
        process::Command,
        sync::{
            atomic::{AtomicU32, Ordering},
            OnceLock,
        },
    };

    /// Sample rate the music is decoded at to compute its waveform.
    const WAVEFORM_SAMPLE_RATE: i64 = 8000;

    #[derive(Debug, Default, CompositeTemplate, Properties)]
    #[properties(wrapper_type = super::VtVideoPreview)]
    #[template(resource = "/io/github/jjolmo/QuickVideoEditor/video_preview.ui")]
    pub struct VtVideoPreview {
        #[template_child]
        overlay: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        picture_video_preview: TemplateChild<gtk::Picture>,
        #[template_child]
        stack_video_preview: TemplateChild<gtk::Stack>,
        #[template_child]
        status_page_no_video: TemplateChild<adw::StatusPage>,
        #[template_child]
        button_play_pause: TemplateChild<gtk::Button>,
        #[template_child]
        button_play_pause_image: TemplateChild<gtk::Image>,
        #[template_child]
        label_current_time: TemplateChild<gtk::Label>,
        #[template_child]
        timeline: TemplateChild<VtTimeline>,
        #[template_child]
        editor_timeline: TemplateChild<VtEditorTimeline>,
        #[template_child]
        image_original_audio: TemplateChild<gtk::Image>,
        #[template_child]
        original_audio_track: TemplateChild<VtOriginalAudioTrack>,
        #[template_child]
        knob_original_volume: TemplateChild<VtKnob>,
        #[template_child]
        box_playback_controls: TemplateChild<gtk::Grid>,
        #[template_child]
        button_remove_music: TemplateChild<gtk::Button>,
        #[template_child]
        image_music: TemplateChild<gtk::Image>,
        #[template_child]
        spinner_music: TemplateChild<adw::Spinner>,
        #[template_child]
        music_track: TemplateChild<VtMusicTrack>,
        #[template_child]
        check_fade_out: TemplateChild<gtk::CheckButton>,
        #[template_child]
        box_music_controls: TemplateChild<gtk::Box>,
        #[template_child]
        knob_music_volume: TemplateChild<VtKnob>,

        #[property(get = Self::duration)]
        duration: PhantomData<i64>,
        #[property(get = Self::is_playing, set = Self::set_is_playing, explicit_notify)]
        is_playing: PhantomData<bool>,

        media_file: OnceCell<VtEngine>,
        start_end: Cell<Option<(u32, u32)>>,
        music_path: RefCell<Option<PathBuf>>,
        /// Copy of the music that the preview plays.
        music_preview_path: RefCell<Option<PathBuf>>,
        /// Bumped on every music change so stale background loads are ignored.
        music_generation: Cell<u32>,
        speed: Cell<f64>,
        frame_time: Cell<Option<Duration>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for VtVideoPreview {
        const NAME: &'static str = "VtVideoPreview";
        type Type = super::VtVideoPreview;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            VtMusicTrack::ensure_type();
            VtEditorTimeline::ensure_type();
            VtOriginalAudioTrack::ensure_type();
            VtKnob::ensure_type();
            Self::bind_template(klass);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for VtVideoPreview {
        fn properties() -> &'static [glib::ParamSpec] {
            Self::derived_properties()
        }

        fn property(&self, id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            self.derived_property(id, pspec)
        }

        fn set_property(&self, id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            self.derived_set_property(id, value, pspec);
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<[Signal; 5]> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                [
                    Signal::builder("set-start-end")
                        .param_types([glib::Type::U32, glib::Type::U32])
                        .build(),
                    Signal::builder("set-start")
                        .param_types([glib::Type::U32])
                        .build(),
                    Signal::builder("set-end")
                        .param_types([glib::Type::U32])
                        .build(),
                    Signal::builder("error").build(),
                    Signal::builder("set-speed")
                        .param_types([glib::Type::F64])
                        .build(),
                ]
            })
        }

        fn constructed(&self) {
            let obj = self.obj();
            self.parent_constructed();

            self.timeline.connect_local("set-start-end", false, {
                let obj = obj.downgrade();
                move |args| {
                    let obj = obj.upgrade().unwrap();
                    obj.emit_by_name_with_values("set-start-end", &args[1..])
                }
            });

            self.timeline.connect_local("set-start", false, {
                let obj = obj.downgrade();
                move |args| {
                    let obj = obj.upgrade().unwrap();
                    obj.emit_by_name_with_values("set-start", &args[1..])
                }
            });

            self.timeline.connect_local("set-end", false, {
                let obj = obj.downgrade();
                move |args| {
                    let obj = obj.upgrade().unwrap();
                    obj.emit_by_name_with_values("set-end", &args[1..])
                }
            });

            // Connect the play-pause button.
            self.button_play_pause.connect_clicked({
                let obj = obj.downgrade();
                move |_| {
                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();
                    let media_file = imp.media_file.get().unwrap();
                    if media_file.is_playing() {
                        media_file.pause();
                    } else {
                        media_file.play();
                    }
                }
            });

            self.speed.set(1.);
            self.timeline.connect_local("set-speed", false, {
                let obj = obj.downgrade();
                move |args| {
                    let speed = args[1].get::<f64>().unwrap();
                    let obj = obj.upgrade().unwrap();
                    obj.imp().apply_speed(speed);
                    obj.emit_by_name::<()>("set-speed", &[&speed]);
                    None
                }
            });

            self.knob_original_volume.configure_volume(
                2.,
                30,
                // Translators: tooltip of the original audio volume knob; the placeholder is a
                // percentage.
                &gettext(
                    "Original audio volume {}\nDrag up or down or scroll to change, \
double-click to reset",
                ),
            );
            self.knob_music_volume.configure_volume(
                2.,
                30,
                // Translators: tooltip of the music volume knob; the placeholder is a percentage.
                &gettext(
                    "Music volume {}\nDrag up or down or scroll to change, double-click to reset",
                ),
            );
            self.knob_music_volume
                .connect_local("value-changed", false, {
                    let obj = obj.downgrade();
                    move |_| {
                        let obj = obj.upgrade().unwrap();
                        obj.imp().update_music();
                        None
                    }
                });
            self.knob_original_volume
                .connect_local("value-changed", false, {
                    let obj = obj.downgrade();
                    move |args| {
                        let volume = args[1].get::<f64>().unwrap();
                        let obj = obj.upgrade().unwrap();
                        let imp = obj.imp();
                        imp.media_file.get().unwrap().set_video_volume(volume);
                        imp.original_audio_track.set_volume(volume);
                        None
                    }
                });

            self.button_remove_music.connect_clicked({
                let obj = obj.downgrade();
                move |_| {
                    let obj = obj.upgrade().unwrap();
                    obj.imp().remove_music();
                }
            });

            self.check_fade_out.connect_active_notify({
                let obj = obj.downgrade();
                move |check| {
                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();
                    imp.music_track.set_fade_out(check.is_active());
                    imp.update_music();
                }
            });

            self.music_track.connect_local("offset-changed", false, {
                let obj = obj.downgrade();
                move |_| {
                    let obj = obj.upgrade().unwrap();
                    obj.imp().update_music();
                    None
                }
            });

            // Media file callbacks.
            let media_file = VtEngine::default();
            media_file.connect_playing_notify({
                let obj = obj.downgrade();
                move |media_file| {
                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();

                    if media_file.is_playing() {
                        imp.button_play_pause_image
                            .set_icon_name(Some("media-playback-pause-symbolic"));
                    } else {
                        imp.button_play_pause_image
                            .set_icon_name(Some("media-playback-start-symbolic"));
                    }

                    obj.notify_is_playing();
                }
            });

            media_file.connect_error_notify({
                let obj = obj.downgrade();
                move |media_file| {
                    let error = media_file.error().unwrap();

                    warn!("Error in the playback engine: {}", error);

                    let obj = obj.upgrade().unwrap();
                    obj.emit_by_name::<()>("error", &[]);
                }
            });

            media_file.connect_prepared_notify({
                let obj = obj.downgrade();
                move |media_file| {
                    let obj = obj.upgrade().unwrap();

                    if media_file.error().is_some() {
                        return;
                    }

                    if !media_file.has_video() {
                        let imp = obj.imp();

                        imp.stack_video_preview
                            .set_visible_child(&*imp.status_page_no_video);
                    }

                    obj.imp()
                        .music_track
                        .set_video_duration(media_file.duration());

                    // GTK API is such that on "prepared" all media info is known and won't change.
                    obj.notify("duration");
                }
            });

            media_file.connect_timestamp_notify({
                let obj = obj.downgrade();
                move |media_file| {
                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();

                    let position = media_file.timestamp();
                    let mut seconds = position / 1_000_000;
                    let mut minutes = seconds / 60;
                    let hours = minutes / 60;
                    seconds %= 60;
                    minutes %= 60;

                    let time = if hours == 0 {
                        format!("{}:{:02}", minutes, seconds)
                    } else {
                        format!("{}:{:02}:{:02}", hours, minutes, seconds)
                    };

                    imp.label_current_time.set_text(&time);

                    imp.music_track.set_position(position);
                    imp.original_audio_track.set_position(position);
                }
            });

            self.picture_video_preview
                .set_paintable(media_file.paintable().as_ref());
            self.editor_timeline.set_engine(&media_file);
            media_file.connect_local("timeline-changed", false, {
                let obj = obj.downgrade();
                move |_| {
                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();
                    if imp.is_editor() {
                        imp.music_track
                            .set_video_duration(imp.media_file.get().unwrap().total_duration());
                        imp.update_music();
                        imp.refresh_original_audio();
                    }
                    None
                }
            });
            self.editor_timeline.connect_local("view-changed", false, {
                let obj = obj.downgrade();
                move |_| {
                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();
                    if imp
                        .media_file
                        .get()
                        .is_some_and(|engine| engine.is_editor())
                    {
                        let (start, length) = imp.editor_timeline.view();
                        imp.music_track.set_view(start, length);
                        imp.original_audio_track.set_view(start, length);
                    }
                    None
                }
            });
            self.timeline
                .set_property("media-file", media_file.upcast_ref::<gtk::MediaStream>());

            self.media_file.set(media_file).unwrap();
        }

        fn dispose(&self) {
            self.remove_music();

            let obj = self.obj();
            while let Some(child) = obj.first_child() {
                child.unparent();
            }
        }
    }

    /// Reads the music duration in microseconds. Fast: it does not decode the audio.
    fn probe_music_duration(path: &Path) -> Result<i64, String> {
        let output = Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "a:0", "-show_entries"])
            .args([
                "format=duration",
                "-of",
                "default=noprint_wrappers=1:nokey=1",
            ])
            .arg(path)
            .output()
            .map_err(|err| format!("could not run ffprobe: {err}"))?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned());
        }

        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|seconds| *seconds > 0.)
            .map(|seconds| (seconds * 1_000_000.) as i64)
            .ok_or_else(|| "the file has no audio duration".to_owned())
    }

    /// Decodes the music into normalized waveform peaks, one per `PEAK_INTERVAL`.
    ///
    /// Decoding a long mix takes seconds, so it is split into segments decoded in parallel.
    fn compute_music_peaks(path: &Path, duration: i64) -> Result<Vec<f32>, String> {
        const MIN_SEGMENT: i64 = 60_000_000;
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()) as i64;
        let segments = (duration / MIN_SEGMENT).clamp(1, threads.min(16));
        let segment_duration = duration / segments + 1;

        let results: Vec<Result<Vec<f32>, String>> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..segments)
                .map(|i| {
                    scope.spawn(move || decode_peaks(path, i * segment_duration, segment_duration))
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .unwrap_or_else(|_| Err("waveform thread panicked".to_owned()))
                })
                .collect()
        });

        let mut peaks = Vec::new();
        for result in results {
            peaks.extend(result?);
        }
        if peaks.is_empty() {
            return Err("the file has no audio samples".to_owned());
        }

        let loudest = peaks.iter().copied().fold(0., f32::max);
        if loudest > 0. {
            peaks.iter_mut().for_each(|peak| *peak /= loudest);
        }
        Ok(peaks)
    }

    fn decode_peaks(path: &Path, start: i64, duration: i64) -> Result<Vec<f32>, String> {
        let output = Command::new("ffmpeg")
            .args(["-v", "error", "-ss"])
            .arg(format!("{:.3}", start as f64 / 1_000_000.))
            .arg("-t")
            .arg(format!("{:.3}", duration as f64 / 1_000_000.))
            .arg("-i")
            .arg(path)
            .args(["-map", "0:a:0", "-ac", "1", "-ar"])
            .arg(WAVEFORM_SAMPLE_RATE.to_string())
            .args(["-f", "s16le", "-"])
            .output()
            .map_err(|err| format!("could not run ffmpeg: {err}"))?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned());
        }

        let samples_per_peak = (WAVEFORM_SAMPLE_RATE * PEAK_INTERVAL / 1_000_000) as usize;
        Ok(output
            .stdout
            .chunks(samples_per_peak * 2)
            .map(|chunk| {
                chunk
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| i16::from_le_bytes(*b).unsigned_abs())
                    .max()
                    .unwrap_or(0) as f32
            })
            .collect())
    }

    fn music_preview_dir() -> PathBuf {
        glib::user_cache_dir().join("quick-video-editor")
    }

    /// Deletes music preview copies left behind by instances that crashed or were killed.
    pub(super) fn remove_stale_music_previews() {
        let Ok(entries) = std::fs::read_dir(music_preview_dir()) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(pid) = name
                .to_str()
                .and_then(|name| name.strip_prefix("preview-"))
                .and_then(|rest| rest.split('-').next())
            else {
                continue;
            };
            if !Path::new("/proc").join(pid).exists() {
                debug!("removing stale music preview {:?}", entry.path());
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    /// Copies the music stream into a Matroska file for the preview, without re-encoding.
    fn remux_music_preview(path: &Path, preview_path: &Path) -> Result<(), String> {
        if let Some(parent) = preview_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("could not create {parent:?}: {err}"))?;
        }
        let output = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-i"])
            .arg(path)
            .args(["-map", "0:a:0", "-c:a", "copy", "-f", "matroska"])
            .arg(preview_path)
            .output()
            .map_err(|err| format!("could not run ffmpeg: {err}"))?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned());
        }
        Ok(())
    }

    impl WidgetImpl for VtVideoPreview {}

    impl VtVideoPreview {
        fn duration(&self) -> i64 {
            self.media_file.get().unwrap().duration()
        }

        fn is_playing(&self) -> bool {
            let Some(media_file) = self.media_file.get() else {
                return false;
            };
            media_file.is_playing()
        }

        fn set_is_playing(&self, value: bool) {
            let Some(media_file) = self.media_file.get() else {
                return;
            };
            media_file.set_playing(value);
        }

        pub fn open(&self, file: &gio::File) {
            self.media_file.get().unwrap().open(file);
        }

        pub fn set_frame_time_approx(&self, value: Duration) {
            self.frame_time.set(Some(value));
            self.timeline.set_frame_time_approx(value)
        }

        pub fn step_forward(&self) {
            self.step(1);
        }

        pub fn step_back(&self) {
            self.step(-1);
        }

        fn step(&self, direction: i64) {
            let engine = self.media_file.get().unwrap();
            if !engine.is_editor() {
                if direction > 0 {
                    self.timeline.step_forward();
                } else {
                    self.timeline.step_back();
                }
                return;
            }
            if let Some(frame_time) = self.frame_time.get() {
                let time = engine.timestamp() + direction * frame_time.as_micros() as i64;
                engine.seek(time.clamp(0, engine.total_duration()));
            }
        }

        pub fn set_start_end(&self, start_end: Option<(u32, u32)>) {
            self.start_end.set(start_end);
            self.timeline.set_start_end(start_end);
            if !self.media_file.get().unwrap().is_editor() {
                self.music_track.set_start_end(start_end);
                self.update_music();
            }
        }

        pub fn is_editor(&self) -> bool {
            self.media_file.get().unwrap().is_editor()
        }

        /// Switches the timeline between the Trimmer and the Editor.
        pub fn set_editor_mode(&self, editor: bool) {
            let engine = self.media_file.get().unwrap();
            if engine.is_editor() == editor {
                self.show_mode(editor);
                return;
            }
            if editor {
                // The music keeps its place in the video: source time becomes timeline time.
                let offset = (self.music_track.offset() as f64 / self.speed.get()) as i64;
                engine.enter_editor();
                self.music_track.set_offset(offset);
            } else {
                engine.enter_trimmer();
                self.set_speed(1.);
            }
            self.show_mode(editor);
            self.update_music();
        }

        fn show_mode(&self, editor: bool) {
            let engine = self.media_file.get().unwrap();
            self.timeline.set_visible(!editor);
            self.editor_timeline.set_visible(editor);
            self.image_original_audio.set_visible(editor);
            self.original_audio_track.set_visible(editor);
            self.knob_original_volume.set_visible(editor);
            if editor {
                self.refresh_original_audio();
            }
            if editor {
                self.music_track.set_speed(1.);
                self.music_track.set_start_end(None);
                self.music_track.set_video_duration(engine.total_duration());
                let (start, length) = self.editor_timeline.view();
                self.music_track.set_view(start, length);
            } else {
                self.music_track.set_speed(self.speed.get());
                self.music_track.set_start_end(self.start_end.get());
                self.music_track.set_video_duration(engine.total_duration());
                self.music_track.set_view(0, 0);
            }
        }

        pub fn add_video(&self, file: &gio::File) -> Result<(), glib::Error> {
            let engine = self.media_file.get().unwrap();
            if !engine.is_editor() {
                self.set_editor_mode(true);
            }
            engine.add_video(file)
        }

        pub fn split_at_playhead(&self) -> bool {
            self.editor_timeline.split_at_playhead()
        }

        pub fn delete_selected_segment(&self) {
            self.editor_timeline.delete_current();
        }

        pub fn has_selected_segment(&self) -> bool {
            self.editor_timeline.selected().is_some()
        }

        pub fn segment_count(&self) -> usize {
            self.media_file.get().unwrap().segments().len()
        }

        pub fn source_count(&self) -> usize {
            self.media_file.get().unwrap().source_count()
        }

        pub fn set_end_fade(&self, enabled: bool) {
            self.media_file.get().unwrap().set_end_fade(enabled);
            self.editor_timeline.set_end_fade(enabled);
        }

        /// Updates the original audio track and computes the waveforms of new videos.
        fn refresh_original_audio(&self) {
            let engine = self.media_file.get().unwrap();
            let segments = engine.segments();
            let (start, length) = self.editor_timeline.view();
            self.original_audio_track.set_view(start, length);

            let mut sources: Vec<usize> = segments.iter().map(|segment| segment.source).collect();
            sources.sort_unstable();
            sources.dedup();
            self.original_audio_track.set_segments(segments);

            for source in sources {
                if self.original_audio_track.has_peaks(source) || !engine.source_has_audio(source) {
                    continue;
                }
                let Some(path) = engine.source_path(source) else {
                    continue;
                };
                // Mark it as loading so it is computed once.
                self.original_audio_track.set_peaks(source, Vec::new());
                let obj = self.obj().clone();
                glib::MainContext::default().spawn_local(async move {
                    let peaks = gio::spawn_blocking(move || {
                        let duration = probe_music_duration(&path)?;
                        compute_music_peaks(&path, duration)
                    })
                    .await
                    .unwrap_or_else(|_| Err("the waveform thread panicked".to_owned()));
                    match peaks {
                        Ok(peaks) => obj.imp().original_audio_track.set_peaks(source, peaks),
                        Err(err) => warn!("could not compute the audio waveform: {err}"),
                    }
                });
            }
        }

        pub fn set_social_snap(&self, enabled: bool) {
            self.timeline.set_social_snap(enabled);
            self.editor_timeline.set_social_snap(enabled);
        }

        /// Holds the current frame on screen while the engine renders an export.
        pub fn set_preview_frozen(&self, frozen: bool) {
            let paintable = self.media_file.get().unwrap().paintable();
            if frozen {
                let image = paintable.map(|paintable| paintable.current_image());
                self.picture_video_preview.set_paintable(image.as_ref());
            } else {
                self.picture_video_preview.set_paintable(paintable.as_ref());
            }
        }

        pub fn engine(&self) -> &VtEngine {
            self.media_file.get().unwrap()
        }

        pub fn editor_timeline(&self) -> &VtEditorTimeline {
            &self.editor_timeline
        }

        pub fn set_music(&self, path: PathBuf) {
            // Results of an earlier load that finish after this one was requested are dropped.
            let generation = self.music_generation.get() + 1;
            self.music_generation.set(generation);

            let obj = self.obj().clone();
            let future = async move {
                let duration = gio::spawn_blocking({
                    let path = path.clone();
                    move || probe_music_duration(&path)
                })
                .await
                .unwrap_or_else(|_| Err("the ffprobe thread panicked".to_owned()));

                let imp = obj.imp();
                if imp.music_generation.get() != generation {
                    return;
                }
                match duration {
                    Ok(duration) => imp.show_music(path, duration, generation),
                    Err(err) => {
                        warn!("error reading music: {err}");
                        imp.show_music_error();
                    }
                }
            };
            glib::MainContext::default().spawn_local(future);
        }

        /// Shows the music row right away; the waveform and the preview load in the background.
        fn show_music(&self, path: PathBuf, duration: i64, generation: u32) {
            self.clear_music();

            // New music starts with the video; dragging it moves it from there.
            let offset = 0;

            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.music_track.set_music(&name, duration, offset);
            self.image_music.set_tooltip_text(Some(&name));
            self.music_path.replace(Some(path.clone()));
            self.set_music_row_visible(true);
            self.set_music_loading(true);

            let obj = self.obj().clone();
            let peaks_future = {
                let obj = obj.clone();
                let path = path.clone();
                async move {
                    let started = std::time::Instant::now();
                    let peaks = gio::spawn_blocking(move || compute_music_peaks(&path, duration))
                        .await
                        .unwrap_or_else(|_| Err("the waveform thread panicked".to_owned()));

                    let imp = obj.imp();
                    if imp.music_generation.get() != generation {
                        return;
                    }
                    match peaks {
                        Ok(peaks) => {
                            debug!("music: waveform computed in {:?}", started.elapsed());
                            imp.music_track.set_peaks(peaks);
                        }
                        Err(err) => warn!("error computing the music waveform: {err}"),
                    }
                    imp.set_music_loading(false);
                }
            };
            glib::MainContext::default().spawn_local(peaks_future);

            // GStreamer 1.28's decodebin3 aborts the whole process on some MP3 files played
            // through GtkMediaFile, so the preview plays a copy remuxed into Matroska. The export
            // uses the original file.
            static PREVIEW_COUNTER: AtomicU32 = AtomicU32::new(0);
            let preview_path = music_preview_dir().join(format!(
                "preview-{}-{}.mka",
                std::process::id(),
                PREVIEW_COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let preview_future = async move {
                let started = std::time::Instant::now();
                let result = gio::spawn_blocking({
                    let preview_path = preview_path.clone();
                    move || remux_music_preview(&path, &preview_path)
                })
                .await
                .unwrap_or_else(|_| Err("the remux thread panicked".to_owned()));

                let imp = obj.imp();
                if imp.music_generation.get() != generation {
                    let _ = std::fs::remove_file(&preview_path);
                    return;
                }
                if let Err(err) = result {
                    warn!("error preparing the music preview: {err}");
                    let _ = std::fs::remove_file(&preview_path);
                    return;
                }
                debug!("music: preview ready in {:?}", started.elapsed());

                let uri = gio::File::for_path(&preview_path).uri();
                imp.media_file.get().unwrap().set_music(Some(&uri));
                imp.music_preview_path.replace(Some(preview_path));
                imp.update_music();
            };
            glib::MainContext::default().spawn_local(preview_future);
        }

        fn show_music_error(&self) {
            // Translators: toast shown when a dropped music file could not be read.
            self.overlay
                .add_toast(adw::Toast::new(&gettext("Could not read the music file")));
        }

        fn remove_music(&self) {
            self.music_generation.set(self.music_generation.get() + 1);
            self.clear_music();
            self.set_music_row_visible(false);
        }

        fn clear_music(&self) {
            if let Some(engine) = self.media_file.get() {
                engine.set_music(None);
            }
            if let Some(preview_path) = self.music_preview_path.take() {
                let _ = std::fs::remove_file(preview_path);
            }
            self.music_path.replace(None);
            self.set_music_loading(false);
        }

        fn set_music_loading(&self, loading: bool) {
            self.music_track.set_loading(loading);
            self.spinner_music
                .set_visible(loading && self.music_track.is_visible());
            self.image_music
                .set_visible(!loading && self.music_track.is_visible());
        }

        fn set_music_row_visible(&self, visible: bool) {
            self.button_remove_music.set_visible(visible);
            self.music_track.set_visible(visible);
            self.box_music_controls.set_visible(visible);
            self.image_music.set_visible(visible);
            self.spinner_music.set_visible(false);
        }

        /// Places the music in the preview timeline.
        fn update_music(&self) {
            if self.music_preview_path.borrow().is_none() {
                return;
            }
            let engine = self.media_file.get().unwrap();
            let end = if engine.is_editor() {
                Some(engine.total_duration())
            } else {
                self.start_end.get().map(|(_, end)| i64::from(end) * 1000)
            };
            let fade_end = end.filter(|_| self.check_fade_out.is_active());
            engine.update_music(
                self.music_track.offset(),
                fade_end,
                self.knob_music_volume.value(),
            );
        }

        /// The added music: its path, the video timestamp it starts at, whether to fade it out,
        /// and its volume (1 for unchanged).
        pub fn music(&self) -> Option<(PathBuf, i64, bool, f64)> {
            let path = self.music_path.borrow().clone()?;
            Some((
                path,
                self.music_track.offset(),
                self.check_fade_out.is_active(),
                self.knob_music_volume.value(),
            ))
        }

        pub fn set_video_muted(&self, muted: bool) {
            self.media_file.get().unwrap().set_video_audio_muted(muted);
        }

        fn apply_speed(&self, speed: f64) {
            self.speed.set(speed);
            self.music_track.set_speed(speed);
            self.media_file.get().unwrap().set_speed(speed);
            self.update_music();
        }

        pub fn set_speed(&self, speed: f64) {
            self.timeline.set_speed(speed);
            self.apply_speed(speed);
        }

        pub fn set_start_as_position(&self) {
            self.timeline.set_start_as_position();
        }

        pub fn set_end_as_position(&self) {
            self.timeline.set_end_as_position();
        }

        pub fn pause(&self) {
            self.media_file.get().unwrap().pause();
        }

        pub fn overlay(&self) -> &adw::ToastOverlay {
            &self.overlay
        }

        pub fn box_playback_controls(&self) -> &gtk::Grid {
            &self.box_playback_controls
        }
    }
}

glib::wrapper! {
    pub struct VtVideoPreview(ObjectSubclass<imp::VtVideoPreview>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl VtVideoPreview {
    pub fn remove_stale_music_previews() {
        imp::remove_stale_music_previews();
    }

    pub fn open(&self, file: &gio::File) {
        self.imp().open(file);
    }

    pub fn step_forward(&self) {
        self.imp().step_forward();
    }

    pub fn step_back(&self) {
        self.imp().step_back();
    }

    pub fn set_frame_time_approx(&self, value: Duration) {
        self.imp().set_frame_time_approx(value);
    }

    pub fn set_start_end(&self, start_end: Option<(u32, u32)>) {
        self.imp().set_start_end(start_end);
    }

    pub fn set_start_as_position(&self) {
        self.imp().set_start_as_position();
    }

    pub fn set_end_as_position(&self) {
        self.imp().set_end_as_position();
    }

    pub fn pause(&self) {
        self.imp().pause();
    }

    pub fn overlay(&self) -> &adw::ToastOverlay {
        self.imp().overlay()
    }

    pub fn box_playback_controls(&self) -> &gtk::Grid {
        self.imp().box_playback_controls()
    }

    pub fn set_music(&self, path: PathBuf) {
        self.imp().set_music(path);
    }

    pub fn music(&self) -> Option<(PathBuf, i64, bool, f64)> {
        self.imp().music()
    }

    pub fn set_video_muted(&self, muted: bool) {
        self.imp().set_video_muted(muted);
    }

    pub fn set_speed(&self, speed: f64) {
        self.imp().set_speed(speed);
    }

    pub fn is_editor(&self) -> bool {
        self.imp().is_editor()
    }

    pub fn set_editor_mode(&self, editor: bool) {
        self.imp().set_editor_mode(editor);
    }

    pub fn add_video(&self, file: &gio::File) -> Result<(), glib::Error> {
        self.imp().add_video(file)
    }

    pub fn split_at_playhead(&self) -> bool {
        self.imp().split_at_playhead()
    }

    pub fn delete_selected_segment(&self) {
        self.imp().delete_selected_segment();
    }

    pub fn has_selected_segment(&self) -> bool {
        self.imp().has_selected_segment()
    }

    pub fn segment_count(&self) -> usize {
        self.imp().segment_count()
    }

    pub fn source_count(&self) -> usize {
        self.imp().source_count()
    }

    pub fn set_end_fade(&self, enabled: bool) {
        self.imp().set_end_fade(enabled);
    }

    pub fn set_preview_frozen(&self, frozen: bool) {
        self.imp().set_preview_frozen(frozen);
    }

    pub fn set_social_snap(&self, enabled: bool) {
        self.imp().set_social_snap(enabled);
    }

    pub fn engine(&self) -> crate::engine::VtEngine {
        self.imp().engine().clone()
    }

    pub fn editor_timeline(&self) -> crate::editor_timeline::VtEditorTimeline {
        self.imp().editor_timeline().clone()
    }
}
