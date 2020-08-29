use gdk::prelude::*;
use glib::{subclass, subclass::prelude::*, translate::*};

#[derive(Debug)]
pub struct VtVideoPreviewPrivate {}

impl ObjectSubclass for VtVideoPreviewPrivate {
    const NAME: &'static str = "VtVideoPreview";
    type ParentType = glib::Object;
    type Instance = subclass::simple::InstanceStruct<Self>;
    type Class = subclass::simple::ClassStruct<Self>;

    glib_object_subclass!();

    fn new() -> Self {
        Self {}
    }
}

impl ObjectImpl for VtVideoPreviewPrivate {
    glib_object_impl!();
}

glib_wrapper! {
    pub struct VtVideoPreview(
        Object<
            subclass::simple::InstanceStruct<VtVideoPreviewPrivate>,
            subclass::simple::ClassStruct<VtVideoPreviewPrivate>,
            VtVideoPreviewClass
        >
    );

    match fn {
        get_type => || VtVideoPreviewPrivate::get_type().to_glib(),
    }
}

impl VtVideoPreview {
    pub fn new() -> Self {
        glib::Object::new(Self::static_type(), &[])
            .unwrap()
            .downcast()
            .unwrap()
    }
}
