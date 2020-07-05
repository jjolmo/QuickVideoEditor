use std::{
    cell::{Cell, RefCell},
    ffi::OsStr,
    path::{Path, PathBuf},
};

use futures_util::future::{abortable, FutureExt};
use gettextrs::*;
use gio::prelude::*;
use glib::{subclass, subclass::prelude::*, translate::*};
use gst::prelude::*;
use gtk::{prelude::*, subclass::prelude::*};
use once_cell::unsync::OnceCell;

use crate::parse;

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
    seek_slider: gtk::Scale,
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
    seek_slider_value_changed: OnceCell<glib::SignalHandlerId>,
    pipeline_playing: Cell<bool>,
    start_end: Cell<(u32, u32)>,
}

impl VtWindowPrivate {
    fn refresh_ui(&self) {
        let pipeline = self.pipeline.get().unwrap();
        let widgets = self.widgets.get().unwrap();
        let id = self.seek_slider_value_changed.get().unwrap();

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
                let value =
                    position.nanoseconds().unwrap() as f64 / duration.nanoseconds().unwrap() as f64;

                widgets.seek_slider.block_signal(&id);
                widgets.seek_slider.set_value(value);
                widgets.seek_slider.unblock_signal(&id);

                let width = widgets.box_timeline_bg.get_allocated_width();
                let margin_start = (value * width as f64).round() as i32;
                widgets.box_timeline_position.set_margin_start(margin_start);
            }
        }
    }

    fn refresh_timeline(&self) {
        let pipeline = self.pipeline.get().unwrap();
        let widgets = self.widgets.get().unwrap();

        let (start, end) = if let Some(duration) = pipeline.query_duration::<gst::ClockTime>() {
            let duration = duration.mseconds().unwrap() as f64;
            let (start, end) = self.start_end.get();
            let start = start as f64 / duration;
            let end = end as f64 / duration;
            (start, end)
        } else {
            (0., 0.)
        };

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

        if let Some(start_end) = validate_entries(&widgets.entry_start, &widgets.entry_end) {
            widgets.button_trim.set_sensitive(true);

            self.start_end.set(start_end);
            self.refresh_timeline();
        } else {
            widgets.button_trim.set_sensitive(false);
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
            seek_slider_value_changed: OnceCell::new(),
            pipeline_playing: Cell::new(false),
            start_end: Cell::new((0, 0)),
        }
    }
}

impl ObjectImpl for VtWindowPrivate {
    glib_object_impl!();

