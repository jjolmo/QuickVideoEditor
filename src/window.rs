use std::{
    cell::RefCell,
    ffi::OsStr,
    path::{Path, PathBuf},
    rc::Rc,
};

use futures::prelude::*;
use gettextrs::*;
use gio::prelude::*;
use glib::{subclass, subclass::prelude::*, translate::*};
use gtk::{prelude::*, subclass::prelude::*};
use once_cell::unsync::OnceCell;

use crate::parse;

// Extracted from Totem.
const VIDEO_MIME_TYPES: &[&str] = &[
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
    "video/x-ms-wmx",
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
}

#[derive(Debug)]
pub struct VtWindowPrivate {
    widgets: OnceCell<Widgets>,
    content_type: RefCell<Option<glib::GString>>,
    input_path: RefCell<Option<PathBuf>>,
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
        }
    }
}

impl ObjectImpl for VtWindowPrivate {
    glib_object_impl!();

    fn constructed(&self, obj: &glib::Object) {
        self.parent_constructed(obj);

        let builder =
            gtk::Builder::new_from_resource("/org/gnome/gitlab/YaLTeR/VideoTrimmer/window.ui");

        let stack_main: gtk::Stack = builder.get_object("stack_main").unwrap();
        let stack_header_bar: gtk::Stack = builder.get_object("stack_header_bar").unwrap();
        let header_bar: gtk::HeaderBar = builder.get_object("header_bar").unwrap();
        let button_open: gtk::Button = builder.get_object("button_open").unwrap();
        let button_trim: gtk::Button = builder.get_object("button_trim").unwrap();
        let entry_start: gtk::Entry = builder.get_object("entry_start").unwrap();
        let entry_end: gtk::Entry = builder.get_object("entry_end").unwrap();

        let self_ = obj.downcast_ref::<VtWindow>().unwrap();
        self_.add(&stack_main);
        self_.set_titlebar(Some(&stack_header_bar));
        self_.set_resizable(false);

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

        // Start and end timestamp validation.
        let on_entry_change = Rc::new(
            clone!(@weak entry_start, @weak entry_end, @weak button_trim => move || {
                if validate_entries(&entry_start, &entry_end).is_some() {
                    button_trim.set_sensitive(true);
                } else {
                    button_trim.set_sensitive(false);
                }
            }),
        );

        entry_start.connect_property_text_notify(clone!(@strong on_entry_change => move |_| {
            on_entry_change()
        }));
        entry_end.connect_property_text_notify(move |_| on_entry_change());

        // The trim button.
        button_trim.connect_clicked(clone!(@weak self_ => move |_| {
            let priv_ = VtWindowPrivate::from_instance(&self_);
            let widgets = priv_.widgets.get().unwrap();

            let result = validate_entries(&widgets.entry_start, &widgets.entry_end);
            if result.is_none() {
                // This should not happen normally because the button should be disabled.
                warn!("Trim pressed with invalid timestamps");
                return;
            }

            let (start, end) = result.unwrap();

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

        app.add_window(&window);

        window
    }

    pub fn open(&self, file: gio::File) {
        let priv_ = VtWindowPrivate::from_instance(self);
        let widgets = priv_.widgets.get().unwrap();

        widgets.stack_main.set_visible_child_name("page_main");
        widgets.stack_header_bar.set_visible_child_name("page_main");

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

fn validate_entries(
    entry_start: &gtk::Entry,
    entry_end: &gtk::Entry,
) -> Option<(glib::GString, glib::GString)> {
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
            return Some((text_start, text_end));
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
                let (future, handle) = futures::future::abortable(future);
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
