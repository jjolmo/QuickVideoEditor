use glib::subclass::prelude::*;
use gtk::{gio, glib};

mod imp {
    use super::*;
    use crate::{config::G_LOG_DOMAIN, timeline::VtTimeline};
    use glib::{subclass::Signal, warn, Properties};
    use gtk::{glib, prelude::*, subclass::prelude::*, CompositeTemplate};
    use std::{cell::OnceCell, marker::PhantomData, sync::OnceLock};

    #[derive(Debug, Default, CompositeTemplate, Properties)]
    #[properties(wrapper_type = super::VtVideoPreview)]
    #[template(resource = "/org/gnome/gitlab/YaLTeR/VideoTrimmer/video_preview.ui")]
    pub struct VtVideoPreview {
        #[template_child]
        overlay: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        picture_video_preview: TemplateChild<gtk::Picture>,
        #[template_child]
        stack_video_preview: TemplateChild<gtk::Stack>,
        #[template_child]
        status_page_no_video: TemplateChild<adw::StatusPage>,
        #[template_child]
        button_play_pause: TemplateChild<gtk::Button>,
        #[template_child]
        button_play_pause_image: TemplateChild<gtk::Image>,
        #[template_child]
        label_current_time: TemplateChild<gtk::Label>,
        #[template_child]
        timeline: TemplateChild<VtTimeline>,
        #[template_child]
        box_playback_controls: TemplateChild<gtk::Box>,

        #[property(get = Self::duration)]
        duration: PhantomData<i64>,
        #[property(get = Self::is_playing, set = Self::set_is_playing, explicit_notify)]
        is_playing: PhantomData<bool>,

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
            Self::derived_properties()
        }

        fn property(&self, id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            self.derived_property(id, pspec)
        }

        fn set_property(&self, id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            self.derived_set_property(id, value, pspec);
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<[Signal; 4]> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                [
                    Signal::builder("set-start-end")
                        .param_types([glib::Type::U32, glib::Type::U32])
                        .build(),
                    Signal::builder("set-start")
                        .param_types([glib::Type::U32])
                        .build(),
                    Signal::builder("set-end")
                        .param_types([glib::Type::U32])
                        .build(),
                    Signal::builder("error").build(),
                ]
            })
        }

        fn constructed(&self) {
            let obj = self.obj();
            self.parent_constructed();

            self.timeline.connect_local("set-start-end", false, {
                let obj = obj.downgrade();
                move |args| {
                    let obj = obj.upgrade().unwrap();
                    obj.emit_by_name_with_values("set-start-end", &args[1..])
                }
            });

            self.timeline.connect_local("set-start", false, {
                let obj = obj.downgrade();
                move |args| {
                    let obj = obj.upgrade().unwrap();
                    obj.emit_by_name_with_values("set-start", &args[1..])
                }
            });

            self.timeline.connect_local("set-end", false, {
                let obj = obj.downgrade();
                move |args| {
                    let obj = obj.upgrade().unwrap();
                    obj.emit_by_name_with_values("set-end", &args[1..])
                }
            });

            // Connect the play-pause button.
            self.button_play_pause.connect_clicked({
                let obj = obj.downgrade();
                move |_| {
                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();
                    let media_file = imp.media_file.get().unwrap();
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
                let obj = obj.downgrade();
                move |media_file| {
                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();

                    if media_file.is_playing() {
                        imp.button_play_pause_image
                            .set_icon_name(Some("media-playback-pause-symbolic"));
                    } else {
                        imp.button_play_pause_image
                            .set_icon_name(Some("media-playback-start-symbolic"));
                    }

                    obj.notify_is_playing();
                }
            });

            media_file.connect_error_notify({
                let obj = obj.downgrade();
                move |media_file| {
                    let error = media_file.error().unwrap();

                    warn!("Error in MediaFile: {}", error);

                    let obj = obj.upgrade().unwrap();
                    obj.emit_by_name::<()>("error", &[]);
                }
            });

            media_file.connect_prepared_notify({
                let obj = obj.downgrade();
                move |media_file| {
                    let obj = obj.upgrade().unwrap();

                    if media_file.error().is_some() {
                        return;
                    }

                    if !media_file.has_video() {
                        let imp = obj.imp();

                        imp.stack_video_preview
                            .set_visible_child(&*imp.status_page_no_video);
                    }

                    // GTK API is such that on "prepared" all media info is known and won't change.
                    obj.notify("duration");
                }
            });

            media_file.connect_timestamp_notify({
                let obj = obj.downgrade();
                move |media_file| {
                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();

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

                    imp.label_current_time.set_text(&time);
                }
            });

            self.picture_video_preview.set_paintable(Some(&media_file));
            self.timeline.set_property("media-file", &media_file);

            self.media_file.set(media_file).unwrap();
        }

        fn dispose(&self) {
            let obj = self.obj();
            while let Some(child) = obj.first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for VtVideoPreview {}

    impl VtVideoPreview {
        fn duration(&self) -> i64 {
            self.media_file.get().unwrap().duration()
        }

        fn is_playing(&self) -> bool {
            let Some(media_file) = self.media_file.get() else {
                return false;
            };
            media_file.is_playing()
        }

        fn set_is_playing(&self, value: bool) {
            let Some(media_file) = self.media_file.get() else {
                return;
            };
            media_file.set_playing(value);
        }

        pub fn open(&self, file: &gio::File) {
            let media_file = self.media_file.get().unwrap();
            media_file.set_file(Some(file));
        }

        pub fn set_start_end(&self, start_end: Option<(u32, u32)>) {
            self.timeline.set_start_end(start_end);
        }

        pub fn set_start_as_position(&self) {
            self.timeline.set_start_as_position();
        }

        pub fn set_end_as_position(&self) {
            self.timeline.set_end_as_position();
        }

        pub fn pause(&self) {
            self.media_file.get().unwrap().pause();
        }

        pub fn overlay(&self) -> &adw::ToastOverlay {
            &self.overlay
        }

        pub fn box_playback_controls(&self) -> &gtk::Box {
            &self.box_playback_controls
        }
    }
}

glib::wrapper! {
    pub struct VtVideoPreview(ObjectSubclass<imp::VtVideoPreview>)
        @extends gtk::Widget;
}

impl VtVideoPreview {
    pub fn open(&self, file: &gio::File) {
        self.imp().open(file);
    }

    pub fn set_start_end(&self, start_end: Option<(u32, u32)>) {
        self.imp().set_start_end(start_end);
    }

    pub fn set_start_as_position(&self) {
        self.imp().set_start_as_position();
    }

    pub fn set_end_as_position(&self) {
        self.imp().set_end_as_position();
    }

    pub fn pause(&self) {
        self.imp().pause();
    }

    pub fn overlay(&self) -> &adw::ToastOverlay {
        self.imp().overlay()
    }

    pub fn box_playback_controls(&self) -> &gtk::Box {
        self.imp().box_playback_controls()
    }
}
