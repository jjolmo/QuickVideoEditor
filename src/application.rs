use gtk::{gio, glib};

use crate::config;

mod imp {
    use super::*;
    use crate::{config::G_LOG_DOMAIN, window::VtWindow};
    use adw::{prelude::AdwApplicationExt, subclass::prelude::*};
    use gettextrs::*;
    use glib::{debug, prelude::*};
    use gtk::prelude::*;
    use std::cell::Cell;

    #[derive(Default)]
    pub struct VtApplication {
        input_file: Cell<Option<gio::File>>,
        output_file: Cell<Option<gio::File>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for VtApplication {
        const NAME: &'static str = "VtApplication";
        type Type = super::VtApplication;
        type ParentType = adw::Application;
    }

    impl ObjectImpl for VtApplication {
        fn constructed(&self) {
            self.parent_constructed();

            self.obj().add_main_option(
                "output",
                glib::Char::from(b'o'),
                glib::OptionFlags::NONE,
                glib::OptionArg::String, // Can't extract filenames from a VariantDict yet.
                // Translators: --output commandline option description.
                &gettext("Output file path"),
                // Translators: --output commandline option arg description.
                Some(&gettext("PATH")),
            );
        }
    }

    impl ApplicationImpl for VtApplication {
        fn activate(&self) {
            let window = VtWindow::new(self.obj().upcast_ref(), self.output_file.take());

            if let Some(file) = self.input_file.take() {
                window.open(file);
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

            self.obj().activate();
        }

        fn startup(&self) {
            let obj = self.obj();
            self.parent_startup();

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
        }

        fn handle_local_options(&self, options: &glib::VariantDict) -> glib::ExitCode {
            self.output_file.set(
                options
                    .lookup_value("output", None)
                    .and_then(|x| x.get::<String>())
                    .map(gio::File::for_path),
            );

            glib::ExitCode::from(-1)
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
            .property("flags", flags)
            .property(
                "resource-base-path",
                "/org/gnome/gitlab/YaLTeR/VideoTrimmer",
            )
            .build()
    }
}
