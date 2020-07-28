use std::{
    cell::{Cell, RefCell},
    ffi::OsStr,
    path::{Path, PathBuf},
};

use futures_util::future::{abortable, FutureExt};
use gdk::prelude::*;
use gettextrs::*;
use gio::prelude::*;
use glib::{subclass, subclass::prelude::*, translate::*};
use gst::prelude::*;
use gtk::{prelude::*, subclass::prelude::*};
use once_cell::unsync::OnceCell;

use crate::{config, parse};

// Extracted from Totem.
const VIDEO_MIME_TYPES: &[&str] = &[
    "image/gif",
    "video/3gp",
    "video/3gpp",
    "video/3gpp2",
    "video/dv",
    "video/divx",
    "video/fli",
    "video/flv",
    "video/mp2t",
    "video/mp4",
    "video/mp4v-es",
    "video/mpeg",
    "video/mpeg-system",
    "video/msvideo",
    "video/ogg",
    "video/quicktime",
    "video/vivo",
    "video/vnd.divx",
    "video/vnd.mpegurl",
    "video/vnd.rn-realvideo",
    "video/vnd.vivo",
    "video/webm",
    "video/x-anim",
    "video/x-avi",
    "video/x-flc",
    "video/x-fli",
    "video/x-flic",
    "video/x-flv",
    "video/x-m4v",
    "video/x-matroska",
    "video/x-mjpeg",
    "video/x-mpeg",
    "video/x-mpeg2",
    "video/x-ms-asf",
    "video/x-ms-asf-plugin",
    "video/x-ms-asx",
    "video/x-msvideo",
    "video/x-ms-wm",
    "video/x-ms-wmv",
    "video/x-ms-wvx",
    "video/x-nsv",
    "video/x-ogm+ogg",
    "video/x-theora",
    "video/x-theora+ogg",
    "video/x-totem-stream",
];

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
struct Widgets {
    header_bar: gtk::HeaderBar,
    stack_main: gtk::Stack,
    stack_header_bar: gtk::Stack,
    button_open: gtk::Button,
    button_trim: gtk::Button,
    entry_start: gtk::Entry,
    entry_end: gtk::Entry,
    label_current_time: gtk::Label,
    box_timeline_bg: gtk::Box,
    box_timeline_selection: gtk::Box,
    box_timeline_position: gtk::Box,
}

#[derive(Debug)]
pub struct VtWindowPrivate {
    widgets: OnceCell<Widgets>,
    content_type: RefCell<Option<glib::GString>>,
    input_path: RefCell<Option<PathBuf>>,
    pipeline: OnceCell<gst::Pipeline>,
    playbin: OnceCell<gst::Element>,
    pipeline_playing: Cell<bool>,
    start_end: Cell<Option<(u32, u32)>>,
    gesture_drag: OnceCell<gtk::GestureDrag>,
    drag_start: Cell<f64>,
    drag_type: Cell<DragType>,
    event_controller_motion: OnceCell<gtk::EventControllerMotion>,
    cursor_type: Cell<CursorType>,
    received_duration: Cell<bool>,
    seeking: Cell<bool>,
}

fn time_to_entry_text(time: gst::ClockTime) -> String {
    let nanoseconds = time.nanoseconds().unwrap();
    let mut seconds = nanoseconds / 1_000_000_000;
    let mut minutes = seconds / 60;
    let hours = minutes / 60;
    seconds %= 60;
    minutes %= 60;

    let fractional = (nanoseconds / 100_000_000) % 10;

    if hours == 0 {
        format!("{}:{:02}.{}", minutes, seconds, fractional)
    } else {
        format!("{}:{:02}:{:02}.{}", hours, minutes, seconds, fractional)
    }
}

