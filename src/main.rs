use std::env;

use gettextrs::*;
use glib::{info, warn, GlibLogger, GlibLoggerDomain, GlibLoggerFormat};
use gtk::{gio, glib, prelude::*};

mod application;
use application::VtApplication;
#[rustfmt::skip]
mod config;
use config::G_LOG_DOMAIN;
mod parse;
mod timeline;
mod video_preview;
mod window;

fn main() {
    static GLIB_LOGGER: GlibLogger =
        GlibLogger::new(GlibLoggerFormat::LineAndFile, GlibLoggerDomain::CrateTarget);

    let _ = log::set_logger(&GLIB_LOGGER);
    log::set_max_level(log::LevelFilter::Debug);

    info!("Video Trimmer version {}", config::VERSION);

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

    let res = match env::var("MESON_DEVENV") {
        Err(_) => gio::Resource::load(config::PKGDATADIR.to_owned() + "/video-trimmer.gresource")
            .expect("could not load the gresource file"),
        Ok(_) => {
            let mut resource_path = env::current_exe().expect("unable to get executable path");
            resource_path.pop();
            resource_path.push("video-trimmer.gresource");
            gio::Resource::load(&resource_path)
                .expect("unable to load video-trimmer.gresource from build dir")
        }
    };

    gio::resources_register(&res);

    let app = VtApplication::new();
    let ret = app.run();
    std::process::exit(ret);
}
