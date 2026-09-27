use gtk::{gio, glib};

use crate::config;

mod imp {
    use super::*;
    use crate::{config::G_LOG_DOMAIN, window::VtWindow};
    use adw::{prelude::AdwApplicationExt, subclass::prelude::*};
    use gettextrs::*;
    use glib::{debug, prelude::*};
    use gtk::prelude::*;
    use std::{
        cell::{Cell, RefCell},
        ops::ControlFlow,
    };

    #[derive(Default)]
    pub struct VtApplication {
        input_file: Cell<Option<gio::File>>,
        /// Further videos from the command line, added to the edit.
        extra_files: RefCell<Vec<gio::File>>,
        output_file: Cell<Option<gio::File>>,
        music_file: Cell<Option<gio::File>>,
        export_file: Cell<Option<gio::File>>,
        start: RefCell<Option<String>>,
        end: RefCell<Option<String>>,
        precise: Cell<bool>,
        remove_audio: Cell<bool>,
        speed: Cell<Option<f64>>,
        export_fps: Cell<Option<i32>>,
        export_size: RefCell<Option<String>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for VtApplication {
        const NAME: &'static str = "VtApplication";
        type Type = super::VtApplication;
        type ParentType = adw::Application;
    }

    impl ObjectImpl for VtApplication {
        fn constructed(&self) {
            let obj = self.obj();
            self.parent_constructed();

            // Translators: shown in --help usage line as: quick-video-editor [OPTION…] [VIDEO]
            obj.set_option_context_parameter_string(Some(&gettext("[VIDEO…]")));

            obj.add_main_option(
                "output",
                glib::Char::from(b'o'),
                glib::OptionFlags::NONE,
                glib::OptionArg::String, // Can't extract filenames from a VariantDict yet.
                // Translators: --output commandline option description.
                &gettext("Output file path"),
                // Translators: --output commandline option arg description.
                Some(&gettext("PATH")),
            );

            obj.add_main_option(
                "music",
                glib::Char::from(b'm'),
                glib::OptionFlags::NONE,
                glib::OptionArg::String,
                // Translators: --music commandline option description.
                &gettext("Music to lay over the video"),
                // Translators: --music commandline option arg description.
                Some(&gettext("PATH")),
            );

            obj.add_main_option(
                "export",
                glib::Char::from(b'x'),
                glib::OptionFlags::NONE,
                glib::OptionArg::String,
                // Translators: --export commandline option description.
                &gettext("Export the videos, edited together, to PATH and quit"),
                // Translators: --export commandline option arg description.
                Some(&gettext("PATH")),
            );

            obj.add_main_option(
                "start",
                glib::Char::from(b's'),
                glib::OptionFlags::NONE,
                glib::OptionArg::String,
                // Translators: --start commandline option description.
                &gettext("Start timestamp"),
                // Translators: --start commandline option arg description.
                Some(&gettext("TIMESTAMP")),
            );

            obj.add_main_option(
                "end",
                glib::Char::from(b'e'),
                glib::OptionFlags::NONE,
                glib::OptionArg::String,
                // Translators: --end commandline option description.
                &gettext("End timestamp"),
                Some(&gettext("TIMESTAMP")),
            );

            obj.add_main_option(
                "precise",
                glib::Char::from(b'p'),
                glib::OptionFlags::NONE,
                glib::OptionArg::None,
                // Translators: --precise commandline option description.
                &gettext("Precise trim (re-encode)"),
                None,
            );

            obj.add_main_option(
                "speed",
                glib::Char::from(0u8),
                glib::OptionFlags::NONE,
                glib::OptionArg::Double,
                // Translators: --speed commandline option description.
                &gettext("Speed of the trimmed video, 1 being normal (0.1 to 10)"),
                // Translators: --speed commandline option arg description.
                Some(&gettext("FACTOR")),
            );

            obj.add_main_option(
                "fps",
                glib::Char::from(0u8),
                glib::OptionFlags::NONE,
                glib::OptionArg::Int,
                // Translators: --fps commandline option description.
                &gettext("Frame rate of the export"),
                Some("FPS"),
            );

            obj.add_main_option(
                "size",
                glib::Char::from(0u8),
                glib::OptionFlags::NONE,
                glib::OptionArg::String,
                // Translators: --size commandline option description.
                &gettext("Size of the export, e.g. 1280x720"),
                Some("WIDTHxHEIGHT"),
            );

            obj.add_main_option(
                "remove-audio",
                glib::Char::from(b'r'),
                glib::OptionFlags::NONE,
                glib::OptionArg::None,
                // Translators: --remove-audio commandline option description.
                &gettext("Remove audio"),
                None,
            );
        }
    }