impl VtWindowPrivate {
    fn refresh_ui(&self) {
        let pipeline = self.pipeline.get().unwrap();
        let widgets = self.widgets.get().unwrap();

        if let Some(position) = pipeline.query_position::<gst::ClockTime>() {
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

            widgets
                .label_current_time
                .set_markup(&format!("<span font_features=\"tnum\">{}</span>", time));

            if let Some(duration) = pipeline.query_duration::<gst::ClockTime>() {
                self.on_got_duration(duration);

                // Don't modify the position during seeking as it's out of date.
                if !self.seeking.get() {
                    let value = position.nanoseconds().unwrap() as f64
                        / duration.nanoseconds().unwrap() as f64;

                    let width = widgets.box_timeline_bg.get_allocated_width();
                    let margin_start = (value * width as f64).round() as i32;
                    widgets.box_timeline_position.set_margin_start(margin_start);
                }
            }
        }
    }

    fn refresh_timeline(&self) {
        let pipeline = self.pipeline.get().unwrap();
        let widgets = self.widgets.get().unwrap();

        let start_end = self.start_end.get();
        let duration = pipeline.query_duration::<gst::ClockTime>();

        if start_end.is_none() || duration.is_none() {
            widgets.box_timeline_selection.set_opacity(0.);
            return;
        }

        widgets.box_timeline_selection.set_opacity(1.);
        let (start, end) = start_end.unwrap();
        let duration = duration.unwrap();

        let duration = duration.mseconds().unwrap() as f64;
        let start = (start as f64 / duration).min(1.).max(0.);
        let end = (end as f64 / duration).min(1.).max(0.);

        let width = widgets.box_timeline_bg.get_allocated_width();
        let margin_start = (start * width as f64).round() as i32;
        let margin_end = ((1. - end) * width as f64).round() as i32;
        widgets
            .box_timeline_selection
            .set_margin_start(margin_start);
        widgets.box_timeline_selection.set_margin_end(margin_end);
    }

    fn on_entry_changed(&self) {
        let widgets = self.widgets.get().unwrap();

        let start_end = validate_entries(&widgets.entry_start, &widgets.entry_end);
        self.start_end.set(start_end);

        widgets.button_trim.set_sensitive(start_end.is_some());

        self.refresh_timeline();
    }

