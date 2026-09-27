use glib::subclass::prelude::*;
use gtk::glib;
use std::time::Duration;

mod imp {
    use super::*;
    use crate::parse::{self, time_to_entry_text};
    use glib::{subclass::Signal, Properties};
    use gtk::{gdk, prelude::*, subclass::prelude::*, CompositeTemplate};
    use std::{
        cell::{Cell, OnceCell},
        sync::OnceLock,
    };

    const TOLERANCE: f64 = 5.;

    #[derive(Debug, Clone, Copy, Eq, PartialEq)]
    enum DragType {
        Playback,
        Start,
        End,
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
        media_file: OnceCell<gtk::MediaFile>,
        frame_time_approx: Cell<Option<Duration>>,
        position: Cell<i64>,
        duration: Cell<i64>,
        start_end: Cell<Option<(u32, u32)>>,
        gesture_drag: OnceCell<gtk::GestureDrag>,
        drag_start: Cell<f64>,
        drag_type: Cell<DragType>,
        cursor_type: Cell<CursorType>,
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
            static SIGNALS: OnceLock<[Signal; 3]> = OnceLock::new();
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

                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();
                    imp.on_drag_start(x, y);
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
        fn set_media_file(&self, media_file: gtk::MediaFile) {
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

            if !media_file.is_seeking() {
                self.position.set(media_file.timestamp());
            }

            obj.queue_allocate();
        }

        fn on_drag_start(&self, x: f64, _y: f64) {
            self.drag_start.set(x);
            self.drag_type.set(DragType::Playback);

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
                media_file.seek(time);

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

                self.obj()
                    .emit_by_name::<()>("set-start-end", &[&start, &end]);
            };
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
}
