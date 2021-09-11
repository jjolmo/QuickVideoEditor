use glib::subclass::prelude::*;
use gtk::{gio, glib};

mod imp {
    use super::*;
    use crate::{config::G_LOG_DOMAIN, timeline::VtTimeline};
    use glib::{subclass, warn};
    use gtk::{glib, prelude::*, subclass::prelude::*, CompositeTemplate};
    use once_cell::unsync::OnceCell;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/org/gnome/gitlab/YaLTeR/VideoTrimmer/video_preview.ui")]
    pub struct VtVideoPreview {
        #[template_child]
        picture_video_preview: TemplateChild<gtk::Picture>,
        #[template_child]
        button_play_pause: TemplateChild<gtk::Button>,
        #[template_child]
        button_play_pause_image: TemplateChild<gtk::Image>,
        #[template_child]
        label_current_time: TemplateChild<gtk::Label>,
        #[template_child]
        timeline: TemplateChild<VtTimeline>,

        media_file: OnceCell<gtk::MediaFile>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for VtVideoPreview {
        const NAME: &'static str = "VtVideoPreview";
        type Type = super::VtVideoPreview;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for VtVideoPreview {
        fn properties() -> &'static [glib::ParamSpec] {
            use once_cell::sync::Lazy;
            static PROPERTIES: Lazy<[glib::ParamSpec; 1]> = Lazy::new(|| {
                [glib::ParamSpec::new_int64(
                    "duration",
                    "duration",
                    "duration",
                    0,
                    std::i64::MAX,
                    0,
                    glib::ParamFlags::READABLE,
                )]
            });

            PROPERTIES.as_ref()
        }

        fn signals() -> &'static [subclass::Signal] {
            use once_cell::sync::Lazy;
            static SIGNALS: Lazy<[subclass::Signal; 2]> = Lazy::new(|| {
                [
                    subclass::Signal::builder(
                        "set-start-end",
                        &[glib::Type::U32.into(), glib::Type::U32.into()],
                        glib::Type::UNIT.into(),
                    )
                    .build(),
                    subclass::Signal::builder("error", &[], glib::Type::UNIT.into()).build(),
                ]
            });

            SIGNALS.as_ref()
        }

        fn property(&self, _obj: &Self::Type, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "duration" => self.media_file.get().unwrap().duration().to_value(),
                _ => unreachable!(),
            }
        }

        fn constructed(&self, self_: &Self::Type) {
            self.parent_constructed(self_);

            self.timeline
                .connect_local("set-start-end", false, {
                    let self_ = self_.downgrade();
                    move |args| {
                        let self_ = self_.upgrade().unwrap();
                        self_
                            .emit_by_name_with_values("set-start-end", &args[1..])
                            .unwrap()
                    }
                })
                .unwrap();

            // Connect the play-pause button.
            self.button_play_pause.connect_clicked({
                let self_ = self_.downgrade();
                move |_| {
                    let self_ = self_.upgrade().unwrap();
                    let priv_ = VtVideoPreview::from_instance(&self_);
                    let media_file = priv_.media_file.get().unwrap();
                    if media_file.is_playing() {
                        media_file.pause();
                    } else {
                        media_file.play();
                    }
                }
            });

            // Media file callbacks.
            let media_file = gtk::MediaFile::new();
            media_file.connect_playing_notify({
                let self_ = self_.downgrade();
                move |media_file| {
                    let self_ = self_.upgrade().unwrap();
                    let priv_ = VtVideoPreview::from_instance(&self_);

                    if media_file.is_playing() {
                        priv_
                            .button_play_pause_image
                            .set_icon_name(Some("media-playback-pause-symbolic"));
                    } else {
                        priv_
                            .button_play_pause_image
                            .set_icon_name(Some("media-playback-start-symbolic"));
                    }
                }
            });

            media_file.connect_error_notify({
                let self_ = self_.downgrade();
                move |media_file| {
                    let error = media_file.error().unwrap();

                    warn!("Error in MediaFile: {}", error);

                    let self_ = self_.upgrade().unwrap();
                    let _ = self_.emit_by_name("error", &[]);
                }
            });

            media_file.connect_prepared_notify({
                let self_ = self_.downgrade();
                move |_| {
                    let self_ = self_.upgrade().unwrap();

                    // GTK API is such that on "prepared" all media info is known and won't change.
                    self_.notify("duration");
                }
            });

            media_file.connect_timestamp_notify({
                let self_ = self_.downgrade();
                move |media_file| {
                    let self_ = self_.upgrade().unwrap();
                    let priv_ = VtVideoPreview::from_instance(&self_);

                    let position = media_file.timestamp();
                    let mut seconds = position / 1_000_000;
                    let mut minutes = seconds / 60;
                    let hours = minutes / 60;
                    seconds %= 60;
                    minutes %= 60;

                    let time = if hours == 0 {
                        format!("{}:{:02}", minutes, seconds)
                    } else {
                        format!("{}:{:02}:{:02}", hours, minutes, seconds)
                    };

                    priv_.label_current_time.set_text(&time);
                }
            });

            self.picture_video_preview.set_paintable(Some(&media_file));
            self.timeline
                .set_property("media-file", &media_file)
                .unwrap();

            self.media_file.set(media_file).unwrap();
        }

        fn dispose(&self, obj: &Self::Type) {
            while let Some(child) = obj.first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for VtVideoPreview {}

    impl VtVideoPreview {
        pub fn open(&self, file: &gio::File) {
            let media_file = self.media_file.get().unwrap();
            media_file.set_file(Some(file));
            media_file.play();
        }

        pub fn set_start_end(&self, start_end: Option<(u32, u32)>) {
            self.timeline.set_start_end(start_end);
        }

        pub fn destroy(&self) {
            self.media_file.get().unwrap().clear();
        }
    }
}

glib::wrapper! {
    pub struct VtVideoPreview(ObjectSubclass<imp::VtVideoPreview>)
        @extends gtk::Widget;
}

impl VtVideoPreview {
    pub fn open(&self, file: &gio::File) {
        imp::VtVideoPreview::from_instance(self).open(file);
    }

    pub fn set_start_end(&self, start_end: Option<(u32, u32)>) {
        imp::VtVideoPreview::from_instance(self).set_start_end(start_end);
    }

    pub fn destroy(&self) {
        imp::VtVideoPreview::from_instance(self).destroy();
    }
}
