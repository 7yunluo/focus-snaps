// 浮层工具条、颜色、线宽与文字输入框
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::cairo;
use gtk::gdk;
use gtk::prelude::*;

use crate::doc::{Doc, Tool, DEFAULT_COLOR};
use crate::icons;

pub type Shared = Rc<RefCell<Doc>>;

/// 工具条上用到的外部动作
pub struct Actions {
    pub on_copy: Rc<dyn Fn()>,
    pub on_save: Rc<dyn Fn()>,
    pub on_cancel: Rc<dyn Fn()>,
}

/// 调色板每行的色块数
const PALETTE_COLS: usize = 8;

/// 调色板大致高度，只用来判断摆在工具条上方还是下方
const PALETTE_EST_H: f64 = 96.0;

/// 工具条大致高度，仅在还没完成分配时用
const BAR_EST_H: f64 = 60.0;

/// 调色板颜色
const PALETTE: [((f64, f64, f64), &str); 24] = [
    ((1.0, 1.0, 1.0), "#ffffff"),
    ((0.78, 0.78, 0.78), "#c8c8c8"),
    ((0.5, 0.5, 0.5), "#808080"),
    ((0.12, 0.12, 0.12), "#1f1f1f"),
    ((1.0, 0.32, 0.28), "#ff5247"),
    ((1.0, 0.54, 0.24), "#ff8a3d"),
    ((1.0, 0.78, 0.2), "#ffc733"),
    ((1.0, 0.91, 0.3), "#ffe94d"),
    ((0.71, 0.91, 0.29), "#b6e84a"),
    ((0.35, 0.85, 0.42), "#59d96b"),
    ((0.18, 0.75, 0.61), "#2fbf9b"),
    ((0.21, 0.79, 0.84), "#35c9d6"),
    ((0.32, 0.66, 1.0), "#52a8ff"),
    ((0.24, 0.42, 1.0), "#3d6bff"),
    ((0.42, 0.36, 1.0), "#6b5bff"),
    ((0.64, 0.36, 1.0), "#a35bff"),
    ((1.0, 0.36, 0.69), "#ff5bb0"),
    ((1.0, 0.3, 0.55), "#ff4d8d"),
    ((1.0, 0.48, 0.56), "#ff7a90"),
    ((0.66, 0.44, 0.29), "#a9714b"),
    ((0.88, 0.79, 0.65), "#e0c9a6"),
    ((0.66, 0.9, 0.81), "#a8e6cf"),
    ((0.82, 0.91, 1.0), "#d0e8ff"),
    ((1.0, 0.85, 0.75), "#ffd9c0"),
];

pub struct Ui {
    pub root: gtk::Box,
    /// 手工拖动后的位置（逻辑坐标），None 表示跟着选区自动摆位
    manual: Rc<Cell<Option<(f64, f64)>>>,
    pub entry: gtk::Entry,
    pub hint: gtk::Label,
    /// 调色板
    pub palette: gtk::Box,
    /// 当前颜色的圆点
    dot: gtk::DrawingArea,
    /// 当前颜色
    color: Rc<Cell<(f64, f64, f64)>>,
    /// 调色板里的色块按钮与各自的颜色
    swatches: Rc<RefCell<Vec<(gtk::Button, (f64, f64, f64))>>>,
    tool_buttons: Vec<gtk::ToggleButton>,
    /// 线宽 / 文字字号的调节器
    pub width_spin: gtk::SpinButton,
    /// 缓存的自然尺寸，避免每次拖动都重新 measure
    natural: Cell<Option<(f64, f64)>>,
}

