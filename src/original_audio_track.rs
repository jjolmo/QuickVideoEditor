//! Thin track under the Editor timeline showing the videos' own audio.

use std::collections::HashMap;

use gtk::{glib, prelude::*, subclass::prelude::*};

use crate::engine::SegmentInfo;

mod imp {
    use super::*;
    use crate::music_track::PEAK_INTERVAL;
    use gtk::{gdk, graphene, gsk};
    use std::cell::{Cell, RefCell};

    #[derive(Debug, Default)]
    pub struct VtOriginalAudioTrack {
        pub(super) segments: RefCell<Vec<SegmentInfo>>,
        /// Waveform peaks per source video.
        pub(super) peaks: RefCell<HashMap<usize, Vec<f32>>>,
        /// Visible span: start and length in timeline µs.
        pub(super) view: Cell<(i64, i64)>,
        pub(super) position: Cell<i64>,
        pub(super) volume: Cell<f64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for VtOriginalAudioTrack {
        const NAME: &'static str = "VtOriginalAudioTrack";
        type Type = super::VtOriginalAudioTrack;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("vt-original-audio-track");
        }
    }

    impl ObjectImpl for VtOriginalAudioTrack {
        fn constructed(&self) {
            self.parent_constructed();
            self.volume.set(1.);
            let obj = self.obj();
            obj.set_size_request(-1, 26);
            obj.set_overflow(gtk::Overflow::Hidden);
        }
    }

    impl WidgetImpl for VtOriginalAudioTrack {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let width = obj.width() as f32;
            let height = obj.height() as f32;
            let fg = obj.color();

            let bounds = graphene::Rect::new(0., 0., width, height);
            snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(bounds, 7.));
            snapshot.append_color(&with_alpha(&fg, 0.1), &bounds);

            let (view_start, view_length) = self.view.get();
            if view_length <= 0 {
                snapshot.pop();
                return;
            }
            let us_per_px = view_length as f64 / width as f64;
            let x_of = |time: i64| ((time - view_start) as f64 / us_per_px) as f32;

            // Louder settings draw brighter, so the knob's effect is visible at a glance.
            let volume = self.volume.get() as f32;
            let body = with_alpha(&fg, 0.12 + 0.1 * volume.min(1.));
            let wave = with_alpha(&fg, (0.25 + 0.45 * volume.min(1.)).min(0.8));
            let max_bar = (height - 6.) * volume.clamp(0.15, 1.);

            let peaks = self.peaks.borrow();
            for segment in self.segments.borrow().iter() {
                let x0 = x_of(segment.start);
                let x1 = x_of(segment.end());
                if x1 < 0. || x0 > width {
                    continue;
                }
                let rect = graphene::Rect::new(x0, 2., (x1 - x0).max(1.), height - 4.);
                snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(rect, 4.));
                snapshot.append_color(&body, &rect);

                if let Some(peaks) = peaks.get(&segment.source) {
                    let first_px = x0.max(0.) as i32;
                    let last_px = x1.min(width).ceil() as i32;
                    for px in first_px..last_px {
                        // Timeline time to source time, through the segment's in-point and speed.
                        let source_time = |px: i32| {
                            let timeline = view_start as f64 + px as f64 * us_per_px;
                            segment.inpoint as f64
                                + (timeline - segment.start as f64) * segment.speed
                        };
                        let from = (source_time(px) as i64 / PEAK_INTERVAL).max(0) as usize;
                        let to = ((source_time(px + 1) as i64 / PEAK_INTERVAL).max(0) as usize + 1)
                            .min(peaks.len());
                        let peak = peaks
                            .get(from..to)
                            .map(|slice| slice.iter().copied().fold(0., f32::max))
                            .unwrap_or(0.);
                        let bar = (peak * max_bar).max(1.);
                        snapshot.append_color(
                            &wave,
                            &graphene::Rect::new(px as f32, (height - bar) / 2., 1., bar),
                        );
                    }
                }
                snapshot.pop();
            }

            let x = x_of(self.position.get());
            snapshot.append_color(&fg, &graphene::Rect::new(x - 1., 0., 2., height));
            snapshot.pop();
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
    pub struct VtOriginalAudioTrack(ObjectSubclass<imp::VtOriginalAudioTrack>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl VtOriginalAudioTrack {
    pub fn set_segments(&self, segments: Vec<SegmentInfo>) {
        self.imp().segments.replace(segments);
        self.queue_draw();
    }

    pub fn has_peaks(&self, source: usize) -> bool {
        self.imp().peaks.borrow().contains_key(&source)
    }

    pub fn set_peaks(&self, source: usize, peaks: Vec<f32>) {
        self.imp().peaks.borrow_mut().insert(source, peaks);
        self.queue_draw();
    }

    pub fn set_view(&self, start: i64, length: i64) {
        self.imp().view.set((start, length));
        self.queue_draw();
    }

    pub fn set_position(&self, position: i64) {
        self.imp().position.set(position);
        self.queue_draw();
    }

    pub fn set_volume(&self, volume: f64) {
        self.imp().volume.set(volume);
        self.queue_draw();
    }
}