    fn on_timeline_drag_start(&self, x: f64, _y: f64) {
        self.drag_start.set(x);
        self.drag_type.set(DragType::Playback);

        if self.start_end.get().is_some() {
            let widgets = self.widgets.get().unwrap();
            let width = widgets.box_timeline_bg.get_allocated_width() as f64;
            let start = widgets.box_timeline_selection.get_margin_start() as f64;
            let end = width - widgets.box_timeline_selection.get_margin_end() as f64;

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
        let widgets = self.widgets.get().unwrap();
        let pipeline = self.pipeline.get().unwrap();

        let x = self.drag_start.get() + offset_x;
        let width = widgets.box_timeline_bg.get_allocated_width() as f64;

        // Sanitize (this can get weird values when resizing the window while dragging).
        let x = x.min(width).max(0.);
        let value = x / width;

        let position_width = widgets.box_timeline_position.get_allocated_width() as f64;
        widgets
            .box_timeline_position
            .set_margin_start(x.min(width - position_width) as i32);

        if let Some(duration) = pipeline.query_duration::<gst::ClockTime>() {
            let time = duration.nanoseconds().unwrap() as f64 * value;
            let time = gst::ClockTime::from_nseconds(time as u64);

            self.seeking.set(true);

            // Seek asynchronously as it takes longer than desirable.
            pipeline.call_async(move |pipeline| {
                pipeline.seek_simple(gst::SeekFlags::FLUSH, time).unwrap()
            });

            let start_end = self.start_end.get();
            if start_end.is_none() {
                return;
            }

            let (start, end) = start_end.unwrap();
            let start = gst::ClockTime::from_mseconds(start as u64);
            let end = gst::ClockTime::from_mseconds(end as u64);

            match self.drag_type.get() {
                DragType::Start => {
                    let text = time_to_entry_text(time);

                    if gst::ClockTime::from_mseconds(parse::timestamp(&text).unwrap() as u64) == end
                    {
                        // Don't set the text if the timestamps will match as that counts as an
                        // invalid region.
                        return;
                    }

                    if time <= end {
                        widgets.entry_start.set_text(&text);
                    } else {
                        widgets.entry_start.set_text(&widgets.entry_end.get_text());
                        widgets.entry_end.set_text(&text);
                        self.drag_type.set(DragType::End);
                    }
                }
                DragType::End => {
                    let text = time_to_entry_text(time);

                    if gst::ClockTime::from_mseconds(parse::timestamp(&text).unwrap() as u64)
                        == start
                    {
                        // Don't set the text if the timestamps will match as that counts as an
                        // invalid region.
                        return;
                    }

                    if time >= start {
                        widgets.entry_end.set_text(&text);
                    } else {
                        widgets.entry_end.set_text(&widgets.entry_start.get_text());
                        widgets.entry_start.set_text(&text);
                        self.drag_type.set(DragType::Start);
                    }
                }
                _ => (),
            }
        }
    }

    fn on_timeline_motion(&self, x: f64, _y: f64) {
        // Don't change the cursor while in drag.
        if self.gesture_drag.get().unwrap().is_active() {
            return;
        }

        let widgets = self.widgets.get().unwrap();

        let resizing_cursor = if self.start_end.get().is_some() {
            let width = widgets.box_timeline_bg.get_allocated_width() as f64;
            let start = widgets.box_timeline_selection.get_margin_start() as f64;
            let end = width - widgets.box_timeline_selection.get_margin_end() as f64;

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
            let display = widgets.box_timeline_bg.get_display();
            let cursor = gdk::Cursor::from_name(&display, cursor_type.gtk_cursor_name()).unwrap();
            widgets
                .box_timeline_bg
                .get_window()
                .unwrap()
                .set_cursor(Some(&cursor));
            self.cursor_type.set(cursor_type);
        }
    }

    fn on_got_duration(&self, duration: gst::ClockTime) {
        if self.received_duration.get() {
            return;
        }

        self.received_duration.set(true);

        let widgets = self.widgets.get().unwrap();

        // If the user hasn't started typing in the timestamp entries, fill them with default
        // values.
        if !widgets.entry_start.get_text().is_empty() || !widgets.entry_end.get_text().is_empty() {
            return;
        }

        let duration = duration.nanoseconds().unwrap() as f64;
        let start = duration / 3.;
        let end = start * 2.;

        let start = start as u64;
        let end = (end as u64).max(start + 1);

        let start = gst::ClockTime::from_nseconds(start);
        let end = gst::ClockTime::from_nseconds(end);

        widgets.entry_start.set_text(&time_to_entry_text(start));
        widgets.entry_end.set_text(&time_to_entry_text(end));

        // Select the text so the behavior of typing doesn't change compared to if we hadn't set
        // the text.
        if widgets.entry_start.is_focus() {
            widgets.entry_start.select_region(0, -1);
        } else if widgets.entry_end.is_focus() {
            widgets.entry_end.select_region(0, -1);
        }
    }
}

impl ObjectSubclass for VtWindowPrivate {
    const NAME: &'static str = "VtWindow";
    type ParentType = gtk::ApplicationWindow;
    type Instance = subclass::simple::InstanceStruct<Self>;
    type Class = subclass::simple::ClassStruct<Self>;

    glib_object_subclass!();

    fn new() -> Self {
        Self {
            widgets: OnceCell::new(),
            content_type: RefCell::new(None),
            input_path: RefCell::new(None),
            pipeline: OnceCell::new(),
            playbin: OnceCell::new(),
            pipeline_playing: Cell::new(false),
            start_end: Cell::new(None),
            gesture_drag: OnceCell::new(),
            drag_start: Cell::new(0.),
            drag_type: Cell::new(DragType::Playback),
            event_controller_motion: OnceCell::new(),
            cursor_type: Cell::new(CursorType::Normal),
            received_duration: Cell::new(false),
            seeking: Cell::new(false),
        }
    }
}

impl ObjectImpl for VtWindowPrivate {
    glib_object_impl!();