impl Ui {
    pub fn build(doc: &Shared, area: &gtk::DrawingArea, actions: &Actions) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 2);
        root.add_css_class("fs-bar");
        root.set_halign(gtk::Align::Start);
        root.set_valign(gtk::Align::Start);
        root.set_visible(false);

        // 文字输入框：透明、无边框，就地显示正在输入的文字
        let entry = gtk::Entry::new();
        entry.add_css_class("fs-entry");
        // Adwaita 的 entry 背景/边框样式靠 flat 去掉
        entry.add_css_class("flat");
        entry.set_visible(false);
        entry.set_halign(gtk::Align::Start);
        entry.set_valign(gtk::Align::Start);
        entry.set_width_chars(2);
        style_entry(&entry, DEFAULT_COLOR, 4.0);
        {
            let area = area.clone();
            let e = entry.clone();
            entry.connect_changed(move |_| {
                fit_entry(&e);
                area.queue_draw();
            });
        }

        // 当前颜色与调色板
        let color: Rc<Cell<(f64, f64, f64)>> = Rc::new(Cell::new(DEFAULT_COLOR));
        let dot = gtk::DrawingArea::new();
        dot.set_content_width(14);
        dot.set_content_height(14);
        {
            let color = color.clone();
            dot.set_draw_func(move |_d, cr, w, h| draw_dot(cr, w, h, color.get()));
        }
        let palette = gtk::Box::new(gtk::Orientation::Vertical, 4);
        palette.add_css_class("fs-palette");
        palette.set_halign(gtk::Align::Start);
        palette.set_valign(gtk::Align::Start);
        palette.set_visible(false);
        let swatches: Rc<RefCell<Vec<(gtk::Button, (f64, f64, f64))>>> =
            Rc::new(RefCell::new(Vec::new()));
        for row in PALETTE.chunks(PALETTE_COLS) {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 3);
            for (rgba, hex) in row {
                let btn = gtk::Button::new();
                btn.add_css_class("fs-swatch");
                btn.set_focusable(false);
                btn.set_can_focus(false);
                btn.set_tooltip_text(Some(hex));
                let da = gtk::DrawingArea::new();
                da.set_content_width(13);
                da.set_content_height(13);
                let c = *rgba;
                da.set_draw_func(move |_d, cr, w, h| draw_dot(cr, w, h, c));
                btn.set_child(Some(&da));
                {
                    let doc = doc.clone();
                    let color = color.clone();
                    let dot = dot.clone();
                    let swatches = swatches.clone();
                    let palette = palette.clone();
                    let entry = entry.clone();
                    btn.connect_clicked(move |_| {
                        let width = {
                            let mut d = doc.borrow_mut();
                            d.color = c;
                            d.width
                        };
                        style_entry(&entry, c, width);
                        if entry.get_visible() {
                            entry.grab_focus();
                        }
                        color.set(c);
                        dot.queue_draw();
                        for (b, sc) in swatches.borrow().iter() {
                            if *sc == c {
                                b.add_css_class("fs-swatch-on");
                            } else {
                                b.remove_css_class("fs-swatch-on");
                            }
                        }
                        palette.set_visible(false);
                    });
                }
                line.append(&btn);
                swatches.borrow_mut().push((btn, c));
            }
            palette.append(&line);
        }

        // 第一行：拖动把手 + 工具
        let row_tools = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        let manual: Rc<std::cell::Cell<Option<(f64, f64)>>> = Rc::new(std::cell::Cell::new(None));
        {
            let grip = gtk::Label::new(Some("⠿"));
            grip.add_css_class("fs-grip");
            grip.set_tooltip_text(Some("拖动工具条"));
            grip.set_cursor_from_name(Some("grab"));
            grip.set_can_focus(false);
            let drag = gtk::GestureDrag::new();
            drag.set_button(1);
            // 拖动起点：指针位置 + 当时边距
            let start: Rc<Cell<Option<(f64, f64, f64, f64)>>> = Rc::new(Cell::new(None));
            let root2 = root.clone();
            let manual2 = manual.clone();
            let area2 = area.clone();
            drag.connect_drag_begin({
                let start = start.clone();
                let root = root.clone();
                let palette = palette.clone();
                move |g, _x, _y| {
                    palette.set_visible(false);
                    start.set(grip_pointer(g).map(|(px, py)| {
                        (
                            px,
                            py,
                            f64::from(root.margin_start()),
                            f64::from(root.margin_top()),
                        )
                    }));
                }
            });
            // 位置按指针位移算，不用手势自带的偏移
            drag.connect_drag_update(move |g, _ox, _oy| {
                let Some((px0, py0, mx, my)) = start.get() else {
                    return;
                };
                let Some((px, py)) = grip_pointer(g) else {
                    return;
                };
                let aw = f64::from(area2.width());
                let ah = f64::from(area2.height());
                let w = f64::from(root2.width());
                let h = f64::from(root2.height());
                let nx = (mx + (px - px0)).clamp(0.0, (aw - w).max(0.0));
                let ny = (my + (py - py0)).clamp(0.0, (ah - h).max(0.0));
                root2.set_margin_start(nx.round() as i32);
                root2.set_margin_top(ny.round() as i32);
                manual2.set(Some((nx, ny)));
                if std::env::var("FOCUSSNAPS_TEST_POINTER").is_ok() {
                    eprintln!("TOOLBAR {nx:.0} {ny:.0}");
                }
            });
            grip.add_controller(drag);
            row_tools.append(&grip);
        }
        row_tools.add_css_class("fs-row");
        let mut tool_buttons = Vec::with_capacity(Tool::ALL.len());
        let mut group: Option<gtk::ToggleButton> = None;
        for tool in Tool::ALL {
            let btn = gtk::ToggleButton::new();
            btn.add_css_class("fs-tool");
            let icon = gtk::DrawingArea::new();
            icon.set_content_width(16);
            icon.set_content_height(16);
            icon.set_draw_func(move |_d, cr, w, h| {
                icons::draw_tool(cr, tool, (f64::from(w), f64::from(h)))
            });
            btn.set_child(Some(&icon));
            btn.set_tooltip_text(Some(&format!(
                "{}（{}）\n{}",
                tool.label(),
                tool.shortcut().to_ascii_uppercase(),
                tool.tip()
            )));
            btn.set_focusable(false);
            btn.set_can_focus(false);
            if let Some(prev) = &group {
                btn.set_group(Some(prev));
            }
            if tool == Tool::Select {
                btn.set_active(true);
            }
            group = Some(btn.clone());
            {
                let doc = doc.clone();
                let area = area.clone();
                btn.connect_toggled(move |b| {
                    if b.is_active() {
                        let mut d = doc.borrow_mut();
                        d.tool = tool;
                        d.text_sel = None;
                        drop(d);
                        area.queue_draw();
                    }
                });
            }
            row_tools.append(&btn);
            tool_buttons.push(btn);
        }
        root.append(&row_tools);

        // 第二行：颜色、线宽、操作
        let row_opts = gtk::Box::new(gtk::Orientation::Horizontal, 3);
        row_opts.add_css_class("fs-row");
        let color_btn = gtk::Button::new();
        color_btn.add_css_class("fs-color");
        color_btn.set_child(Some(&dot));
        color_btn.set_focusable(false);
        color_btn.set_can_focus(false);
        color_btn.set_tooltip_text(Some("颜色"));
        {
            let palette = palette.clone();
            let root = root.clone();
            let area = area.clone();
            color_btn.connect_clicked(move |_| {
                toggle_palette(
                    &palette,
                    &root,
                    (f64::from(area.width()), f64::from(area.height())),
                )
            });
        }
        row_opts.append(&color_btn);

        row_opts.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        let spin = gtk::SpinButton::with_range(1.0, 24.0, 1.0);
        spin.set_value(4.0);
        spin.set_tooltip_text(Some("线宽"));
        spin.set_can_focus(false);
        {
            let doc = doc.clone();
            let area = area.clone();
            let entry = entry.clone();
            spin.connect_value_changed(move |s| {
                let (color, width) = {
                    let mut d = doc.borrow_mut();
                    d.width = s.value();
                    (d.color, d.width)
                };
                style_entry(&entry, color, width);
                fit_entry(&entry);
                if entry.get_visible() {
                    entry.grab_focus();
                }
                area.queue_draw();
            });
        }
        row_opts.append(&spin);
        row_opts.append(&gtk::Separator::new(gtk::Orientation::Vertical));

        for (label, tip) in [("撤销", "撤销（Ctrl+Z）"), ("重做", "重做（Ctrl+Shift+Z）")] {
            let btn = gtk::Button::with_label(label);
            btn.set_focusable(false);
            btn.set_can_focus(false);
            btn.set_tooltip_text(Some(tip));
            {
                let doc = doc.clone();
                let area = area.clone();
                let redo = label == "重做";
                btn.connect_clicked(move |_| {
                    let changed = if redo {
                        doc.borrow_mut().redo()
                    } else {
                        doc.borrow_mut().undo()
                    };
                    if changed {
                        area.queue_draw();
                    }
                });
            }
            row_opts.append(&btn);
        }
        row_opts.append(&gtk::Separator::new(gtk::Orientation::Vertical));

        let copy_btn = gtk::Button::with_label("复制");
        copy_btn.add_css_class("fs-primary");
        copy_btn.set_focusable(false);
        copy_btn.set_can_focus(false);
        copy_btn.set_tooltip_text(Some("复制到剪贴板（回车/空格/双击）"));
        {
            let on_copy = actions.on_copy.clone();
            copy_btn.connect_clicked(move |_| on_copy());
        }
        row_opts.append(&copy_btn);

        let save_btn = gtk::Button::with_label("保存");
        save_btn.set_focusable(false);
        save_btn.set_can_focus(false);
        save_btn.set_tooltip_text(Some("保存到图片目录（Ctrl+S）"));
        {
            let on_save = actions.on_save.clone();
            save_btn.connect_clicked(move |_| on_save());
        }
        row_opts.append(&save_btn);

        let cancel_btn = gtk::Button::with_label("取消");
        cancel_btn.set_focusable(false);
        cancel_btn.set_can_focus(false);
        cancel_btn.set_tooltip_text(Some("退出截图（Esc）"));
        {
            let on_cancel = actions.on_cancel.clone();
            cancel_btn.connect_clicked(move |_| on_cancel());
        }
        row_opts.append(&cancel_btn);

        root.append(&row_opts);

        let hint = gtk::Label::new(Some("拖动框选或点选窗口 · 回车/双击复制 · Ctrl+S 保存 · Esc 退出"));
        hint.set_can_target(false);
        hint.add_css_class("fs-hint");
        hint.set_halign(gtk::Align::Center);
        hint.set_valign(gtk::Align::End);
        hint.set_margin_bottom(48);

        Self {
            root,
            manual,
            entry,
            hint,
            palette,
            dot,
            color,
            swatches,
            tool_buttons,
            width_spin: spin,
            natural: Cell::new(None),
        }
    }

    /// 让按钮状态跟上当前工具
    pub fn sync_tool(&self, tool: Tool) {
        if let Some(i) = Tool::ALL.iter().position(|t| *t == tool) {
            if !self.tool_buttons[i].is_active() {
                self.tool_buttons[i].set_active(true);
            }
        }
    }

    /// 把工具条摆在选区附近，放不下就压进选区内部
    pub fn place(&self, sel: (f64, f64, f64, f64), area: (f64, f64)) {
        // 用户拖过就保持在手放的位置，只做边界钳制
        if let Some((mx, my)) = self.manual.get() {
            let w = f64::from(self.root.width());
            let h = f64::from(self.root.height());
            let nx = mx.min((area.0 - w).max(0.0)).max(0.0);
            let ny = my.min((area.1 - h).max(0.0)).max(0.0);
            if nx != mx || ny != my {
                self.manual.set(Some((nx, ny)));
            }
            self.root.set_margin_start(nx.round() as i32);
            self.root.set_margin_top(ny.round() as i32);
            return;
        }
        let (nat_w, nat_h) = match self.natural.get() {
            Some(v) => v,
            None => {
                let v = (
                    self.root.measure(gtk::Orientation::Horizontal, -1).1 as f64,
                    self.root.measure(gtk::Orientation::Vertical, -1).1 as f64,
                );
                if v.0 > 0.0 && v.1 > 0.0 {
                    self.natural.set(Some(v));
                }
                v
            }
        };
        let gap = 10.0;
        let (x0, y0, _x1, y1) = sel;

        let x = if x0 + nat_w <= area.0 - 8.0 {
            x0
        } else {
            (area.0 - nat_w - 8.0).max(4.0)
        };
        let y = if y1 + gap + nat_h <= area.1 - 8.0 {
            y1 + gap
        } else if y0 - gap - nat_h >= 4.0 {
            y0 - gap - nat_h
        } else if y1 - nat_h - gap >= 4.0 {
            y1 - nat_h - gap
        } else {
            (y1 - nat_h - 4.0).max(4.0)
        };
        if std::env::var("FOCUSSNAPS_TEST_POINTER").is_ok() {
            eprintln!(
                "PLACE sel={sel:?} area={area:?} nat={nat_w:.0}x{nat_h:.0} -> ({x:.0},{y:.0})"
            );
        }
        self.root.set_margin_start(x.round() as i32);
        self.root.set_margin_top(y.round() as i32);
    }

    /// 高亮当前颜色
    pub fn sync_color(&self, color: (f64, f64, f64)) {
        self.color.set(color);
        self.dot.queue_draw();
        style_entry(&self.entry, color, self.width_spin.value());
        for (btn, c) in self.swatches.borrow().iter() {
            if *c == color {
                btn.add_css_class("fs-swatch-on");
            } else {
                btn.remove_css_class("fs-swatch-on");
            }
        }
    }
    /// 展开或收起调色板
    pub fn toggle_palette(&self, area: (f64, f64)) {
        toggle_palette(&self.palette, &self.root, area);
    }
}

