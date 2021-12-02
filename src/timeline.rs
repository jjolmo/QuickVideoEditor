use glib::subclass::prelude::*;
use gtk::glib;

mod imp {
    use super::*;
    use crate::parse::{self, time_to_entry_text};
    use gtk::{gdk, glib::subclass, prelude::*, subclass::prelude::*, CompositeTemplate};
    use once_cell::unsync::OnceCell;
    use std::{cell::Cell, time::Duration};

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

    #[derive(Debug, CompositeTemplate)]
    #[template(resource = "/org/gnome/gitlab/YaLTeR/VideoTrimmer/timeline.ui")]
    pub struct VtTimeline {
        #[template_child]
        box_timeline_position: TemplateChild<gtk::Box>,
        #[template_child]
        box_timeline_selection: TemplateChild<gtk::Box>,

        media_file: OnceCell<gtk::MediaFile>,
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
            use once_cell::sync::Lazy;
            static PROPERTIES: Lazy<[glib::ParamSpec; 1]> = Lazy::new(|| {
                [glib::ParamSpec::new_object(
                    "media-file",
                    "media-file",
                    "media-file",
                    gtk::MediaFile::static_type(),
                    glib::ParamFlags::WRITABLE,
                )]
            });

            PROPERTIES.as_ref()
        }

        fn signals() -> &'static [subclass::Signal] {
            use once_cell::sync::Lazy;
            static SIGNALS: Lazy<[subclass::Signal; 1]> = Lazy::new(|| {
                [subclass::Signal::builder(
                    "set-start-end",
                    &[glib::Type::U32.into(), glib::Type::U32.into()],
                    glib::Type::UNIT.into(),
                )
                .build()]
            });

