use std::cell::{Cell, RefCell};

use gdk::prelude::*;
use glib::{subclass, subclass::prelude::*, translate::*};
use gst::prelude::*;
use gtk::prelude::*;
use once_cell::unsync::OnceCell;

use crate::{config, parse, window::time_to_entry_text};

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

#[derive(Debug)]
struct Immutable {
    label_current_time: gtk::Label,
    box_timeline_bg: gtk::Box,
    box_timeline_selection: gtk::Box,
    box_timeline_position: gtk::Box,
    pipeline: gst::Pipeline,
    playbin: gst::Element,
    gesture_drag: gtk::GestureDrag,
    event_controller_motion: gtk::EventControllerMotion,
    bus: gst::Bus,
}

static PROPERTIES: [subclass::Property; 2] = [
    subclass::Property("builder", |name| {
        glib::ParamSpec::object(
            name,
            "builder",
            "builder",
            gtk::Builder::static_type(),
            glib::ParamFlags::READWRITE | glib::ParamFlags::CONSTRUCT_ONLY,
        )
    }),
    subclass::Property("duration", |name| {
        glib::ParamSpec::uint64(
            name,
            "duration",
            "duration",
            0,
            std::u64::MAX,
            gst::CLOCK_TIME_NONE.to_glib(),
            glib::ParamFlags::READABLE,
        )
    }),
];

#[derive(Debug)]
pub struct VtVideoPreviewPrivate {
    immutable: OnceCell<Immutable>,
    builder: OnceCell<gtk::Builder>,
    pipeline_playing: Cell<bool>,
    start_end: Cell<Option<(u32, u32)>>,
    drag_start: Cell<f64>,
    drag_type: Cell<DragType>,
    cursor_type: Cell<CursorType>,
    duration: Cell<gst::ClockTime>,
    seeking: Cell<bool>,
    timeout_id: RefCell<Option<glib::SourceId>>,
}

impl ObjectSubclass for VtVideoPreviewPrivate {
    const NAME: &'static str = "VtVideoPreview";
    type ParentType = glib::Object;
    type Instance = subclass::simple::InstanceStruct<Self>;
    type Class = subclass::simple::ClassStruct<Self>;

    glib_object_subclass!();

    fn new() -> Self {
        Self {
            immutable: OnceCell::new(),
            builder: OnceCell::new(),
            pipeline_playing: Cell::new(false),
            start_end: Cell::new(None),
            drag_start: Cell::new(0.),
            drag_type: Cell::new(DragType::Playback),
            cursor_type: Cell::new(CursorType::Normal),
            duration: Cell::new(gst::ClockTime::none()),
            seeking: Cell::new(false),
            timeout_id: RefCell::new(None),
        }
    }

    fn class_init(klass: &mut Self::Class) {
        klass.install_properties(&PROPERTIES);
        klass.add_signal(
            "set-start-end",
            glib::SignalFlags::RUN_FIRST,
            &[glib::Type::U32, glib::Type::U32],
            glib::Type::Unit,
        );
    }
}

impl ObjectImpl for VtVideoPreviewPrivate {
    glib_object_impl!();

    fn set_property(&self, _obj: &glib::Object, id: usize, value: &glib::Value) {
        let prop = &PROPERTIES[id];

        match *prop {
            subclass::Property("builder", ..) => {
                self.builder.set(value.get().unwrap().unwrap()).unwrap()
            }
            _ => unreachable!(),
        }
    }

    fn get_property(&self, _obj: &glib::Object, id: usize) -> Result<glib::Value, ()> {
        let prop = &PROPERTIES[id];

        match *prop {
            subclass::Property("builder", ..) => Ok(self.builder.get().unwrap().to_value()),
            subclass::Property("duration", ..) => Ok(self.duration.get().to_value()),
            _ => unreachable!(),
        }
    }