/// 指针在窗口坐标里的位置，控件自身移动不会影响它
fn grip_pointer(drag: &gtk::GestureDrag) -> Option<(f64, f64)> {
    drag.current_event()?.position()
}

/// 画一个圆形色块
fn draw_dot(cr: &cairo::Context, w: i32, h: i32, c: (f64, f64, f64)) {
    let r = f64::from(w.min(h)) / 2.0 - 0.5;
    cr.arc(
        f64::from(w) / 2.0,
        f64::from(h) / 2.0,
        r,
        0.0,
        std::f64::consts::TAU,
    );
    cr.set_source_rgb(c.0, c.1, c.2);
    let _ = cr.fill();
}

/// 在工具条下方展开或收起调色板
fn toggle_palette(palette: &gtk::Box, root: &gtk::Box, area: (f64, f64)) {
    if palette.is_visible() {
        palette.set_visible(false);
        return;
    }
    place_palette(palette, root, area);
    palette.set_visible(true);
}

/// 把调色板摆在工具条下方，下方放不下就贴到工具条上方
fn place_palette(palette: &gtk::Box, root: &gtk::Box, area: (f64, f64)) {
    // 位置只用工具条的位置和窗口尺寸算：调色板还没显示时量不到自己的高度，
    // 上下两端交给 valign，让 GTK 自己按它的实际高度摆放
    let ah = area.1;
    let top = f64::from(root.margin_top());
    let bar_h = match root.height() {
        0 => BAR_EST_H,
        h => f64::from(h),
    };
    let below = top + bar_h + 6.0;
    // 下方够高就贴工具条下面，否则让 GTK 从窗口底部往上摆
    let (valign, margin_top, margin_bottom) = if below + PALETTE_EST_H <= ah - 4.0 {
        (gtk::Align::Start, below, 0.0)
    } else {
        (gtk::Align::End, 0.0, (ah - top + 6.0).max(0.0))
    };
    palette.set_valign(valign);
    palette.set_margin_start(root.margin_start());
    palette.set_margin_top(margin_top.round() as i32);
    palette.set_margin_bottom(margin_bottom.round() as i32);
    if std::env::var("FOCUSSNAPS_TEST_POINTER").is_ok() {
        eprintln!(
            "PALETTE {valign:?} bar_h={bar_h:.0} top={margin_top:.0} bottom={margin_bottom:.0}"
        );
    }
}

