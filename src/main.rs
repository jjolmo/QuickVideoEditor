use std::{cell::Cell, rc::Rc};

use gettextrs::*;
use glib::{clone, debug, info, warn, GlibLogger, GlibLoggerDomain, GlibLoggerFormat};
use gtk::{gdk, gio, glib, prelude::*};

mod config;
use config::G_LOG_DOMAIN;
mod parse;
mod timeline;
mod video_preview;
mod window;
use crate::window::VtWindow;

fn main() {
    static GLIB_LOGGER: GlibLogger =
        GlibLogger::new(GlibLoggerFormat::LineAndFile, GlibLoggerDomain::CrateTarget);

    let _ = log::set_logger(&GLIB_LOGGER);
    log::set_max_level(log::LevelFilter::Debug);

    info!("Video Trimmer version {}", config::VERSION);

    gtk::init().unwrap_or_else(|_| panic!("Failed to initialize GTK."));

    setlocale(LocaleCategory::LcAll, "");
    if let Err(err) = bindtextdomain("video-trimmer", config::LOCALEDIR) {
        warn!("Error in bindtextdomain(): {}", err);
    }
    if let Err(err) = bind_textdomain_codeset("video-trimmer", "UTF-8") {
        warn!("Error in bind_textdomain_codeset(): {}", err);
    }
    if let Err(err) = textdomain("video-trimmer") {
        warn!("Error in textdomain(): {}", err);
    }

    glib::set_application_name(&format!(
        "{}{}",
        gettext("Video Trimmer"),
        config::NAME_SUFFIX
    ));

    let res = gio::Resource::load(config::PKGDATADIR.to_owned() + "/video-trimmer.gresource")
        .expect("Could not load resources");
    gio::resources_register(&res);

    // Make GTK aware of the custom widgets.
    let _ = window::VtWindow::static_type();

    let app = gtk::Application::new(
        Some(config::APP_ID),
        gio::ApplicationFlags::NON_UNIQUE | gio::ApplicationFlags::HANDLES_OPEN,
    )
    .unwrap();

    let file = Rc::new(Cell::new(None));
    app.connect_open(clone!(@weak file => move |app, files, _hint| {
        debug!(
            "open: {:?}",
            files
                .iter()
                .map(|x| x.get_uri().into())
                .collect::<Vec<String>>()
        );

        file.set(Some(files[0].clone()));

        app.activate();
    }));

    app.add_main_option(
        "output",
        glib::Char::new('o').unwrap(),
        glib::OptionFlags::NONE,
        glib::OptionArg::String, // Can't extract filenames from a VariantDict yet.
        // Translators: --output commandline option description.
        &gettext("Output file path"),
        // Translators: --output commandline option arg description.
        Some(&gettext("PATH")),
    );

    let output_file = Rc::new(Cell::new(None));
    app.connect_handle_local_options({
        let output_file = Rc::downgrade(&output_file);

        move |_, options| {
            output_file.upgrade().unwrap().set(
                options
                    .lookup_value("output", None)
                    .and_then(|x| x.get::<String>())
                    .map(gio::File::new_for_path),
            );
            -1
        }
    });

    app.connect_activate(move |app| {
        let file = file.replace(None);
        let output_file = output_file.replace(None);

        let provider = gtk::CssProvider::new();
        provider.load_from_resource("/org/gnome/gitlab/YaLTeR/VideoTrimmer/style.css");
        gtk::StyleContext::add_provider_for_display(
            &gdk::Display::get_default().unwrap(),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        let window = VtWindow::new(app, output_file);
        if let Some(file) = file {
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
                    let app = app.clone();
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
    });

    let action = gio::SimpleAction::new("quit", None);
    action.connect_activate({
        let app = app.downgrade();
        move |_, _| {
            let app = app.upgrade().unwrap();
            app.quit();
        }
    });
    app.add_action(&action);
    app.set_accels_for_action("app.quit", &["<Ctrl>q"]);

    let ret = app.run(&std::env::args().collect::<Vec<_>>());
    std::process::exit(ret);
}