    fn constructed(&self, obj: &glib::Object) {
        self.parent_constructed(obj);
        let self_ = obj.downcast_ref::<VtWindow>().unwrap();

        let builder =
            gtk::Builder::new_from_resource("/org/gnome/gitlab/YaLTeR/VideoTrimmer/window.ui");

        let stack_main: gtk::Stack = builder.get_object("stack_main").unwrap();
        let stack_header_bar: gtk::Stack = builder.get_object("stack_header_bar").unwrap();
        let header_bar: gtk::HeaderBar = builder.get_object("header_bar").unwrap();
        let button_open: gtk::Button = builder.get_object("button_open").unwrap();
        let button_trim: gtk::Button = builder.get_object("button_trim").unwrap();
        let entry_start: gtk::Entry = builder.get_object("entry_start").unwrap();
        let entry_end: gtk::Entry = builder.get_object("entry_end").unwrap();
        let seek_slider: gtk::Scale = builder.get_object("seek_slider").unwrap();
        let box_main: gtk::Box = builder.get_object("box_main").unwrap();
        let label_current_time: gtk::Label = builder.get_object("label_current_time").unwrap();
        let button_play_pause: gtk::Button = builder.get_object("button_play_pause").unwrap();
        let button_play_pause_image: gtk::Image =
            builder.get_object("button_play_pause_image").unwrap();
        let overlay_timeline: gtk::Overlay = builder.get_object("overlay_timeline").unwrap();
        let box_timeline_bg: gtk::Box = builder.get_object("box_timeline_bg").unwrap();
        let box_timeline_selection: gtk::Box =
            builder.get_object("box_timeline_selection").unwrap();
        let box_timeline_position: gtk::Box = builder.get_object("box_timeline_position").unwrap();

        overlay_timeline.add_overlay(&box_timeline_selection);
        overlay_timeline.add_overlay(&box_timeline_position);

        let adjustment = gtk::Adjustment::new(0., 0., 1., 0., 0., 0.);
        seek_slider.set_adjustment(&adjustment);

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

        // Connect the seek slider.
        self.seek_slider_value_changed
            .set(seek_slider.connect_value_changed({
                let pipeline = pipeline.downgrade();
                move |seek_slider| {
                    if let Some(pipeline) = pipeline.upgrade() {
                        let value = seek_slider.get_value();
                        if let Some(duration) = pipeline.query_duration::<gst::ClockTime>() {
                            let time = duration.nanoseconds().unwrap() as f64 * value;
                            let time = gst::ClockTime::from_nseconds(time as u64);
                            pipeline.seek_simple(gst::SeekFlags::FLUSH, time).unwrap();
                        }
                    }
                }
            }))
            .unwrap();

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
                    MessageView::Error(err) => {
                        warn!(
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
                warn!("Trim pressed with invalid timestamps");
                return;
            }

            let start = widgets.entry_start.get_text().unwrap();
            let end = widgets.entry_end.get_text().unwrap();

            let extension = priv_.content_type
                .borrow()
                .as_ref()
                .map(glib::GString::as_str)
                .and_then(mime_guess::get_mime_extensions_str)
                .and_then(|exts| exts.get(0))
                .unwrap_or(&"mp4");

            let input_path = priv_.input_path.borrow();
            if input_path.is_none() {
                // This should not happen normally because if the button is visible then we should
                // have the input path already.
                warn!("Trim pressed without input path");
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
            seek_slider,
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
        let window = glib::Object::new(Self::static_type(), &[("application", app)])
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
        priv_
            .pipeline
            .get()
            .unwrap()
            .call_async(|pipeline| drop(pipeline.set_state(gst::State::Playing).unwrap()));

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
                            debug!("fast-content-type: {}", fast_content_type);
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

    let text_start = entry_start.get_text().unwrap();
    let timestamp_start = parse::timestamp(text_start.as_str());
    let text_end = entry_end.get_text().unwrap();
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
    debug!("trim: from {} to {}", start, end);

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
        debug!("filename: {:?}", filename);

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
        debug!("invoking: {:?}", args);

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
                    match subprocess_clone.communicate_utf8_async_future(None).await {
                        Ok((_, stderr)) => {
                            trimming_dialog_clone.destroy();

                            if subprocess_clone.get_if_exited()
                                && subprocess_clone.get_exit_status() == 0
                            {
                                let dialog = gtk::MessageDialogBuilder::new()
                                    // Translators: message dialog text.
                                    .text(&gettext("Done!"))
                                    .message_type(gtk::MessageType::Info)
                                    .buttons(gtk::ButtonsType::Ok)
                                    .transient_for(&window)
                                    .build();
                                dialog.run();
                                dialog.destroy();
                            } else {
                                let dialog = gtk::MessageDialogBuilder::new()
                                    // Translators: error dialog text.
                                    .text(&gettext("Error trimming video"))
                                    .secondary_text(stderr.as_str())
                                    .message_type(gtk::MessageType::Error)
                                    .buttons(gtk::ButtonsType::Ok)
                                    .transient_for(&window)
                                    .build();
                                dialog.run();
                                dialog.destroy();
                            }
                        }
                        Err(err) => {
                            trimming_dialog_clone.destroy();

                            let dialog = gtk::MessageDialogBuilder::new()
                                // Translators: error dialog text.
                                .text(&gettext("Could not communicate with the ffmpeg subprocess"))
                                .secondary_text(&format!("{}", err))
                                .message_type(gtk::MessageType::Error)
                                .buttons(gtk::ButtonsType::Ok)
                                .transient_for(&window)
                                .build();
                            dialog.run();
                            dialog.destroy();
                        }
                    }
                };
                let (future, handle) = abortable(future);
                let future = future.map(|_| ());

                trimming_dialog.connect_response(move |dialog, _| {
                    debug!("force exiting the subprocess");
                    subprocess.force_exit();
                    handle.abort();
                    dialog.destroy();
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
                    .build();
                dialog.run();
                dialog.destroy();
                return;
            }
        }
    }
}
