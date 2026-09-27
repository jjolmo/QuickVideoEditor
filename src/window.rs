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
        engine::{ExportSettings, RenderEvent},
        knob::VtKnob,
        music_track::FADE_OUT_DURATION,
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
    #[template(resource = "/io/github/jjolmo/QuickVideoEditor/window.ui")]
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
        #[template_child]
        combo_row_fps: TemplateChild<adw::ComboRow>,
        #[template_child]
        spin_row_fps: TemplateChild<adw::SpinRow>,
        #[template_child]
        switch_row_social_snap: TemplateChild<adw::SwitchRow>,
        #[template_child]
        combo_row_resolution: TemplateChild<adw::ComboRow>,
        #[template_child]
        spin_row_width: TemplateChild<adw::SpinRow>,
        #[template_child]
        spin_row_height: TemplateChild<adw::SpinRow>,
        #[template_child]
        knob_speed: TemplateChild<VtKnob>,
        #[template_child]
        button_options: TemplateChild<gtk::MenuButton>,
        #[template_child]
        toggle_mode: TemplateChild<adw::ToggleGroup>,
        #[template_child]
        box_editor: TemplateChild<gtk::Box>,
        #[template_child]
        box_editor_options: TemplateChild<gtk::Box>,
        #[template_child]
        button_add_video: TemplateChild<gtk::Button>,
        #[template_child]
        button_split: TemplateChild<gtk::Button>,
        #[template_child]
        button_delete_segment: TemplateChild<gtk::Button>,
        #[template_child]
        check_end_fade: TemplateChild<gtk::CheckButton>,

        #[property(get = Self::is_playing, set = Self::set_is_playing, explicit_notify)]
        is_playing: PhantomData<bool>,

        content_type: RefCell<Option<glib::GString>>,
        input_path: RefCell<Option<PathBuf>>,
        #[property(set, construct_only)]
        output_file: RefCell<Option<gio::File>>,
        do_not_default_to_mp4: Cell<bool>,
        has_audio: Cell<bool>,
        /// Music requested before the video duration was known.
        pending_music: RefCell<Option<PathBuf>>,
        /// Videos to add once the first one is ready.
        pending_videos: RefCell<Vec<gio::File>>,
        pending_speed: Cell<Option<f64>>,
        /// Command-line export to run once everything is loaded.
        pending_export: RefCell<Option<PathBuf>>,
        speed: Cell<f64>,
        /// Width and height of the first video.
        source_size: Cell<Option<(i32, i32)>>,
        /// Set while the mode switch is changed from code, so its handler ignores it.
        updating_mode: Cell<bool>,
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

        pub fn set_music(&self, file: gio::File) {
            debug!("music: requested {}", file.uri());
            let Some(path) = file.path() else {
                warn!("music must be a local file: {}", file.uri());
                return;
            };

            if self.video_preview.duration() > 0 {
                self.video_preview.set_music(path);
            } else {
                self.pending_music.replace(Some(path));
            }
        }

        fn on_speed_changed(&self, speed: f64) {
            self.speed.set(speed);
            self.knob_speed.set_value(speed);
            self.video_preview.set_speed(speed);
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
            // With a video open, the chosen video is added to the edit.
            let add = self.input_path.borrow().is_some();
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
                                    "Quick Video Editor can only operate on local files. \
Please choose another file.",
                                ))
                                .build();
                            // Translators: error dialog button.
                            dialog.add_response("ok", &gettext("_OK"));
                            dialog.present(Some(&obj));
                            return;
                        }

                        if add {
                            obj.imp().add_video(file);
                        } else {
                            obj.open(file);
                        }
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

        pub fn queue_speed(&self, speed: f64) {
            let speed = speed.clamp(crate::knob::MIN_SPEED, crate::knob::MAX_SPEED);
            if self.video_preview.duration() > 0 {
                self.on_speed_changed(speed);
            } else {
                self.pending_speed.set(Some(speed));
            }
        }

        pub fn queue_export(&self, file: gio::File) {
            if let Some(path) = file.path() {
                self.pending_export.replace(Some(path));
            }
        }

        /// Renders the edit without dialogs and quits, for `--export`.
        fn export_headless(&self, path: PathBuf) {
            if !self.video_preview.is_editor() {
                self.video_preview.set_editor_mode(true);
                self.show_mode(true);
            }
            let app = self.obj().application().unwrap();
            let hold = app.hold();
            let result = self
                .video_preview
                .engine()
                .render(&path, self.export_settings(), {
                    let path = path.clone();
                    move |event| match event {
                        RenderEvent::Progress(fraction) => {
                            debug!("export: {:.0}%", fraction * 100.);
                        }
                        RenderEvent::Done => {
                            println!("{}", path.display());
                            let _ = (&hold, &app);
                            // Quitting normally right after a render can crash while GES tears the
                            // pipeline down; the file is complete, so leave at once.
                            std::process::exit(0);
                        }
                        RenderEvent::Failed(message) => {
                            eprintln!("export failed: {message}");
                            std::process::exit(1);
                        }
                    }
                });
            if let Err(err) = result {
                eprintln!("export failed: {err}");
                std::process::exit(1);
            }
        }

        pub fn queue_video(&self, file: gio::File) {
            if self.video_preview.duration() > 0 {
                self.add_video(file);
            } else {
                self.pending_videos.borrow_mut().push(file);
            }
        }

        /// Appends a video to the edit, switching to the Editor.
        fn add_video(&self, file: gio::File) {
            if file.path().is_none() {
                self.show_error(&gettext(
                    // Translators: error dialog text.
                    "Quick Video Editor can only operate on local files. \
Please choose another file.",
                ));
                return;
            }
            match self.video_preview.add_video(&file) {
                Ok(()) => self.show_mode(true),
                Err(err) => {
                    warn!("could not add {}: {err}", file.uri());
                    self.show_error(&gettext_f(
                        // Translators: error dialog text; the placeholder is the error.
                        "Could not add the video: {}",
                        &[&err.to_string()],
                    ));
                }
            }
        }

        fn show_error(&self, body: &str) {
            let dialog = adw::AlertDialog::builder()
                // Translators: error dialog title.
                .heading(gettext("Error"))
                .body(body)
                .build();
            // Translators: error dialog button.
            dialog.add_response("ok", &gettext("_OK"));
            dialog.present(Some(&*self.obj()));
        }

        /// Updates the controls for the Trimmer or the Editor.
        fn show_mode(&self, editor: bool) {
            self.updating_mode.set(true);
            self.toggle_mode
                .set_active_name(Some(if editor { "editor" } else { "trimmer" }));
            self.updating_mode.set(false);

            self.box_start_end.set_visible(!editor);
            self.box_editor.set_visible(editor);
            let options: &gtk::Widget = self.button_options.upcast_ref();
            let target: &gtk::Box = if editor {
                &self.box_editor_options
            } else {
                &self.box_start_end
            };
            if options.parent().as_ref() != Some(target.upcast_ref()) {
                options.unparent();
                target.append(options);
            }
            self.switch_row_reencode.set_sensitive(!editor);

            if editor {
                // Translators: the main button in the Editor, which renders the edit.
                self.button_trim.set_label(&gettext("Export"));
                // Translators: tooltip of the main button in the Editor.
                self.button_trim
                    .set_tooltip_text(Some(&gettext("Export the Edited Video")));
            } else {
                self.button_trim.set_label(&gettext("Trim"));
                self.button_trim
                    .set_tooltip_text(Some(&gettext("Trim Video")));
            }
            self.on_edit_changed();
        }

        /// Refreshes the controls that depend on the edit.
        fn on_edit_changed(&self) {
            let editor = self.video_preview.is_editor();
            let sources = self.video_preview.source_count();
            if let Some(trimmer) = self.toggle_mode.toggle_by_name("trimmer") {
                trimmer.set_enabled(sources <= 1);
            }
            if editor {
                self.button_trim
                    .set_sensitive(self.video_preview.segment_count() > 0);
                if sources > 1 {
                    self.title.set_subtitle(&gettext_f(
                        // Translators: window subtitle; the placeholder is how many videos.
                        "{} videos",
                        &[&sources.to_string()],
                    ));
                }
            } else {
                self.on_entry_changed();
            }
            // Deletes the selected segment, or the one under the playhead.
            self.button_delete_segment
                .set_sensitive(editor && self.video_preview.segment_count() > 1);
        }

        fn on_mode_toggled(&self) {
            if self.updating_mode.get() {
                return;
            }
            let editor = self.toggle_mode.active_name().as_deref() == Some("editor");
            if editor == self.video_preview.is_editor() {
                return;
            }
            if editor {
                self.video_preview.set_editor_mode(true);
                self.show_mode(true);
                return;
            }
            if self.video_preview.source_count() > 1 {
                self.show_mode(true);
                return;
            }
            if self.video_preview.segment_count() <= 1 {
                self.leave_editor();
                return;
            }

            let dialog = adw::AlertDialog::builder()
                // Translators: dialog heading when switching from the Editor to the Trimmer.
                .heading(gettext("Discard the Edit?"))
                .body(gettext(
                    // Translators: dialog text when switching from the Editor to the Trimmer.
                    "The Trimmer works on the whole video. Cuts, moves and speed changes \
will be lost.",
                ))
                .close_response("cancel")
                .build();
            // Translators: dialog button.
            dialog.add_response("cancel", &gettext("_Cancel"));
            // Translators: dialog button that discards the edit.
            dialog.add_response("discard", &gettext("_Discard"));
            dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
            let obj = self.obj().clone();
            dialog.connect_response(None, move |_, response| {
                let imp = obj.imp();
                if response == "discard" {
                    imp.leave_editor();
                } else {
                    imp.show_mode(true);
                }
            });
            dialog.present(Some(&*self.obj()));
        }

        fn leave_editor(&self) {
            self.video_preview.set_editor_mode(false);
            self.speed.set(1.);
            self.knob_speed.set_value(1.);
            self.show_mode(false);
        }

        pub fn set_export_options(&self, fps: Option<i32>, size: Option<(i32, i32)>) {
            if let Some(fps) = fps {
                match [24, 25, 30, 50, 60]
                    .iter()
                    .position(|&preset| preset == fps)
                {
                    Some(index) => self.combo_row_fps.set_selected(index as u32 + 1),
                    None => {
                        self.combo_row_fps.set_selected(6);
                        self.spin_row_fps.set_value(fps.into());
                    }
                }
            }
            if let Some((width, height)) = size {
                self.combo_row_resolution.set_selected(3);
                self.spin_row_width.set_value(width.into());
                self.spin_row_height.set_value(height.into());
            }
        }

        /// Export frame rate and size chosen in the options menu.
        fn export_settings(&self) -> ExportSettings {
            let fps = match self.combo_row_fps.selected() {
                1 => Some((24, 1)),
                2 => Some((25, 1)),
                3 => Some((30, 1)),
                4 => Some((50, 1)),
                5 => Some((60, 1)),
                6 => Some((self.spin_row_fps.value().round() as i32, 1)),
                _ => None,
            };
            // Presets keep the source's aspect ratio; sizes are even, as encoders require.
            let even = |value: f64| ((value / 2.).round() as i32 * 2).max(2);
            let preset = |height: i32| {
                let (width, source_height) = self.source_size.get().unwrap_or((16, 9));
                (
                    even(height as f64 * width as f64 / source_height as f64),
                    height,
                )
            };
            let size = match self.combo_row_resolution.selected() {
                1 => Some(preset(1080)),
                2 => Some(preset(720)),
                3 => Some((
                    even(self.spin_row_width.value()),
                    even(self.spin_row_height.value()),
                )),
                _ => None,
            };
            ExportSettings { fps, size }
        }

        fn export_edit(&self) {
            self.video_preview.pause();
            let input_path = self.input_path.borrow().clone().unwrap_or_default();
            let extension = input_path
                .extension()
                .and_then(OsStr::to_str)
                .filter(|extension| matches!(*extension, "webm" | "mkv"))
                .unwrap_or("mp4");
            let name = format!(
                "{}{}.{extension}",
                input_path.file_stem().and_then(OsStr::to_str).unwrap_or(""),
                // Translators: appended to the file name of an exported edit, e.g.
                // "my video (edited).mp4".
                gettext(" (edited)"),
            );

            let file_dialog = gtk::FileDialog::builder()
                .modal(true)
                .initial_name(name)
                .build();
            if let Some(parent) = input_path.parent().filter(|p| !p.as_os_str().is_empty()) {
                file_dialog.set_initial_folder(Some(&gio::File::for_path(parent)));
            }

            let obj = self.obj().clone();
            glib::MainContext::default().spawn_local(async move {
                match file_dialog.save_future(Some(&obj)).await {
                    Ok(file) => match file.path() {
                        Some(path) => obj.imp().render_edit(path),
                        None => obj.imp().show_error(&gettext(
                            // Translators: error dialog text.
                            "Quick Video Editor can only operate on local files. \
Please choose another file.",
                        )),
                    },
                    Err(err) => {
                        if !err.matches(gtk::DialogError::Dismissed) {
                            warn!("file dialog error: {err:?}");
                        }
                    }
                }
            });
        }

        fn render_edit(&self, path: PathBuf) {
            let progress = gtk::ProgressBar::builder().show_text(true).build();
            let dialog = adw::AlertDialog::builder()
                // Translators: dialog heading while the edited video is exported.
                .heading(gettext("Exporting…"))
                .extra_child(&progress)
                .build();
            // Translators: export dialog button.
            dialog.add_response("cancel", &gettext("_Cancel"));

            let engine = self.video_preview.engine();
            let obj = self.obj().clone();
            // The engine's pipeline renders instead of previewing; keep the frame on screen.
            self.video_preview.set_preview_frozen(true);
            let result = engine.render(&path, self.export_settings(), {
                let dialog = dialog.clone();
                let path = path.clone();
                move |event| match event {
                    RenderEvent::Progress(fraction) => progress.set_fraction(fraction),
                    RenderEvent::Done => {
                        obj.imp().video_preview.set_preview_frozen(false);
                        dialog.close();
                        obj.imp().show_saved_toast(path.clone());
                    }
                    RenderEvent::Failed(message) => {
                        obj.imp().video_preview.set_preview_frozen(false);
                        dialog.close();
                        if message != "cancelled" {
                            warn!("export failed: {message}");
                            obj.imp().show_error(&gettext_f(
                                // Translators: error dialog text; the placeholder is the error.
                                "Could not export the video: {}",
                                &[&message],
                            ));
                        }
                    }
                }
            });
            if let Err(err) = result {
                self.video_preview.set_preview_frozen(false);
                self.show_error(&err);
                return;
            }

            dialog.connect_response(None, move |_, _| engine.cancel_render());
            dialog.present(Some(&*self.obj()));
        }

        fn show_saved_toast(&self, output_path: PathBuf) {
            let file_name = output_path
                .file_name()
                .map(|file_name| file_name.to_string_lossy().into_owned())
                .unwrap_or_else(|| output_path.to_string_lossy().into_owned());
            let toast = adw::Toast::new(&gettext_f(
                // Translators: text on the toast after trimming was done.
                // The placeholder is the video filename.
                "{} has been saved",
                &[&file_name],
            ));
            // Translators: text on the button of the toast after trimming was done to show the
            // output file in the file manager.
            toast.set_button_label(Some(&gettext("Show in Files")));
            toast.set_action_name(Some("toast.show-in-files"));
            toast.set_action_target(Some(&output_path.into_os_string().into_vec()));

            if self.stack_video_preview.visible_child_name().as_deref() == Some("page_error") {
                self.overlay_error_page.add_toast(toast);
            } else {
                self.video_preview.overlay().add_toast(toast);
            }
        }

        fn verify_and_trim(&self) {
            if self.video_preview.is_editor() {
                self.export_edit();
                return;
            }

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
            let music = self.video_preview.music();

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
                                    "Quick Video Editor can only operate on local files. \
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
                imp.do_trim(
                    &input_path,
                    output_path,
                    no_audio,
                    reencode,
                    music,
                    start,
                    end,
                );
            };

            glib::MainContext::default().spawn_local(future);
        }

        fn do_trim(
            &self,
            input_path: &Path,
            output_path: PathBuf,
            no_audio: bool,
            reencode: bool,
            music: Option<(PathBuf, i64, bool, f64)>,
            start: glib::GString,
            end: glib::GString,
        ) {
            let obj = self.obj().clone();

            debug!("output path: {:?}", output_path);

            let speed = self.speed.get();
            let video_filters = self.export_settings().ffmpeg_filters();
            let filter = if music.is_some() || speed != 1. || !video_filters.is_empty() {
                parse::timestamp(&start)
                    .zip(parse::timestamp(&end))
                    .map(|(start, end)| {
                        FilterArgs::new(
                            speed,
                            &video_filters,
                            music,
                            start,
                            end,
                            self.has_audio.get() && !no_audio,
                            &output_path,
                        )
                    })
            } else {
                None
            };

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
                // GoPro recordings include data streams with "none" tag which FFmpeg fails to process.
                // It fails to even simply copy them over, so I'm assuming this is an FFmpeg bug and
                // disabling data stream copying altogether as a workaround.
                "-dn".as_ref(),
                // Without reencoding, the output video can only start from a keyframe. We expect
                // the -ss argument placed before -i to make the output video start from the
                // earliest keyframe before the starting timestamp.
                //
                // However, when outputting .mp4, FFmpeg will by default use negative timestamps to
                // make the video start at the start timestamp. This is problematic because frames
                // from the keyframe to the start timestamp are still present in the video, just not
                // shown by players. To make matters worse, some players (VLC, Firefox) ignore
                // negative timestamps and show those starting frames. If the user has anything
                // sensitive there, they may not even realize they're leaking it.
                //
                // The following -avoid_negative_ts make_zero flag makes FFmpeg shift negative
                // timestamps forward so the video starts at zero. It makes the output video play
                // from the starting keyframe in all players, preventing leakage (it will be clearly
                // visible to the user).
                "-avoid_negative_ts".as_ref(),
                "make_zero".as_ref(),
                "-y".as_ref(),
            ]
            .to_vec();
            if let Some(filter) = &filter {
                args.extend(filter.args.iter().map(OsString::as_os_str));
                if !filter.reencode_video && !reencode {
                    args.push("-c:v".as_ref());
                    args.push("copy".as_ref());
                }
            } else {
                // By default FFmpeg selects only a single ("best") stream of each type. We'd rather
                // include all of them, however. This also fixes our trimmed down FFmpeg not including
                // the subtitle track by default.
                args.push("-map".as_ref());
                args.push("0".as_ref());
                if !reencode {
                    args.push("-c".as_ref());
                    args.push("copy".as_ref());
                }
            }
            if reencode
                && !filter.as_ref().is_some_and(|filter| filter.reencode_video)
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
            if no_audio && filter.is_none() {
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
                                    obj.imp().show_saved_toast(output_path);
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
                        "Quick Video Editor can only operate on local files. \
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
                                        .inspect(|name| debug!("audio codec: {}", name))
                                        .peekable();

                                    imp.has_audio.set(audio_formats.peek().is_some());

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

                                    let size = streams
                                        .members()
                                        .find(|stream| {
                                            stream["codec_type"].as_str() == Some("video")
                                        })
                                        .and_then(|stream| {
                                            Some((
                                                stream["width"].as_i32()?,
                                                stream["height"].as_i32()?,
                                            ))
                                        });
                                    if let Some((width, height)) = size {
                                        imp.source_size.set(Some((width, height)));
                                        // Suggest the source size, unless a custom one was set.
                                        if imp.combo_row_resolution.selected() != 3 {
                                            imp.spin_row_width.set_value(width.into());
                                            imp.spin_row_height.set_value(height.into());
                                        }
                                    }

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
            VtKnob::ensure_type();
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
                let resource_path = "/io/github/jjolmo/QuickVideoEditor/\
                                     io.github.jjolmo.QuickVideoEditor.metainfo.xml";
                let about_window = adw::AboutDialog::from_appdata(resource_path, Some("0.1.0"));
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
                gettext("This release improves the default output file naming and adds command-line options.");
                gettext("The default output filename now includes start and end timestamps.");
                gettext("Added command-line flags for start and end timestamps, precise trimming, and removing audio.");
                gettext("Increased the default window size to make the video 960×540.");
                gettext("Updated to the GNOME 50 platform.");
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
            klass.install_action("win.split", None, |window, _, _| {
                window.imp().video_preview.split_at_playhead();
            });
            klass.install_action("win.delete-segment", None, |window, _, _| {
                window.imp().video_preview.delete_selected_segment();
            });
            klass.install_action("win.undo", None, |window, _, _| {
                let engine = window.imp().video_preview.engine();
                if engine.is_editor() {
                    engine.undo();
                }
            });
            klass.install_action("win.redo", None, |window, _, _| {
                let engine = window.imp().video_preview.engine();
                if engine.is_editor() {
                    engine.redo();
                }
            });
            klass.add_binding_action(Key::z, ModifierType::CONTROL_MASK, "win.undo");
            klass.add_binding_action(
                Key::Z,
                ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK,
                "win.redo",
            );
            klass.add_binding_action(Key::y, ModifierType::CONTROL_MASK, "win.redo");
            klass.add_binding_action(Key::s, ModifierType::empty(), "win.split");
            klass.add_binding_action(Key::S, ModifierType::SHIFT_MASK, "win.split");
            for key in [Key::Delete, Key::KP_Delete, Key::BackSpace] {
                klass.add_binding_action(key, ModifierType::empty(), "win.delete-segment");
            }
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
            self.toolbar_view.remove(&*self.box_editor);
            self.toolbar_view.add_bottom_bar(&*self.box_editor);

            self.toggle_mode.connect_active_name_notify({
                let obj = obj.downgrade();
                move |_| {
                    let obj = obj.upgrade().unwrap();
                    obj.imp().on_mode_toggled();
                }
            });
            self.button_add_video.connect_clicked({
                let obj = obj.downgrade();
                move |_| {
                    let obj = obj.upgrade().unwrap();
                    obj.imp().show_open_dialog();
                }
            });
            self.button_split.connect_clicked({
                let obj = obj.downgrade();
                move |_| {
                    let obj = obj.upgrade().unwrap();
                    obj.imp().video_preview.split_at_playhead();
                }
            });
            self.button_delete_segment.connect_clicked({
                let obj = obj.downgrade();
                move |_| {
                    let obj = obj.upgrade().unwrap();
                    obj.imp().video_preview.delete_selected_segment();
                }
            });
            self.check_end_fade.connect_active_notify({
                let obj = obj.downgrade();
                move |check| {
                    let obj = obj.upgrade().unwrap();
                    obj.imp().video_preview.set_end_fade(check.is_active());
                }
            });
            self.video_preview
                .engine()
                .connect_local("timeline-changed", false, {
                    let obj = obj.downgrade();
                    move |_| {
                        let obj = obj.upgrade().unwrap();
                        obj.imp().on_edit_changed();
                        None
                    }
                });
            self.video_preview
                .editor_timeline()
                .connect_local("selection-changed", false, {
                    let obj = obj.downgrade();
                    move |_| {
                        let obj = obj.upgrade().unwrap();
                        obj.imp().on_edit_changed();
                        None
                    }
                });

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

                        if let Some(speed) = imp.pending_speed.take() {
                            imp.on_speed_changed(speed);
                        }
                        for file in imp.pending_videos.take() {
                            imp.add_video(file);
                        }
                        if let Some(music) = imp.pending_music.take() {
                            imp.video_preview.set_music(music);
                        }
                        if let Some(path) = imp.pending_export.take() {
                            imp.export_headless(path);
                        }

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

                        if obj.imp().input_path.borrow().is_some() && is_audio_file(&file) {
                            obj.imp().set_music(file);
                            return true;
                        }

                        if obj.imp().input_path.borrow().is_some() {
                            obj.imp().add_video(file);
                            return true;
                        }

                        obj.open(file);
                        return true;
                    }

                    false
                }
            });
            self.stack.add_controller(drop_target);

            self.speed.set(1.);
            self.knob_speed.connect_local("value-changed", false, {
                let obj = obj.downgrade();
                move |args| {
                    let speed = args[1].get::<f64>().unwrap();
                    let obj = obj.upgrade().unwrap();
                    obj.imp().on_speed_changed(speed);
                    None
                }
            });

            self.combo_row_fps.connect_selected_notify({
                let obj = obj.downgrade();
                move |row| {
                    let obj = obj.upgrade().unwrap();
                    obj.imp().spin_row_fps.set_visible(row.selected() == 6);
                }
            });
            self.switch_row_social_snap.connect_active_notify({
                let obj = obj.downgrade();
                move |row| {
                    let obj = obj.upgrade().unwrap();
                    obj.imp().video_preview.set_social_snap(row.is_active());
                }
            });

            self.combo_row_resolution.connect_selected_notify({
                let obj = obj.downgrade();
                move |row| {
                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();
                    let custom = row.selected() == 3;
                    imp.spin_row_width.set_visible(custom);
                    imp.spin_row_height.set_visible(custom);
                }
            });

            self.switch_row_remove_audio.connect_active_notify({
                let obj = obj.downgrade();
                move |row| {
                    let obj = obj.upgrade().unwrap();
                    obj.imp().video_preview.set_video_muted(row.is_active());
                }
            });

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

    /// Only local files are accepted, since ffmpeg needs a path.
    fn is_audio_file(file: &gio::File) -> bool {
        let Some(path) = file.path() else {
            return false;
        };
        let (content_type, _) = gio::content_type_guess(Some(&path), None);
        gio::content_type_is_a(&content_type, "audio/*")
    }

    /// Extra FFmpeg arguments for a speed change and/or music laid over the trimmed video.
    pub(super) struct FilterArgs {
        pub(super) args: Vec<OsString>,
        /// Whether the video goes through filters and is encoded rather than copied.
        pub(super) reencode_video: bool,
    }

    impl FilterArgs {
        /// `music` holds the path, the video timestamp in microseconds where the music starts and
        /// whether to fade it out; `start` and `end` are the trim bounds in milliseconds.
        pub(super) fn new(
            speed: f64,
            video_filters: &[String],
            music: Option<(PathBuf, i64, bool, f64)>,
            start: u32,
            end: u32,
            keep_video_audio: bool,
            output_path: &Path,
        ) -> Self {
            let output_duration = f64::from(end - start) / 1000. / speed;
            let extension = output_path.extension().and_then(OsStr::to_str);

            let mut args: Vec<OsString> = Vec::new();
            let mut filters: Vec<String> = Vec::new();
            let mut maps: Vec<String> = Vec::new();

            let mut video_chain: Vec<String> = Vec::new();
            if speed != 1. {
                video_chain.push(format!("setpts=PTS/{speed:.6}"));
            }
            video_chain.extend(video_filters.iter().cloned());
            let reencode_video = !video_chain.is_empty();
            if reencode_video {
                filters.push(format!("[0:v:0]{}[vout]", video_chain.join(",")));
                maps.push("[vout]".to_owned());
            } else {
                maps.push("0:v".to_owned());
            }

            let video_audio = keep_video_audio.then(|| {
                if speed != 1. {
                    filters.push(format!("[0:a:0]{}[vaudio]", atempo_chain(speed)));
                    "[vaudio]"
                } else {
                    "[0:a:0]"
                }
            });

            let music = music.map(|(path, offset, fade_out, volume)| {
                // Where the music starts in the output. The music keeps its own speed.
                let relative = (offset - i64::from(start) * 1000) as f64 / speed;
                if relative < 0. {
                    args.push("-ss".into());
                    args.push(format!("{:.3}", -relative / 1_000_000.).into());
                }
                args.push("-i".into());
                args.push(path.into());

                let mut chain = String::from("[1:a]");
                if relative > 0. {
                    chain.push_str(&format!(
                        "adelay=delays={}:all=1,",
                        (relative / 1000.) as i64
                    ));
                }
                chain.push_str(&format!("atrim=end={output_duration:.3}"));
                if volume != 1. {
                    chain.push_str(&format!(",volume={volume:.3}"));
                }
                if fade_out {
                    let fade = (FADE_OUT_DURATION as f64 / 1_000_000.).min(output_duration);
                    chain.push_str(&format!(
                        ",afade=t=out:st={:.3}:d={fade:.3}",
                        output_duration - fade
                    ));
                }
                filters.push(format!("{chain}[music]"));
                "[music]"
            });

            let audio = match (video_audio, music) {
                (Some(video_audio), Some(music)) => {
                    filters.push(format!(
                        "{video_audio}{music}amix=inputs=2:duration=first:\
dropout_transition=0:normalize=0[aout]"
                    ));
                    Some("[aout]")
                }
                (video_audio, music) => music.or(video_audio),
            };
            match audio {
                // Unfiltered input streams are mapped without brackets.
                Some(label) if label.starts_with("[0:") => {
                    maps.push(label.trim_matches(['[', ']']).to_owned())
                }
                Some(label) => maps.push(label.to_owned()),
                None => {}
            }

            if !filters.is_empty() {
                args.push("-filter_complex".into());
                args.push(filters.join(";").into());
            }
            for map in maps {
                args.push("-map".into());
                args.push(map.into());
            }

            if reencode_video {
                let video_codec: &[&str] = match extension {
                    Some("webm") => &[
                        "-c:v",
                        "libvpx-vp9",
                        "-crf",
                        "32",
                        "-b:v",
                        "0",
                        "-row-mt",
                        "1",
                    ],
                    _ => &[
                        "-c:v", "libx264", "-preset", "faster", "-crf", "17", "-pix_fmt", "yuv420p",
                    ],
                };
                args.extend(video_codec.iter().map(OsString::from));
            }

            if audio.is_some() {
                let audio_codec = match extension {
                    Some("webm" | "ogg" | "ogv") => "libopus",
                    _ => "aac",
                };
                args.extend(["-c:a", audio_codec, "-b:a", "192k"].map(OsString::from));
            } else {
                args.push("-an".into());
            }

            Self {
                args,
                reencode_video,
            }
        }
    }

    /// `atempo` only accepts factors between 0.5 and 2, so larger changes are chained.
    pub(super) fn atempo_chain(speed: f64) -> String {
        let mut remaining = speed;
        let mut parts = Vec::new();
        while remaining > 2. {
            parts.push("atempo=2".to_owned());
            remaining /= 2.;
        }
        while remaining < 0.5 {
            parts.push("atempo=0.5".to_owned());
            remaining /= 0.5;
        }
        parts.push(format!("atempo={remaining:.6}"));
        parts.join(",")
    }

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

    pub fn set_music(&self, file: gio::File) {
        self.imp().set_music(file);
    }

    /// Selects the export frame rate and size, as the options menu does.
    pub fn set_export_options(&self, fps: Option<i32>, size: Option<(i32, i32)>) {
        self.imp().set_export_options(fps, size);
    }

    /// Sets the Trimmer speed once the video is loaded.
    pub fn set_speed(&self, speed: f64) {
        self.imp().queue_speed(speed);
    }

    /// Exports the edit to `file` and quits once the videos are loaded.
    pub fn export_when_ready(&self, file: gio::File) {
        self.imp().queue_export(file);
    }

    /// Adds a video to the edit, once the first video is ready.
    pub fn add_video(&self, file: gio::File) {
        self.imp().queue_video(file);
    }

    pub fn open(&self, file: gio::File) {
        self.imp().open(file);
    }
}

