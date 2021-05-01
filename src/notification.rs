use glib::subclass::prelude::*;
use gtk::glib;

mod imp {
    use super::*;
    use crate::config::G_LOG_DOMAIN;
    use gettextrs::gettext;
    use glib::warn;
    use gtk::{prelude::*, subclass::prelude::*, CompositeTemplate};
    use std::{cell::RefCell, mem};

    #[derive(Debug)]
    enum State {
        Closed,
        Opening(glib::SourceId, Option<String>),
        Open(glib::SourceId),
        Closing(Option<String>),
    }

    impl Default for State {
        fn default() -> Self {
            State::Closed
        }
    }

    #[derive(Default, CompositeTemplate)]
    #[template(file = "notification.ui")]
    pub struct VtNotification {
        #[template_child]
        revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        label: TemplateChild<gtk::Label>,
        #[template_child]
        button_close: TemplateChild<gtk::Button>,

        state: RefCell<State>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for VtNotification {
        const NAME: &'static str = "VtNotification";
        type Type = super::VtNotification;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for VtNotification {
        fn constructed(&self, self_: &Self::Type) {
            self.parent_constructed(self_);

            self.revealer.connect_property_child_revealed_notify({
                let self_ = self_.downgrade();
                move |_| {
                    let self_ = self_.upgrade().unwrap();
                    let priv_ = Self::from_instance(&self_);
                    priv_.on_child_revealed_changed();
                }
            });

            self.button_close.connect_clicked({
                let self_ = self_.downgrade();
                move |_| {
                    let self_ = self_.upgrade().unwrap();
                    let priv_ = Self::from_instance(&self_);
                    priv_.close(None);
                }
            });
        }

        fn dispose(&self, obj: &Self::Type) {
            while let Some(child) = obj.first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for VtNotification {}

    impl VtNotification {
        pub fn show_notification(&self, file_name: String) {
            let mut state = self.state.borrow_mut();
            match *state {
                State::Closed => {
                    let source = glib::timeout_add_seconds_local_once(5, {
                        let self_ = self.instance().downgrade();
                        move || {
                            let self_ = self_.upgrade().unwrap();
                            let priv_ = Self::from_instance(&self_);
                            priv_.close(None);
                        }
                    });

                    *state = State::Opening(source, None);
                    drop(state);

                    self.label.set_text(&format!(
                        "{} {}",
                        file_name,
                        // Translators: text on the in-app notification after trimming was done.
                        // The template is: <video filename> has been saved
                        gettext("has been saved")
                    ));
                    self.revealer.set_reveal_child(true);
                }
                State::Opening(_, ref mut new_file_name)
                | State::Closing(ref mut new_file_name) => {
                    *new_file_name = Some(file_name);
                }
                State::Open(_) => {
                    drop(state);
                    self.close(Some(file_name));
                }
            }
        }

        fn close(&self, new_file_name: Option<String>) {
            let mut state = self.state.borrow_mut();

            if !matches!(*state, State::Open(_) | State::Opening(_, _)) {
                return;
            }

            let file_name = if let State::Opening(_, file_name) = &mut *state {
                file_name.take()
            } else {
                None
            };

            let new_file_name = new_file_name.or(file_name);
            if let State::Open(source) | State::Opening(source, _) =
                mem::replace(&mut *state, State::Closing(new_file_name))
            {
                glib::source_remove(source);
            }
            drop(state);

            self.revealer.set_reveal_child(false);
        }

        fn on_child_revealed_changed(&self) {
            let mut state = self.state.borrow_mut();

            if self.revealer.is_child_revealed() {
                match *state {
                    State::Opening(_, None) => {
                        let source = if let State::Opening(source, _) =
                            mem::replace(&mut *state, State::Closed)
                        {
                            source
                        } else {
                            unreachable!()
                        };
                        *state = State::Open(source);
                    }
                    State::Opening(_, ref mut new_file_name @ Some(_)) => {
                        let new_file_name = new_file_name.take();
                        drop(state);
                        self.close(new_file_name);
                    }
                    ref other => {
                        warn!("Unexpected notification state: {:?}", other);

                        let source = glib::timeout_add_seconds_local_once(5, {
                            let self_ = self.instance().downgrade();
                            move || {
                                let self_ = self_.upgrade().unwrap();
                                let priv_ = Self::from_instance(&self_);
                                priv_.close(None);
                            }
                        });

                        *state = State::Open(source);
                    }
                }
            } else {
                match *state {
                    State::Closing(None) => {
                        *state = State::Closed;
                    }
                    State::Closing(ref mut new_file_name @ Some(_)) => {
                        let new_file_name = new_file_name.take().unwrap();
                        let source = glib::timeout_add_seconds_local_once(5, {
                            let self_ = self.instance().downgrade();
                            move || {
                                let self_ = self_.upgrade().unwrap();
                                let priv_ = Self::from_instance(&self_);
                                priv_.close(None);
                            }
                        });
                        *state = State::Opening(source, None);
                        drop(state);

                        self.label.set_text(&format!(
                            "{} {}",
                            new_file_name,
                            // Translators: text on the in-app notification after trimming was done.
                            // The template is: <video filename> has been saved
                            gettext("has been saved")
                        ));
                        self.revealer.set_reveal_child(true);
                    }
                    ref other => {
                        warn!("Unexpected notification state: {:?}", other);

                        *state = State::Closed;
                    }
                }
            }
        }
    }
}

glib::wrapper! {
    pub struct VtNotification(ObjectSubclass<imp::VtNotification>)
        @extends gtk::Widget;
}

impl VtNotification {
    pub fn show_notification(&self, file_name: String) {
        imp::VtNotification::from_instance(self).show_notification(file_name)
    }
}