    fn constructed(&self, obj: &glib::Object) {
        self.parent_constructed(obj);
        let self_ = obj.downcast_ref::<VtWindow>().unwrap();

        let builder =
            gtk::Builder::from_resource("/org/gnome/gitlab/YaLTeR/VideoTrimmer/window.ui");

        let stack_main: gtk::Stack = builder.get_object("stack_main").unwrap();
        let stack_header_bar: gtk::Stack = builder.get_object("stack_header_bar").unwrap();
        let header_bar: gtk::HeaderBar = builder.get_object("header_bar").unwrap();
        let button_open: gtk::Button = builder.get_object("button_open").unwrap();
        let button_trim: gtk::Button = builder.get_object("button_trim").unwrap();
        let entry_start: gtk::Entry = builder.get_object("entry_start").unwrap();
        let entry_end: gtk::Entry = builder.get_object("entry_end").unwrap();
        let box_main: gtk::Box = builder.get_object("box_main").unwrap();
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
                let priv_ = VtWindowPrivate::from_instance(&self_);
                priv_.on_timeline_drag_start(x, y);
            }
        });
        gesture_drag.connect_drag_update({
            let self_ = self_.downgrade();
            move |_, offset_x, offset_y| {
                let self_ = self_.upgrade().unwrap();
                let priv_ = VtWindowPrivate::from_instance(&self_);
                priv_.on_timeline_drag_update(offset_x, offset_y);
            }
        });
        self.gesture_drag.set(gesture_drag).unwrap();

        let event_controller_motion = gtk::EventControllerMotion::new(&event_box_timeline_bg);
        event_controller_motion.connect_motion({
            let self_ = self_.downgrade();
            move |_, x, y| {
                let self_ = self_.upgrade().unwrap();
                let priv_ = VtWindowPrivate::from_instance(&self_);
                priv_.on_timeline_motion(x, y);
            }
        });
        self.event_controller_motion
            .set(event_controller_motion)
            .unwrap();

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
        self.playbin.set(playbin).unwrap();

        // Connect the timeline resize.
        box_timeline_bg.connect_size_allocate({
            let self_ = self_.downgrade();
            move |_, _| {
                let self_ = self_.upgrade().unwrap();
                let priv_ = VtWindowPrivate::from_instance(&self_);
                priv_.refresh_timeline();
                priv_.refresh_ui();
            }
        });

        // Connect the play-pause button.
        button_play_pause.connect_clicked({
            let self_ = self_.downgrade();
            move |_| {
                let self_ = self_.upgrade().unwrap();
                let priv_ = VtWindowPrivate::from_instance(&self_);
                priv_
                    .pipeline
                    .get()
                    .unwrap()
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
                    let priv_ = VtWindowPrivate::from_instance(&self_);
                    priv_.refresh_ui();
                    glib::Continue(true)
                } else {
                    glib::Continue(false)
                }
            }
        });

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
                let priv_ = VtWindowPrivate::from_instance(&self_);

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

        // Clean up upon window closing.
        let timeout_id = RefCell::new(Some(timeout_id));
        self_.connect_destroy(move |self_| {
            let self_ = self_.clone().downcast::<VtWindow>().unwrap();
            let priv_ = VtWindowPrivate::from_instance(&self_);

            priv_
                .pipeline
                .get()
                .unwrap()
                .set_state(gst::State::Null)
                .unwrap();

            bus.remove_watch().unwrap();
            if let Some(timeout_id) = timeout_id.borrow_mut().take() {
                glib::source_remove(timeout_id);
            }
        });

        self.pipeline.set(pipeline).unwrap();

        // Add the video widget to the UI.
        box_main.pack_start(&widget, true, true, 0);

        self_.add(&stack_main);
        self_.set_titlebar(Some(&stack_header_bar));

        // The open button.
        button_open.connect_clicked(clone!(@weak self_ => move |_| {
            let filter = gtk::FileFilter::new();
            for mime_type in VIDEO_MIME_TYPES {
                filter.add_mime_type(mime_type);
            }

            let file_chooser = gtk::FileChooserNativeBuilder::new()
                .transient_for(&self_)
                .action(gtk::FileChooserAction::Open)
                // Translators: file chooser dialog title.
                .title(&gettext("Open video"))
                .filter(&filter)
                .build();

            let response = file_chooser.run();
            if response == gtk::ResponseType::Accept {
                self_.open(file_chooser.get_file().unwrap());
            }
        }));

        // Start and end timestamp validation and visualization.
        entry_start.connect_property_text_notify({
            let self_ = self_.downgrade();
            move |_| {
                let self_ = self_.upgrade().unwrap();
                let priv_ = VtWindowPrivate::from_instance(&self_);
                priv_.on_entry_changed();
            }
        });
        entry_end.connect_property_text_notify({
            let self_ = self_.downgrade();
            move |_| {
                let self_ = self_.upgrade().unwrap();
                let priv_ = VtWindowPrivate::from_instance(&self_);
                priv_.on_entry_changed();
            }
        });

        // The trim button.
        button_trim.connect_clicked(clone!(@weak self_ => move |_| {
            let priv_ = VtWindowPrivate::from_instance(&self_);
            let widgets = priv_.widgets.get().unwrap();

            if validate_entries(&widgets.entry_start, &widgets.entry_end).is_none() {
                // This should not happen normally because the button should be disabled.
                g_warning!(config::LOG_DOMAIN,"Trim pressed with invalid timestamps");
                return;
            }

            let start = widgets.entry_start.get_text();
            let end = widgets.entry_end.get_text();

            let extension = priv_.content_type
                .borrow()
                .as_ref()
                .map(glib::GString::as_str)
                .and_then(|content_type| {
                    if content_type == "video/x-matroska" {
                        // mime_guess returns "mk3d" for matroska which is weird.
                        Some(&["mkv"][..])
                    } else {
                        mime_guess::get_mime_extensions_str(content_type)
                    }
                })
                .and_then(|exts| exts.get(0))
                .unwrap_or(&"mp4");

            let input_path = priv_.input_path.borrow();
            if input_path.is_none() {
                // This should not happen normally because if the button is visible then we should
                // have the input path already.
                g_warning!(config::LOG_DOMAIN,"Trim pressed without input path");
                return;
            }

            let input_path = input_path.as_deref().unwrap();

            trim(self_.clone(), input_path, extension, start, end);
        }));

        let widgets = Widgets {
            header_bar,
            stack_main,
            stack_header_bar,
            button_open,
            button_trim,
            entry_start,
            entry_end,
            label_current_time,
            box_timeline_bg,
            box_timeline_selection,
            box_timeline_position,
        };
        self.widgets.set(widgets).unwrap();
    }
}

