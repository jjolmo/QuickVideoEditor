//! Timeline of the Editor mode: segments that can be selected, moved, trimmed and sped up.

use gtk::{glib, prelude::*, subclass::prelude::*};

use crate::engine::{SegmentInfo, VtEngine};

mod imp {
    use super::*;
    use crate::{
        engine::SegmentEdge,
        knob::{MAX_SPEED, MIN_SPEED},
    };
    use glib::subclass::Signal;
    use gtk::{gdk, graphene, gsk};
    use std::{
        cell::{Cell, OnceCell, RefCell},
        sync::OnceLock,
    };

    const RULER_HEIGHT: f32 = 16.;
    /// Pointer distance in pixels that grabs a segment edge.
    const EDGE_TOLERANCE: f64 = 6.;
    /// Distance in pixels within which dragged edges snap to other edges and the playhead.
    const SNAP_DISTANCE: f64 = 8.;
    /// Pointer travel in pixels before a press on a segment becomes a move.
    const DRAG_THRESHOLD: f64 = 4.;
    /// Shortest segment a trim can leave, in µs.
    const MIN_DURATION: i64 = 100_000;
    const SPEED_SNAP: f64 = 0.03;
    const MAX_ZOOM: f64 = 200.;
    /// Empty room shown after the edit, as a fraction of its length, to drag segments into.
    const TRAILING_ROOM: f64 = 0.3;
    /// Length limit of many social networks: 0:59.
    const SOCIAL_LENGTH: i64 = 59_000_000;
    /// Mirrors the engine's fade to black at the end of the edit.
    const END_FADE: i64 = 1_000_000;

    /// GNOME palette, one colour per source video.
    const PALETTE: [(f32, f32, f32); 6] = [
        (0.21, 0.52, 0.89),
        (0.20, 0.82, 0.48),
        (1.00, 0.47, 0.00),
        (0.57, 0.25, 0.67),
        (0.88, 0.11, 0.14),
        (0.96, 0.83, 0.18),
    ];

    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Drag {
        None,
        Scrub,
        /// Pressed on a segment; becomes a move once the pointer travels.
        Pending {
            index: usize,
        },
        Move {
            index: usize,
            grab: i64,
        },
        Trim {
            index: usize,
            edge: SegmentEdge,
        },
        Speed {
            index: usize,
            edge: SegmentEdge,
        },
    }

    /// Where a segment is drawn while being dragged, before the edit is applied.
    #[derive(Debug, Clone, Copy)]
    struct Ghost {
        index: usize,
        start: i64,
        duration: i64,
        speed: f64,
    }

    #[derive(Debug)]
    pub struct VtEditorTimeline {
        engine: OnceCell<VtEngine>,
        segments: RefCell<Vec<SegmentInfo>>,
        total: Cell<i64>,
        /// Length of the added videos at normal speed, in µs.
        source_total: Cell<i64>,
        position: Cell<i64>,
        zoom: Cell<f64>,
        view_start: Cell<i64>,
        selected: Cell<Option<usize>>,
        drag: Cell<Drag>,
        drag_x: Cell<f64>,
        ghost: Cell<Option<Ghost>>,
        end_fade: Cell<bool>,
        /// Snaps to 0:59, the length limit of many social networks, and marks it.
        social_snap: Cell<bool>,
        gesture: OnceCell<gtk::GestureDrag>,
    }

    impl Default for VtEditorTimeline {
        fn default() -> Self {
            Self {
                engine: OnceCell::new(),
                segments: RefCell::new(Vec::new()),
                total: Cell::new(0),
                source_total: Cell::new(0),
                position: Cell::new(0),
                zoom: Cell::new(1.),
                view_start: Cell::new(0),
                selected: Cell::new(None),
                drag: Cell::new(Drag::None),
                drag_x: Cell::new(0.),
                ghost: Cell::new(None),
                end_fade: Cell::new(false),
                social_snap: Cell::new(false),
                gesture: OnceCell::new(),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for VtEditorTimeline {
        const NAME: &'static str = "VtEditorTimeline";
        type Type = super::VtEditorTimeline;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("vt-editor-timeline");
        }
    }

    impl ObjectImpl for VtEditorTimeline {
        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<[Signal; 2]> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                [
                    Signal::builder("view-changed").build(),
                    Signal::builder("selection-changed").build(),
                ]
            })
        }

