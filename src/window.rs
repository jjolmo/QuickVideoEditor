use glib::subclass::prelude::*;
use gtk::{gio, glib};

mod imp {
    use std::{
        cell::{Cell, RefCell},
        ffi::{OsStr, OsString},
        marker::PhantomData,
        os::unix::prelude::OsStringExt,
        path::{Component, Path, PathBuf},
        str,
        time::Duration,
    };

    use adw::{prelude::*, subclass::prelude::*};
    use futures_util::future::{abortable, FutureExt};
    use gettextrs::*;
    use glib::{debug, error, warn, Properties};
    use gtk::{
        gdk::{self, Key, ModifierType},
        gio, glib, CompositeTemplate,
    };

    use crate::{
        config::{self, G_LOG_DOMAIN},
        parse::{self, time_to_entry_text},
        util::{gettext_f, with_recursive_children},
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
        #[template_child]
        popover_options: TemplateChild<gtk::Popover>,
        #[template_child]
        switch_row_reencode: TemplateChild<adw::SwitchRow>,
        #[template_child]
        switch_row_remove_audio: TemplateChild<adw::SwitchRow>,

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

        pub fn set_start(&self, timestamp: &str) {
            self.entry_start.set_text(timestamp);
        }

        pub fn set_end(&self, timestamp: &str) {
            self.entry_end.set_text(timestamp);
        }

        pub fn set_precise(&self, value: bool) {
            self.switch_row_reencode.set_active(value);
        }

        pub fn set_remove_audio(&self, value: bool) {
            self.switch_row_remove_audio.set_active(value);
        }

        fn on_entry_changed(&self) {
            let start_end = validate_entries(&self.entry_start, &self.entry_end);
            self.video_preview.set_start_end(start_end);
            self.button_trim.set_sensitive(start_end.is_some());
        }

        fn on_got_duration(&self, duration: i64) {
            let has_start = !self.entry_start.text().is_empty();
            let has_end = !self.entry_end.text().is_empty();

            if has_start && has_end {
                return;
            }

            // The start and end can be pre-filled, either via command-line arguments
            // or if the user types something before we get duration. If the entries are both empty,
            // default to 1/3 and 2/3 of the duration.
            let duration = duration as f64;
            let start = duration / 3.;
            let end = start * 2.;

            let mut start = start as u64;
            let mut end = (end as u64).max(start + 1);

            if !has_start {
                // If the other entry was pre-filled, default to the start of the video.
                if has_end {
                    start = 0;
                }
                self.entry_start
                    .set_text(&time_to_entry_text(Duration::from_micros(start)));
            }

            if !has_end {
                // If the other entry was pre-filled, default to the end of the video.
                if has_start {
                    end = duration as u64;
                }
                self.entry_end
                    .set_text(&time_to_entry_text(Duration::from_micros(end)));
            }

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

        fn show_open_dialog(&self) {
            if self.input_path.borrow().is_some() {
                // FIXME: replace the current file when that is supported.
                let app = self.obj().application().unwrap();
                let window = super::VtWindow::new(&app, None);
                let group = gtk::WindowGroup::new();
                group.add_window(&window);
                window.present();
                window.imp().show_open_dialog();
                return;
            }

            let obj = self.obj().clone();

            let filter = gtk::FileFilter::new();
            // Translators: file chooser file filter name.
            filter.set_name(Some(&gettext("Video files")));
            for mime_type in VIDEO_MIME_TYPES {
                filter.add_mime_type(mime_type);
            }

            let file_dialog = gtk::FileDialog::builder()
                // Translators: file chooser dialog title.
                .title(gettext("Open video"))
                .modal(true)
                .filters(&[filter].into_iter().collect::<gio::ListStore>())
                .build();

            let future = async move {
                match file_dialog.open_future(Some(&obj)).await {
                    Ok(file) => {
                        if file.path().is_none() {
                            let dialog = adw::AlertDialog::builder()
                                // Translators: error dialog title.
                                .heading(gettext("Error"))
                                .body(gettext(
                                    // Translators: error dialog text.
                                    "Video Trimmer can only operate on local files. \
Please choose another file.",
                                ))
                                .build();
                            // Translators: error dialog button.
                            dialog.add_response("ok", &gettext("_OK"));
                            dialog.present(Some(&obj));
                            return;
                        }

                        obj.open(file);
                    }
                    Err(err) => {
                        if !err.matches(gtk::DialogError::Dismissed) {
                            warn!("file dialog error: {err:?}");
                        }
                    }
                }
            };

            glib::MainContext::default().spawn_local(future);
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

            let output_path = self
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

                    let normalize = |time: &str| {
                        parse::timestamp(time)
                            .map(|ms| time_to_entry_text(Duration::from_millis(ms.into())))
                            .unwrap_or_else(|| time.to_string())
                    };
                    let start_text = normalize(&start);
                    let end_text = normalize(&end);

                    path.push(format!(
                        "{}{}.{}",
                        input_path.file_stem().and_then(OsStr::to_str).unwrap_or(""),
                        // Translators: this is appended to the output video file name.
                        // The two {} are replaced with the start and end timestamps.
                        // So for example "my video.mp4" will become "my video (0:32 - 1:06).mp4".
                        gettext_f(
                            " ({} - {})",
                            &[
                                parse::time_for_filename(&start_text),
                                parse::time_for_filename(&end_text),
                            ],
                        ),
                        extension
                    ));

                    path
                });

            let file_dialog = gtk::FileDialog::builder().modal(true).build();
            if let Some(parent) = output_path.parent() {
                if parent.to_str().map(|x| !x.is_empty()).unwrap_or(false) {
                    debug!("setting initial folder to {:?}", parent);
                    file_dialog.set_initial_folder(Some(&gio::File::for_path(parent)));
                }
            }
            if let Some(name) = output_path.file_name().and_then(OsStr::to_str) {
                debug!("setting initial name to {:?}", name);
                file_dialog.set_initial_name(Some(name));
            }

            let obj = self.obj().clone();
            let reencode = self.switch_row_reencode.is_active();
            let no_audio = self.switch_row_remove_audio.is_active();

            let future = async move {
                let output_path = match file_dialog.save_future(Some(&obj)).await {
                    Ok(file) => {
                        if let Some(path) = file.path() {
                            path
                        } else {
                            let dialog = adw::AlertDialog::builder()
                                // Translators: error dialog title.
                                .heading(gettext("Error"))
                                .body(gettext(
                                    // Translators: error dialog text.
                                    "Video Trimmer can only operate on local files. \
Please choose another file.",
                                ))
                                .build();
                            // Translators: error dialog button.
                            dialog.add_response("ok", &gettext("_OK"));
                            dialog.present(Some(&obj));
                            return;
                        }
                    }
                    Err(err) => {
                        if !err.matches(gtk::DialogError::Dismissed) {
                            warn!("file dialog error: {err:?}");
                        }
                        return;
                    }
                };

                let imp = obj.imp();
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
                    let trimming_dialog = adw::AlertDialog::builder()
                        // Translators: message dialog text.
                        .heading(gettext("Trimming…"))
                        .build();
                    // Translators: trimming dialog button.
                    trimming_dialog.add_response("cancel", &gettext("_Cancel"));

                    let obj_ = obj.clone();
                    let trimming_dialog_clone = trimming_dialog.clone();
                    let subprocess_clone = subprocess.clone();
                    let future = async move {
                        let dialog = match subprocess_clone.communicate_future(None).await {
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

                                    let view = gtk::TextView::new();
                                    view.buffer().set_text(&String::from_utf8_lossy(&stderr));
                                    view.set_editable(false);
                                    view.set_monospace(true);
                                    view.add_css_class("card");

                                    let child = gtk::ScrolledWindow::new();
                                    child.set_child(Some(&view));
                                    child.set_vscrollbar_policy(gtk::PolicyType::Never);

                                    adw::AlertDialog::builder()
                                        // Translators: error dialog heading.
                                        .heading(gettext("Error trimming video"))
                                        .body(gettext(
                                            // Translators: error dialog text before the FFmpeg
                                            // error output.
                                            "Please attach the following information \
when reporting an issue.",
                                        ))
                                        .extra_child(&child)
                                        .build()
                                }
                            }
                            Err(err) => {
                                adw::AlertDialog::builder()
                                    // Translators: error dialog text.
                                    .heading(gettext(
                                        "Could not communicate with the ffmpeg subprocess",
                                    ))
                                    .body(format!("{}", err))
                                    .build()
                            }
                        };

                        // This will invoke the signal handler, but it shouldn't be a big deal
                        // since the process has already exited and the future has already
                        // completed by then.
                        trimming_dialog_clone.close();

                        // Translators: error dialog button.
                        dialog.add_response("ok", &gettext("_OK"));
                        dialog.present(Some(&obj));
                    };
                    let (future, handle) = abortable(future);
                    let future = future.map(|_| ());

