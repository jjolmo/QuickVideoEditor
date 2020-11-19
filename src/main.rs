#[macro_use]
extern crate glib;
extern crate gstreamer as gst;

use std::{cell::Cell, rc::Rc};

use gettextrs::*;
use gio::prelude::*;
use gtk::prelude::*;

mod config;
mod parse;
mod video_preview;
mod window;
use crate::window::VtWindow;

fn fatal_error(text: &str) {
    let dialog = gtk::MessageDialogBuilder::new()
        // Translators: fatal error message dialog title.
        .text(&gettext("Fatal Error"))
        .secondary_text(text)
        .message_type(gtk::MessageType::Error)
        .buttons(gtk::ButtonsType::Ok)
        .build();
    dialog.run();
}

fn main() {
    // This is required for doing GStreamer pipeline.set_state(Playing) asynchronously. Otherwise,
    // on X11 the process aborts with an xcb assertion failure.
    //
    // TODO: change this cfg to gdk_backend = "x11" when this is released:
    // https://github.com/gtk-rs/sys/pull/167
    #[cfg(target_os = "linux")]
    unsafe {
        #[link(name = "X11")]
        extern "C" {
            fn XInitThreads() -> std::os::raw::c_int;
        }

        XInitThreads();
    }

    g_message!(
        config::LOG_DOMAIN,
        "Video Trimmer version {}",
        config::VERSION
    );

    gst::init().unwrap();
    gtk::init().unwrap_or_else(|_| panic!("Failed to initialize GTK."));

    setlocale(LocaleCategory::LcAll, "");
    bindtextdomain("video-trimmer", config::LOCALEDIR);
    textdomain("video-trimmer");

    glib::set_application_name(&format!(
        "{}{}",
        gettext("Video Trimmer"),
        config::NAME_SUFFIX
    ));

    let res = gio::Resource::load(config::PKGDATADIR.to_owned() + "/video-trimmer.gresource")
        .expect("Could not load resources");
    gio::resources_register(&res);

    let app = gtk::Application::new(
        Some(config::APP_ID),
        gio::ApplicationFlags::NON_UNIQUE | gio::ApplicationFlags::HANDLES_OPEN,
    )
    .unwrap();

    let file = Rc::new(Cell::new(None));
    app.connect_open(clone!(@weak file => move |app, files, _hint| {
        g_debug!(
            config::LOG_DOMAIN,
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
        "Output file path",
        Some("PATH"),
    );

    let output_file = Rc::new(Cell::new(None));
    app.connect_handle_local_options({
        let output_file = Rc::downgrade(&output_file);

        move |_, options| {
            output_file.upgrade().unwrap().set(
                options
                    .lookup_value("output", None)
                    .and_then(|x| x.get::<String>())
                    .map(|x| gio::File::new_for_path(x)),
            );
            -1
        }
    });

    app.connect_activate(move |app| {
        let file = file.replace(None);
        let output_file = output_file.replace(None);

        let window = VtWindow::new(app, output_file);
        if let Some(file) = file {
            if file.get_path().is_none() {
                // Translators: error dialog text.
                fatal_error(&gettext("Video Trimmer can only operate on local files."));
                app.quit();
                return;
            }

            window.open(file);
        }

        window.show_all();
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
