use glib::subclass::prelude::*;
use gtk::glib;
use std::time::Duration;

mod imp {
    use super::*;
    use crate::{
        engine::VtEngine,
        parse::{self, time_to_entry_text},
    };
    use glib::{subclass::Signal, Properties};
    use gtk::{gdk, graphene, gsk, prelude::*, subclass::prelude::*, CompositeTemplate};
    use std::{
        cell::{Cell, OnceCell},
        sync::OnceLock,
    };

    const TOLERANCE: f64 = 5.;

    pub const MIN_SPEED: f64 = 0.25;
    pub const MAX_SPEED: f64 = 4.;
    /// Speeds this close to 100% snap to it, so it is easy to go back to normal speed.
    const SPEED_SNAP: f64 = 0.03;
    /// Number of coils in the drawn spring.
    const SPRING_COILS: usize = 10;

    /// Which trim edge stays in place while the other one stretches or compresses the video.
    #[derive(Debug, Clone, Copy, Eq, PartialEq)]
    enum SpeedAnchor {
        Start,
        End,
    }

    #[derive(Debug, Clone, Copy, Eq, PartialEq)]
    enum DragType {
        Playback,
        Start,
        End,
        /// Ctrl-dragging the start edge: changes the speed, the end stays in place.
        SpeedStart,
        /// Ctrl-dragging the end edge: changes the speed, the start stays in place.
        SpeedEnd,
    }

    #[derive(Debug, Clone, Copy, Eq, PartialEq)]
    enum CursorType {
        Normal,
        StartEnd,
    }