                    trimming_dialog.connect_response(None, move |_, _| {
                        debug!("force exiting the subprocess");
                        subprocess.force_exit();
                        handle.abort();
                    });
                    trimming_dialog.present(Some(&obj_));

                    glib::MainContext::default().spawn_local(future);
                }
                Err(err) => {
                    let dialog = adw::AlertDialog::builder()
                        // Translators: error dialog text.
                        .heading(gettext("Could not create the ffmpeg subprocess"))
                        .body(format!("{}", err))
                        .build();
                    // Translators: error dialog button.
                    dialog.add_response("ok", &gettext("_OK"));
                    dialog.present(Some(&obj));
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

            obj.present();
        }

        pub fn open(&self, file: gio::File) {
            let obj = self.obj().clone();

            debug!("VtWindow::open(\"{}\")", file.uri());

            if self.input_path.borrow().is_some() {
                debug!("a file is already open, cannot replace it");
                return;
            }

            if file.path().is_none() {
                obj.present();
                let dialog = adw::AlertDialog::builder()
                    // Translators: error dialog title.
                    .heading(gettext("Error"))
                    .body(gettext(
                        // Translators: error dialog text.
                        "Video Trimmer can only operate on local files. \
Please choose another file.",
                    ))
                    .build();
                // Translators: error dialog button.
                dialog.add_response("ok", &gettext("_OK"));
                dialog.present(Some(&obj));
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
                                    let streams = &output["streams"];

                                    let mut audio_formats = streams
                                        .members()
                                        .filter_map(|stream| {
                                            if stream["codec_type"].as_str() == Some("audio") {
                                                stream["codec_name"].as_str()
                                            } else {
                                                None
                                            }
                                        })
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

                                    // Only get the first one we can find, because that's what GTK is going to display
                                    let frame_rate_fraction = streams
                                        .members()
                                        .find(|stream| {
                                            stream["codec_type"].as_str() == Some("video")
                                        })
                                        .and_then(|video_stream| {
                                            video_stream["r_frame_rate"].as_str()
                                        })
                                        .inspect(|value| debug!("r_frame_rate: {value}"))
                                        .and_then(|frame_rate| frame_rate.split_once('/'));

                                    let mut frame_time = None;
                                    if let Some((numerator, denominator)) = frame_rate_fraction {
                                        if let Some((numerator, denominator)) = Option::zip(
                                            numerator.parse::<f64>().ok(),
                                            denominator.parse::<f64>().ok(),
                                        ) {
                                            if numerator > 0. && denominator > 0. {
                                                frame_time = Some(Duration::from_secs_f64(
                                                    denominator / numerator,
                                                ));
                                            }
                                        }
                                    }

                                    if let Some(frame_time) = frame_time {
                                        debug!("computed frame time: {frame_time:?}");
                                        imp.video_preview.get().set_frame_time_approx(frame_time);
                                    } else {
                                        warn!("failed get frame time, stepping will not work")
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

        pub fn step_forward(&self) {
            self.video_preview.step_forward()
        }

        pub fn step_back(&self) {
            self.video_preview.step_back()
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

            klass.install_action("win.step-forward", None, |window, _, _| {
                window.imp().step_forward()
            });
            klass.install_action("win.step-back", None, |window, _, _| {
                window.imp().step_back()
            });

            klass.install_action("win.close", None, |window, _, _| window.close());

            klass.install_action("win.trim", None, |window, _, _| {
                window.imp().verify_and_trim()
            });

            klass.install_action("win.open", None, |window, _, _| {
                window.imp().show_open_dialog()
            });

            klass.install_action("win.about", None, |window, _, _| {
                let resource_path = "/org/gnome/gitlab/YaLTeR/VideoTrimmer/\
                                     org.gnome.gitlab.YaLTeR.VideoTrimmer.metainfo.xml";
                let about_window = adw::AboutDialog::from_appdata(resource_path, Some("25.03"));
                about_window.set_version(config::VERSION);
                // Translators: shown in the About dialog, put your name here.
                about_window.set_translator_credits(&gettext("translator-credits"));
                about_window.add_link(
                    // Translators: link title in the About dialog.
                    &gettext("Contribute Translations"),
                    "https://l10n.gnome.org/module/video-trimmer/",
                );
                about_window.add_other_app(
                    "org.gnome.gitlab.YaLTeR.Identity",
                    // Translators: name of https://gitlab.gnome.org/YaLTeR/identity
                    &gettext("Identity"),
                    // Translators: summary of https://gitlab.gnome.org/YaLTeR/identity
                    &gettext("Compare images and videos"),
                );
                about_window.present(Some(window));

                // DL doesn't extract release notes from metainfo, so let's help it out with the
                // ones shown in the dialog.
                let gettext = |_| ();
                gettext("This release makes the trimming options more discoverable.");
                gettext("Changed the versioning scheme to year.month.");
                gettext("Moved the accurate trimming and remove audio options from the save dialog to the main window.");
                gettext("Added , and . shortcuts for short video stepping.");
                gettext("Updated to the GNOME 48 platform.");
                gettext("Updated translations.");
            });

            klass.install_action(
                "toast.show-in-files",
                Some(&Vec::<u8>::static_variant_type()),
                |window, _, path| {
                    let path = Vec::<u8>::from_variant(path.unwrap()).unwrap();
                    let path = PathBuf::from(OsString::from_vec(path));
                    let file = gio::File::for_path(path);

                    gtk::FileLauncher::new(Some(&file)).open_containing_folder(
                        Some(window),
                        gio::Cancellable::NONE,
                        move |res| {
                            if let Err(err) = res {
                                warn!("OpenDirectory returned an error: {:?}", err);
                            }
                        },
                    );
                },
            );

            klass.install_action("win.set-start-as-position", None, |window, _, _| {
                window.imp().video_preview.set_start_as_position()
            });
            klass.install_action("win.set-end-as-position", None, |window, _, _| {
                window.imp().video_preview.set_end_as_position()
            });

            // Add these here instead of set_accels_for_action so that they don't override typing in
            // the time entries.
            klass.add_binding_action(Key::period, ModifierType::empty(), "win.step-forward");
            klass.add_binding_action(Key::comma, ModifierType::empty(), "win.step-back");
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
                obj.add_css_class("devel");
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
                let imp = self.downgrade();
                move |_| {
                    let imp = imp.upgrade().unwrap();
                    imp.show_open_dialog();
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

                        if obj.imp().input_path.borrow().is_some() {
                            // FIXME: replace the current file when that is supported.
                            let app = obj.application().unwrap();
                            let window = super::VtWindow::new(&app, None);
                            window.open(file);
                            let group = gtk::WindowGroup::new();
                            group.add_window(&window);
                            window.present();
                            return true;
                        }

                        obj.open(file);
                        return true;
                    }

                    false
                }
            });
            self.stack.add_controller(drop_target);

            // HACK: Make options popover action row subtitles wrap eagerly to ensure that they fit
            // into the mobile window widths even with especially long translations, etc.
            with_recursive_children(self.popover_options.upcast_ref(), &mut |widget| {
                if let Some(label) = widget.downcast_ref::<gtk::Label>() {
                    if label.has_css_class("subtitle") {
                        label.set_max_width_chars(20);
                    }
                }
            });
        }
    }

    impl WidgetImpl for VtWindow {}
    impl WindowImpl for VtWindow {}
    impl ApplicationWindowImpl for VtWindow {}
    impl AdwApplicationWindowImpl for VtWindow {}

    fn validate_entries(entry_start: &gtk::Entry, entry_end: &gtk::Entry) -> Option<(u32, u32)> {
        entry_start.remove_css_class("error");
        entry_end.remove_css_class("error");

        let text_start = entry_start.text();
        let timestamp_start = parse::timestamp(text_start.as_str());
        let text_end = entry_end.text();
        let timestamp_end = parse::timestamp(text_end.as_str());

        if timestamp_start.is_none() {
            entry_start.add_css_class("error");
        }
        if timestamp_end.is_none() {
            entry_end.add_css_class("error");
        }
        if let (Some(timestamp_start), Some(timestamp_end)) = (timestamp_start, timestamp_end) {
            if timestamp_start >= timestamp_end {
                entry_end.add_css_class("error");
            } else {
                return Some((timestamp_start, timestamp_end));
            }
        }

        None
    }
}

glib::wrapper! {
    pub struct VtWindow(ObjectSubclass<imp::VtWindow>)
        @extends gtk::Widget, gtk::Window, gtk::ApplicationWindow, adw::ApplicationWindow,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
            gtk::Native, gtk::Root, gtk::ShortcutManager, gio::ActionGroup, gio::ActionMap;
}

impl VtWindow {
    pub fn new(app: &gtk::Application, output_file: Option<gio::File>) -> Self {
        glib::Object::builder()
            .property("application", app)
            .property("output-file", &output_file)
            .build()
    }

    pub fn set_start(&self, timestamp: &str) {
        self.imp().set_start(timestamp);
    }

    pub fn set_end(&self, timestamp: &str) {
        self.imp().set_end(timestamp);
    }

    pub fn set_precise(&self, value: bool) {
        self.imp().set_precise(value);
    }

    pub fn set_remove_audio(&self, value: bool) {
        self.imp().set_remove_audio(value);
    }

    pub fn open(&self, file: gio::File) {
        self.imp().open(file);
    }
}
