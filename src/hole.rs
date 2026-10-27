// 亮洞：选区里那块没被压暗的画面
//
// 洞里的内容就是本屏画面的原样，只是它盖在压暗层上面。这里走纹理节点：
// 抓屏纹理上传一次就常驻显卡，每帧只是让合成器把其中一小块采出来，CPU 一个
// 像素都不碰。
//
// 以前是标注层用 cairo 把物理分辨率的 ImageSurface 贴上去：每帧都要在 CPU 上
// 把洞里那几百万像素再拷一遍，而且得自己算物理→逻辑→全局→画布这几层坐标，
// 算错一点就是半个屏幕的偏移。纹理节点只要说清楚「整屏画面在本层坐标系的哪个
// 矩形里」，剩下的变换和底下那层 GtkPicture 是同一套，天然对得上。
use gtk::gdk;
use gtk::glib;
use gtk::graphene;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use std::cell::{Cell, RefCell};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Hole {
        pub texture: RefCell<Option<gdk::Texture>>,
        /// 整屏画面的左上角在本层坐标系里的位置：本层一般不在屏幕原点，所以是负的
        pub at: Cell<(f64, f64)>,
        /// 整屏画面的逻辑尺寸
        pub area: Cell<(f64, f64)>,
        /// 已经打过几次「布局没落地」的日志
        pub mismatches: Cell<u32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Hole {
        const NAME: &'static str = "FocusSnapsHole";
        type Type = super::Hole;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Hole {}

    impl WidgetImpl for Hole {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let Some(tex) = self.texture.borrow().clone() else {
                return;
            };
            let (ax, ay) = self.actual_at();
            let (aw, ah) = self.area.get();
            let (w, h) = self.size();
            if aw <= 0.0 || ah <= 0.0 || w <= 0.0 || h <= 0.0 {
                return;
            }
            // 必须自己裁：要贴的是整屏那么大一块，不裁就把旁边的压暗也点亮了
            snapshot.push_clip(&graphene::Rect::new(0.0, 0.0, w as f32, h as f32));
            snapshot.append_texture(
                &tex,
                &graphene::Rect::new(-ax as f32, -ay as f32, aw as f32, ah as f32),
            );
            snapshot.pop();
        }
    }

    impl Hole {
        fn size(&self) -> (f64, f64) {
            let w = self.obj().width();
            let h = self.obj().height();
            (f64::from(w), f64::from(h))
        }

        /// 本层「实际」落在屏幕的哪个位置
        ///
        /// 优先问布局要去哪了：拖得很快时我们设的位置和实际分配可能差一帧，
        /// 画面要是按设的位置贴，就会和边框错开一格。
        fn actual_at(&self) -> (f64, f64) {
            let want = self.at.get();
            let Some(parent) = self.obj().parent() else {
                return want;
            };
            let Some(b) = self.obj().compute_bounds(&parent) else {
                return want;
            };
            let got = (f64::from(b.x()), f64::from(b.y()));
            let want_size = (
                f64::from(self.obj().width_request()),
                f64::from(self.obj().height_request()),
            );
            let got_size = (f64::from(b.width()), f64::from(b.height()));
            // 差多少只在排查时打，正常情况下两者一致
            let off = (got.0 - want.0).abs() > 0.5
                || (got.1 - want.1).abs() > 0.5
                || (got_size.0 - want_size.0).abs() > 0.5
                || (got_size.1 - want_size.1).abs() > 0.5;
            if off && self.mismatches.replace(self.mismatches.get() + 1) < 5 {
                crate::overlay::trace(&format!(
                    "洞的布局还没落地：设的 {want:?}@{want_size:?} 实际 {got:?}@{got_size:?}"
                ));
            }
            got
        }
    }
}

glib::wrapper! {
    pub struct Hole(ObjectSubclass<imp::Hole>) @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Hole {
    fn default() -> Self {
        glib::Object::builder().build()
    }
}

impl Hole {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_texture(&self, texture: Option<&gdk::Texture>) {
        *self.imp().texture.borrow_mut() = texture.cloned();
        self.queue_draw();
    }

    /// 说明本层贴在屏幕的哪个位置、整屏画面有多大
    ///
    /// 位置一变整层内容就整体挪，得重画。
    pub fn place(&self, at: (f64, f64), area: (f64, f64)) {
        let imp = self.imp();
        if imp.at.get() == at && imp.area.get() == area {
            return;
        }
        imp.at.set(at);
        imp.area.set(area);
        self.queue_draw();
    }
}
