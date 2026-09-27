use gtk::{glib, prelude::*, subclass::prelude::*};

/// The speed range of the knobs and the Editor segments.
pub const MIN_SPEED: f64 = 0.1;
pub const MAX_SPEED: f64 = 10.;

mod imp {
    use super::*;
    use crate::util::gettext_f;
    use glib::subclass::Signal;
    use gtk::{gdk, graphene, gsk};
    use std::{
        cell::{Cell, RefCell},
        f32::consts::PI,
        sync::OnceLock,
    };

    /// Vertical drag distance in pixels that sweeps the whole range.
    const DRAG_RANGE: f64 = 200.;
    /// Fraction of the range one scroll step moves.
    const SCROLL_STEP: f64 = 0.02;
    /// Speeds this close to 100% snap to it.
    const SNAP: f64 = 0.04;
    /// The dial sweeps 270°, starting at the bottom left.
    const START_ANGLE: f32 = 0.75 * PI;
    const SWEEP: f32 = 1.5 * PI;
    /// Scrolling applies the speed after this pause, so a scroll gesture is one change.
    const SCROLL_COMMIT_DELAY: std::time::Duration = std::time::Duration::from_millis(300);

    #[derive(Debug, Default)]
    pub struct VtKnob {
        pub(super) value: Cell<f64>,
        pub(super) min: Cell<f64>,
        pub(super) max: Cell<f64>,
        /// Logarithmic scales put e.g. 50% and 200% symmetrically around 100%.
        pub(super) logarithmic: Cell<bool>,
        /// Tooltip text; `{}` is replaced with the value.
        pub(super) tooltip: RefCell<String>,
        drag_start: Cell<f64>,
        scroll_commit: RefCell<Option<glib::SourceId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for VtKnob {
        const NAME: &'static str = "VtKnob";
        type Type = super::VtKnob;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("vt-knob");
            klass.set_accessible_role(gtk::AccessibleRole::Slider);
        }
    }

