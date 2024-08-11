use glib::subclass::prelude::*;
use gtk::{gio, glib};

mod imp {
    use ashpd::desktop::open_uri::OpenDirectoryRequest;
    use std::{
        cell::{Cell, RefCell},
        ffi::{OsStr, OsString},
        fs::File,
        marker::PhantomData,
        os::{fd::AsFd, unix::prelude::OsStringExt},
        path::{Component, Path, PathBuf},
        str,
        time::Duration,
    };

    use adw::{prelude::*, subclass::prelude::*};
    use futures_util::future::{abortable, FutureExt};
    use gettextrs::*;
    use glib::{debug, error, warn, Properties};
    use gtk::{
        gdk,
        gdk::{Key, ModifierType},
        gio, glib, CompositeTemplate,
    };

    use crate::{
        config::{self, G_LOG_DOMAIN},
        parse::{self, time_to_entry_text},
        util::gettext_f,
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

    #[derive(Debug, Default, CompositeTemplate, Properties)]
    #[properties(wrapper_type = super::VtWindow)]
    #[template(resource = "/org/gnome/gitlab/YaLTeR/VideoTrimmer/window.ui")]
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
        stack: TemplateChild<gtk::Stack>,
        #[template_child]
        title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        box_start_end: TemplateChild<gtk::Box>,
        #[template_child]
        overlay_error_page: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        toolbar_view: TemplateChild<adw::ToolbarView>,

        #[property(get = Self::is_playing, set = Self::set_is_playing, explicit_notify)]
        is_playing: PhantomData<bool>,

        content_type: RefCell<Option<glib::GString>>,
        input_path: RefCell<Option<PathBuf>>,
        #[property(set, construct_only)]
        output_file: RefCell<Option<gio::File>>,
        do_not_default_to_mp4: Cell<bool>,
    }

    impl VtWindow {
        fn is_playing(&self) -> bool {
            self.video_preview.is_playing()
        }

        fn set_is_playing(&self, value: bool) {
            self.video_preview.set_is_playing(value)
        }

        fn on_entry_changed(&self) {
            let start_end = validate_entries(&self.entry_start, &self.entry_end);
            self.video_preview.set_start_end(start_end);
            self.button_trim.set_sensitive(start_end.is_some());
        }

