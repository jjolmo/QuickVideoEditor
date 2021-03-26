use gtk::{gio, glib};

use crate::config;

mod imp {
    use super::*;
    use crate::{config::G_LOG_DOMAIN, window::VtWindow};
    use gettextrs::*;
    use glib::{debug, prelude::*};
    use gtk::{gdk, prelude::*, subclass::prelude::*};
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
        type ParentType = gtk::Application;
    }

    impl ObjectImpl for VtApplication {
        fn constructed(&self, self_: &Self::Type) {
            self.parent_constructed(self_);

            self_.add_main_option(
                "output",
                glib::Char::new('o').unwrap(),
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
        fn activate(&self, self_: &Self::Type) {
            let window = VtWindow::new(self_.upcast_ref(), self.output_file.take());

            if let Some(file) = self.input_file.take() {
                if file.get_path().is_none() {
                    let dialog = gtk::MessageDialogBuilder::new()
                        // Translators: fatal error message dialog title.
                        .text(&gettext("Fatal Error"))
                        // Translators: error dialog text.
                        .secondary_text(&gettext("Video Trimmer can only operate on local files."))
                        .message_type(gtk::MessageType::Error)
                        .buttons(gtk::ButtonsType::Ok)
                        .build();
                    dialog.connect_response({
                        let app = self_.clone();
                        move |_, _| {
                            app.quit();
                        }
                    });
                    dialog.show();
                    return;
                }

                window.open(file);
            }

            window.show();
        }

        fn open(&self, self_: &Self::Type, files: &[gio::File], _hint: &str) {
            debug!(
                "open: {:?}",
                files
                    .iter()
                    .map(|x| x.get_uri().into())
                    .collect::<Vec<String>>()
            );

            self.input_file.set(Some(files[0].clone()));

            self_.activate();
        }

        fn startup(&self, self_: &Self::Type) {
            self.parent_startup(self_);

            let provider = gtk::CssProvider::new();
            provider.load_from_resource("/org/gnome/gitlab/YaLTeR/VideoTrimmer/style.css");
            gtk::StyleContext::add_provider_for_display(
                &gdk::Display::get_default().unwrap(),
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );

            let action = gio::SimpleAction::new("quit", None);
            action.connect_activate({
                let app = self_.downgrade();
                move |_, _| {
                    let app = app.upgrade().unwrap();
                    app.quit();
                }
            });
            self_.add_action(&action);
            self_.set_accels_for_action("app.quit", &["<Ctrl>q"]);
        }

        fn handle_local_options(&self, _self_: &Self::Type, options: &glib::VariantDict) -> i32 {
            self.output_file.set(
                options
                    .lookup_value("output", None)
                    .and_then(|x| x.get::<String>())
                    .map(gio::File::new_for_path),
            );

            -1
        }
    }

    impl GtkApplicationImpl for VtApplication {}
}

glib::wrapper! {
    pub struct VtApplication(ObjectSubclass<imp::VtApplication>)
        @extends gtk::Application, gio::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl VtApplication {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let flags = gio::ApplicationFlags::NON_UNIQUE | gio::ApplicationFlags::HANDLES_OPEN;
        glib::Object::new(&[("application-id", &config::APP_ID), ("flags", &flags)]).unwrap()
    }
}