    fn constructed(&self, obj: &glib::Object) {
        self.parent_constructed(obj);
        let self_ = obj.downcast_ref::<VtVideoPreview>().unwrap();

        let builder = self.builder.get().unwrap();

        let box_video_preview: gtk::Box = builder.get_object("box_video_preview").unwrap();
        let label_current_time: gtk::Label = builder.get_object("label_current_time").unwrap();
        let button_play_pause: gtk::Button = builder.get_object("button_play_pause").unwrap();
        let button_play_pause_image: gtk::Image =
            builder.get_object("button_play_pause_image").unwrap();
        let overlay_timeline: gtk::Overlay = builder.get_object("overlay_timeline").unwrap();
        let event_box_timeline_bg: gtk::EventBox =
            builder.get_object("event_box_timeline_bg").unwrap();
        let box_timeline_bg: gtk::Box = builder.get_object("box_timeline_bg").unwrap();
        let box_timeline_selection: gtk::Box =
            builder.get_object("box_timeline_selection").unwrap();
        let box_timeline_position: gtk::Box = builder.get_object("box_timeline_position").unwrap();

        overlay_timeline.add_overlay(&box_timeline_selection);
        overlay_timeline.set_overlay_pass_through(&box_timeline_selection, true);
        overlay_timeline.add_overlay(&box_timeline_position);
        overlay_timeline.set_overlay_pass_through(&box_timeline_position, true);

        // Set up the drag gesture.
        event_box_timeline_bg.set_events(gdk::EventMask::all());

        let gesture_drag = gtk::GestureDrag::new(&event_box_timeline_bg);
        gesture_drag.connect_drag_begin({
            let self_ = self_.downgrade();
            move |_, x, y| {
                let self_ = self_.upgrade().unwrap();
                let priv_ = VtVideoPreviewPrivate::from_instance(&self_);
                priv_.on_timeline_drag_start(x, y);
            }
        });
        gesture_drag.connect_drag_update({
            let self_ = self_.downgrade();
            move |_, offset_x, offset_y| {
                let self_ = self_.upgrade().unwrap();
                let priv_ = VtVideoPreviewPrivate::from_instance(&self_);
                priv_.on_timeline_drag_update(offset_x, offset_y);
            }
        });

        let event_controller_motion = gtk::EventControllerMotion::new(&event_box_timeline_bg);
        event_controller_motion.connect_motion({
            let self_ = self_.downgrade();
            move |_, x, y| {
                let self_ = self_.upgrade().unwrap();
                let priv_ = VtVideoPreviewPrivate::from_instance(&self_);
                priv_.on_timeline_motion(x, y);
            }
        });

        // Create the GStreamer objects.
        let gtkglsink = gst::ElementFactory::make("gtkglsink", None).expect("TODO");
        let glsinkbin = gst::ElementFactory::make("glsinkbin", None).unwrap();
        glsinkbin
            .set_property("sink", &gtkglsink.to_value())
            .unwrap();
        let widget = gtkglsink
            .get_property("widget")
            .unwrap()
            .get::<gtk::Widget>()
            .unwrap()
            .unwrap();

        let playbin = gst::ElementFactory::make("playbin3", None).unwrap();
        playbin
            .set_property("video-sink", &glsinkbin.to_value())
            .unwrap();

        let pipeline = gst::Pipeline::new(None);
        pipeline.add(&playbin).unwrap();

        // Connect the timeline resize.
        box_timeline_bg.connect_size_allocate({
            let self_ = self_.downgrade();
            move |_, _| {
                let self_ = self_.upgrade().unwrap();
                let priv_ = VtVideoPreviewPrivate::from_instance(&self_);
                priv_.refresh_timeline();
                priv_.refresh_ui();
            }
        });

        // Connect the play-pause button.
        button_play_pause.connect_clicked({
            let self_ = self_.downgrade();
            move |_| {
                let self_ = self_.upgrade().unwrap();
                let priv_ = VtVideoPreviewPrivate::from_instance(&self_);
                priv_
                    .immutable
                    .get()
                    .unwrap()
                    .pipeline
                    .set_state(if priv_.pipeline_playing.get() {
                        gst::State::Paused
                    } else {
                        gst::State::Playing
                    })
                    .unwrap();
            }
        });

        // Refresh the time label and seek slider position on a timer.
        let timeout_id = gtk::timeout_add(100, {
            let self_ = self_.downgrade();
            move || {
                if let Some(self_) = self_.upgrade() {
                    let priv_ = VtVideoPreviewPrivate::from_instance(&self_);
                    priv_.refresh_ui();
                    glib::Continue(true)
                } else {
                    glib::Continue(false)
                }
            }
        });
        *self.timeout_id.borrow_mut() = Some(timeout_id);

        // Handle GStreamer messages.
        let bus = pipeline.get_bus().unwrap();
        bus.add_watch_local({
            let self_ = self_.downgrade();
            move |_, msg| {
                let self_ = if let Some(self_) = self_.upgrade() {
                    self_
                } else {
                    return glib::Continue(false);
                };
                let priv_ = VtVideoPreviewPrivate::from_instance(&self_);

                use gst::MessageView;
                match msg.view() {
                    MessageView::Eos(_) => {
                        button_play_pause_image
                            .set_property_icon_name(Some("media-playback-start-symbolic"));

                        priv_.refresh_ui();
                    }
                    MessageView::StateChanged(state_changed) => {
                        if state_changed.get_current() == gst::State::Playing {
                            priv_.pipeline_playing.set(true);
                            button_play_pause_image
                                .set_property_icon_name(Some("media-playback-pause-symbolic"));
                        } else {
                            priv_.pipeline_playing.set(false);
                            button_play_pause_image
                                .set_property_icon_name(Some("media-playback-start-symbolic"));
                        }

                        priv_.refresh_ui();
                    }
                    MessageView::AsyncDone(_) => {
                        // The seek has finished.
                        priv_.seeking.set(false);
                        priv_.refresh_ui();
                    }
                    MessageView::Error(err) => {
                        g_warning!(
                            config::LOG_DOMAIN,
                            "Error from {:?}: {} ({:?})",
                            err.get_src().map(|s| s.get_path_string()),
                            err.get_error(),
                            err.get_debug()
                        );
                    }
                    _ => (),
                };

                glib::Continue(true)
            }
        })
        .unwrap();

        // Add the video widget to the UI.
        box_video_preview.pack_start(&widget, true, true, 0);

        self.immutable
            .set(Immutable {
                label_current_time,
                box_timeline_bg,
                box_timeline_selection,
                box_timeline_position,
                pipeline,
                playbin,
                gesture_drag,
                event_controller_motion,
                bus,
            })
            .unwrap();
    }
}

