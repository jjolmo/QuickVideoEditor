use std::env;

use gettextrs::*;
use glib::{info, warn, GlibLogger, GlibLoggerDomain, GlibLoggerFormat};
use gtk::{gio, glib, prelude::*};

mod application;
use application::VtApplication;
#[rustfmt::skip]
mod config;
use config::G_LOG_DOMAIN;
mod editor_timeline;
mod engine;
mod knob;
mod music_track;
mod original_audio_track;
mod parse;
mod timeline;
mod util;
mod video_preview;
mod window;

/// NVDEC decoders intermittently hang the preview pipeline on seek, so prefer software decoding.
/// avenc_aac, the only AAC encoder in the GNOME runtime, has no rank, so encodebin would never
/// pick it for exports; higher-ranked encoders like fdkaacenc still win. Set before GTK
/// initializes GStreamer; a user-provided value wins.
const GST_RANK_OVERRIDES: &str = "nvh264dec:NONE,nvh265dec:NONE,nvav1dec:NONE,nvvp9dec:NONE,\
                                  nvvp8dec:NONE,nvmpeg2videodec:NONE,nvmpeg4videodec:NONE,\
                                  avenc_aac:MARGINAL";

fn main() -> glib::ExitCode {
    if env::var_os("GST_PLUGIN_FEATURE_RANK").is_none() {
        env::set_var("GST_PLUGIN_FEATURE_RANK", GST_RANK_OVERRIDES);
    }

    static GLIB_LOGGER: GlibLogger =
        GlibLogger::new(GlibLoggerFormat::LineAndFile, GlibLoggerDomain::CrateTarget);

    let _ = log::set_logger(&GLIB_LOGGER);
    log::set_max_level(log::LevelFilter::Debug);

    info!("Quick Video Editor version {}", config::VERSION);

    if let Err(err) = ges::init() {
        glib::error!("could not initialize GStreamer Editing Services: {err}");
    }

    setlocale(LocaleCategory::LcAll, "");
    if let Err(err) = bindtextdomain("quick-video-editor", config::LOCALEDIR) {
        warn!("Error in bindtextdomain(): {}", err);
    }
    if let Err(err) = bind_textdomain_codeset("quick-video-editor", "UTF-8") {
        warn!("Error in bind_textdomain_codeset(): {}", err);
    }
    if let Err(err) = textdomain("quick-video-editor") {
        warn!("Error in textdomain(): {}", err);
    }

    glib::set_application_name(&format!(
        "{}{}",
        gettext("Quick Video Editor"),
        config::NAME_SUFFIX
    ));

    let res = match env::var("MESON_DEVENV") {
        Err(_) => {
            gio::Resource::load(config::PKGDATADIR.to_owned() + "/quick-video-editor.gresource")
                .expect("could not load the gresource file")
        }
        Ok(_) => {
            let mut resource_path = env::current_exe().expect("unable to get executable path");
            resource_path.pop();
            resource_path.push("quick-video-editor.gresource");
            gio::Resource::load(&resource_path)
                .expect("unable to load quick-video-editor.gresource from build dir")
        }
    };

    gio::resources_register(&res);

    video_preview::VtVideoPreview::remove_stale_music_previews();

    let app = VtApplication::new();
    app.run()
}
