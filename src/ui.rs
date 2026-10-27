// 浮层工具条、颜色、线宽与文字输入框
use std::cell::RefCell;
use std::rc::Rc;

use gtk::gdk;
use gtk::prelude::*;

use crate::doc::{Doc, Tool};

pub type Shared = Rc<RefCell<Doc>>;

/// 工具条上用到的外部动作
pub struct Actions {
    pub on_copy: Rc<dyn Fn()>,
    pub on_save: Rc<dyn Fn()>,
    pub on_cancel: Rc<dyn Fn()>,
}

const COLORS: [((f64, f64, f64), &str); 6] = [
    ((1.0, 0.32, 0.28), "#ff5247"),
    ((1.0, 0.78, 0.2), "#ffc733"),
    ((0.35, 0.85, 0.42), "#59d96b"),
    ((0.32, 0.66, 1.0), "#52a8ff"),
    ((1.0, 1.0, 1.0), "#ffffff"),
    ((0.12, 0.12, 0.12), "#1f1f1f"),
];

pub struct Ui {
    pub root: gtk::Box,
    /// 手工拖动后的位置（逻辑坐标），None 表示跟着选区自动摆位
    manual: Rc<std::cell::Cell<Option<(f64, f64)>>>,
    pub entry: gtk::Entry,
    pub hint: gtk::Label,
    tool_buttons: Vec<gtk::ToggleButton>,
    color_buttons: Vec<gtk::Button>,
    /// 缓存的自然尺寸，避免每次拖动都重新 measure
    natural: std::cell::Cell<Option<(f64, f64)>>,
}

impl Ui {
    pub fn build(doc: &Shared, area: &gtk::DrawingArea, actions: &Actions) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 2);
        root.add_css_class("fs-bar");
        root.set_halign(gtk::Align::Start);
        root.set_valign(gtk::Align::Start);
        root.set_visible(false);

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
            let start = Rc::new(std::cell::Cell::new((0.0f64, 0.0f64)));
            let root2 = root.clone();
            let manual2 = manual.clone();
            let area2 = area.clone();
            drag.connect_drag_begin({
                let start = start.clone();
                let root = root.clone();
                move |_g, _x, _y| {
                    start.set((
                        f64::from(root.margin_start()),
                        f64::from(root.margin_top()),
                    ));
                }
            });
            drag.connect_drag_update(move |_g, ox, oy| {
                let (sx, sy) = start.get();
                let aw = f64::from(area2.width());
                let ah = f64::from(area2.height());
                let w = f64::from(root2.width());
                let h = f64::from(root2.height());
                let nx = (sx + ox).clamp(0.0, (aw - w).max(0.0));
                let ny = (sy + oy).clamp(0.0, (ah - h).max(0.0));
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
            let btn = gtk::ToggleButton::with_label(tool.label());
            btn.set_tooltip_text(Some(&format!(
                "{}（{}）",
                tool.tip(),
                tool.shortcut().to_ascii_uppercase()
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
                        doc.borrow_mut().tool = tool;
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
        let mut color_buttons = Vec::with_capacity(COLORS.len());
        let shared_buttons: Rc<RefCell<Vec<gtk::Button>>> = Rc::new(RefCell::new(Vec::new()));
        for (rgba, hex) in COLORS {
            let btn = gtk::Button::new();
            btn.add_css_class("fs-swatch");
            btn.set_focusable(false);
            btn.set_can_focus(false);
            btn.set_tooltip_text(Some(hex));
            let da = gtk::DrawingArea::new();
            da.set_content_width(13);
            da.set_content_height(13);
            let (r, g, b) = rgba;
            da.set_draw_func(move |_d, cr, w, h| {
                cr.arc(
                    w as f64 / 2.0,
                    h as f64 / 2.0,
                    (w.min(h) as f64) / 2.0 - 0.5,
                    0.0,
                    std::f64::consts::TAU,
                );
                cr.set_source_rgb(r, g, b);
                let _ = cr.fill();
            });
            btn.set_child(Some(&da));
            {
                let doc = doc.clone();
                let buttons = shared_buttons.clone();
                btn.connect_clicked(move |_| {
                    doc.borrow_mut().color = rgba;
                    for (b, (c, _)) in buttons.borrow().iter().zip(COLORS.iter()) {
                        if *c == rgba {
                            b.add_css_class("fs-swatch-on");
                        } else {
                            b.remove_css_class("fs-swatch-on");
                        }
                    }
                });
            }
            row_opts.append(&btn);
            shared_buttons.borrow_mut().push(btn.clone());
            color_buttons.push(btn);
        }

        row_opts.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        let spin = gtk::SpinButton::with_range(1.0, 24.0, 1.0);
        spin.set_value(4.0);
        spin.set_tooltip_text(Some("线宽"));
        spin.set_can_focus(false);
        {
            let doc = doc.clone();
            spin.connect_value_changed(move |s| {
                doc.borrow_mut().width = s.value();
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

        // 文字输入框
        let entry = gtk::Entry::new();
        entry.add_css_class("fs-entry");
        entry.set_width_chars(18);
        entry.set_visible(false);
        entry.set_halign(gtk::Align::Start);
        entry.set_valign(gtk::Align::Start);

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
            tool_buttons,
            color_buttons,
            natural: std::cell::Cell::new(None),
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
        for (btn, (rgba, _)) in self.color_buttons.iter().zip(COLORS.iter()) {
            if (rgba.0 - color.0).abs() < 1e-6
                && (rgba.1 - color.1).abs() < 1e-6
                && (rgba.2 - color.2).abs() < 1e-6
            {
                btn.add_css_class("fs-swatch-on");
            } else {
                btn.remove_css_class("fs-swatch-on");
            }
        }
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
        .fs-bar separator { background: rgba(255, 255, 255, 0.14); margin: 2px 3px; }
        .fs-bar spinbutton { min-height: 20px; }
        .fs-grip {
            color: rgba(255, 217, 192, 0.55);
            padding: 0 3px 0 1px;
            font-size: 13px;
        }
        .fs-bar spinbutton text { background: transparent; color: #eee2dc; min-width: 22px; }
        .fs-hint {
            background: rgba(24, 19, 16, 0.82);
            color: #eee2dc;
            padding: 6px 14px;
            border-radius: 10px;
            font-size: 13px;
        }
        .fs-entry {
            background: rgba(24, 19, 16, 0.94);
            color: #ffe8dc;
            border: 1px solid rgba(255, 182, 139, 0.5);
            border-radius: 7px;
            padding: 3px 7px;
            min-width: 180px;
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