glib_wrapper! {
    pub struct VtVideoPreview(
        Object<
            subclass::simple::InstanceStruct<VtVideoPreviewPrivate>,
            subclass::simple::ClassStruct<VtVideoPreviewPrivate>,
            VtVideoPreviewClass
        >
    );

    match fn {
        get_type => || VtVideoPreviewPrivate::get_type().to_glib(),
    }
}

impl VtVideoPreview {
    pub fn new(builder: &gtk::Builder) -> Self {
        glib::Object::new(Self::static_type(), &[("builder", builder)])
            .unwrap()
            .downcast()
            .unwrap()
    }

    pub fn open(&self, uri: &glib::GString) {
        VtVideoPreviewPrivate::from_instance(self).open(uri);
    }

    pub fn set_start_end(&self, start_end: Option<(u32, u32)>) {
        VtVideoPreviewPrivate::from_instance(self).set_start_end(start_end);
    }

    pub fn refresh_timeline(&self) {
        VtVideoPreviewPrivate::from_instance(self).refresh_timeline();
    }

    pub fn destroy(&self) {
        VtVideoPreviewPrivate::from_instance(self).destroy();
    }
}

impl VtVideoPreviewPrivate {
    pub fn destroy(&self) {
        let imm = self.immutable.get().unwrap();

        imm.pipeline.set_state(gst::State::Null).unwrap();
        imm.bus.remove_watch().unwrap();

        if let Some(timeout_id) = self.timeout_id.borrow_mut().take() {
            glib::source_remove(timeout_id);
        }
    }

    pub fn open(&self, uri: &glib::GString) {
        let imm = self.immutable.get().unwrap();

        imm.playbin.set_property("uri", uri).unwrap();

        // Start the playback.
        // Do it asynchronously since it can take a while on a network mount.
        imm.pipeline.call_async(|pipeline| {
            if let Err(err) = pipeline.set_state(gst::State::Playing) {
                // This fails for example when the GL dependencies aren't installed for the flatpak
                // (when installing from Ubuntu 18.04 Software on a clean system, it doesn't
                // install the dependencies properly).

                g_warning!(
                    config::LOG_DOMAIN,
                    "pipeline.set_state(Playing) error: {}",
                    err
                );
            }
        });
    }

    pub fn set_start_end(&self, start_end: Option<(u32, u32)>) {
        self.start_end.set(start_end);
    }

    fn refresh_ui(&self) {
        let imm = self.immutable.get().unwrap();

        if let Some(position) = imm.pipeline.query_position::<gst::ClockTime>() {
            let nanoseconds = position.nanoseconds().unwrap();
            let mut seconds = nanoseconds / 1_000_000_000;
            let mut minutes = seconds / 60;
            let hours = minutes / 60;
            seconds %= 60;
            minutes %= 60;

            let time = if hours == 0 {
                format!("{}:{:02}", minutes, seconds)
            } else {
                format!("{}:{:02}:{:02}", hours, minutes, seconds)
            };

            imm.label_current_time
                .set_markup(&format!("<span font_features=\"tnum\">{}</span>", time));

            if let Some(duration) = imm.pipeline.query_duration::<gst::ClockTime>() {
                // There's a DurationChanged message, however it is delivered before the first
                // AsyncDone, which means it's possible that query_duration won't work yet. For
                // instance, with GST_DEBUG=5 querying the duration upon receiving DurationChanged
                // returns None all of the time.
                //
                // Hence, update the duration from here; this callback is called on a timer as well
                // as upon receiving AsyncDone.
                if self.duration.get() != duration {
                    self.duration.set(duration);
                    self.get_instance().notify("duration");
                }

                // Don't modify the position during seeking as it's out of date.
                if !self.seeking.get() {
                    let value = position.nanoseconds().unwrap() as f64
                        / duration.nanoseconds().unwrap() as f64;

                    let width = imm.box_timeline_bg.get_allocated_width();
                    let margin_start = (value * width as f64).round() as i32;
                    imm.box_timeline_position.set_margin_start(margin_start);
                }
            }
        }
    }