    impl ObjectImpl for VtKnob {
        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<[Signal; 1]> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                [Signal::builder("value-changed")
                    .param_types([glib::Type::F64])
                    .build()]
            })
        }

        fn constructed(&self) {
            let obj = self.obj();
            self.parent_constructed();

            self.value.set(1.);
            self.min.set(MIN_SPEED);
            self.max.set(MAX_SPEED);
            self.logarithmic.set(true);
            // Translators: tooltip of the speed knob; the placeholder is a percentage.
            self.tooltip.replace(gettext_f(
                "Speed {}\nDrag up or down or scroll to change, double-click to reset",
                &["{}"],
            ));
            obj.set_size_request(52, 52);
            obj.set_cursor_from_name(Some("ns-resize"));

            let drag = gtk::GestureDrag::new();
            drag.connect_drag_begin({
                let obj = obj.downgrade();
                move |gesture, _, _| {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();
                    imp.drag_start.set(imp.fraction_of(imp.value.get()));
                }
            });
            drag.connect_drag_update({
                let obj = obj.downgrade();
                move |_, _, offset_y| {
                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();
                    imp.set_fraction(imp.drag_start.get() - offset_y / DRAG_RANGE);
                }
            });
            drag.connect_drag_end({
                let obj = obj.downgrade();
                move |_, _, _| {
                    let obj = obj.upgrade().unwrap();
                    obj.imp().commit();
                }
            });
            obj.add_controller(drag);

            let click = gtk::GestureClick::new();
            click.connect_pressed({
                let obj = obj.downgrade();
                move |_, n_press, _, _| {
                    if n_press == 2 {
                        let obj = obj.upgrade().unwrap();
                        obj.imp().set_value(1.);
                        obj.imp().commit();
                    }
                }
            });
            obj.add_controller(click);

            let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
            scroll.connect_scroll({
                let obj = obj.downgrade();
                move |_, _, dy| {
                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();
                    imp.set_fraction(imp.fraction_of(imp.value.get()) - dy * SCROLL_STEP);
                    imp.schedule_scroll_commit();
                    glib::Propagation::Stop
                }
            });
            obj.add_controller(scroll);

            self.update_tooltip();
        }
    }

    impl WidgetImpl for VtKnob {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let size = obj.width().min(obj.height()) as f32;
            let center = graphene::Point::new(obj.width() as f32 / 2., obj.height() as f32 / 2.);
            let line = if size < 40. { 3. } else { 4. };
            let radius = size / 2. - line;
            let fg = obj.color();
            let accent = adw::StyleManager::default().accent_color_rgba();

            let arc = |from: f32, to: f32| {
                let builder = gsk::PathBuilder::new();
                let steps = 48;
                for step in 0..=steps {
                    let angle = from + (to - from) * step as f32 / steps as f32;
                    let x = center.x() + radius * angle.cos();
                    let y = center.y() + radius * angle.sin();
                    if step == 0 {
                        builder.move_to(x, y);
                    } else {
                        builder.line_to(x, y);
                    }
                }
                builder.to_path()
            };

            let stroke = gsk::Stroke::new(line);
            stroke.set_line_cap(gsk::LineCap::Round);
            snapshot.append_stroke(
                &arc(START_ANGLE, START_ANGLE + SWEEP),
                &stroke,
                &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), fg.alpha() * 0.15),
            );

            // The filled arc grows from the 100% mark, so faster and slower read differently.
            let angle_of = |value: f64| START_ANGLE + SWEEP * self.fraction_of(value) as f32;
            let normal = angle_of(1.);
            let current = angle_of(self.value.get());
            if (current - normal).abs() > 0.01 {
                snapshot.append_stroke(
                    &arc(normal.min(current), normal.max(current)),
                    &stroke,
                    &accent,
                );
            }
            let tick = graphene::Rect::new(
                center.x() + (radius - 6.) * normal.cos() - 1.,
                center.y() + (radius - 6.) * normal.sin() - 1.,
                2.,
                2.,
            );
            snapshot.append_color(&fg, &tick);

            let label = format!("{:.0}%", self.value.get() * 100.);
            let layout = obj.create_pango_layout(Some(&label));
            let mut font = gtk::pango::FontDescription::new();
            font.set_size(if size < 40. { 6 } else { 8 } * gtk::pango::SCALE);
            font.set_weight(gtk::pango::Weight::Bold);
            layout.set_font_description(Some(&font));
            let (width, height) = layout.pixel_size();
            snapshot.save();
            snapshot.translate(&graphene::Point::new(
                center.x() - width as f32 / 2.,
                center.y() - height as f32 / 2.,
            ));
            snapshot.append_layout(&layout, &fg);
            snapshot.restore();
        }
    }

    impl VtKnob {
        fn set_fraction(&self, fraction: f64) {
            self.set_value(self.value_at(fraction.clamp(0., 1.)));
        }

        pub(super) fn set_value(&self, value: f64) {
            let mut value = value.clamp(self.min.get(), self.max.get());
            if (value - 1.).abs() < SNAP {
                value = 1.;
            }
            if value != self.value.get() {
                self.value.set(value);
                self.update_tooltip();
                self.obj().queue_draw();
            }
        }

        fn commit(&self) {
            if let Some(source) = self.scroll_commit.take() {
                source.remove();
            }
            self.obj()
                .emit_by_name::<()>("value-changed", &[&self.value.get()]);
        }

        fn schedule_scroll_commit(&self) {
            if let Some(source) = self.scroll_commit.take() {
                source.remove();
            }
            let source = glib::timeout_add_local_once(SCROLL_COMMIT_DELAY, {
                let obj = self.obj().downgrade();
                move || {
                    if let Some(obj) = obj.upgrade() {
                        obj.imp().scroll_commit.take();
                        obj.imp().commit();
                    }
                }
            });
            self.scroll_commit.replace(Some(source));
        }

        pub(super) fn update_tooltip(&self) {
            let value = format!("{:.0}%", self.value.get() * 100.);
            let text = self.tooltip.borrow().replacen("{}", &value, 1);
            self.obj().set_tooltip_text(Some(&text));
        }

        fn fraction_of(&self, value: f64) -> f64 {
            let (min, max) = (self.min.get(), self.max.get());
            if self.logarithmic.get() {
                (value / min).ln() / (max / min).ln()
            } else {
                (value - min) / (max - min)
            }
        }

        fn value_at(&self, fraction: f64) -> f64 {
            let (min, max) = (self.min.get(), self.max.get());
            if self.logarithmic.get() {
                min * (max / min).powf(fraction)
            } else {
                min + (max - min) * fraction
            }
        }
    }
}

glib::wrapper! {
    pub struct VtKnob(ObjectSubclass<imp::VtKnob>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl VtKnob {
    /// Makes this a linear volume knob from 0% to `max`, `diameter` pixels wide.
    pub fn configure_volume(&self, max: f64, diameter: i32, tooltip: &str) {
        let imp = self.imp();
        imp.min.set(0.);
        imp.max.set(max);
        imp.logarithmic.set(false);
        imp.tooltip.replace(tooltip.to_owned());
        self.set_size_request(diameter, diameter);
        imp.update_tooltip();
        self.queue_draw();
    }

    pub fn value(&self) -> f64 {
        self.imp().value.get()
    }

    pub fn set_value(&self, value: f64) {
        self.imp().set_value(value);
    }
}
