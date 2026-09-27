use glib::subclass::prelude::*;
use gtk::{glib, prelude::*};

/// Length of the music fade-out at the end of the trim, in microseconds.
pub const FADE_OUT_DURATION: i64 = 1_000_000;

/// Waveform peaks are computed per this many microseconds of music.
pub const PEAK_INTERVAL: i64 = 10_000;

mod imp {
    use super::*;
    use crate::util::gettext_f;
    use glib::subclass::Signal;
    use gtk::{gdk, graphene, gsk, subclass::prelude::*};
    use std::{
        cell::{Cell, RefCell},
        sync::OnceLock,
    };

    /// Distance in pixels within which the music start or end snaps to an anchor.
    const SNAP_DISTANCE: f64 = 8.;

    #[derive(Debug, Default)]
    pub struct VtMusicTrack {
        pub(super) video_duration: Cell<i64>,
        pub(super) music_duration: Cell<i64>,
        /// Video timestamp at which the music starts playing. Can be negative.
        pub(super) offset: Cell<i64>,
        pub(super) peaks: RefCell<Vec<f32>>,
        pub(super) name: RefCell<String>,
        pub(super) start_end: Cell<Option<(u32, u32)>>,
        pub(super) position: Cell<i64>,
        pub(super) fade_out: Cell<bool>,
        pub(super) loading: Cell<bool>,
        /// Video speed; the music keeps its own speed, so it spans `duration * speed` of video.
        pub(super) speed: Cell<f64>,
        /// Visible span, so the track lines up with a zoomed timeline. A zero length shows the
        /// whole video.
        pub(super) view: Cell<(i64, i64)>,
        drag_start_offset: Cell<i64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for VtMusicTrack {
        const NAME: &'static str = "VtMusicTrack";
        type Type = super::VtMusicTrack;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("vt-music-track");
        }
    }

    impl ObjectImpl for VtMusicTrack {
        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<[Signal; 1]> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                [Signal::builder("offset-changed")
                    .param_types([glib::Type::I64])
                    .build()]
            })
        }

        fn constructed(&self) {
            let obj = self.obj();
            self.parent_constructed();

            self.speed.set(1.);
            obj.set_size_request(-1, 40);
            obj.set_overflow(gtk::Overflow::Hidden);
            obj.set_cursor_from_name(Some("grab"));

            let gesture_drag = gtk::GestureDrag::new();
            gesture_drag.connect_drag_begin({
                let obj = obj.downgrade();
                move |gesture, _, _| {
                    gesture.set_state(gtk::EventSequenceState::Claimed);

                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();
                    imp.drag_start_offset.set(imp.offset.get());
                    obj.set_cursor_from_name(Some("grabbing"));
                }
            });
            gesture_drag.connect_drag_update({
                let obj = obj.downgrade();
                move |_, offset_x, _| {
                    let obj = obj.upgrade().unwrap();
                    obj.imp().on_drag_update(offset_x);
                }
            });
            gesture_drag.connect_drag_end({
                let obj = obj.downgrade();
                move |_, _, _| {
                    let obj = obj.upgrade().unwrap();
                    obj.set_cursor_from_name(Some("grab"));
                }
            });
            obj.add_controller(gesture_drag);
        }
    }

    impl WidgetImpl for VtMusicTrack {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let width = obj.width() as f32;
            let height = obj.height() as f32;
            let fg = obj.color();
            let accent = adw::StyleManager::default().accent_color_rgba();

            let bounds = graphene::Rect::new(0., 0., width, height);
            snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(bounds, 9.));
            snapshot.append_color(&with_alpha(&fg, 0.1), &bounds);

            let video_duration = self.video_duration.get();
            if video_duration <= 0 {
                snapshot.pop();
                return;
            }
            let (view_start, view_length) = self.view();
            let x_of =
                |time: i64| ((time - view_start) as f64 / view_length as f64 * width as f64) as f32;

            let start_end = self
                .start_end
                .get()
                .map(|(start, end)| (i64::from(start) * 1000, i64::from(end) * 1000));
            if let Some((start, end)) = start_end {
                let x = x_of(start);
                snapshot.append_color(
                    &with_alpha(&accent, 0.15),
                    &graphene::Rect::new(x, 0., x_of(end) - x, height),
                );
            }

            let offset = self.offset.get();
            let speed = self.speed.get();
            let clip_start = x_of(offset);
            let clip_end = x_of(offset + self.music_span());
            let clip = graphene::Rect::new(clip_start, 0., clip_end - clip_start, height);
            snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(clip, 6.));
            snapshot.append_color(&with_alpha(&accent, 0.3), &clip);

            // One bar per pixel column, showing the loudest peak in the music it covers.
            let peaks = self.peaks.borrow();
            let wave_color = with_alpha(&accent, 0.9);
            let max_bar = height - 8.;
            let us_per_px = view_length as f64 / width as f64;
            let first_px = clip_start.max(0.) as i32;
            let last_px = clip_end.min(width).ceil() as i32;
            for px in first_px..last_px {
                let pixel_time = |px: i32| view_start as f64 + px as f64 * us_per_px;
                let music_from = ((pixel_time(px) - offset as f64) / speed) as i64;
                let music_to = ((pixel_time(px + 1) - offset as f64) / speed) as i64;
                let from = (music_from / PEAK_INTERVAL).max(0) as usize;
                let to = ((music_to / PEAK_INTERVAL).max(0) as usize + 1).min(peaks.len());
                let peak = peaks
                    .get(from..to)
                    .map(|slice| slice.iter().copied().fold(0., f32::max))
                    .unwrap_or(0.);
                let bar = (peak * max_bar).max(1.);
                snapshot.append_color(
                    &wave_color,
                    &graphene::Rect::new(px as f32, (height - bar) / 2., 1., bar),
                );
            }

            let label = if self.loading.get() {
                // Translators: shown on the music track while its waveform is being computed.
                // The placeholder is the music file name.
                gettext_f("{} — loading…", &[&self.name.borrow()])
            } else {
                self.name.borrow().clone()
            };
            let layout = obj.create_pango_layout(Some(&label));
            layout.set_ellipsize(gtk::pango::EllipsizeMode::End);
            let label_x = clip_start.max(0.) + 6.;
            layout.set_width(((clip_end.min(width) - label_x - 6.).max(0.) * 1024.) as i32);
            snapshot.save();
            snapshot.translate(&graphene::Point::new(label_x, 2.));
            snapshot.append_layout(&layout, &with_alpha(&fg, 0.8));
            snapshot.restore();

            snapshot.pop();

            if self.fade_out.get() {
                if let Some((_, end)) = start_end {
                    let fade_start = x_of(end - (FADE_OUT_DURATION as f64 * speed) as i64);
                    let fade_end = x_of(end);
                    snapshot.append_linear_gradient(
                        &graphene::Rect::new(fade_start, 0., fade_end - fade_start, height),
                        &graphene::Point::new(fade_start, 0.),
                        &graphene::Point::new(fade_end, 0.),
                        &[
                            gsk::ColorStop::new(0., with_alpha(&fg, 0.)),
                            gsk::ColorStop::new(1., with_alpha(&fg, 0.4)),
                        ],
                    );
                }
            }

            // Past the end of the video the music is cut, in the preview and in the export.
            let video_end = x_of(video_duration);
            if video_end < width {
                snapshot.append_color(
                    &gdk::RGBA::new(0., 0., 0., 0.45),
                    &graphene::Rect::new(video_end, 0., width - video_end, height),
                );
            }

            snapshot.append_color(
                &fg,
                &graphene::Rect::new(x_of(self.position.get()) - 1., 0., 2., height),
            );

            snapshot.pop();
        }
    }

    impl VtMusicTrack {
        fn view(&self) -> (i64, i64) {
            let (start, length) = self.view.get();
            if length > 0 {
                (start, length)
            } else {
                (0, self.video_duration.get().max(1))
            }
        }

        /// How much video time the music covers at the current speed.
        fn music_span(&self) -> i64 {
            (self.music_duration.get() as f64 * self.speed.get()) as i64
        }

        fn on_drag_update(&self, offset_x: f64) {
            let obj = self.obj();
            let width = obj.width() as f64;
            let video_duration = self.video_duration.get();
            if width <= 0. || video_duration <= 0 {
                return;
            }

            let us_per_px = self.view().1 as f64 / width;
            let music_span = self.music_span();
            let mut offset = self.drag_start_offset.get() + (offset_x * us_per_px) as i64;

            let tolerance = (SNAP_DISTANCE * us_per_px) as i64;
            let mut anchors = vec![self.position.get()];
            if let Some((start, end)) = self.start_end.get() {
                anchors.push(i64::from(start) * 1000);
                // Snapping the music end onto the trim end.
                anchors.push(i64::from(end) * 1000 - music_span);
            }
            if let Some(anchor) = anchors
                .into_iter()
                .filter(|anchor| (offset - anchor).abs() <= tolerance)
                .min_by_key(|anchor| (offset - anchor).abs())
            {
                offset = anchor;
            }

            let offset = offset.clamp(-music_span, video_duration);
            if offset != self.offset.get() {
                self.offset.set(offset);
                obj.queue_draw();
                obj.emit_by_name::<()>("offset-changed", &[&offset]);
            }
        }
    }

    fn with_alpha(color: &gdk::RGBA, alpha: f32) -> gdk::RGBA {
        gdk::RGBA::new(
            color.red(),
            color.green(),
            color.blue(),
            color.alpha() * alpha,
        )
    }
}