            SIGNALS.as_ref()
        }

        fn set_property(
            &self,
            self_: &Self::Type,
            _id: usize,
            value: &glib::Value,
            pspec: &glib::ParamSpec,
        ) {
            match pspec.name() {
                "media-file" => {
                    let media_file: gtk::MediaFile = value.get().unwrap();

                    media_file.connect_timestamp_notify({
                        let self_ = self_.downgrade();
                        move |_| {
                            let self_ = self_.upgrade().unwrap();
                            let priv_ = VtTimeline::from_instance(&self_);
                            priv_.refresh();
                        }
                    });

                    media_file.connect_duration_notify({
                        let self_ = self_.downgrade();
                        move |_| {
                            let self_ = self_.upgrade().unwrap();
                            let priv_ = VtTimeline::from_instance(&self_);
                            priv_.refresh();
                        }
                    });

                    media_file.connect_seeking_notify({
                        let self_ = self_.downgrade();
                        move |media_file| {
                            // This callback is for updating position once seeking has completed.
                            if media_file.is_seeking() {
                                return;
                            }

                            let self_ = self_.upgrade().unwrap();
                            let priv_ = VtTimeline::from_instance(&self_);
                            priv_.refresh();
                        }
                    });

                    self.media_file.set(media_file).unwrap();
                }
                _ => unreachable!(),
            }
        }

        fn constructed(&self, obj: &Self::Type) {
            self.parent_constructed(obj);

            // Invisible until we get duration.
            self.box_timeline_position.set_child_visible(false);
            self.box_timeline_selection.set_child_visible(false);

            // For some reason doesn't work from the .ui file.
            obj.set_overflow(gtk::Overflow::Hidden);

            // Set up the drag gesture.
            let gesture_drag = gtk::GestureDrag::new();
            gesture_drag.connect_drag_begin({
                let obj = obj.downgrade();
                move |_, x, y| {
                    let obj = obj.upgrade().unwrap();
                    let priv_ = Self::from_instance(&obj);
                    priv_.on_drag_start(x, y);
                }
            });
            gesture_drag.connect_drag_update({
                let obj = obj.downgrade();
                move |_, offset_x, offset_y| {
                    let obj = obj.upgrade().unwrap();
                    let priv_ = Self::from_instance(&obj);
                    priv_.on_drag_update(offset_x, offset_y);
                }
            });
            obj.add_controller(&gesture_drag);
            self.gesture_drag.set(gesture_drag).unwrap();

            let event_controller_motion = gtk::EventControllerMotion::new();
            event_controller_motion.connect_motion({
                let obj = obj.downgrade();
                move |_, x, y| {
                    let obj = obj.upgrade().unwrap();
                    let priv_ = Self::from_instance(&obj);
                    priv_.on_motion(x, y);
                }
            });
            obj.add_controller(&event_controller_motion);
        }

        fn dispose(&self, obj: &Self::Type) {
            while let Some(child) = obj.first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for VtTimeline {
        fn size_allocate(&self, _widget: &Self::Type, width: i32, height: i32, baseline: i32) {
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
                &gtk::Allocation {
                    x: x - position_width / 2,
                    y: 0,
                    width: position_width,
                    height: position_height,
                },
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
                    &gtk::Allocation {
                        x,
                        y: 0,
                        width: selection_width,
                        height: selection_height,
                    },
                    baseline,
                );
            }
        }
    }

    impl VtTimeline {
        pub fn set_start_end(&self, start_end: Option<(u32, u32)>) {
            self.start_end.set(start_end);
            self.refresh();
        }

        pub fn refresh(&self) {
            let self_ = self.instance();

            let media_file = self.media_file.get().unwrap();
            let duration = media_file.duration();
            self.duration.set(duration);
            if duration == 0 {
                self.box_timeline_position.set_child_visible(false);
                self.box_timeline_selection.set_child_visible(false);
                self_.queue_allocate();
                return;
            }

            self.box_timeline_position.set_child_visible(true);
            self.box_timeline_selection
                .set_child_visible(self.start_end.get().is_some());

            if !media_file.is_seeking() {
                self.position.set(media_file.timestamp());
            }

            self_.queue_allocate();
        }

        fn on_drag_start(&self, x: f64, _y: f64) {
            self.drag_start.set(x);
            self.drag_type.set(DragType::Playback);

            if self.start_end.get().is_some() {
                let allocation = self.box_timeline_selection.allocation();
                let start = allocation.x as f64;
                let end = (allocation.x + allocation.width) as f64;

                if (x - end).abs() <= TOLERANCE {
                    self.drag_type.set(DragType::End);
                    self.drag_start.set(end);
                } else if (x - start).abs() <= TOLERANCE {
                    self.drag_type.set(DragType::Start);
                    self.drag_start.set(start);
                }
            }

            self.on_drag_update(0., 0.);
        }

        fn on_drag_update(&self, offset_x: f64, _offset_y: f64) {
            let self_ = self.instance();

            let x = self.drag_start.get() + offset_x;
            let width = self_.allocated_width() as f64;

            // Sanitize (this can get weird values when resizing the window while dragging).
            let x = x.min(width).max(0.);
            let value = x / width;

            let media_file = self.media_file.get().unwrap();
            let duration = media_file.duration();
            if duration != 0 {
                let time = (duration as f64 * value) as i64;
                media_file.seek(time);

                // Update the position for responsive seeking.
                self.position.set(time);
                self_.queue_allocate();

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

                self.instance()
                    .emit_by_name("set-start-end", &[&start, &end])
                    .unwrap();
            };
        }

        fn on_motion(&self, x: f64, _y: f64) {
            let self_ = self.instance();

            // Don't change the cursor while in drag.
            if self.gesture_drag.get().unwrap().is_active() {
                return;
            }

            let resizing_cursor = if self.start_end.get().is_some() {
                let allocation = self.box_timeline_selection.allocation();
                let start = allocation.x as f64;
                let end = (allocation.x + allocation.width) as f64;

                (x - end).abs() <= TOLERANCE || (x - start).abs() <= TOLERANCE
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
                self_.set_cursor(Some(&cursor));
                self.cursor_type.set(cursor_type);
            }
        }
    }
}

glib::wrapper! {
    pub struct VtTimeline(ObjectSubclass<imp::VtTimeline>)
        @extends gtk::Widget;
}

impl VtTimeline {
    pub fn set_start_end(&self, start_end: Option<(u32, u32)>) {
        imp::VtTimeline::from_instance(self).set_start_end(start_end);
    }
}