    pub fn refresh_timeline(&self) {
        let imm = self.immutable.get().unwrap();

        let start_end = self.start_end.get();
        let duration = imm.pipeline.query_duration::<gst::ClockTime>();

        if start_end.is_none() || duration.is_none() {
            imm.box_timeline_selection.set_opacity(0.);
            return;
        }

        imm.box_timeline_selection.set_opacity(1.);
        let (start, end) = start_end.unwrap();
        let duration = duration.unwrap();

        let duration = duration.mseconds().unwrap() as f64;
        let start = (start as f64 / duration).min(1.).max(0.);
        let end = (end as f64 / duration).min(1.).max(0.);

        let width = imm.box_timeline_bg.get_allocated_width();
        let margin_start = (start * width as f64).round() as i32;
        let margin_end = ((1. - end) * width as f64).round() as i32;
        imm.box_timeline_selection.set_margin_start(margin_start);
        imm.box_timeline_selection.set_margin_end(margin_end);
    }

    fn on_timeline_drag_start(&self, x: f64, _y: f64) {
        self.drag_start.set(x);
        self.drag_type.set(DragType::Playback);

        if self.start_end.get().is_some() {
            let imm = self.immutable.get().unwrap();
            let width = imm.box_timeline_bg.get_allocated_width() as f64;
            let start = imm.box_timeline_selection.get_margin_start() as f64;
            let end = width - imm.box_timeline_selection.get_margin_end() as f64;

            if (x - end).abs() <= TOLERANCE {
                self.drag_type.set(DragType::End);
                self.drag_start.set(end);
            } else if (x - start).abs() <= TOLERANCE {
                self.drag_type.set(DragType::Start);
                self.drag_start.set(start);
            }
        }

        self.on_timeline_drag_update(0., 0.);
    }

    fn on_timeline_drag_update(&self, offset_x: f64, _offset_y: f64) {
        let imm = self.immutable.get().unwrap();

        let x = self.drag_start.get() + offset_x;
        let width = imm.box_timeline_bg.get_allocated_width() as f64;

        // Sanitize (this can get weird values when resizing the window while dragging).
        let x = x.min(width).max(0.);
        let value = x / width;

        let position_width = imm.box_timeline_position.get_allocated_width() as f64;
        imm.box_timeline_position
            .set_margin_start(x.min(width - position_width) as i32);

        if let Some(duration) = imm.pipeline.query_duration::<gst::ClockTime>() {
            let time = duration.nanoseconds().unwrap() as f64 * value;
            let time = gst::ClockTime::from_nseconds(time as u64);

            self.seeking.set(true);

            // Seek asynchronously as it takes longer than desirable.
            imm.pipeline.call_async(move |pipeline| {
                pipeline.seek_simple(gst::SeekFlags::FLUSH, time).unwrap()
            });

            let start_end = self.start_end.get();
            if start_end.is_none() {
                return;
            }

            let (start, end) = start_end.unwrap();
            let time = time.mseconds().unwrap() as u32;

            let (start, end) = match self.drag_type.get() {
                DragType::Start => {
                    let text = time_to_entry_text(gst::ClockTime::from_mseconds(time.into()));

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
                    let text = time_to_entry_text(gst::ClockTime::from_mseconds(time.into()));

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

            self.get_instance()
                .emit("set-start-end", &[&start, &end])
                .unwrap();
        }
    }

    fn on_timeline_motion(&self, x: f64, _y: f64) {
        let imm = self.immutable.get().unwrap();

        // Don't change the cursor while in drag.
        if imm.gesture_drag.is_active() {
            return;
        }

        let resizing_cursor = if self.start_end.get().is_some() {
            let width = imm.box_timeline_bg.get_allocated_width() as f64;
            let start = imm.box_timeline_selection.get_margin_start() as f64;
            let end = width - imm.box_timeline_selection.get_margin_end() as f64;

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
            let display = imm.box_timeline_bg.get_display();
            let cursor = gdk::Cursor::from_name(&display, cursor_type.gtk_cursor_name()).unwrap();
            imm.box_timeline_bg
                .get_window()
                .unwrap()
                .set_cursor(Some(&cursor));
            self.cursor_type.set(cursor_type);
        }
    }
}
