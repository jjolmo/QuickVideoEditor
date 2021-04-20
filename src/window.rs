use glib::subclass::prelude::*;
use gtk::{gio, glib};

mod imp {
    use std::{
        cell::RefCell,
        ffi::OsStr,
        path::{Path, PathBuf},
        time::Duration,
    };

    use futures_util::future::{abortable, FutureExt};
    use gettextrs::*;
    use glib::{clone, debug, warn};
    use gtk::{gdk, gio, glib, prelude::*, subclass::prelude::*, CompositeTemplate};

    use crate::{
        config::{self, G_LOG_DOMAIN},
        notification::VtNotification,
        parse::{self, time_to_entry_text},
        video_preview::VtVideoPreview,
    };

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

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(file = "window.ui")]
    pub struct VtWindow {
        #[template_child]
        video_preview: TemplateChild<VtVideoPreview>,
        #[template_child]
        button_trim: TemplateChild<gtk::Button>,
        #[template_child]
        entry_start: TemplateChild<gtk::Entry>,
        #[template_child]
        entry_end: TemplateChild<gtk::Entry>,
        #[template_child]
        stack_video_preview: TemplateChild<gtk::Stack>,
        #[template_child]
        button_open: TemplateChild<gtk::Button>,
        #[template_child]
        box_empty_state: TemplateChild<gtk::Box>,
        #[template_child]
        stack_main: TemplateChild<gtk::Stack>,
        #[template_child]
        stack_header_bar: TemplateChild<gtk::Stack>,
        #[template_child]
        label_subtitle: TemplateChild<gtk::Label>,
        #[template_child]
        done_notification: TemplateChild<VtNotification>,

        content_type: RefCell<Option<glib::GString>>,
        input_path: RefCell<Option<PathBuf>>,
        output_file: RefCell<Option<gio::File>>,
    }

    impl VtWindow {
        fn on_entry_changed(&self) {
            let start_end = validate_entries(&self.entry_start, &self.entry_end);
            self.video_preview.set_start_end(start_end);
            self.button_trim.set_sensitive(start_end.is_some());
        }

        fn on_got_duration(&self, duration: i64) {
            // If the user hasn't started typing in the timestamp entries, fill them with default
            // values.
            if !self.entry_start.get_text().is_empty() || !self.entry_end.get_text().is_empty() {
                return;
            }

            let duration = duration as f64;
            let start = duration / 3.;
            let end = start * 2.;

            let start = start as u64;
            let end = (end as u64).max(start + 1);

            let start = Duration::from_micros(start);
            let end = Duration::from_micros(end);

            self.entry_start.set_text(&time_to_entry_text(start));
            self.entry_end.set_text(&time_to_entry_text(end));

            // Select the text so the behavior of typing doesn't change compared to if we hadn't set
            // the text.
            if self.entry_start.get_focus_child().is_some() {
                self.entry_start.select_region(0, -1);
            } else if self.entry_end.get_focus_child().is_some() {
                self.entry_end.select_region(0, -1);
            }
        }

        fn on_set_start_end(&self, start: Duration, end: Duration) {
            let text = time_to_entry_text(start);
            if parse::timestamp(&self.entry_start.get_text())
                .map(|x| x != parse::timestamp(&text).unwrap())
                .unwrap_or(true)
            {
                self.entry_start.set_text(&text);
            }

            let text = time_to_entry_text(end);
            if parse::timestamp(&self.entry_end.get_text())
                .map(|x| x != parse::timestamp(&text).unwrap())
                .unwrap_or(true)
            {
                self.entry_end.set_text(&text);
            }
        }

        fn on_video_preview_error(&self) {
            self.video_preview.destroy();
            self.stack_video_preview
                .set_visible_child_name("page_error");
        }

        fn trim(
            &self,
            input_path: PathBuf,
            extension: String,
            start: glib::GString,
            end: glib::GString,
        ) {
            debug!("trim: from {} to {}", start, end);

            let self_ = self.get_instance();

            let future = async move {
                let priv_ = VtWindow::from_instance(&self_);

                let current_name = priv_
                    .output_file
                    .borrow()
                    .as_ref()
                    .and_then(|file| file.get_path())
                    .and_then(|path| path.into_os_string().into_string().ok())
                    .unwrap_or_else(|| {
                        format!(
                            "{}{}.{}",
                            input_path.file_stem().and_then(OsStr::to_str).unwrap_or(""),
                            // Translators: this is appended to the output video file name.
                            // So for example "my video.mp4" will become "my video (trimmed).mp4".
                            gettext(" (trimmed)"),
                            extension
                        )
                    });

                let file_chooser = gtk::FileChooserNativeBuilder::new()
                    .transient_for(&self_)
                    .action(gtk::FileChooserAction::Save)
                    .modal(true)
                    .build();
                file_chooser.set_current_name(&current_name);

                let (tx, rx) = futures_channel::oneshot::channel();

                let tx = RefCell::new(Some(tx));
                file_chooser.connect_response({
                    let self_ = self_.downgrade();
                    move |file_chooser, response| {
                        if let Some(tx) = tx.borrow_mut().take() {
                            if response == gtk::ResponseType::Accept {
                                if let Some(path) = file_chooser.get_file().unwrap().get_path() {
                                    tx.send(Some(path)).unwrap();
                                } else {
                                    let dialog = gtk::MessageDialogBuilder::new()
                                        // Translators: error dialog title.
                                        .text(&gettext("Error"))
                                        .secondary_text(&gettext(
                                            // Translators: error dialog text.
                                            "Video Trimmer can only operate on local files. Please choose another file.",
                                        ))
                                        .message_type(gtk::MessageType::Error)
                                        .buttons(gtk::ButtonsType::Ok)
                                        .transient_for(&self_.upgrade().unwrap())
                                        .modal(true)
                                        .build();
                                    dialog.connect_response(|dialog, _| {
                                        dialog.close();
                                    });
                                    dialog.show();

                                    tx.send(None).unwrap();
                                }
                            } else {
                                tx.send(None).unwrap();
                            }
                        }
                    }
                });

                file_chooser.show();

                let output_path = if let Some(output_path) = rx.await.unwrap() {
                    output_path
                } else {
                    return;
                };

                priv_.do_trim(&input_path, output_path, start, end);
            };

            glib::MainContext::default().spawn_local(future);
        }

        fn do_trim(
            &self,
            input_path: &Path,
            output_path: PathBuf,
            start: glib::GString,
            end: glib::GString,
        ) {
            let self_ = self.get_instance();

            debug!("output path: {:?}", output_path);

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
                // By default FFmpeg selects only a single ("best") stream of each type. We'd rather
                // include all of them, however. This also fixes our trimmed down FFmpeg not including
                // the subtitle track by default.
                "-map".as_ref(),
                "0".as_ref(),
                // GoPro recordings include data streams with "none" tag which FFmpeg fails to process.
                // It fails to even simply copy them over, so I'm assuming this is an FFmpeg bug and
                // disabling data stream copying altogether as a workaround.
                "-dn".as_ref(),
                "-c".as_ref(),
                "copy".as_ref(),
                "-y".as_ref(),
            ]
            .to_vec();
            if output_path.extension().map(|x| x == "mp4").unwrap_or(false) {
                args.push("-movflags".as_ref());
                args.push("+faststart".as_ref());
            }
            args.push(output_path.as_ref());
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
                                        let file_name = output_path
                                            .file_name()
                                            .map(|file_name| file_name.to_string_lossy())
                                            .unwrap_or_else(|| output_path.to_string_lossy());

                                        let priv_ = VtWindow::from_instance(&self_);
                                        priv_.done_notification.show_notification(file_name.into());
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
                        glib::idle_add_local_once(move || {
                            dialog.show();
                        });
                    };
                    let (future, handle) = abortable(future);
                    let future = future.map(|_| ());

                    trimming_dialog.connect_response(move |dialog, _| {
                        debug!("force exiting the subprocess");
                        subprocess.force_exit();
                        handle.abort();
                        dialog.close();
                    });
                    trimming_dialog.show();

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
                    dialog.show();
                }
            }
        }

        fn switch_to_main_page(&self) {
            if self
                .stack_main
                .get_visible_child_name()
                .as_ref()
                .map(|x| x.as_str())
                == Some("page_main")
            {
                return;
            }

            let self_ = self.get_instance();

            self.stack_main.set_visible_child_name("page_main");
            self.stack_header_bar.set_visible_child_name("page_main");
            self_.set_default_widget(Some(&*self.button_trim));

            // Focus the entry when coming from the empty state.
            self.entry_start.grab_focus();

            self_.show();
        }

        pub fn open(&self, file: gio::File) {
            let self_ = self.get_instance();

            self.video_preview.open(&file);

            // Unconditionally switch to main page after 300 ms
            // (if the video takes too long to load).
            glib::timeout_add_local_once(Duration::from_millis(300), {
                let self_ = self_.downgrade();
                move || {
                    let self_ = self_.upgrade().unwrap();
                    let priv_ = VtWindow::from_instance(&self_);
                    priv_.switch_to_main_page();
                }
            });

            // Verified in callers.
            *self.input_path.borrow_mut() = Some(file.get_path().unwrap());

            // Get the display name and content type.
            let future = async move {
                let priv_ = VtWindow::from_instance(&self_);

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
                        priv_.label_subtitle.set_text(display_name.as_str());
                        priv_.label_subtitle.set_visible(true);

                        if let Some(fast_content_type) =
                            info.get_attribute_string("standard::fast-content-type")
                        {
                            debug!("fast-content-type: {}", fast_content_type);
                            *priv_.content_type.borrow_mut() = Some(fast_content_type);
                        }
                    }
                    // Fails when the file does not exist.
                    Err(err) => {
                        self_.show();

                        let dialog = gtk::MessageDialogBuilder::new()
                            // Translators: error dialog text when the input file information could
                            // not be retrieved (e.g. there's no such file on disk).
                            .text(&gettext("Could not get input video information"))
                            .secondary_text(&format!("{}", err))
                            .message_type(gtk::MessageType::Error)
                            .buttons(gtk::ButtonsType::Ok)
                            .transient_for(&self_)
                            .modal(true)
                            .build();
                        dialog.connect_response(move |_, _| {
                            self_.get_application().unwrap().quit();
                        });
                        dialog.show();
                    }
                }
            };
            glib::MainContext::default().spawn_local(future);
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for VtWindow {
        const NAME: &'static str = "VtWindow";
        type Type = super::VtWindow;
        type ParentType = gtk::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for VtWindow {
        fn properties() -> &'static [glib::ParamSpec] {
            use once_cell::sync::Lazy;
            static PROPERTIES: Lazy<[glib::ParamSpec; 1]> = Lazy::new(|| {
                [glib::ParamSpec::object(
                    "output-file",
                    "output-file",
                    "output-file",
                    gio::File::static_type(),
                    glib::ParamFlags::WRITABLE | glib::ParamFlags::CONSTRUCT_ONLY,
                )]
            });

            PROPERTIES.as_ref()
        }

        fn set_property(
            &self,
            _obj: &Self::Type,
            _id: usize,
            value: &glib::Value,
            pspec: &glib::ParamSpec,
        ) {
            match pspec.get_name() {
                "output-file" => {
                    *self.output_file.borrow_mut() = value.get().unwrap();
                }
                _ => unreachable!(),
            }
        }

        fn constructed(&self, self_: &Self::Type) {
            self.parent_constructed(self_);

            if config::PROFILE == "Devel" {
                self_.get_style_context().add_class("devel");
            }

            self.video_preview
                .connect_local("notify::duration", false, {
                    let self_ = self_.downgrade();
                    move |_| {
                        let self_ = self_.upgrade().unwrap();
                        let priv_ = VtWindow::from_instance(&self_);

                        let duration: i64 = priv_
                            .video_preview
                            .get_property("duration")
                            .unwrap()
                            .get()
                            .unwrap()
                            .unwrap();

                        priv_
                            .stack_video_preview
                            .set_visible_child(&*priv_.video_preview);
                        priv_.switch_to_main_page();

                        if duration == 0 {
                            return None;
                        }

                        priv_.on_got_duration(duration);

                        None
                    }
                })
                .unwrap();

            self.video_preview
                .connect_local("set-start-end", false, {
                    let self_ = self_.downgrade();
                    move |args| {
                        let mut args = args.iter().skip(1).map(|x| {
                            Duration::from_millis(x.get::<u32>().unwrap().unwrap().into())
                        });
                        let start = args.next().unwrap();
                        let end = args.next().unwrap();

                        let self_ = self_.upgrade().unwrap();
                        let priv_ = VtWindow::from_instance(&self_);
                        priv_.on_set_start_end(start, end);

                        None
                    }
                })
                .unwrap();

            self.video_preview
                .connect_local("error", false, {
                    let self_ = self_.downgrade();
                    move |_| {
                        let self_ = self_.upgrade().unwrap();
                        let priv_ = VtWindow::from_instance(&self_);
                        priv_.on_video_preview_error();
                        priv_.switch_to_main_page();

                        None
                    }
                })
                .unwrap();

            // The open button.
            self.button_open.connect_clicked({
                let self_ = self_.downgrade();
                move |_| {
                    let self_ = self_.upgrade().unwrap();
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
                        .transient_for(&self_)
                        .modal(true)
                        .build();

                    file_chooser.add_filter(&filter);

                    file_chooser.connect_response({
                        let file_chooser = RefCell::new(Some(file_chooser.clone()));
                        move |_, response| {
                            let file_chooser = file_chooser.borrow_mut().take().unwrap();

                            if response != gtk::ResponseType::Accept {
                                return;
                            }

                            let file = file_chooser.get_file().unwrap();
                            if file.get_path().is_none() {
                                let dialog = gtk::MessageDialogBuilder::new()
                                    // Translators: error dialog title.
                                    .text(&gettext("Error"))
                                    .secondary_text(&gettext(
                                        // Translators: error dialog text.
                                        "Video Trimmer can only operate on local files. Please choose another file.",
                                    ))
                                    .message_type(gtk::MessageType::Error)
                                    .buttons(gtk::ButtonsType::Ok)
                                    .transient_for(&self_)
                                    .modal(true)
                                    .build();
                                dialog.connect_response(|dialog, _| {
                                    dialog.close();
                                });
                                dialog.show();
                                return;
                            }

                            self_.open(file);
                        }
                    });

                    file_chooser.show();
                }
            });

            // Start and end timestamp validation and visualization.
            self.entry_start.connect_property_text_notify({
                let self_ = self_.downgrade();
                move |_| {
                    let self_ = self_.upgrade().unwrap();
                    let priv_ = VtWindow::from_instance(&self_);
                    priv_.on_entry_changed();
                }
            });
            self.entry_end.connect_property_text_notify({
                let self_ = self_.downgrade();
                move |_| {
                    let self_ = self_.upgrade().unwrap();
                    let priv_ = VtWindow::from_instance(&self_);
                    priv_.on_entry_changed();
                }
            });

            // The trim button.
            self.button_trim
                .connect_clicked(clone!(@weak self_ => move |_| {
                    let priv_ = VtWindow::from_instance(&self_);

                    if validate_entries(&priv_.entry_start, &priv_.entry_end).is_none() {
                        // This should not happen normally because the button should be disabled.
                        warn!("Trim pressed with invalid timestamps");
                        return;
                    }

                    let start = priv_.entry_start.get_text();
                    let end = priv_.entry_end.get_text();

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
                        .unwrap_or(&"mp4")
                        .to_string();

                    let input_path = priv_.input_path.borrow();
                    if input_path.is_none() {
                        // This should not happen normally because if the button is visible then we should
                        // have the input path already.
                        warn!("Trim pressed without input path");
                        return;
                    }

                    let input_path = input_path.clone().unwrap();

                    priv_.trim(input_path, extension, start, end);
                }));

            // Clean up upon window closing.
            self_.connect_destroy(move |self_| {
                let self_ = self_.clone().downcast::<super::VtWindow>().unwrap();
                let priv_ = VtWindow::from_instance(&self_);
                priv_.video_preview.destroy();
            });

            let drop_target = gtk::DropTarget::new(gio::File::static_type(), gdk::DragAction::COPY);
            drop_target.connect_drop({
                let self_ = self_.downgrade();
                move |_, data, _, _| {
                    if let Some(file) = data.downcast_ref::<gio::File>().and_then(|x| x.get()) {
                        let self_ = self_.upgrade().unwrap();
                        self_.open(file);
                        return true;
                    }

                    false
                }
            });
            self.box_empty_state.add_controller(&drop_target);
        }
    }

    impl WidgetImpl for VtWindow {}
    impl WindowImpl for VtWindow {}
    impl ApplicationWindowImpl for VtWindow {}

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
}

glib::wrapper! {
    pub struct VtWindow(ObjectSubclass<imp::VtWindow>)
        @extends gtk::Widget, gtk::Window, gtk::ApplicationWindow,
        @implements gio::ActionMap, gio::ActionGroup;
}

impl VtWindow {
    pub fn new(app: &gtk::Application, output_file: Option<gio::File>) -> Self {
        glib::Object::new(&[("application", app), ("output-file", &output_file)]).unwrap()
    }

    pub fn open(&self, file: gio::File) {
        imp::VtWindow::from_instance(self).open(file);
    }
}
