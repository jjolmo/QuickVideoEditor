use gdk::prelude::*;
use glib::{subclass, subclass::prelude::*, translate::*};
use once_cell::unsync::OnceCell;

static PROPERTIES: [subclass::Property; 1] = [subclass::Property("builder", |name| {
    glib::ParamSpec::object(
        name,
        "builder",
        "builder",
        gtk::Builder::static_type(),
        glib::ParamFlags::READWRITE | glib::ParamFlags::CONSTRUCT_ONLY,
    )
})];

#[derive(Debug)]
pub struct VtVideoPreviewPrivate {
    builder: OnceCell<gtk::Builder>,
}

impl ObjectSubclass for VtVideoPreviewPrivate {
    const NAME: &'static str = "VtVideoPreview";
    type ParentType = glib::Object;
    type Instance = subclass::simple::InstanceStruct<Self>;
    type Class = subclass::simple::ClassStruct<Self>;

    glib_object_subclass!();

    fn new() -> Self {
        Self {
            builder: OnceCell::new(),
        }
    }

    fn class_init(klass: &mut Self::Class) {
        klass.install_properties(&PROPERTIES);
    }
}

impl ObjectImpl for VtVideoPreviewPrivate {
    glib_object_impl!();

    fn set_property(&self, _obj: &glib::Object, id: usize, value: &glib::Value) {
        let prop = &PROPERTIES[id];

        match *prop {
            subclass::Property("builder", ..) => {
                self.builder.set(value.get().unwrap().unwrap()).unwrap()
            }
            _ => unreachable!(),
        }
    }

    fn get_property(&self, _obj: &glib::Object, id: usize) -> Result<glib::Value, ()> {
        let prop = &PROPERTIES[id];

        match *prop {
            subclass::Property("builder", ..) => Ok(self.builder.get().unwrap().to_value()),
            _ => unreachable!(),
        }
    }
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
    pub fn new(builder: &gtk::Builder) -> Self {
        glib::Object::new(Self::static_type(), &[("builder", builder)])
            .unwrap()
            .downcast()
            .unwrap()
    }
}