impl WidgetImpl for VtWindowPrivate {}
impl ContainerImpl for VtWindowPrivate {}
impl BinImpl for VtWindowPrivate {}
impl WindowImpl for VtWindowPrivate {}
impl ApplicationWindowImpl for VtWindowPrivate {}

glib_wrapper! {
    pub struct VtWindow(
        Object<
            subclass::simple::InstanceStruct<VtWindowPrivate>,
            subclass::simple::ClassStruct<VtWindowPrivate>,
            VtAppWindowClass
        >
    )
        @extends gtk::Widget, gtk::Container, gtk::Bin, gtk::Window, gtk::ApplicationWindow;

    match fn {
        get_type => || VtWindowPrivate::get_type().to_glib(),
    }
}

impl VtWindow {
    pub fn new(app: &gtk::Application) -> Self {
        let window = glib::Object::new(
            Self::static_type(),
            &[
                ("application", app),
                // These parameters are chosen to make the default size of the video 640×360.
                ("default-width", &640),
                ("default-height", &488),
            ],
        )
        .expect("Failed to create VtWindow")
        .downcast::<VtWindow>()
        .expect("Created VtWindow is of wrong type");

        let provider = gtk::CssProvider::new();
        provider.load_from_resource("/org/gnome/gitlab/YaLTeR/VideoTrimmer/style.css");
        gtk::StyleContext::add_provider_for_screen(
            &gdk::Screen::get_default().unwrap(),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        app.add_window(&window);

        window
    }

    pub fn open(&self, file: gio::File) {
        let priv_ = VtWindowPrivate::from_instance(self);
        let widgets = priv_.widgets.get().unwrap();

        widgets.stack_main.set_visible_child_name("page_main");
        widgets.stack_header_bar.set_visible_child_name("page_main");

        priv_
            .playbin
            .get()
            .unwrap()
            .set_property("uri", &file.get_uri())
            .unwrap();

        // Start the playback.
        // Do it asynchronously since it can take a while on a network mount.
        priv_.pipeline.get().unwrap().call_async(|pipeline| {
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

        // Focus the entry when coming from the empty state.
        widgets.entry_start.grab_focus();

        // Verified in callers.
        *priv_.input_path.borrow_mut() = Some(file.get_path().unwrap());

        // Get the display name and content type.
        let future = {
            let self_ = self.clone();
            async move {
                let priv_ = VtWindowPrivate::from_instance(&self_);
                let widgets = priv_.widgets.get().unwrap();

                // May take a long time on a network mount.
                let info = file
                    .query_info_async_future(
                        "standard::display-name,standard::fast-content-type",
                        gio::FileQueryInfoFlags::NONE,
                        glib::PRIORITY_DEFAULT,
                    )
                    .await;

                match info {
                    Ok(info) => {
                        let display_name = info.get_display_name();
                        widgets
                            .header_bar
                            .set_subtitle(display_name.as_ref().map(glib::GString::as_str));

                        if let Some(fast_content_type) =
                            info.get_attribute_string("standard::fast-content-type")
                        {
                            g_debug!(
                                config::LOG_DOMAIN,
                                "fast-content-type: {}",
                                fast_content_type
                            );
                            *priv_.content_type.borrow_mut() = Some(fast_content_type);
                        }
                    }
                    // Fails when the file does not exist.
                    Err(err) => {
                        let dialog = gtk::MessageDialogBuilder::new()
                            // Translators: error dialog text when the input file information could
                            // not be retrieved (e.g. there's no such file on disk).
                            .text(&gettext("Could not get input video information"))
                            .secondary_text(&format!("{}", err))
                            .message_type(gtk::MessageType::Error)
                            .buttons(gtk::ButtonsType::Ok)
                            .transient_for(&self_)
                            .build();
                        dialog.run();
                        self_.get_application().unwrap().quit();
                        return;
                    }
                }
            }
        };
        glib::MainContext::default().spawn_local(future);
    }
}

fn validate_entries(entry_start: &gtk::Entry, entry_end: &gtk::Entry) -> Option<(u32, u32)> {
    let style_start = entry_start.get_style_context();
    let style_end = entry_end.get_style_context();
    style_start.remove_class("error");
    style_end.remove_class("error");

    let text_start = entry_start.get_text();
    let timestamp_start = parse::timestamp(text_start.as_str());
    let text_end = entry_end.get_text();
    let timestamp_end = parse::timestamp(text_end.as_str());

    if timestamp_start.is_none() {
        style_start.add_class("error");
    }
    if timestamp_end.is_none() {
        style_end.add_class("error");
    }
    if let (Some(timestamp_start), Some(timestamp_end)) = (timestamp_start, timestamp_end) {
        if timestamp_start >= timestamp_end {
            style_end.add_class("error");
        } else {
            return Some((timestamp_start, timestamp_end));
        }
    }

    None
}

fn trim(
    window: VtWindow,
    input_path: &Path,
    extension: &str,
    start: glib::GString,
    end: glib::GString,
) {
    g_debug!(config::LOG_DOMAIN, "trim: from {} to {}", start, end);

    let file_chooser = gtk::FileChooserNativeBuilder::new()
        .transient_for(&window)
        .action(gtk::FileChooserAction::Save)
        .do_overwrite_confirmation(true)
        .build();
    // Translators: this is the name part of the default filename presented in the save dialog.
    file_chooser.set_current_name(format!("{}.{}", gettext("Trimmed video"), extension));

    let response = file_chooser.run();
    if response == gtk::ResponseType::Accept {
        let filename = file_chooser.get_filename().unwrap();
        g_debug!(config::LOG_DOMAIN, "filename: {:?}", filename);

        let mut args: Vec<&OsStr> = [
            "ffmpeg".as_ref(),
            "-loglevel".as_ref(),
            "error".as_ref(),
            "-ss".as_ref(),
            start.as_ref(),
            "-to".as_ref(),
            end.as_ref(),
            "-i".as_ref(),
            input_path.as_ref(),
            "-c".as_ref(),
            "copy".as_ref(),
            "-y".as_ref(),
        ]
        .to_vec();
        if filename.extension().map(|x| x == "mp4").unwrap_or(false) {
            args.push("-movflags".as_ref());
            args.push("+faststart".as_ref());
        }
        args.push(filename.as_ref());
        g_debug!(config::LOG_DOMAIN, "invoking: {:?}", args);

        match gio::Subprocess::newv(
            &args,
            gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_PIPE,
        ) {
            Ok(subprocess) => {
                let trimming_dialog = gtk::MessageDialogBuilder::new()
                    // Translators: message dialog text.
                    .text(&gettext("Trimming…"))
                    .message_type(gtk::MessageType::Info)
                    .buttons(gtk::ButtonsType::Cancel)
                    .transient_for(&window)
                    .modal(true)
                    .build();

                let trimming_dialog_clone = trimming_dialog.clone();
                let subprocess_clone = subprocess.clone();
                let future = async move {
                    let builder = match subprocess_clone.communicate_utf8_async_future(None).await {
                        Ok((_, stderr)) => {
                            if subprocess_clone.get_if_exited()
                                && subprocess_clone.get_exit_status() == 0
                            {
                                gtk::MessageDialogBuilder::new()
                                    // Translators: message dialog text.
                                    .text(&gettext("Done!"))
                                    .message_type(gtk::MessageType::Info)
                            } else {
                                gtk::MessageDialogBuilder::new()
                                    // Translators: error dialog text.
                                    .text(&gettext("Error trimming video"))
                                    .secondary_text(stderr.as_deref().unwrap_or(""))
                                    .message_type(gtk::MessageType::Error)
                            }
                        }
                        Err(err) => {
                            gtk::MessageDialogBuilder::new()
                                // Translators: error dialog text.
                                .text(&gettext("Could not communicate with the ffmpeg subprocess"))
                                .secondary_text(&format!("{}", err))
                                .message_type(gtk::MessageType::Error)
                        }
                    };

                    // This will invoke the signal handler, but it shouldn't be a big deal since
                    // the process has already exited and the future has already completed by then.
                    trimming_dialog_clone.close();

                    let dialog = builder
                        .buttons(gtk::ButtonsType::Ok)
                        .transient_for(&window)
                        .modal(true)
                        .build();
                    dialog.connect_response(move |dialog, _| dialog.close());

                    // Has to be in an idle to not block the close() above.
                    // https://gitlab.gnome.org/GNOME/gtk/-/issues/2926
                    gtk::idle_add(move || {
                        dialog.show_all();
                        Continue(false)
                    });
                };
                let (future, handle) = abortable(future);
                let future = future.map(|_| ());

                trimming_dialog.connect_response(move |dialog, _| {
                    g_debug!(config::LOG_DOMAIN, "force exiting the subprocess");
                    subprocess.force_exit();
                    handle.abort();
                    dialog.close();
                });
                trimming_dialog.show_all();

                glib::MainContext::default().spawn_local(future);
            }
            Err(err) => {
                let dialog = gtk::MessageDialogBuilder::new()
                    // Translators: error dialog text.
                    .text(&gettext("Could not create the ffmpeg subprocess"))
                    .secondary_text(&format!("{}", err))
                    .message_type(gtk::MessageType::Error)
                    .buttons(gtk::ButtonsType::Ok)
                    .transient_for(&window)
                    .modal(true)
                    .build();
                dialog.connect_response(move |dialog, _| dialog.close());
                dialog.show_all();
            }
        }
    }
}