    impl CursorType {
        fn gtk_cursor_name(self) -> &'static str {
            match self {
                CursorType::Normal => "default",
                CursorType::StartEnd => "col-resize",
            }
        }
    }

    #[derive(Debug, CompositeTemplate, Properties)]
    #[properties(wrapper_type = super::VtTimeline)]
    #[template(resource = "/io/github/jjolmo/QuickVideoEditor/timeline.ui")]
    pub struct VtTimeline {
        #[template_child]
        box_timeline_position: TemplateChild<gtk::Box>,
        #[template_child]
        box_timeline_selection: TemplateChild<gtk::Box>,

        #[property(set = Self::set_media_file)]
        media_file: OnceCell<gtk::MediaStream>,
        frame_time_approx: Cell<Option<Duration>>,
        position: Cell<i64>,
        duration: Cell<i64>,
        start_end: Cell<Option<(u32, u32)>>,
        gesture_drag: OnceCell<gtk::GestureDrag>,
        drag_start: Cell<f64>,
        drag_type: Cell<DragType>,
        cursor_type: Cell<CursorType>,
        speed: Cell<f64>,
        speed_anchor: Cell<SpeedAnchor>,
        /// Ctrl-dragging a trim edge changes the speed. Off in the Trimmer, which has a knob.
        speed_drag_enabled: Cell<bool>,
        /// Snaps the selection to 59 s, the length limit of many social networks.
        social_snap: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for VtTimeline {
        const NAME: &'static str = "VtTimeline";
        type Type = super::VtTimeline;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);

            klass.set_css_name("vt-timeline");
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }

        fn new() -> Self {
            Self {
                box_timeline_position: Default::default(),
                box_timeline_selection: Default::default(),
                media_file: OnceCell::new(),
                frame_time_approx: Cell::new(None),
                position: Cell::new(0),
                duration: Cell::new(0),
                start_end: Cell::new(None),
                gesture_drag: OnceCell::new(),
                drag_start: Cell::new(0.),
                drag_type: Cell::new(DragType::Playback),
                cursor_type: Cell::new(CursorType::Normal),
                speed: Cell::new(1.),
                speed_anchor: Cell::new(SpeedAnchor::Start),
                speed_drag_enabled: Cell::new(false),
                social_snap: Cell::new(false),
            }
        }
    }

    impl ObjectImpl for VtTimeline {
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
            static SIGNALS: OnceLock<[Signal; 4]> = OnceLock::new();
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
                    Signal::builder("set-speed")
                        .param_types([glib::Type::F64])
                        .build(),
                ]
            })
        }

        fn constructed(&self) {
            let obj = self.obj();
            self.parent_constructed();

            // Invisible until we get duration.
            self.box_timeline_position.set_child_visible(false);
            self.box_timeline_selection.set_child_visible(false);

            // For some reason doesn't work from the .ui file.
            obj.set_overflow(gtk::Overflow::Hidden);

            // Set up the drag gesture.
            let gesture_drag = gtk::GestureDrag::new();
            gesture_drag.connect_drag_begin({
                let obj = obj.downgrade();
                move |gesture, x, y| {
                    gesture.set_state(gtk::EventSequenceState::Claimed);

                    let ctrl = gesture
                        .current_event_state()
                        .contains(gdk::ModifierType::CONTROL_MASK);

                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();
                    imp.on_drag_start(x, y, ctrl);
                }
            });
            gesture_drag.connect_drag_update({
                let obj = obj.downgrade();
                move |_, offset_x, offset_y| {
                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();
                    imp.on_drag_update(offset_x, offset_y);
                }
            });
            gesture_drag.connect_drag_end({
                let obj = obj.downgrade();
                move |_, _, _| {
                    let obj = obj.upgrade().unwrap();
                    obj.imp().on_drag_end();
                }
            });
            obj.add_controller(gesture_drag.clone());
            self.gesture_drag.set(gesture_drag).unwrap();

            let event_controller_motion = gtk::EventControllerMotion::new();
            event_controller_motion.connect_motion({
                let obj = obj.downgrade();
                move |_, x, y| {
                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();
                    imp.on_motion(x, y);
                }
            });
            obj.add_controller(event_controller_motion);
        }

        fn dispose(&self) {
            let obj = self.obj();
            while let Some(child) = obj.first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for VtTimeline {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.parent_snapshot(snapshot);

            let Some((x0, x1)) = self.speed_extent() else {
                return;
            };
            let obj = self.obj();
            let height = obj.height() as f32;
            let fg = obj.color();

            // The span the trimmed video will occupy after the speed change.
            snapshot.append_color(
                &with_alpha(&fg, 0.12),
                &graphene::Rect::new(x0, 0., x1 - x0, height),
            );

            let middle = height / 2.;
            let amplitude = height * 0.12;
            let builder = gsk::PathBuilder::new();
            builder.move_to(x0, middle);
            let steps = SPRING_COILS * 2;
            for step in 0..steps {
                let x = x0 + (x1 - x0) * (step as f32 + 0.5) / steps as f32;
                let y = if step % 2 == 0 {
                    middle - amplitude
                } else {
                    middle + amplitude
                };
                builder.line_to(x, y);
            }
            builder.line_to(x1, middle);
            snapshot.append_stroke(
                &builder.to_path(),
                &gsk::Stroke::new(2.),
                &with_alpha(&fg, 0.9),
            );

            let label = format!("{:.0}%", self.speed.get() * 100.);
            let layout = obj.create_pango_layout(Some(&label));
            let (label_width, label_height) = layout.pixel_size();
            let (label_width, label_height) = (label_width as f32, label_height as f32);
            let label_x = ((x0 + x1 - label_width) / 2.)
                .clamp(0., (obj.width() as f32 - label_width).max(0.));
            let label_y = (height - label_height) / 2.;
            let background =
                graphene::Rect::new(label_x - 4., label_y, label_width + 8., label_height);
            let accent = adw::StyleManager::default().accent_color_rgba();
            snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(background, 6.));
            snapshot.append_color(&accent, &background);
            snapshot.pop();
            snapshot.save();
            snapshot.translate(&graphene::Point::new(label_x, label_y));
            snapshot.append_layout(&layout, &gdk::RGBA::WHITE);
            snapshot.restore();
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let duration = self.duration.get();
            if duration == 0 {
                return;
            }

            let position = self.position.get();
            let x = ((position as f64 / duration as f64).clamp(0., 1.) * width as f64) as i32;
            let position_width = self
                .box_timeline_position
                .measure(gtk::Orientation::Horizontal, -1)
                .0;
            let position_height = self
                .box_timeline_position
                .measure(gtk::Orientation::Vertical, position_width)
                .0
                .max(height);
            self.box_timeline_position.size_allocate(
                &gtk::Allocation::new(x - position_width / 2, 0, position_width, position_height),
                baseline,
            );

            if let Some((start, end)) = self.start_end.get() {
                let duration = duration as f64 / 1000.;
                let x = ((start as f64 / duration).clamp(0., 1.) * width as f64) as i32;
                let x_end = ((end as f64 / duration).clamp(0., 1.) * width as f64) as i32;

                let selection_width = self
                    .box_timeline_selection
                    .measure(gtk::Orientation::Horizontal, -1)
                    .0
                    .max(x_end - x);
                let selection_height = self
                    .box_timeline_selection
                    .measure(gtk::Orientation::Vertical, selection_width)
                    .0
                    .max(height);

                self.box_timeline_selection.size_allocate(
                    &gtk::Allocation::new(x, 0, selection_width, selection_height),
                    baseline,
                );
            }
        }
    }

    impl VtTimeline {
        fn set_media_file(&self, media_file: gtk::MediaStream) {
            let obj = self.obj();

            media_file.connect_timestamp_notify({
                let obj = obj.downgrade();
                move |_| {
                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();
                    imp.refresh();
                }
            });

            media_file.connect_duration_notify({
                let obj = obj.downgrade();
                move |_| {
                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();
                    imp.refresh();
                }
            });

            media_file.connect_seeking_notify({
                let obj = obj.downgrade();
                move |media_file| {
                    // This callback is for updating position once seeking has completed.
                    if media_file.is_seeking() {
                        return;
                    }

                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();
                    imp.refresh();
                }
            });

            self.media_file.set(media_file).unwrap();
        }

        pub fn set_frame_time_approx(&self, value: Duration) {
            self.frame_time_approx.set(Some(value));
        }

        pub fn step(&self, direction: i64) {
            if let Some(frame_time) = self.frame_time_approx.get() {
                let media_file = self.media_file.get().unwrap();
                let seek = direction * frame_time.as_micros() as i64;
                let time = media_file.timestamp() + seek;
                let time = time.max(0);

                media_file.seek(time);
                self.position.set(time);
                self.obj().queue_allocate();
            }
        }

        pub fn set_start_end(&self, start_end: Option<(u32, u32)>) {
            self.start_end.set(start_end);
            self.refresh();
        }

        pub fn set_start_as_position(&self) {
            let media_file = self.media_file.get().unwrap();
            let start = (media_file.timestamp() / 1000) as u32;

            self.obj().emit_by_name::<()>("set-start", &[&start]);
        }

        pub fn set_end_as_position(&self) {
            let media_file = self.media_file.get().unwrap();
            let end = (media_file.timestamp() / 1000) as u32;

            self.obj().emit_by_name::<()>("set-end", &[&end]);
        }

        pub fn refresh(&self) {
            let obj = self.obj();

            let media_file = self.media_file.get().unwrap();
            let duration = media_file.duration();
            self.duration.set(duration);
            if duration == 0 {
                self.box_timeline_position.set_child_visible(false);
                self.box_timeline_selection.set_child_visible(false);
                obj.queue_allocate();
                return;
            }

            self.box_timeline_position.set_child_visible(true);
            self.box_timeline_selection
                .set_child_visible(self.start_end.get().is_some());

            let dragging = self
                .gesture_drag
                .get()
                .is_some_and(|gesture| gesture.is_active());
            if !media_file.is_seeking() && !dragging {
                self.position.set(media_file.timestamp());
            }

            obj.queue_allocate();
        }

        fn on_drag_start(&self, x: f64, _y: f64, ctrl: bool) {
            self.drag_start.set(x);
            self.drag_type.set(DragType::Playback);

            if ctrl && self.speed_drag_enabled.get() {
                if let Some((drag_type, edge)) = self.speed_handle_at(x) {
                    self.drag_type.set(drag_type);
                    self.drag_start.set(edge);
                    return;
                }
            }

            if self.start_end.get().is_some() {
                if let Some(bounds) = self.box_timeline_selection.compute_bounds(&*self.obj()) {
                    let start = bounds.x() as f64;
                    let end = (bounds.x() + bounds.width()) as f64;

                    if (x - end).abs() <= TOLERANCE {
                        self.drag_type.set(DragType::End);
                        self.drag_start.set(end);
                    } else if (x - start).abs() <= TOLERANCE {
                        self.drag_type.set(DragType::Start);
                        self.drag_start.set(start);
                    }
                }
            }

            self.on_drag_update(0., 0.);
        }

        fn on_drag_update(&self, offset_x: f64, _offset_y: f64) {
            let obj = self.obj();

            let x = self.drag_start.get() + offset_x;
            let width = obj.width() as f64;

            // Sanitize (this can get weird values when resizing the window while dragging).
            let x = x.clamp(0., width);
            let value = x / width;

            let media_file = self.media_file.get().unwrap();
            let duration = media_file.duration();
            if duration != 0 {
                let time = (duration as f64 * value) as i64;

                if matches!(
                    self.drag_type.get(),
                    DragType::SpeedStart | DragType::SpeedEnd
                ) {
                    self.on_speed_drag(time);
                    return;
                }

                // Keyframe seeks keep up with the pointer; the exact frame is sought on release.
                match media_file.downcast_ref::<VtEngine>() {
                    Some(engine) => engine.seek_fast(time),
                    None => media_file.seek(time),
                }

                // Update the position for responsive seeking.
                self.position.set(time);
                obj.queue_allocate();

                let start_end = self.start_end.get();
                if start_end.is_none() {
                    return;
                }

                let (start, end) = start_end.unwrap();
                let time = (time / 1000) as u32;

                let (start, end) = match self.drag_type.get() {
                    DragType::Start => {
                        let text = time_to_entry_text(Duration::from_millis(time.into()));

                        if parse::timestamp(&text).unwrap() == end {
                            // Don't set the text if the timestamps will match as that counts as an
                            // invalid region.
                            return;
                        }

                        if time <= end {
                            (time, end)
                        } else {
                            self.drag_type.set(DragType::End);
                            (end, time)
                        }
                    }
                    DragType::End => {
                        let text = time_to_entry_text(Duration::from_millis(time.into()));

                        if parse::timestamp(&text).unwrap() == start {
                            // Don't set the text if the timestamps will match as that counts as an
                            // invalid region.
                            return;
                        }

                        if time >= start {
                            (start, time)
                        } else {
                            self.drag_type.set(DragType::Start);
                            (time, start)
                        }
                    }
                    _ => return,
                };

                let (start, end) = self.snap_social(start, end);
                self.obj()
                    .emit_by_name::<()>("set-start-end", &[&start, &end]);
            };
        }

        /// With social snapping on, a selection close to 59 s long becomes exactly that long.
        fn snap_social(&self, start: u32, end: u32) -> (u32, u32) {
            const SOCIAL_LENGTH: u32 = 59_000;
            let width = self.obj().width().max(1) as f64;
            let duration_ms = self.duration.get() as f64 / 1000.;
            let tolerance = (TOLERANCE * 2. * duration_ms / width) as u32;
            if !self.social_snap.get() || end - start == SOCIAL_LENGTH {
                return (start, end);
            }
            if (end - start).abs_diff(SOCIAL_LENGTH) > tolerance {
                return (start, end);
            }
            match self.drag_type.get() {
                DragType::Start if end >= SOCIAL_LENGTH => (end - SOCIAL_LENGTH, end),
                DragType::End => (start, start + SOCIAL_LENGTH),
                _ => (start, end),
            }
        }

        pub fn set_social_snap(&self, enabled: bool) {
            self.social_snap.set(enabled);
        }

        /// Finds the edge under `x` that a Ctrl-drag would grab: a trim edge, or the moving edge
        /// of the current speed span.
        fn speed_handle_at(&self, x: f64) -> Option<(DragType, f64)> {
            let bounds = self.box_timeline_selection.compute_bounds(&*self.obj())?;
            let mut edges = vec![
                (DragType::SpeedStart, bounds.x() as f64),
                (DragType::SpeedEnd, (bounds.x() + bounds.width()) as f64),
            ];
            if let Some((x0, x1)) = self.speed_extent() {
                match self.speed_anchor.get() {
                    SpeedAnchor::Start => edges.push((DragType::SpeedEnd, x1 as f64)),
                    SpeedAnchor::End => edges.push((DragType::SpeedStart, x0 as f64)),
                }
            }
            edges
                .into_iter()
                .filter(|(_, edge)| (x - edge).abs() <= TOLERANCE)
                .min_by(|(_, a), (_, b)| (x - a).abs().total_cmp(&(x - b).abs()))
        }

        fn on_speed_drag(&self, time: i64) {
            let Some((start, end)) = self.start_end.get() else {
                return;
            };
            let (start, end) = (i64::from(start) * 1000, i64::from(end) * 1000);
            let source_length = (end - start) as f64;

            let (output_length, anchor) = match self.drag_type.get() {
                DragType::SpeedEnd => (time - start, SpeedAnchor::Start),
                _ => (end - time, SpeedAnchor::End),
            };
            let mut speed = if output_length > 0 {
                (source_length / output_length as f64).clamp(MIN_SPEED, MAX_SPEED)
            } else {
                MAX_SPEED
            };
            if (speed - 1.).abs() < SPEED_SNAP {
                speed = 1.;
            }

            self.speed_anchor.set(anchor);
            if speed != self.speed.get() {
                self.speed.set(speed);
                self.obj().queue_draw();
                self.obj().emit_by_name::<()>("set-speed", &[&speed]);
            }
        }

        pub fn set_speed(&self, speed: f64) {
            self.speed.set(speed);
            self.obj().queue_draw();
        }

        /// Horizontal span of the trimmed video after the speed change, if the speed isn't 100%.
        fn speed_extent(&self) -> Option<(f32, f32)> {
            let speed = self.speed.get();
            let duration = self.duration.get();
            let (start, end) = self.start_end.get()?;
            if speed == 1. || duration == 0 {
                return None;
            }

            let width = self.obj().width() as f64;
            let (start, end) = (i64::from(start) * 1000, i64::from(end) * 1000);
            let output_length = (end - start) as f64 / speed;
            let (from, to) = match self.speed_anchor.get() {
                SpeedAnchor::Start => (start as f64, start as f64 + output_length),
                SpeedAnchor::End => (end as f64 - output_length, end as f64),
            };
            let x_of = |time: f64| (time / duration as f64 * width) as f32;
            Some((x_of(from), x_of(to)))
        }

        fn on_drag_end(&self) {
            if matches!(
                self.drag_type.get(),
                DragType::SpeedStart | DragType::SpeedEnd
            ) {
                return;
            }
            let media_file = self.media_file.get().unwrap();
            if media_file.duration() != 0 {
                media_file.seek(self.position.get());
            }
        }

        fn on_motion(&self, x: f64, _y: f64) {
            let obj = self.obj();

            // Don't change the cursor while in drag.
            if self.gesture_drag.get().unwrap().is_active() {
                return;
            }

            let resizing_cursor = if self.start_end.get().is_some() {
                if let Some(bounds) = self.box_timeline_selection.compute_bounds(&*self.obj()) {
                    let start = bounds.x() as f64;
                    let end = (bounds.x() + bounds.width()) as f64;

                    (x - end).abs() <= TOLERANCE || (x - start).abs() <= TOLERANCE
                } else {
                    false
                }
            } else {
                false
            };

            let cursor_type = if resizing_cursor {
                CursorType::StartEnd
            } else {
                CursorType::Normal
            };

            if self.cursor_type.get() != cursor_type {
                let cursor = gdk::Cursor::from_name(cursor_type.gtk_cursor_name(), None).unwrap();
                obj.set_cursor(Some(&cursor));
                self.cursor_type.set(cursor_type);
            }
        }
    }
}

fn with_alpha(color: &gtk::gdk::RGBA, alpha: f32) -> gtk::gdk::RGBA {
    gtk::gdk::RGBA::new(
        color.red(),
        color.green(),
        color.blue(),
        color.alpha() * alpha,
    )
}

glib::wrapper! {
    pub struct VtTimeline(ObjectSubclass<imp::VtTimeline>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl VtTimeline {
    pub fn set_frame_time_approx(&self, value: Duration) {
        self.imp().set_frame_time_approx(value);
    }
    pub fn step_forward(&self) {
        self.imp().step(1)
    }
    pub fn step_back(&self) {
        self.imp().step(-1)
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
    pub fn set_speed(&self, speed: f64) {
        self.imp().set_speed(speed);
    }
    pub fn set_social_snap(&self, enabled: bool) {
        self.imp().set_social_snap(enabled);
    }
}