    impl ApplicationImpl for VtApplication {
        fn activate(&self) {
            let window = VtWindow::new(self.obj().upcast_ref(), self.output_file.take());

            if let Some(start) = self.start.take() {
                window.set_start(&start);
            }
            if let Some(end) = self.end.take() {
                window.set_end(&end);
            }
            if self.precise.get() {
                window.set_precise(true);
            }
            if self.remove_audio.get() {
                window.set_remove_audio(true);
            }
            if let Some(speed) = self.speed.take() {
                window.set_speed(speed);
            }
            let size = self.export_size.take().and_then(|size| {
                let (width, height) = size.split_once('x')?;
                Some((width.parse().ok()?, height.parse().ok()?))
            });
            window.set_export_options(self.export_fps.take(), size);
            if let Some(music) = self.music_file.take() {
                window.set_music(music);
            }
            if let Some(export) = self.export_file.take() {
                window.export_when_ready(export);
            }

            if let Some(file) = self.input_file.take() {
                window.open(file);
                for file in self.extra_files.take() {
                    window.add_video(file);
                }
            } else {
                window.present();
            }
        }

        fn open(&self, files: &[gio::File], _hint: &str) {
            debug!(
                "open: {:?}",
                files
                    .iter()
                    .map(|x| x.uri().into())
                    .collect::<Vec<String>>()
            );

            self.input_file.set(Some(files[0].clone()));
            self.extra_files.replace(files[1..].to_vec());

            self.obj().activate();
        }

        fn startup(&self) {
            let obj = self.obj();
            self.parent_startup();

            gtk::Window::set_default_icon_name(config::APP_ID);

            obj.style_manager()
                .set_color_scheme(adw::ColorScheme::PreferDark);

            let action = gio::SimpleAction::new("quit", None);
            action.connect_activate({
                let app = obj.downgrade();
                move |_, _| {
                    let app = app.upgrade().unwrap();
                    app.quit();
                }
            });
            obj.add_action(&action);
            obj.set_accels_for_action("app.quit", &["<primary>q"]);

            let action = gio::SimpleAction::new("new-window", None);
            action.connect_activate({
                let app = obj.downgrade();
                move |_, _| {
                    let app = app.upgrade().unwrap();
                    let window = VtWindow::new(app.upcast_ref(), None);

                    // Put it in a new window group so modal dialogs don't block other windows.
                    let group = gtk::WindowGroup::new();
                    group.add_window(&window);

                    window.present();
                }
            });
            obj.add_action(&action);
            obj.set_accels_for_action("app.new-window", &["<primary>n"]);

            obj.set_accels_for_action("win.play-pause", &["p", "k", "space", "<ctrl>space"]);
            obj.set_accels_for_action("win.close", &["<ctrl>w"]);
            obj.set_accels_for_action("win.trim", &["<ctrl>s"]);
            obj.set_accels_for_action("win.open", &["<ctrl>o"]);
            obj.set_accels_for_action("win.set-start-as-position", &["i"]);
            obj.set_accels_for_action("win.set-end-as-position", &["o"]);
        }

        fn handle_local_options(&self, options: &glib::VariantDict) -> ControlFlow<glib::ExitCode> {
            self.output_file.set(
                options
                    .lookup_value("output", None)
                    .and_then(|x| x.get::<String>())
                    .map(gio::File::for_path),
            );

            self.export_file.set(
                options
                    .lookup_value("export", None)
                    .and_then(|x| x.get::<String>())
                    .map(gio::File::for_path),
            );

            self.music_file.set(
                options
                    .lookup_value("music", None)
                    .and_then(|x| x.get::<String>())
                    .map(gio::File::for_path),
            );

            *self.start.borrow_mut() = options
                .lookup_value("start", None)
                .and_then(|x| x.get::<String>());

            *self.end.borrow_mut() = options
                .lookup_value("end", None)
                .and_then(|x| x.get::<String>());

            self.precise.set(options.contains("precise"));

            self.remove_audio.set(options.contains("remove-audio"));

            self.export_fps.set(
                options
                    .lookup_value("fps", None)
                    .and_then(|x| x.get::<i32>()),
            );
            *self.export_size.borrow_mut() = options
                .lookup_value("size", None)
                .and_then(|x| x.get::<String>());

            self.speed.set(
                options
                    .lookup_value("speed", None)
                    .and_then(|x| x.get::<f64>()),
            );

            self.parent_handle_local_options(options)
        }
    }

    impl GtkApplicationImpl for VtApplication {}
    impl AdwApplicationImpl for VtApplication {}
}

glib::wrapper! {
    pub struct VtApplication(ObjectSubclass<imp::VtApplication>)
        @extends adw::Application, gtk::Application, gio::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl VtApplication {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let flags = gio::ApplicationFlags::NON_UNIQUE | gio::ApplicationFlags::HANDLES_OPEN;
        glib::Object::builder()
            .property("application-id", config::APP_ID)
            .property("version", config::VERSION)
            .property("flags", flags)
            .property("resource-base-path", "/io/github/jjolmo/QuickVideoEditor")
            .build()
    }
}
