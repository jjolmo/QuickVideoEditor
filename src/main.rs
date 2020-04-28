#[macro_use]
extern crate log;
#[macro_use]
extern crate glib;

use std::{cell::Cell, rc::Rc};

use gettextrs::*;
use gio::prelude::*;
use gtk::prelude::*;

mod config;
mod parse;
mod window;
use crate::window::VtWindow;

fn fatal_error(text: &str) {
    let dialog = gtk::MessageDialogBuilder::new()
        .text("Fatal Error")
        .secondary_text(text)
        .message_type(gtk::MessageType::Error)
        .buttons(gtk::ButtonsType::Ok)
        .build();
    dialog.run();
}

fn main() {
    env_logger::init();

    gtk::init().unwrap_or_else(|_| panic!("Failed to initialize GTK."));

    setlocale(LocaleCategory::LcAll, "");
    bindtextdomain("video-trimmer", config::LOCALEDIR);
    textdomain("video-trimmer");

    let res = gio::Resource::load(config::PKGDATADIR.to_owned() + "/video-trimmer.gresource")
        .expect("Could not load resources");
    gio::resources_register(&res);

    let app = gtk::Application::new(
        Some("org.gnome.gitlab.YaLTeR.VideoTrimmer"),
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

    app.connect_activate(move |app| {
        let file = file.replace(None);

        let window = VtWindow::new(app);
        if let Some(file) = file {
            if file.get_path().is_none() {
                fatal_error("Video Trimmer can only operate on local files.");
                app.quit();
                return;
            }

            window.open(file);
        }

        window.show_all();
    });

    let ret = app.run(&std::env::args().collect::<Vec<_>>());
    std::process::exit(ret);
}
