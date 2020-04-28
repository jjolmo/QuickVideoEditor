use std::{
    cell::RefCell,
    ffi::OsStr,
    path::{Path, PathBuf},
    rc::Rc,
};

use futures::prelude::*;
use gio::prelude::*;
use glib::{subclass, subclass::prelude::*, translate::*};
use gtk::{prelude::*, subclass::prelude::*};
use once_cell::unsync::OnceCell;

use crate::parse;

#[derive(Debug)]
struct Widgets {
    header_bar: gtk::HeaderBar,
    stack_main: gtk::Stack,
    stack_header_bar: gtk::Stack,
    button_open: gtk::Button,
    button_trim: gtk::Button,
    entry_from: gtk::Entry,
    entry_to: gtk::Entry,
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
        let entry_from: gtk::Entry = builder.get_object("entry_from").unwrap();
        let entry_to: gtk::Entry = builder.get_object("entry_to").unwrap();

        let self_ = obj.downcast_ref::<VtWindow>().unwrap();
        self_.add(&stack_main);
        self_.set_titlebar(Some(&stack_header_bar));
        self_.set_resizable(false);

        // Start and end timestamp validation.
        let on_entry_change = Rc::new(
            clone!(@weak entry_from, @weak entry_to, @weak button_trim => move || {
                if validate_entries(&entry_from, &entry_to).is_some() {
                    button_trim.set_sensitive(true);
                } else {
                    button_trim.set_sensitive(false);
                }
            }),
        );

        entry_from.connect_property_text_notify(clone!(@strong on_entry_change => move |_| {
            on_entry_change()
        }));
        entry_to.connect_property_text_notify(move |_| on_entry_change());

        button_trim.connect_clicked(clone!(@weak self_ => move |_| {
            let priv_ = VtWindowPrivate::from_instance(&self_);
            let widgets = priv_.widgets.get().unwrap();

            let result = validate_entries(&widgets.entry_from, &widgets.entry_to);
            if result.is_none() {
                // This should not happen normally because the button should be disabled.
                warn!("Trim pressed with invalid timestamps");
                return;
            }

            let (from, to) = result.unwrap();

            let extension = priv_.content_type
                .borrow()
                .as_ref()
                .and_then(mime_db::extension)
                .unwrap_or("mp4");

            let input_path = priv_.input_path.borrow();
            if input_path.is_none() {
                // This should not happen normally because if the button is visible then we should
                // have the input path already.
                warn!("Trim pressed without input path");
                return;
            }

            let input_path = input_path.as_deref().unwrap();

            trim(self_.clone(), input_path, extension, from, to);
        }));

        let widgets = Widgets {
            header_bar,
            stack_main,
            stack_header_bar,
            button_open,
            button_trim,
            entry_from,
            entry_to,
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
                            .text("Could not get input video information")
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
    entry_from: &gtk::Entry,
    entry_to: &gtk::Entry,
) -> Option<(glib::GString, glib::GString)> {
    let style_from = entry_from.get_style_context();
    let style_to = entry_to.get_style_context();
    style_from.remove_class("error");
    style_to.remove_class("error");

    let text_from = entry_from.get_text().unwrap();
    let timestamp_from = parse::timestamp(text_from.as_str());
    let text_to = entry_to.get_text().unwrap();
    let timestamp_to = parse::timestamp(text_to.as_str());

    if timestamp_from.is_none() {
        style_from.add_class("error");
    }
    if timestamp_to.is_none() {
        style_to.add_class("error");
    }
    if let (Some(timestamp_from), Some(timestamp_to)) = (timestamp_from, timestamp_to) {
        if timestamp_from >= timestamp_to {
            style_to.add_class("error");
        } else {
            return Some((text_from, text_to));
        }
    }

    None
}

fn trim(
    window: VtWindow,
    input_path: &Path,
    extension: &str,
    from: glib::GString,
    to: glib::GString,
) {
    debug!("trim: from {} to {}", from, to);

    let file_chooser = gtk::FileChooserNativeBuilder::new()
        .transient_for(&window)
        .action(gtk::FileChooserAction::Save)
        .do_overwrite_confirmation(true)
        .build();
    file_chooser.set_current_name(format!("Trimmed video.{}", extension));

    let response = file_chooser.run();
    if response == gtk::ResponseType::Accept {
        let filename = file_chooser.get_filename().unwrap();
        debug!("filename: {:?}", filename);

        let mut args: Vec<&OsStr> = [
            "ffmpeg".as_ref(),
            "-loglevel".as_ref(),
            "error".as_ref(),
            "-ss".as_ref(),
            from.as_ref(),
            "-to".as_ref(),
            to.as_ref(),
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
                    .text("Trimming…")
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
                                    .text("Done!")
                                    .message_type(gtk::MessageType::Info)
                                    .buttons(gtk::ButtonsType::Ok)
                                    .transient_for(&window)
                                    .build();
                                dialog.run();
                                dialog.destroy();
                            } else {
                                let dialog = gtk::MessageDialogBuilder::new()
                                    .text("Error trimming video")
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
                                .text("Could not communicate with the ffmpeg subprocess")
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
                    .text("Could not create the ffmpeg subprocess")
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