glib::wrapper! {
    pub struct VtMusicTrack(ObjectSubclass<imp::VtMusicTrack>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl VtMusicTrack {
    pub fn set_music(&self, name: &str, duration: i64, offset: i64) {
        let imp = self.imp();
        imp.name.replace(name.to_owned());
        imp.music_duration.set(duration);
        imp.peaks.replace(Vec::new());
        imp.offset.set(offset);
        self.queue_draw();
    }

    pub fn set_peaks(&self, peaks: Vec<f32>) {
        self.imp().peaks.replace(peaks);
        self.queue_draw();
    }

    pub fn set_loading(&self, loading: bool) {
        self.imp().loading.set(loading);
        self.queue_draw();
    }

    pub fn offset(&self) -> i64 {
        self.imp().offset.get()
    }

    pub fn music_duration(&self) -> i64 {
        self.imp().music_duration.get()
    }

    pub fn set_video_duration(&self, duration: i64) {
        self.imp().video_duration.set(duration);
        self.queue_draw();
    }

    pub fn set_start_end(&self, start_end: Option<(u32, u32)>) {
        self.imp().start_end.set(start_end);
        self.queue_draw();
    }

    pub fn set_position(&self, position: i64) {
        self.imp().position.set(position);
        self.queue_draw();
    }

    pub fn set_speed(&self, speed: f64) {
        self.imp().speed.set(speed);
        self.queue_draw();
    }

    /// Shows `length` µs from `start`; a zero length shows the whole video.
    pub fn set_view(&self, start: i64, length: i64) {
        self.imp().view.set((start, length));
        self.queue_draw();
    }

    pub fn set_offset(&self, offset: i64) {
        self.imp().offset.set(offset);
        self.queue_draw();
    }

    pub fn set_fade_out(&self, fade_out: bool) {
        self.imp().fade_out.set(fade_out);
        self.queue_draw();
    }
}