        fn on_got_duration(&self, duration: i64) {
            // If the user hasn't started typing in the timestamp entries, fill them with default
            // values.
            if !self.entry_start.text().is_empty() || !self.entry_end.text().is_empty() {
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
            if self.entry_start.focus_child().is_some() {
                self.entry_start.select_region(0, -1);
            } else if self.entry_end.focus_child().is_some() {
                self.entry_end.select_region(0, -1);
            }
        }

        fn on_set_start_end(&self, start: Duration, end: Duration) {
            let text = time_to_entry_text(start);
            if parse::timestamp(&self.entry_start.text())
                .map(|x| x != parse::timestamp(&text).unwrap())
                .unwrap_or(true)
            {
                self.entry_start.set_text(&text);
            }

            let text = time_to_entry_text(end);
            if parse::timestamp(&self.entry_end.text())
                .map(|x| x != parse::timestamp(&text).unwrap())
                .unwrap_or(true)
            {
                self.entry_end.set_text(&text);
            }
        }

        fn on_set_start(&self, start: Duration) {
            let text = time_to_entry_text(start);
            if parse::timestamp(&self.entry_start.text())
                .map(|x| x != parse::timestamp(&text).unwrap())
                .unwrap_or(true)
            {
                self.entry_start.set_text(&text);
            }
        }

        fn on_set_end(&self, end: Duration) {
            let text = time_to_entry_text(end);
            if parse::timestamp(&self.entry_end.text())
                .map(|x| x != parse::timestamp(&text).unwrap())
                .unwrap_or(true)
            {
                self.entry_end.set_text(&text);
            }
        }

        fn verify_and_trim(&self) {
            if validate_entries(&self.entry_start, &self.entry_end).is_none() {
                debug!("the timestamps are invalid");
                return;
            }

            let start = self.entry_start.text();
            let end = self.entry_end.text();

            let extension = self
                .content_type
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
                .and_then(|exts| exts.first())
                .unwrap_or(&"mp4");

            let extension = if *extension == "mp4" && self.do_not_default_to_mp4.get() {
                "mkv"
            } else {
                extension
            }
            .to_string();

            let input_path = self.input_path.borrow();
            if input_path.is_none() {
                // This should not happen normally because if the button is visible then we should
                // have the input path already.
                debug!("the input path is unset");
                return;
            }

            let input_path = input_path.clone().unwrap();

            self.trim(input_path, extension, start, end);
        }

        fn trim(
            &self,
            input_path: PathBuf,
            extension: String,
            start: glib::GString,
            end: glib::GString,
        ) {
            debug!("trim: from {} to {}", start, end);
            debug!("input_path: {:?}", input_path);

            self.video_preview.pause();

            let obj = self.obj().clone();

            let future = async move {
                let imp = obj.imp();

                let output_path = imp
                    .output_file
                    .borrow()
                    .as_ref()
                    .and_then(|file| file.path())
                    .unwrap_or_else(|| {
                        let document_portal_components = [
                            Component::RootDir,
                            Component::Normal(OsStr::new("run")),
                            Component::Normal(OsStr::new("user")),
                            Component::Normal(OsStr::new("doc")),
                        ];

                        let mut components = input_path.components();

                        let prefix = if components.next() == Some(document_portal_components[0])
                            && components.next() == Some(document_portal_components[1])
                            && components.next() == Some(document_portal_components[2])
                            && components.nth(1) == Some(document_portal_components[3])
                        {
                            // input_path comes from the document portal, no use in opening the
                            // file chooser there.
                            None
                        } else {
                            input_path.parent().map(Path::to_path_buf)
                        };

                        let mut path = prefix.unwrap_or_default();

                        path.push(format!(
                            "{}{}.{}",
                            input_path.file_stem().and_then(OsStr::to_str).unwrap_or(""),
                            // Translators: this is appended to the output video file name.
                            // So for example "my video.mp4" will become "my video (trimmed).mp4".
                            gettext(" (trimmed)"),
                            extension
                        ));

                        path
                    });

                let file_chooser = gtk::FileChooserNative::builder()
                    .transient_for(&obj)
                    .action(gtk::FileChooserAction::Save)
                    .modal(true)
                    .build();
                if let Some(parent) = output_path.parent() {
                    if parent.to_str().map(|x| !x.is_empty()).unwrap_or(false) {
                        debug!("setting current folder to {:?}", parent);
                        let _ = file_chooser.set_current_folder(Some(&gio::File::for_path(parent)));
                    }
                }
                if let Some(name) = output_path.file_name().and_then(OsStr::to_str) {
                    debug!("setting current name to {:?}", name);
                    file_chooser.set_current_name(name);
                }

                // Translators: checkbox in output file selection dialog that strips audio from the
                // video file.
                file_chooser.add_choice("no-audio", gettext("Remove audio"), &[]);

                file_chooser.add_choice(
                    "reencode",
                    // Translators: checkbox in output file selection dialog.
                    gettext("Accurate trimming, but slower and may lose quality"),
                    &[],
                );

                let (tx, rx) = futures_channel::oneshot::channel();

                let tx = RefCell::new(Some(tx));
                file_chooser.connect_response({
                    let obj = obj.downgrade();
                    move |file_chooser, response| {
                        if let Some(tx) = tx.borrow_mut().take() {
                            if response == gtk::ResponseType::Accept {
                                if let Some(path) = file_chooser.file().and_then(|file| file.path()) {
                                    let no_audio = file_chooser.choice("no-audio").map(|choice| choice == "true").unwrap_or(false);
                                    let reencode = file_chooser.choice("reencode").map(|choice| choice == "true").unwrap_or(false);
                                    tx.send(Some((path, no_audio, reencode))).unwrap();
                                } else {
                                    let dialog = gtk::MessageDialog::builder()
                                        // Translators: error dialog title.
                                        .text(gettext("Error"))
                                        .secondary_text(gettext(
                                            // Translators: error dialog text.
                                            "Video Trimmer can only operate on local files. Please choose another file.",
                                        ))
                                        .message_type(gtk::MessageType::Error)
                                        .buttons(gtk::ButtonsType::Ok)
                                        .transient_for(&obj.upgrade().unwrap())
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

                let (output_path, no_audio, reencode) = if let Some(value) = rx.await.unwrap() {
                    value
                } else {
                    return;
                };

                imp.do_trim(&input_path, output_path, no_audio, reencode, start, end);
            };

            glib::MainContext::default().spawn_local(future);
        }

        fn do_trim(
            &self,
            input_path: &Path,
            output_path: PathBuf,
            no_audio: bool,
            reencode: bool,
            start: glib::GString,
            end: glib::GString,
        ) {
            let obj = self.obj().clone();

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
                "-y".as_ref(),
            ]
            .to_vec();
            if !reencode {
                args.push("-c".as_ref());
                args.push("copy".as_ref());
            }
            if reencode
                && output_path
                    .extension()
                    .map(|x| x == "mp4" || x == "mkv")
                    .unwrap_or(false)
            {
                // The default mp4 and mkv encoder selected by org.freedesktop.Platform.ffmpeg-full
                // is mpeg4 which has terrible quality and file size. Use libvpx-vp9 instead.
                args.push("-c:v".as_ref());
                args.push("libvpx-vp9".as_ref());
            }
            if no_audio {
                args.push("-an".as_ref());
            }
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
                    let trimming_dialog = gtk::MessageDialog::builder()
                        // Translators: message dialog text.
                        .text(gettext("Trimming…"))
                        .message_type(gtk::MessageType::Info)
                        .buttons(gtk::ButtonsType::Cancel)
                        .transient_for(&obj)
                        .modal(true)
                        .build();

                    let trimming_dialog_clone = trimming_dialog.clone();
                    let subprocess_clone = subprocess.clone();
                    let future = async move {
                        let builder = match subprocess_clone.communicate_future(None).await {
                            Ok((_, stderr)) => {
                                if subprocess_clone.has_exited()
                                    && subprocess_clone.exit_status() == 0
                                {
                                    let file_name = output_path
                                        .file_name()
                                        .map(|file_name| file_name.to_string_lossy())
                                        .unwrap_or_else(|| output_path.to_string_lossy());

                                    let imp = obj.imp();
                                    let toast = adw::Toast::new(&gettext_f(
                                        // Translators: text on the toast after trimming was done.
                                        // The placeholder is the video filename.
                                        "{} has been saved",
                                        &[&file_name],
                                    ));

                                    // Translators: text on the button of the toast after
                                    // trimming was done to show the output file in the file
                                    // manager.
                                    toast.set_button_label(Some(&gettext("Show in Files")));
                                    toast.set_action_name(Some("toast.show-in-files"));
                                    toast.set_action_target(Some(
                                        &output_path.into_os_string().into_vec(),
                                    ));

                                    if imp.stack_video_preview.visible_child_name().as_deref()
                                        == Some("page_error")
                                    {
                                        imp.overlay_error_page.add_toast(toast);
                                    } else {
                                        imp.video_preview.overlay().add_toast(toast);
                                    }

                                    trimming_dialog_clone.close();
                                    return;
                                } else {
                                    let stderr = stderr
                                        .expect("should be Some() because we passed STDERR_PIPE");
                                    gtk::MessageDialog::builder()
                                        // Translators: error dialog text.
                                        .text(gettext("Error trimming video"))
                                        .secondary_text(String::from_utf8_lossy(&stderr))
                                        .message_type(gtk::MessageType::Error)
                                }
                            }
                            Err(err) => {
                                gtk::MessageDialog::builder()
                                    // Translators: error dialog text.
                                    .text(gettext(
                                        "Could not communicate with the ffmpeg subprocess",
                                    ))
                                    .secondary_text(format!("{}", err))
                                    .message_type(gtk::MessageType::Error)
                            }
                        };

                        // This will invoke the signal handler, but it shouldn't be a big deal
                        // since the process has already exited and the future has already
                        // completed by then.
                        trimming_dialog_clone.close();

                        let dialog = builder
                            .buttons(gtk::ButtonsType::Ok)
                            .transient_for(&obj)
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
                    let dialog = gtk::MessageDialog::builder()
                        // Translators: error dialog text.
                        .text(gettext("Could not create the ffmpeg subprocess"))
                        .secondary_text(format!("{}", err))
                        .message_type(gtk::MessageType::Error)
                        .buttons(gtk::ButtonsType::Ok)
                        .transient_for(&obj)
                        .modal(true)
                        .build();
                    dialog.connect_response(move |dialog, _| dialog.close());
                    dialog.show();
                }
            }
        }

        fn switch_to_main_page(&self) {
            if self.stack.visible_child_name().as_ref().map(|x| x.as_str()) == Some("page_main") {
                return;
            }

            let obj = self.obj();

            self.stack.set_visible_child_name("page_main");
            obj.set_default_widget(Some(&*self.button_trim));

            // Focus the entry when coming from the empty state.
            self.entry_start.grab_focus();

            obj.show();
        }

        pub fn open(&self, file: gio::File) {
            let obj = self.obj().clone();

            debug!("VtWindow::open(\"{}\")", file.uri());

            if file.path().is_none() {
                obj.show();
                let dialog = gtk::MessageDialog::builder()
                    // Translators: error dialog title.
                    .text(gettext("Error"))
                    .secondary_text(gettext(
                        // Translators: error dialog text.
                        "Video Trimmer can only operate on local files. Please choose another file.",
                    ))
                    .message_type(gtk::MessageType::Error)
                    .buttons(gtk::ButtonsType::Ok)
                    .transient_for(&obj)
                    .modal(true)
                    .build();
                dialog.connect_response(|dialog, _| {
                    dialog.close();
                });
                dialog.show();
                return;
            }

            self.video_preview.open(&file);

            // Unconditionally switch to main page after 300 ms
            // (if the video takes too long to load).
            glib::timeout_add_local_once(Duration::from_millis(300), {
                let obj = obj.downgrade();
                move || {
                    let obj = match obj.upgrade() {
                        Some(obj) => obj,
                        None => return,
                    };
                    let imp = obj.imp();
                    imp.switch_to_main_page();
                }
            });

            // Verified in callers.
            *self.input_path.borrow_mut() = Some(file.path().unwrap());

            // Get the display name and content type.
            let future = async move {
                let imp = obj.imp();

                // May take a long time on a network mount.
                let info = file
                    .query_info_future(
                        "standard::display-name,standard::fast-content-type",
                        gio::FileQueryInfoFlags::NONE,
                        glib::Priority::DEFAULT,
                    )
                    .await;

                match info {
                    Ok(info) => {
                        let display_name = info.display_name();
                        imp.title.set_subtitle(display_name.as_str());

                        if let Some(fast_content_type) =
                            info.attribute_string("standard::fast-content-type")
                        {
                            debug!("fast-content-type: {}", fast_content_type);
                            *imp.content_type.borrow_mut() = Some(fast_content_type);
                        }
                    }
                    // Fails when the file does not exist.
                    Err(err) => {
                        error!("error getting file information: {err:?}");

                        if let Some(basename) = file.basename() {
                            imp.title.set_subtitle(&basename.to_string_lossy());
                        }
                    }
                }
            };
            glib::MainContext::default().spawn_local(future);

            // Run ffprobe to get information we need.
            let input_path = self.input_path.borrow();
            let args: Vec<&OsStr> = vec![
                "ffprobe".as_ref(),
                "-print_format".as_ref(),
                "json".as_ref(),
                "-select_streams".as_ref(),
                "a".as_ref(),
                "-show_streams".as_ref(),
                input_path.as_ref().unwrap().as_ref(),
            ];
            debug!("invoking: {:?}", args);

            let subprocess = match gio::Subprocess::newv(
                &args,
                gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_PIPE,
            ) {
                Ok(subprocess) => subprocess,
                Err(err) => {
                    warn!("error spawning ffprobe: {err:?}");
                    return;
                }
            };

            let obj = self.obj().clone();
            let future = async move {
                match subprocess.communicate_future(None).await {
                    Ok((stdout, stderr)) => {
                        if subprocess.has_exited() && subprocess.exit_status() == 0 {
                            let stdout =
                                stdout.expect("should be Some() because we passed STDOUT_PIPE");
                            match str::from_utf8(&stdout) {
                                Ok(stdout) => {
                                    let imp = obj.imp();
                                    let output = json::parse(stdout)
                                        .expect("ffprobe should return valid JSON");
                                    let mut audio_formats = output["streams"]
                                        .members()
                                        .filter_map(|stream| stream["codec_name"].as_str())
                                        .inspect(|name| debug!("audio codec: {}", name));

                                    // Some Sony cameras produce .mp4 videos with PCM audio. This is invalid
                                    // according to the MP4 standard, so FFmpeg refuses to mux them back. To work
                                    // around this limitation, we change the default output file extension when a
                                    // PCM audio track is detected.
                                    if audio_formats.any(|name| name.starts_with("pcm_")) {
                                        debug!(
                                            "avoiding default .mp4 extension: PCM audio detected"
                                        );
                                        imp.do_not_default_to_mp4.set(true);
                                    }
                                }
                                Err(err) => {
                                    // ffmpeg's JSON output fixes up invalid UTF-8 for us with
                                    // replacement characters, so this should be unreachable.
                                    // However, leave it as a warning because it's not a big deal if
                                    // it somehow fails, and not worth crashing the process.
                                    warn!("ffprobe returned invalid UTF-8: {err:?}");
                                }
                            }
                        } else {
                            let stderr =
                                stderr.expect("should be Some() because we passed STDERR_PIPE");
                            warn!("ffprobe error: {}", String::from_utf8_lossy(&stderr));
                        }
                    }
                    Err(err) => {
                        warn!("error communicating with ffprobe: {err:?}");
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
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);

            klass.install_property_action("win.play-pause", "is-playing");
            klass.add_binding_action(Key::p, ModifierType::empty(), "win.play-pause");
            klass.add_binding_action(Key::k, ModifierType::empty(), "win.play-pause");
            klass.add_binding_action(Key::space, ModifierType::CONTROL_MASK, "win.play-pause");

            klass.install_action("win.close", None, |window, _, _| window.close());
            klass.add_binding_action(Key::w, ModifierType::CONTROL_MASK, "win.close");

            klass.install_action("win.trim", None, |window, _, _| {
                window.imp().verify_and_trim()
            });
            klass.add_binding_action(Key::s, ModifierType::CONTROL_MASK, "win.trim");

            klass.install_action("win.about", None, |window, _, _| {
                let resource_path = "/org/gnome/gitlab/YaLTeR/VideoTrimmer/\
                                     org.gnome.gitlab.YaLTeR.VideoTrimmer.metainfo.xml";
                let about_window = adw::AboutDialog::from_appdata(resource_path, Some("0.9.0"));
                about_window.set_version(config::VERSION);
                // Translators: shown in the About dialog, put your name here.
                about_window.set_translator_credits(&gettext("translator-credits"));
                about_window.add_link(
                    // Translators: link title in the About dialog.
                    &gettext("Contribute Translations"),
                    "https://l10n.gnome.org/module/video-trimmer/",
                );
                about_window.present(Some(window));

                // DL doesn't extract release notes from metainfo, so let's help it out with the
                // ones shown in the dialog.
                let gettext = |_| ();
                gettext("This release contains a minor visual refresh for GNOME 45.");
                gettext("Tweaked the visual style for the GNOME 45 release.");
                gettext("Fixed app crashing when ffprobe is missing.");
                gettext("Updated to the GNOME 45 platform.");
                gettext("Updated translations.");
            });

            klass.install_action(
                "toast.show-in-files",
                Some(&Vec::<u8>::static_variant_type()),
                |window, _, path| {
                    let path = Vec::<u8>::from_variant(path.unwrap()).unwrap();
                    let path = PathBuf::from(OsString::from_vec(path));
                    let file = match File::open(path) {
                        Ok(value) => value,
                        Err(err) => {
                            warn!("couldn't open the output file: {:?}", err);
                            return;
                        }
                    };

                    let native = if let Some(value) = window.native() {
                        value
                    } else {
                        warn!("window.native() returned None");
                        return;
                    };

                    let future = async move {
                        let identifier = ashpd::WindowIdentifier::from_native(&native).await;
                        if let Err(err) = OpenDirectoryRequest::default()
                            .identifier(identifier)
                            .send(&file.as_fd())
                            .await
                        {
                            warn!("OpenDirectory returned an error: {:?}", err);
                        }
                    };
                    glib::MainContext::default().spawn_local(future);
                },
            );

            klass.install_action("win.set-start-as-position", None, |window, _, _| {
                window.imp().video_preview.set_start_as_position()
            });
            klass.add_binding_action(Key::i, ModifierType::empty(), "win.set-start-as-position");

            klass.install_action("win.set-end-as-position", None, |window, _, _| {
                window.imp().video_preview.set_end_as_position()
            });
            klass.add_binding_action(Key::o, ModifierType::empty(), "win.set-end-as-position");
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for VtWindow {
        fn properties() -> &'static [glib::ParamSpec] {
            Self::derived_properties()
        }

        fn property(&self, id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            self.derived_property(id, pspec)
        }

        fn set_property(&self, id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            self.derived_set_property(id, value, pspec);
        }

        fn constructed(&self) {
            let obj = self.obj();
            self.parent_constructed();

            if config::PROFILE == "Devel" {
                obj.style_context().add_class("devel");
            }

            // Start entry is always on the left, just like the timeline.
            self.box_start_end.set_direction(gtk::TextDirection::Ltr);

            // Add playback controls as a bottom bar into our ToolbarView.
            self.toolbar_view.remove(&*self.box_start_end);
            self.toolbar_view
                .add_bottom_bar(self.video_preview.box_playback_controls());
            self.toolbar_view.add_bottom_bar(&*self.box_start_end);

            self.video_preview
                .connect_local("notify::duration", false, {
                    let obj = obj.downgrade();
                    move |_| {
                        let obj = obj.upgrade().unwrap();
                        let imp = obj.imp();

                        let duration: i64 = imp.video_preview.property("duration");

                        imp.stack_video_preview
                            .set_visible_child(&*imp.video_preview);
                        imp.switch_to_main_page();

                        if duration == 0 {
                            return None;
                        }

                        imp.on_got_duration(duration);

                        None
                    }
                });

            self.video_preview.connect_local("set-start-end", false, {
                let obj = obj.downgrade();
                move |args| {
                    let mut args = args
                        .iter()
                        .skip(1)
                        .map(|x| Duration::from_millis(x.get::<u32>().unwrap().into()));
                    let start = args.next().unwrap();
                    let end = args.next().unwrap();

                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();
                    imp.on_set_start_end(start, end);

                    None
                }
            });

            self.video_preview.connect_local("set-start", false, {
                let obj = obj.downgrade();
                move |args| {
                    let mut args = args
                        .iter()
                        .skip(1)
                        .map(|x| Duration::from_millis(x.get::<u32>().unwrap().into()));
                    let start = args.next().unwrap();

                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();
                    imp.on_set_start(start);

                    None
                }
            });

            self.video_preview.connect_local("set-end", false, {
                let obj = obj.downgrade();
                move |args| {
                    let mut args = args
                        .iter()
                        .skip(1)
                        .map(|x| Duration::from_millis(x.get::<u32>().unwrap().into()));
                    let end = args.next().unwrap();

                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();
                    imp.on_set_end(end);

                    None
                }
            });

            self.video_preview.connect_local("error", false, {
                let obj = obj.downgrade();
                move |_| {
                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();
                    imp.stack_video_preview.set_visible_child_name("page_error");
                    imp.switch_to_main_page();

                    None
                }
            });

            // The open button.
            self.button_open.connect_clicked({
                let obj = obj.downgrade();
                move |_| {
                    let obj = obj.upgrade().unwrap();
                    let filter = gtk::FileFilter::new();
                    // Translators: file chooser file filter name.
                    filter.set_name(Some(&gettext("Video files")));
                    for mime_type in VIDEO_MIME_TYPES {
                        filter.add_mime_type(mime_type);
                    }

                    let file_chooser = gtk::FileChooserNative::builder()
                        .transient_for(&obj)
                        .action(gtk::FileChooserAction::Open)
                        // Translators: file chooser dialog title.
                        .title(gettext("Open video"))
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

                            // This is normally safe to unwrap(), however, due to a bug, it returns
                            // None for remote files.
                            // https://gitlab.gnome.org/GNOME/xdg-desktop-portal-gnome/-/issues/45
                            let file = file_chooser.file();
                            if file.is_none() || file.clone().unwrap().path().is_none() {
                                let dialog = gtk::MessageDialog::builder()
                                    // Translators: error dialog title.
                                    .text(gettext("Error"))
                                    .secondary_text(gettext(
                                        // Translators: error dialog text.
                                        "Video Trimmer can only operate on local files. \
Please choose another file.",
                                    ))
                                    .message_type(gtk::MessageType::Error)
                                    .buttons(gtk::ButtonsType::Ok)
                                    .transient_for(&obj)
                                    .modal(true)
                                    .build();
                                dialog.connect_response(|dialog, _| {
                                    dialog.close();
                                });
                                dialog.show();
                                return;
                            }

                            obj.open(file.unwrap());
                        }
                    });

                    file_chooser.show();
                }
            });

            // Start and end timestamp validation and visualization.
            self.entry_start.connect_text_notify({
                let obj = obj.downgrade();
                move |_| {
                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();
                    imp.on_entry_changed();
                }
            });
            self.entry_end.connect_text_notify({
                let obj = obj.downgrade();
                move |_| {
                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();
                    imp.on_entry_changed();
                }
            });

            // The trim button.
            self.button_trim.connect_clicked({
                let obj = obj.downgrade();
                move |_| {
                    let Some(obj) = obj.upgrade() else {
                        return;
                    };

                    obj.imp().verify_and_trim();
                }
            });

            let drop_target = gtk::DropTarget::new(gio::File::static_type(), gdk::DragAction::COPY);
            drop_target.connect_drop({
                let obj = obj.downgrade();
                move |_, data, _, _| {
                    if let Ok(file) = data.get::<gio::File>() {
                        let obj = obj.upgrade().unwrap();
                        obj.open(file);
                        return true;
                    }

                    false
                }
            });
            self.stack.add_controller(drop_target);
        }
    }

    impl WidgetImpl for VtWindow {}
    impl WindowImpl for VtWindow {}
    impl ApplicationWindowImpl for VtWindow {}
    impl AdwApplicationWindowImpl for VtWindow {}

    fn validate_entries(entry_start: &gtk::Entry, entry_end: &gtk::Entry) -> Option<(u32, u32)> {
        let style_start = entry_start.style_context();
        let style_end = entry_end.style_context();
        style_start.remove_class("error");
        style_end.remove_class("error");

        let text_start = entry_start.text();
        let timestamp_start = parse::timestamp(text_start.as_str());
        let text_end = entry_end.text();
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
        glib::Object::builder()
            .property("application", app)
            .property("output-file", &output_file)
            .build()
    }

    pub fn open(&self, file: gio::File) {
        self.imp().open(file);
    }
}