        fn constructed(&self) {
            let obj = self.obj();
            self.parent_constructed();

            obj.set_size_request(-1, 56);
            obj.set_overflow(gtk::Overflow::Hidden);
            obj.set_focusable(true);

            let gesture = gtk::GestureDrag::new();
            gesture.connect_drag_begin({
                let obj = obj.downgrade();
                move |gesture, x, y| {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    let ctrl = gesture
                        .current_event_state()
                        .contains(gdk::ModifierType::CONTROL_MASK);
                    let obj = obj.upgrade().unwrap();
                    obj.grab_focus();
                    obj.imp().on_drag_begin(x, y, ctrl);
                }
            });
            gesture.connect_drag_update({
                let obj = obj.downgrade();
                move |_, dx, _| {
                    let obj = obj.upgrade().unwrap();
                    obj.imp().on_drag_update(dx);
                }
            });
            gesture.connect_drag_end({
                let obj = obj.downgrade();
                move |_, dx, _| {
                    let obj = obj.upgrade().unwrap();
                    obj.imp().on_drag_end(dx);
                }
            });
            obj.add_controller(gesture.clone());
            self.gesture.set(gesture).unwrap();

            let motion = gtk::EventControllerMotion::new();
            motion.connect_motion({
                let obj = obj.downgrade();
                move |controller, x, y| {
                    let ctrl = controller
                        .current_event_state()
                        .contains(gdk::ModifierType::CONTROL_MASK);
                    let obj = obj.upgrade().unwrap();
                    obj.imp().update_cursor(x, y, ctrl);
                }
            });
            obj.add_controller(motion);

            let scroll =
                gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
            scroll.connect_scroll({
                let obj = obj.downgrade();
                move |controller, dx, dy| {
                    let ctrl = controller
                        .current_event_state()
                        .contains(gdk::ModifierType::CONTROL_MASK);
                    let obj = obj.upgrade().unwrap();
                    obj.imp().on_scroll(dx, dy, ctrl)
                }
            });
            obj.add_controller(scroll);

            let keys = gtk::EventControllerKey::new();
            keys.connect_key_pressed({
                let obj = obj.downgrade();
                move |_, key, _, _| {
                    let obj = obj.upgrade().unwrap();
                    let imp = obj.imp();
                    match key {
                        gdk::Key::s | gdk::Key::S => {
                            imp.split_at_playhead();
                            glib::Propagation::Stop
                        }
                        gdk::Key::Delete | gdk::Key::KP_Delete | gdk::Key::BackSpace => {
                            imp.delete_current();
                            glib::Propagation::Stop
                        }
                        _ => glib::Propagation::Proceed,
                    }
                }
            });
            obj.add_controller(keys);
        }
    }

    impl WidgetImpl for VtEditorTimeline {
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            self.clamp_view();
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let width = obj.width() as f32;
            let height = obj.height() as f32;
            let fg = obj.color();

            let bounds = graphene::Rect::new(0., 0., width, height);
            snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(bounds, 9.));
            snapshot.append_color(&with_alpha(&fg, 0.1), &bounds);

            if self.visible_duration() <= 0 {
                snapshot.pop();
                return;
            }

            self.draw_ruler(snapshot, width, &fg);

            let segments = self.drawn_segments();
            let top = RULER_HEIGHT + 2.;
            let bottom = height - 3.;

            for (index, segment) in segments.iter().enumerate() {
                let x0 = self.x_of(segment.start);
                let x1 = self.x_of(segment.end());
                if x1 < 0. || x0 > width {
                    continue;
                }
                let rect = graphene::Rect::new(x0, top, (x1 - x0).max(1.), bottom - top);
                let rounded = gsk::RoundedRect::from_rect(rect, 6.);
                let (r, g, b) = PALETTE[segment.source % PALETTE.len()];
                snapshot.push_rounded_clip(&rounded);
                snapshot.append_color(&gdk::RGBA::new(r, g, b, 0.8), &rect);

                let mut label = segment.name.clone();
                if segment.speed != 1. {
                    label = format!("{:.0}% · {label}", segment.speed * 100.);
                }
                let layout = obj.create_pango_layout(Some(&label));
                layout.set_ellipsize(gtk::pango::EllipsizeMode::End);
                let label_x = x0.max(0.) + 6.;
                layout.set_width(((x1.min(width) - label_x - 6.).max(0.) * 1024.) as i32);
                snapshot.save();
                snapshot.translate(&graphene::Point::new(label_x, top + 3.));
                snapshot.append_layout(&layout, &gdk::RGBA::WHITE);
                snapshot.restore();

                if segment.speed != 1. {
                    // Tighter coils for faster segments, looser for slower ones.
                    let coils = ((x1 - x0) / 14. * segment.speed as f32).clamp(2., 80.) as usize;
                    draw_spring(snapshot, x0, x1, (top + bottom) / 2. + 6., 5., coils);
                }
                snapshot.pop();

                if self.selected.get() == Some(index) {
                    snapshot.append_border(&rounded, &[2.; 4], &[gdk::RGBA::WHITE; 4]);
                }
            }

            // Crossfades where segments overlap.
            for (i, a) in segments.iter().enumerate() {
                for b in segments.iter().skip(i + 1) {
                    let from = a.start.max(b.start);
                    let to = a.end().min(b.end());
                    if from >= to {
                        continue;
                    }
                    let x0 = self.x_of(from);
                    let x1 = self.x_of(to);
                    let rect = graphene::Rect::new(x0, top, x1 - x0, bottom - top);
                    snapshot.append_color(&gdk::RGBA::new(0., 0., 0., 0.35), &rect);
                    let builder = gsk::PathBuilder::new();
                    builder.move_to(x0, top);
                    builder.line_to(x1, bottom);
                    builder.move_to(x0, bottom);
                    builder.line_to(x1, top);
                    snapshot.append_stroke(
                        &builder.to_path(),
                        &gsk::Stroke::new(1.5),
                        &gdk::RGBA::WHITE,
                    );
                }
            }

            if self.end_fade.get() {
                if let Some(last) = segments.iter().max_by_key(|segment| segment.end()) {
                    let fade = END_FADE.min(last.duration / 2);
                    let x0 = self.x_of(last.end() - fade);
                    let x1 = self.x_of(last.end());
                    snapshot.append_linear_gradient(
                        &graphene::Rect::new(x0, top, x1 - x0, bottom - top),
                        &graphene::Point::new(x0, 0.),
                        &graphene::Point::new(x1, 0.),
                        &[
                            gsk::ColorStop::new(0., gdk::RGBA::new(0., 0., 0., 0.)),
                            gsk::ColorStop::new(1., gdk::RGBA::new(0., 0., 0., 0.85)),
                        ],
                    );
                }
            }

            if self.social_snap.get() {
                let x = self.x_of(SOCIAL_LENGTH);
                if (0. ..=width).contains(&x) {
                    let accent = adw::StyleManager::default().accent_color_rgba();
                    snapshot.append_color(&accent, &graphene::Rect::new(x - 1., 0., 2., height));
                    let layout = obj.create_pango_layout(Some("0:59"));
                    let mut font = gtk::pango::FontDescription::new();
                    font.set_size(7 * gtk::pango::SCALE);
                    font.set_weight(gtk::pango::Weight::Bold);
                    layout.set_font_description(Some(&font));
                    let (label_width, _) = layout.pixel_size();
                    snapshot.save();
                    snapshot.translate(&graphene::Point::new(x - label_width as f32 - 3., 1.));
                    snapshot.append_layout(&layout, &accent);
                    snapshot.restore();
                }
            }

            let x = self.x_of(self.position.get());
            snapshot.append_color(&fg, &graphene::Rect::new(x - 1., 0., 2., height));
            let builder = gsk::PathBuilder::new();
            builder.move_to(x - 6., 0.);
            builder.line_to(x + 6., 0.);
            builder.line_to(x, 8.);
            builder.close();
            snapshot.append_fill(&builder.to_path(), gsk::FillRule::Winding, &fg);

            snapshot.pop();
        }
    }

    impl VtEditorTimeline {
        pub(super) fn set_engine(&self, engine: &VtEngine) {
            engine.connect_local("timeline-changed", false, {
                let obj = self.obj().downgrade();
                move |_| {
                    if let Some(obj) = obj.upgrade() {
                        obj.imp().refresh();
                    }
                    None
                }
            });
            engine.connect_timestamp_notify({
                let obj = self.obj().downgrade();
                move |engine| {
                    if let Some(obj) = obj.upgrade() {
                        let imp = obj.imp();
                        if !matches!(imp.drag.get(), Drag::Scrub) {
                            imp.position.set(engine.timestamp());
                        }
                        imp.follow_playhead();
                        obj.queue_draw();
                    }
                }
            });
            self.engine.set(engine.clone()).unwrap();
            self.refresh();
        }

        fn engine(&self) -> &VtEngine {
            self.engine.get().unwrap()
        }

        pub(super) fn refresh(&self) {
            let segments = self.engine().segments();
            let count = segments.len();
            self.segments.replace(segments);
            self.total.set(self.engine().total_duration());
            self.source_total.set(self.engine().sources_duration());
            if self.selected.get().is_some_and(|index| index >= count) {
                self.selected.set(None);
                self.obj().emit_by_name::<()>("selection-changed", &[]);
            }
            self.clamp_view();
            self.obj().queue_draw();
        }

        pub(super) fn selected(&self) -> Option<usize> {
            self.selected.get()
        }

        pub(super) fn split_at_playhead(&self) -> bool {
            let engine = self.engine();
            engine.is_editor() && engine.split(engine.timestamp())
        }

        /// Deletes the selected segment, or the one under the playhead. The last one stays.
        pub(super) fn delete_current(&self) {
            let engine = self.engine();
            let segments = self.segments.borrow().clone();
            if !engine.is_editor() || segments.len() <= 1 {
                return;
            }
            let position = engine.timestamp();
            let index = self.selected.get().or_else(|| {
                segments
                    .iter()
                    .enumerate()
                    .filter(|(_, segment)| segment.start <= position && position < segment.end())
                    .map(|(index, _)| index)
                    .next_back()
            });
            if let Some(index) = index {
                self.selected.set(None);
                engine.remove_segment(index);
                self.obj().emit_by_name::<()>("selection-changed", &[]);
            }
        }

        pub(super) fn set_social_snap(&self, enabled: bool) {
            self.social_snap.set(enabled);
            self.obj().queue_draw();
        }

        pub(super) fn set_end_fade(&self, enabled: bool) {
            self.end_fade.set(enabled);
            self.obj().queue_draw();
        }

        pub(super) fn view(&self) -> (i64, i64) {
            (self.view_start.get(), self.visible_duration())
        }

        /// Length of the whole scrollable span: the videos' original length plus some empty room,
        /// so speed changes don't rescale the timeline. It only grows if the edit outgrows it.
        fn span(&self) -> i64 {
            let room = (self.source_total.get() as f64 * (1. + TRAILING_ROOM)) as i64;
            // A sliver past the end keeps the last edge grabbable.
            room.max((self.total.get() as f64 * 1.03) as i64)
        }

        /// Timeline span shown across the widget, in µs.
        fn visible_duration(&self) -> i64 {
            (self.span() as f64 / self.zoom.get()) as i64
        }

        fn x_of(&self, time: i64) -> f32 {
            let width = self.obj().width() as f64;
            ((time - self.view_start.get()) as f64 / self.visible_duration().max(1) as f64 * width)
                as f32
        }

        fn time_of(&self, x: f64) -> i64 {
            let width = self.obj().width().max(1) as f64;
            self.view_start.get() + (x / width * self.visible_duration() as f64) as i64
        }

        fn us_per_px(&self) -> f64 {
            self.visible_duration() as f64 / self.obj().width().max(1) as f64
        }

        fn clamp_view(&self) {
            let max_start = (self.span() - self.visible_duration()).max(0);
            let start = self.view_start.get().clamp(0, max_start);
            if start != self.view_start.get() {
                self.view_start.set(start);
            }
            self.obj().emit_by_name::<()>("view-changed", &[]);
        }

        /// Keeps the playhead in view while playing when zoomed in.
        fn follow_playhead(&self) {
            if self.zoom.get() <= 1. || self.drag.get() != Drag::None {
                return;
            }
            let position = self.position.get();
            let visible = self.visible_duration();
            let start = self.view_start.get();
            if position < start || position > start + visible {
                self.view_start.set(position - visible / 10);
                self.clamp_view();
            }
        }

        /// Segments with the dragged one at its ghost position.
        fn drawn_segments(&self) -> Vec<SegmentInfo> {
            let mut segments = self.segments.borrow().clone();
            if let Some(ghost) = self.ghost.get() {
                if let Some(segment) = segments.get_mut(ghost.index) {
                    segment.start = ghost.start;
                    segment.duration = ghost.duration;
                    segment.speed = ghost.speed;
                }
            }
            segments
        }

        fn draw_ruler(&self, snapshot: &gtk::Snapshot, width: f32, fg: &gdk::RGBA) {
            // Pick a tick interval that leaves at least 70 px between labels.
            let steps = [
                100_000i64,
                250_000,
                500_000,
                1_000_000,
                2_000_000,
                5_000_000,
                10_000_000,
                15_000_000,
                30_000_000,
                60_000_000,
                120_000_000,
                300_000_000,
                600_000_000,
            ];
            let step = steps
                .into_iter()
                .find(|step| *step as f64 / self.us_per_px() >= 70.)
                .unwrap_or(1_800_000_000);
            let mut tick = self.view_start.get() / step * step;
            let end = self.view_start.get() + self.visible_duration();
            while tick <= end {
                let x = self.x_of(tick);
                snapshot.append_color(
                    &with_alpha(fg, 0.4),
                    &graphene::Rect::new(x, RULER_HEIGHT - 5., 1., 5.),
                );
                let seconds = tick / 1_000_000;
                let label = if step < 1_000_000 {
                    format!(
                        "{}:{:02}.{}",
                        seconds / 60,
                        seconds % 60,
                        (tick / 100_000) % 10
                    )
                } else {
                    format!("{}:{:02}", seconds / 60, seconds % 60)
                };
                let layout = self.obj().create_pango_layout(Some(&label));
                let mut font = gtk::pango::FontDescription::new();
                font.set_size(7 * gtk::pango::SCALE);
                layout.set_font_description(Some(&font));
                if x + 3. < width {
                    snapshot.save();
                    snapshot.translate(&graphene::Point::new(x + 3., 1.));
                    snapshot.append_layout(&layout, &with_alpha(fg, 0.6));
                    snapshot.restore();
                }
                tick += step;
            }
        }

        /// The segment and edge under the pointer, preferring edges.
        fn hit(&self, x: f64, y: f64) -> Option<(usize, Option<SegmentEdge>)> {
            if y < RULER_HEIGHT as f64 {
                return None;
            }
            let segments = self.segments.borrow();
            let mut best: Option<(usize, Option<SegmentEdge>, f64)> = None;
            for (index, segment) in segments.iter().enumerate() {
                let x0 = self.x_of(segment.start) as f64;
                let x1 = self.x_of(segment.end()) as f64;
                for (edge, edge_x) in [(SegmentEdge::Start, x0), (SegmentEdge::End, x1)] {
                    let distance = (x - edge_x).abs();
                    // An edge only counts from inside its segment, so touching segments don't
                    // steal each other's edges.
                    let inside = match edge {
                        SegmentEdge::Start => x >= x0 - 1.,
                        SegmentEdge::End => x <= x1 + 1.,
                    };
                    if distance <= EDGE_TOLERANCE
                        && inside
                        && best.is_none_or(|(_, _, d)| distance < d)
                    {
                        best = Some((index, Some(edge), distance));
                    }
                }
            }
            if let Some((index, edge, _)) = best {
                return Some((index, edge));
            }
            segments
                .iter()
                .enumerate()
                .rev()
                .find(|(_, segment)| {
                    let x0 = self.x_of(segment.start) as f64;
                    let x1 = self.x_of(segment.end()) as f64;
                    x0 <= x && x <= x1
                })
                .map(|(index, _)| (index, None))
        }

        fn select(&self, index: Option<usize>) {
            if self.selected.get() != index {
                self.selected.set(index);
                self.obj().emit_by_name::<()>("selection-changed", &[]);
                self.obj().queue_draw();
            }
        }

        fn on_drag_begin(&self, x: f64, y: f64, ctrl: bool) {
            self.drag_x.set(x);
            let drag = match self.hit(x, y) {
                None => {
                    self.select(None);
                    Drag::Scrub
                }
                Some((index, Some(edge))) => {
                    self.select(Some(index));
                    if ctrl {
                        Drag::Speed { index, edge }
                    } else {
                        Drag::Trim { index, edge }
                    }
                }
                Some((index, None)) => {
                    self.select(Some(index));
                    Drag::Pending { index }
                }
            };
            self.drag.set(drag);
            if drag == Drag::Scrub {
                self.scrub_to(x, true);
            }
        }

        fn on_drag_update(&self, dx: f64) {
            let x = self.drag_x.get() + dx;
            match self.drag.get() {
                Drag::None => {}
                Drag::Scrub => self.scrub_to(x, true),
                Drag::Pending { index } => {
                    if dx.abs() >= DRAG_THRESHOLD {
                        let start = self.segments.borrow()[index].start;
                        let grab = self.time_of(self.drag_x.get()) - start;
                        self.drag.set(Drag::Move { index, grab });
                        self.on_drag_update(dx);
                    }
                }
                Drag::Move { index, grab } => self.ghost_move(index, self.time_of(x) - grab),
                Drag::Trim { index, edge } => self.ghost_trim(index, edge, self.time_of(x)),
                Drag::Speed { index, edge } => self.ghost_speed(index, edge, self.time_of(x)),
            }
        }

        fn on_drag_end(&self, dx: f64) {
            let drag = self.drag.replace(Drag::None);
            let ghost = self.ghost.take();
            let engine = self.engine();
            match drag {
                Drag::None => {}
                Drag::Scrub => self.scrub_to(self.drag_x.get() + dx, false),
                // A click on a segment selects it and moves the playhead there.
                Drag::Pending { .. } => self.scrub_to(self.drag_x.get(), false),
                Drag::Move { index, .. } => {
                    if let Some(ghost) = ghost {
                        engine.move_segment(index, ghost.start);
                    }
                }
                Drag::Trim { index, edge } => {
                    if let Some(ghost) = ghost {
                        let position = match edge {
                            SegmentEdge::Start => ghost.start,
                            SegmentEdge::End => ghost.start + ghost.duration,
                        };
                        engine.trim_segment(index, edge, position);
                    }
                }
                Drag::Speed { index, edge } => {
                    if let Some(ghost) = ghost {
                        let fixed = match edge {
                            SegmentEdge::Start => SegmentEdge::End,
                            SegmentEdge::End => SegmentEdge::Start,
                        };
                        engine.set_segment_speed(index, ghost.speed, fixed);
                    }
                }
            }
            self.obj().queue_draw();
        }

        fn scrub_to(&self, x: f64, fast: bool) {
            let time = self.time_of(x).clamp(0, self.total.get());
            self.position.set(time);
            if fast {
                self.engine().seek_fast(time);
            } else {
                self.engine().seek(time);
            }
            self.obj().queue_draw();
        }

        /// Snaps `time` to the nearest edge of another segment or the playhead.
        fn snap(&self, time: i64, exclude: usize) -> Option<i64> {
            let tolerance = (SNAP_DISTANCE * self.us_per_px()) as i64;
            let segments = self.segments.borrow();
            let mut anchors = vec![0, self.position.get()];
            if self.social_snap.get() {
                anchors.push(SOCIAL_LENGTH);
            }
            for (index, segment) in segments.iter().enumerate() {
                if index != exclude {
                    anchors.push(segment.start);
                    anchors.push(segment.end());
                }
            }
            anchors
                .into_iter()
                .filter(|anchor| (time - anchor).abs() <= tolerance)
                .min_by_key(|anchor| (time - anchor).abs())
        }

        fn ghost_move(&self, index: usize, start: i64) {
            let segment = self.segments.borrow()[index].clone();
            let mut start = start.max(0);
            if let Some(anchor) = self.snap(start, index) {
                start = anchor;
            } else if let Some(anchor) = self.snap(start + segment.duration, index) {
                start = anchor - segment.duration;
            }
            self.set_ghost(index, start.max(0), segment.duration, segment.speed);
        }

        fn ghost_trim(&self, index: usize, edge: SegmentEdge, time: i64) {
            let segment = self.segments.borrow()[index].clone();
            let time = self.snap(time, index).unwrap_or(time);
            match edge {
                SegmentEdge::Start => {
                    let start = time
                        .clamp(
                            segment.start - segment.extend_left,
                            segment.end() - MIN_DURATION,
                        )
                        .max(0);
                    self.set_ghost(index, start, segment.end() - start, segment.speed);
                }
                SegmentEdge::End => {
                    let end = time.clamp(
                        segment.start + MIN_DURATION,
                        segment.end() + segment.extend_right,
                    );
                    self.set_ghost(index, segment.start, end - segment.start, segment.speed);
                }
            }
        }

        fn ghost_speed(&self, index: usize, edge: SegmentEdge, time: i64) {
            let segment = self.segments.borrow()[index].clone();
            // Source time the segment plays, which the speed change keeps.
            let span = segment.duration as f64 * segment.speed;
            let length = match edge {
                SegmentEdge::End => time - segment.start,
                SegmentEdge::Start => segment.end() - time,
            };
            let mut speed = if length > 0 {
                (span / length as f64).clamp(MIN_SPEED, MAX_SPEED)
            } else {
                MAX_SPEED
            };
            if (speed - 1.).abs() < SPEED_SNAP {
                speed = 1.;
            }
            let duration = (span / speed) as i64;
            let start = match edge {
                SegmentEdge::End => segment.start,
                SegmentEdge::Start => (segment.end() - duration).max(0),
            };
            self.set_ghost(index, start, duration, speed);
        }

        fn set_ghost(&self, index: usize, start: i64, duration: i64, speed: f64) {
            self.ghost.set(Some(Ghost {
                index,
                start,
                duration,
                speed,
            }));
            self.obj().queue_draw();
        }

        fn update_cursor(&self, x: f64, y: f64, ctrl: bool) {
            if self.gesture.get().unwrap().is_active() {
                return;
            }
            let name = match self.hit(x, y) {
                Some((_, Some(_))) if ctrl => "ew-resize",
                Some((_, Some(_))) => "col-resize",
                Some((_, None)) => "grab",
                None => "default",
            };
            self.obj().set_cursor_from_name(Some(name));
        }

        fn on_scroll(&self, dx: f64, dy: f64, ctrl: bool) -> glib::Propagation {
            if ctrl {
                // Zoom around the playhead.
                let anchor = self.position.get();
                let anchor_x = self.x_of(anchor) as f64;
                let zoom = (self.zoom.get() * 1.15f64.powf(-dy)).clamp(1., MAX_ZOOM);
                self.zoom.set(zoom);
                let start = anchor - (anchor_x * self.us_per_px()) as i64;
                self.view_start.set(start);
            } else if self.zoom.get() > 1. {
                let delta = if dx != 0. { dx } else { dy };
                let shift = (delta * self.visible_duration() as f64 * 0.1) as i64;
                self.view_start.set(self.view_start.get() + shift);
            } else {
                return glib::Propagation::Proceed;
            }
            self.clamp_view();
            self.obj().queue_draw();
            glib::Propagation::Stop
        }
    }

    fn draw_spring(
        snapshot: &gtk::Snapshot,
        x0: f32,
        x1: f32,
        middle: f32,
        amplitude: f32,
        coils: usize,
    ) {
        let builder = gsk::PathBuilder::new();
        builder.move_to(x0, middle);
        let steps = coils * 2;
        for step in 0..steps {
            let x = x0 + (x1 - x0) * (step as f32 + 0.5) / steps as f32;
            let y = if step % 2 == 0 {
                middle - amplitude
            } else {
                middle + amplitude
            };
            builder.line_to(x, y);
        }
        builder.line_to(x1, middle);
        snapshot.append_stroke(
            &builder.to_path(),
            &gsk::Stroke::new(1.5),
            &gdk::RGBA::WHITE,
        );
    }

    fn with_alpha(color: &gdk::RGBA, alpha: f32) -> gdk::RGBA {
        gdk::RGBA::new(
            color.red(),
            color.green(),
            color.blue(),
            color.alpha() * alpha,
        )
    }
}

glib::wrapper! {
    pub struct VtEditorTimeline(ObjectSubclass<imp::VtEditorTimeline>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl VtEditorTimeline {
    pub fn set_engine(&self, engine: &VtEngine) {
        self.imp().set_engine(engine);
    }

    pub fn refresh(&self) {
        self.imp().refresh();
    }

    pub fn selected(&self) -> Option<usize> {
        self.imp().selected()
    }

    pub fn split_at_playhead(&self) -> bool {
        self.imp().split_at_playhead()
    }

    pub fn delete_current(&self) {
        self.imp().delete_current();
    }

    pub fn set_end_fade(&self, enabled: bool) {
        self.imp().set_end_fade(enabled);
    }

    pub fn set_social_snap(&self, enabled: bool) {
        self.imp().set_social_snap(enabled);
    }

    /// The visible span: its start and length in µs.
    pub fn view(&self) -> (i64, i64) {
        self.imp().view()
    }
}