pub fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(
        "
        .fs-bar {
            background: rgba(24, 19, 16, 0.94);
            border-radius: 13px;
            padding: 5px;
            border: 1px solid rgba(255, 182, 139, 0.28);
            box-shadow: 0 6px 24px rgba(0, 0, 0, 0.45);
        }
        .fs-bar button {
            min-width: 26px;
            min-height: 22px;
            padding: 2px 7px;
            background: transparent;
            color: #eee2dc;
            border-radius: 7px;
            font-size: 12px;
            box-shadow: none;
            border: none;
        }
        .fs-bar button:hover { background: rgba(255, 182, 139, 0.16); }
        .fs-bar button:checked,
        .fs-bar button.fs-swatch-on {
            background: rgba(255, 182, 139, 0.30);
            color: #ffd9c0;
        }
        .fs-bar button.fs-swatch-on { box-shadow: inset 0 0 0 1.5px #ffb68b; }
        .fs-bar button.fs-primary { color: #ffcaa8; }
        .fs-bar button.fs-tool { min-width: 22px; min-height: 22px; padding: 3px 4px; }
        .fs-bar button.fs-color { min-width: 22px; min-height: 22px; padding: 4px 6px; }
        .fs-bar separator { background: rgba(255, 255, 255, 0.14); margin: 2px 3px; }
        .fs-bar spinbutton { min-height: 20px; }
        .fs-grip {
            color: rgba(255, 217, 192, 0.55);
            padding: 0 3px 0 1px;
            font-size: 13px;
        }
        .fs-palette {
            background: rgba(24, 19, 16, 0.94);
            border-radius: 11px;
            padding: 7px;
            border: 1px solid rgba(255, 182, 139, 0.28);
            box-shadow: 0 6px 24px rgba(0, 0, 0, 0.45);
        }
        .fs-palette button {
            min-width: 19px;
            min-height: 19px;
            padding: 3px;
            background: transparent;
            border: none;
            border-radius: 50%;
            box-shadow: none;
        }
        .fs-palette button:hover { background: rgba(255, 182, 139, 0.22); }
        .fs-palette button.fs-swatch-on { box-shadow: inset 0 0 0 2px #ffb68b; }
        .fs-bar spinbutton text { background: transparent; color: #eee2dc; min-width: 22px; }
        .fs-hint {
            background: rgba(24, 19, 16, 0.82);
            color: #eee2dc;
            padding: 6px 14px;
            border-radius: 10px;
            font-size: 13px;
        }
        .fs-entry, .fs-entry:focus, .fs-entry:focus-within {
            background-color: transparent;
            background-image: none;
            border: none;
            box-shadow: none;
            outline: none;
            padding: 0;
            margin: 0;
            min-width: 0;
            min-height: 0;
        }
        .fs-entry text {
            background-color: transparent;
            background-image: none;
            border: none;
            box-shadow: none;
            outline: none;
            padding: 0;
            min-width: 0;
            min-height: 0;
        }
        ",
    );
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

/// 让输入框按标注的字体、字号、颜色就地显示文字
fn style_entry(entry: &gtk::Entry, color: (f64, f64, f64), width: f64) {
    let attrs = gtk::pango::AttrList::new();
    attrs.insert(gtk::pango::AttrString::new_family(&crate::paint::system_font_family()));
    attrs.insert(gtk::pango::AttrInt::new_weight(gtk::pango::Weight::Bold));
    attrs.insert(gtk::pango::AttrSize::new_size_absolute(
        (crate::paint::text_px(width) * f64::from(gtk::pango::SCALE)) as i32,
    ));
    attrs.insert(gtk::pango::AttrColor::new_foreground(
        (color.0 * 65535.0) as u16,
        (color.1 * 65535.0) as u16,
        (color.2 * 65535.0) as u16,
    ));
    entry.set_attributes(&attrs);
}

/// 让输入框宽度跟着文字走，光标停在字后面
fn fit_entry(entry: &gtk::Entry) {
    let mut cols = 2;
    for c in entry.text().chars() {
        cols += if c.is_ascii() { 1 } else { 2 };
    }
    entry.set_width_chars(cols);
}