#[cfg(test)]
mod tests {
    use super::imp::{atempo_chain, FilterArgs};
    use std::path::{Path, PathBuf};

    fn args(filter: &FilterArgs) -> Vec<String> {
        filter
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn export_settings_scale_and_resample_in_ffmpeg() {
        let settings = crate::engine::ExportSettings {
            fps: Some((30, 1)),
            size: Some((1280, 720)),
        };
        let filters = settings.ffmpeg_filters();
        let filter = FilterArgs::new(1., &filters, None, 0, 4000, true, Path::new("out.mp4"));
        let args = args(&filter);
        let graph = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        assert_eq!(
            graph,
            "[0:v:0]scale=1280:720:force_original_aspect_ratio=decrease,\
             pad=1280:720:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=30/1[vout]"
        );
        assert!(filter.reencode_video);
        assert!(args.windows(2).any(|w| w == ["-c:v", "libx264"]));
    }

    #[test]
    fn atempo_chains_factors_out_of_range() {
        assert_eq!(atempo_chain(1.5), "atempo=1.500000");
        assert_eq!(atempo_chain(4.), "atempo=2,atempo=2.000000");
        assert_eq!(atempo_chain(0.25), "atempo=0.5,atempo=0.500000");
    }

    #[test]
    fn speed_without_music_reencodes_video_and_audio() {
        let filter = FilterArgs::new(2., &[], None, 2000, 7000, true, Path::new("out.mp4"));
        let args = args(&filter);
        let graph = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        assert_eq!(
            graph,
            "[0:v:0]setpts=PTS/2.000000[vout];[0:a:0]atempo=2.000000[vaudio]"
        );
        assert!(args.windows(2).any(|w| w == ["-map", "[vout]"]));
        assert!(args.windows(2).any(|w| w == ["-map", "[vaudio]"]));
        assert!(args.windows(2).any(|w| w == ["-c:v", "libx264"]));
    }

    #[test]
    fn music_offset_and_fade_follow_the_output_timeline() {
        // Music starts 1 s of source after the trim start; at 50% speed that is 2 s of output.
        let music = Some((PathBuf::from("m.mp3"), 3_000_000, true, 1.));
        let filter = FilterArgs::new(0.5, &[], music, 2000, 7000, false, Path::new("out.mkv"));
        let args = args(&filter);
        let graph = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        assert_eq!(
            graph,
            "[0:v:0]setpts=PTS/0.500000[vout];\
             [1:a]adelay=delays=2000:all=1,atrim=end=10.000,afade=t=out:st=9.000:d=1.000[music]"
        );
        assert!(args.windows(2).any(|w| w == ["-map", "[music]"]));
    }

    #[test]
    fn music_at_normal_speed_keeps_video_stream_mapping() {
        let music = Some((PathBuf::from("m.mp3"), 500_000, false, 0.5));
        let filter = FilterArgs::new(1., &[], music, 2000, 7000, true, Path::new("out.mp4"));
        let args = args(&filter);
        assert!(args.windows(2).any(|w| w == ["-ss", "1.500"]));
        let graph = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        assert!(graph.starts_with("[1:a]atrim=end=5.000,volume=0.500[music];"));
        assert!(args.windows(2).any(|w| w == ["-map", "0:v"]));
        assert!(args.windows(2).any(|w| w == ["-map", "[aout]"]));
        assert!(!args.iter().any(|a| a == "libx264"));
    }
}
