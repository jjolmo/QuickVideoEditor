use std::{
    cell::RefCell,
    ffi::OsStr,
    mem,
    path::{Path, PathBuf},
};

use futures_util::future::{abortable, FutureExt};
use gdk::prelude::*;
use gettextrs::*;
use gio::prelude::*;
use glib::{subclass, subclass::prelude::*, translate::*};
use gtk::{prelude::*, subclass::prelude::*};
use once_cell::unsync::OnceCell;

use crate::{config, parse, video_preview::VtVideoPreview};

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
    stack_video_preview: gtk::Stack,
    revealer_done_notification: gtk::Revealer,
    label_done_notification: gtk::Label,
}

#[derive(Debug)]
enum NotificationState {
    Closed,
    Opening(glib::SourceId, Option<String>),
    Open(glib::SourceId),
    Closing(Option<String>),
}

#[derive(Debug)]
pub struct VtWindowPrivate {
    widgets: OnceCell<Widgets>,
    content_type: RefCell<Option<glib::GString>>,
    input_path: RefCell<Option<PathBuf>>,
    video_preview: OnceCell<VtVideoPreview>,
    done_notification_state: RefCell<NotificationState>,
}

pub fn time_to_entry_text(time: gst::ClockTime) -> String {
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
    fn on_entry_changed(&self) {
        let widgets = self.widgets.get().unwrap();
        let video_preview = self.video_preview.get().unwrap();

        let start_end = validate_entries(&widgets.entry_start, &widgets.entry_end);
        video_preview.set_start_end(start_end);
        video_preview.refresh_timeline();

        widgets.button_trim.set_sensitive(start_end.is_some());
    }

    fn on_got_duration(&self, duration: gst::ClockTime) {
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

    fn on_set_start_end(&self, start: gst::ClockTime, end: gst::ClockTime) {
        let widgets = self.widgets.get().unwrap();

        let text = time_to_entry_text(start);
        if parse::timestamp(&widgets.entry_start.get_text())
            .map(|x| x != parse::timestamp(&text).unwrap())
            .unwrap_or(true)
        {
            widgets.entry_start.set_text(&text);
        }

        let text = time_to_entry_text(end);
        if parse::timestamp(&widgets.entry_end.get_text())
            .map(|x| x != parse::timestamp(&text).unwrap())
            .unwrap_or(true)
        {
            widgets.entry_end.set_text(&text);
        }
    }

    fn on_video_preview_error(&self) {
        self.video_preview.get().unwrap().destroy();
        self.widgets
            .get()
            .unwrap()
            .stack_video_preview
            .set_visible_child_name("page_error");
    }

    fn show_done_notification(&self, file_name: String) {
        let widgets = self.widgets.get().unwrap();

        let mut state = self.done_notification_state.borrow_mut();
        match *state {
            NotificationState::Closed => {
                let source = glib::timeout_add_local(5000, {
                    let self_ = self.get_instance().downgrade();
                    move || {
                        let self_ = self_.upgrade().unwrap();
                        let priv_ = VtWindowPrivate::from_instance(&self_);
                        priv_.close_done_notification(None);

                        glib::Continue(false)
                    }
                });

                *state = NotificationState::Opening(source, None);
                drop(state);

                widgets.label_done_notification.set_text(&format!(
                    "{} {}",
                    file_name,
                    // Translators: text on the in-app notification after trimming was done.
                    // The template is: <video filename> has been saved
                    gettext("has been saved")
                ));
                widgets.revealer_done_notification.set_reveal_child(true);
            }
            NotificationState::Opening(_, ref mut new_file_name)
            | NotificationState::Closing(ref mut new_file_name) => {
                *new_file_name = Some(file_name);
            }
            NotificationState::Open(_) => {
                drop(state);
                self.close_done_notification(Some(file_name));
            }
        }
    }

    fn close_done_notification(&self, new_file_name: Option<String>) {
        let mut state = self.done_notification_state.borrow_mut();

        if !matches!(*state, NotificationState::Open(_) | NotificationState::Opening(_, _)) {
            return;
        }

        let file_name = if let NotificationState::Opening(_, file_name) = &mut *state {
            file_name.take()
        } else {
            None
        };

        let new_file_name = new_file_name.or(file_name);
        if let NotificationState::Open(source) | NotificationState::Opening(source, _) =
            mem::replace(&mut *state, NotificationState::Closing(new_file_name))
        {
            glib::source_remove(source);
        }
        drop(state);

        self.widgets
            .get()
            .unwrap()
            .revealer_done_notification
            .set_reveal_child(false);
    }

    fn on_child_revealed_changed(&self) {
        let widgets = self.widgets.get().unwrap();
        let mut state = self.done_notification_state.borrow_mut();

        if widgets.revealer_done_notification.get_child_revealed() {
            match *state {
                NotificationState::Opening(_, None) => {
                    let source = if let NotificationState::Opening(source, _) =
                        mem::replace(&mut *state, NotificationState::Closed)
                    {
                        source
                    } else {
                        unreachable!()
                    };
                    *state = NotificationState::Open(source);
                }
                NotificationState::Opening(_, ref mut new_file_name @ Some(_)) => {
                    let new_file_name = new_file_name.take();
                    drop(state);
                    self.close_done_notification(new_file_name);
                }
                ref other => {
                    g_warning!(
                        config::LOG_DOMAIN,
                        "Unexpected notification state: {:?}",
                        other
                    );

                    let source = glib::timeout_add_local(5000, {
                        let self_ = self.get_instance().downgrade();
                        move || {
                            let self_ = self_.upgrade().unwrap();
                            let priv_ = VtWindowPrivate::from_instance(&self_);
                            priv_.close_done_notification(None);

                            glib::Continue(false)
                        }
                    });

                    *state = NotificationState::Open(source);
                }
            }
        } else {
            match *state {
                NotificationState::Closing(None) => {
                    *state = NotificationState::Closed;
                }
                NotificationState::Closing(ref mut new_file_name @ Some(_)) => {
                    let new_file_name = new_file_name.take().unwrap();
                    let source = glib::timeout_add_local(5000, {
                        let self_ = self.get_instance().downgrade();
                        move || {
                            let self_ = self_.upgrade().unwrap();
                            let priv_ = VtWindowPrivate::from_instance(&self_);
                            priv_.close_done_notification(None);

                            glib::Continue(false)
                        }
                    });
                    *state = NotificationState::Opening(source, None);
                    drop(state);

                    widgets.label_done_notification.set_text(&format!(
                        "{} {}",
                        new_file_name,
                        // Translators: text on the in-app notification after trimming was done.
                        // The template is: <video filename> has been saved
                        gettext("has been saved")
                    ));
                    widgets.revealer_done_notification.set_reveal_child(true);
                }
                ref other => {
                    g_warning!(
                        config::LOG_DOMAIN,
                        "Unexpected notification state: {:?}",
                        other
                    );

                    *state = NotificationState::Closed;
                }
            }
        }
    }

    fn trim(&self, input_path: &Path, extension: &str, start: glib::GString, end: glib::GString) {
        g_debug!(config::LOG_DOMAIN, "trim: from {} to {}", start, end);

        let self_ = self.get_instance();

        let file_chooser = gtk::FileChooserNativeBuilder::new()
            .transient_for(&self_)
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
                        .transient_for(&self_)
                        .modal(true)
                        .build();

                    let trimming_dialog_clone = trimming_dialog.clone();
                    let subprocess_clone = subprocess.clone();
                    let future = async move {
                        let builder =
                            match subprocess_clone.communicate_utf8_async_future(None).await {
                                Ok((_, stderr)) => {
                                    if subprocess_clone.get_if_exited()
                                        && subprocess_clone.get_exit_status() == 0
                                    {
                                        let file_name = filename
                                            .file_name()
                                            .map(|file_name| file_name.to_string_lossy())
                                            .unwrap_or_else(|| filename.to_string_lossy());

                                        let priv_ = VtWindowPrivate::from_instance(&self_);
                                        priv_.show_done_notification(file_name.into());
                                        trimming_dialog_clone.close();
                                        return;
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
                                        .text(&gettext(
                                            "Could not communicate with the ffmpeg subprocess",
                                        ))
                                        .secondary_text(&format!("{}", err))
                                        .message_type(gtk::MessageType::Error)
                                }
                            };

                        // This will invoke the signal handler, but it shouldn't be a big deal
                        // since the process has already exited and the future has already
                        // completed by then.
                        trimming_dialog_clone.close();

                        let dialog = builder
                            .buttons(gtk::ButtonsType::Ok)
                            .transient_for(&self_)
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
                        .transient_for(&self_)
                        .modal(true)
                        .build();
                    dialog.connect_response(move |dialog, _| dialog.close());
                    dialog.show_all();
                }
            }
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
            video_preview: OnceCell::new(),
            done_notification_state: RefCell::new(NotificationState::Closed),
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

        let video_preview = VtVideoPreview::new(&builder);

        video_preview
            .connect_local("notify::duration", false, {
                let self_ = self_.downgrade();
                let video_preview = video_preview.downgrade();
                move |_| {
                    let value = video_preview
                        .upgrade()
                        .unwrap()
                        .get_property("duration")
                        .unwrap()
                        .get()
                        .unwrap()
                        .unwrap();
                    let duration = gst::ClockTime::from_glib(value);
                    if duration.is_none() {
                        return None;
                    }

                    let self_ = self_.upgrade().unwrap();
                    let priv_ = VtWindowPrivate::from_instance(&self_);
                    priv_.on_got_duration(duration);

                    None
                }
            })
            .unwrap();

        video_preview
            .connect_local("set-start-end", false, {
                let self_ = self_.downgrade();
                move |args| {
                    let mut args = args.into_iter().skip(1).map(|x| {
                        gst::ClockTime::from_mseconds(x.get::<u32>().unwrap().unwrap().into())
                    });
                    let start = args.next().unwrap();
                    let end = args.next().unwrap();

                    let self_ = self_.upgrade().unwrap();
                    let priv_ = VtWindowPrivate::from_instance(&self_);
                    priv_.on_set_start_end(start, end);

                    None
                }
            })
            .unwrap();

        video_preview
            .connect_local("error", false, {
                let self_ = self_.downgrade();
                move |_| {
                    let self_ = self_.upgrade().unwrap();
                    let priv_ = VtWindowPrivate::from_instance(&self_);
                    priv_.on_video_preview_error();

                    None
                }
            })
            .unwrap();

        self.video_preview.set(video_preview).unwrap();

        if config::PROFILE == "Devel" {
            self_.get_style_context().add_class("devel");
        }

        let stack_main: gtk::Stack = builder.get_object("stack_main").unwrap();
        let stack_header_bar: gtk::Stack = builder.get_object("stack_header_bar").unwrap();
        let header_bar: gtk::HeaderBar = builder.get_object("header_bar").unwrap();
        let button_open: gtk::Button = builder.get_object("button_open").unwrap();
        let button_trim: gtk::Button = builder.get_object("button_trim").unwrap();
        let entry_start: gtk::Entry = builder.get_object("entry_start").unwrap();
        let entry_end: gtk::Entry = builder.get_object("entry_end").unwrap();
        let stack_video_preview: gtk::Stack = builder.get_object("stack_video_preview").unwrap();
        let revealer_done_notification: gtk::Revealer =
            builder.get_object("revealer_done_notification").unwrap();
        let label_done_notification: gtk::Label =
            builder.get_object("label_done_notification").unwrap();
        let button_close_done_notification: gtk::Button = builder
            .get_object("button_close_done_notification")
            .unwrap();
        let overlay_main: gtk::Overlay = builder.get_object("overlay_main").unwrap();
        let box_empty_state: gtk::Box = builder.get_object("box_empty_state").unwrap();

        self_.add(&overlay_main);
        self_.set_titlebar(Some(&stack_header_bar));

        // The open button.
        button_open.connect_clicked(clone!(@weak self_ => move |_| {
            let filter = gtk::FileFilter::new();
            // Translators: file chooser file filter name.
            filter.set_name(Some(&gettext("Video files")));
            for mime_type in VIDEO_MIME_TYPES {
                filter.add_mime_type(mime_type);
            }

            let file_chooser = gtk::FileChooserNativeBuilder::new()
                .transient_for(&self_)
                .action(gtk::FileChooserAction::Open)
                // Translators: file chooser dialog title.
                .title(&gettext("Open video"))
                .build();

            file_chooser.add_filter(&filter);

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

            priv_.trim(input_path, extension, start, end);
        }));

        revealer_done_notification.connect_property_child_revealed_notify({
            let self_ = self_.downgrade();
            move |_| {
                let self_ = self_.upgrade().unwrap();
                let priv_ = VtWindowPrivate::from_instance(&self_);
                priv_.on_child_revealed_changed();
            }
        });

        button_close_done_notification.connect_clicked({
            let self_ = self_.downgrade();
            move |_| {
                let self_ = self_.upgrade().unwrap();
                let priv_ = VtWindowPrivate::from_instance(&self_);
                priv_.close_done_notification(None);
            }
        });

        // Clean up upon window closing.
        self_.connect_destroy(move |self_| {
            let self_ = self_.clone().downcast::<VtWindow>().unwrap();
            let priv_ = VtWindowPrivate::from_instance(&self_);
            priv_.video_preview.get().unwrap().destroy();
        });

        box_empty_state.drag_dest_set(gtk::DestDefaults::ALL, &[], gdk::DragAction::COPY);
        box_empty_state.drag_dest_add_uri_targets();
        box_empty_state.connect_drag_data_received({
            let self_ = self_.downgrade();
            move |_, context, _, _, data, _, time| {
                let self_ = self_.upgrade().unwrap();

                let uris = data.get_uris();
                for uri in uris.get(0) {
                    self_.open(gio::File::new_for_uri(&uri));
                }

                context.drag_finish(true, false, time);
            }
        });

        let widgets = Widgets {
            header_bar,
            stack_main,
            stack_header_bar,
            button_open,
            button_trim,
            entry_start,
            entry_end,
            stack_video_preview,
            revealer_done_notification,
            label_done_notification,
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

        priv_.video_preview.get().unwrap().open(&file.get_uri());

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
