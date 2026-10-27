// 截图浮层：多显示器窗口、选区交互、标注、导出
use std::cell::RefCell;
use std::rc::Rc;

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::capture;
use crate::doc::{Doc, OutputLayer, Selection, Stroke, Tool};
use crate::export;
use crate::paint::{self, HANDLE_HIT};
use crate::ui::{Actions, Shared, Ui};

pub struct Options {
    /// 只在指定显示器上开浮层
    pub only: Option<String>,
    /// 抓屏时是否带上鼠标指针
    pub cursor: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum DragKind {
    None,
    NewSel,
    Resize(usize),
    Move,
    MoveText,
    Draw,
}

struct DragState {
    kind: DragKind,
    anchor: (f64, f64),
    /// 按下时的选区，用于取消时还原
    saved_sel: Option<Selection>,
    saved_strokes: usize,
    saved_number: u32,
    /// 拖动文字时的原始落点
    text_from: (f64, f64),
    moved: bool,
}

impl Default for DragState {
    fn default() -> Self {
        Self {
            kind: DragKind::None,
            anchor: (0.0, 0.0),
            saved_sel: None,
            saved_strokes: 0,
            saved_number: 1,
            text_from: (0.0, 0.0),
            moved: false,
        }
    }
}

struct Win {
    idx: usize,
    window: gtk::ApplicationWindow,
    area: gtk::DrawingArea,
    ui: Ui,
}

struct Session {
    app: gtk::Application,
    doc: Shared,
    wins: Vec<Rc<Win>>,
    drag: DragState,
    /// 文字工具待输入的落点
    text_at: Option<(usize, f64, f64)>,
    /// 上次重绘时选区所在的显示器
    last_out: Option<usize>,
    /// 当前持有键盘焦点的窗口
    focused: Option<usize>,
    /// 上���次单击（时间 + 位置），用于识别双击
    last_click: Option<(std::time::Instant, (f64, f64))>,
}

type SessionRef = Rc<RefCell<Session>>;

/// 单实例锁，避免同时开两个浮层
fn acquire_lock() -> bool {
    use std::os::fd::AsFd;
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let path = format!("{dir}/FocusSnapS.lock");
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
    else {
        return true;
    };
    match rustix::fs::flock(
        file.as_fd(),
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    ) {
        Ok(()) => {
            std::mem::forget(file);
            true
        }
        Err(_) => false,
    }
}

pub fn run(opts: Options) -> i32 {
    if !acquire_lock() {
        eprintln!("已经有一个截图浮层开着");
        crate::export::notify("FocusSnapS", "已经有一个截图浮层开着");
        return 1;
    }
    let app = gtk::Application::builder()
        .application_id("dev.yunluo.FocusSnapS")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let opts = Rc::new(opts);
    let failed = Rc::new(std::cell::Cell::new(false));
    let failed2 = failed.clone();
    app.connect_activate(move |app| {
        if let Err(e) = build(app, &opts) {
            eprintln!("启动浮层失败: {e}");
            crate::export::notify("FocusSnapS 启动失败", &e);
            failed2.set(true);
            app.quit();
        }
    });
    // 命令行参数已经在本进程解析过，不能再交给 GApplication 解析
    let _ = app.run_with_args::<&str>(&[]);
    if failed.get() {
        1
    } else {
        0
    }
}

/// 建好所有显示器的浮层并显示
fn build(app: &gtk::Application, opts: &Options) -> Result<(), String> {
    if !gtk4_layer_shell::is_supported() {
        return Err("当前合成器不支持 wlr-layer-shell".into());
    }
    crate::ui::load_css();

    let display = gdk::Display::default().ok_or("拿不到 GDK display")?;
    let monitors = list_monitors(&display);
    if monitors.is_empty() {
        return Err("没有找到任何显示器".into());
    }

    let metas = capture::probe().unwrap_or_default();
    let mut outs = Vec::new();
    let mut picks: Vec<gdk::Monitor> = Vec::new();

    for m in monitors {
        let Some(connector) = m.connector().map(|s| s.to_string()) else {
            eprintln!("跳过无法识别名称的显示器");
            continue;
        };
        if let Some(only) = opts.only.as_ref() {
            if only != &connector {
                continue;
            }
        }
        if !metas.iter().any(|x| x.name == connector) {
            eprintln!("显示器 {connector} 在 Wayland 侧找不到对应输出，跳过");
            continue;
        }
        let geo = m.geometry();
        if geo.width() <= 0 || geo.height() <= 0 {
            eprintln!("显示器 {connector} 逻辑尺寸异常，跳过");
            continue;
        }
        let cap = match capture::capture_output(&connector, opts.cursor) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("抓取显示器 {connector} 失败: {e}");
                continue;
            }
        };
        let mut phys = export::surface_from_bgra(cap.width, cap.height, &cap.pixels)?;
        let sx = cap.width as f64 / geo.width() as f64;
        let sy = cap.height as f64 / geo.height() as f64;
        if (sx / sy - 1.0).abs() > 0.02 {
            eprintln!("显示器 {connector} 横纵缩放不一致（{sx:.3} / {sy:.3}），画面可能轻微拉伸");
        }
        let size = (f64::from(geo.width()), f64::from(geo.height()));
        let win_rects = crate::windows::detect_for_output(&connector, &mut phys, sx, sy, size);
        outs.push(OutputLayer::new(size, sx, sy, phys, win_rects));
        picks.push(m);
    }

    if outs.is_empty() {
        return Err("没有一个显示器能抓取画面".into());
    }

    let doc: Shared = Rc::new(RefCell::new(Doc::new(outs, crate::doc::default_dir())));
    let sess: SessionRef = Rc::new(RefCell::new(Session {
        app: app.clone(),
        doc: doc.clone(),
        wins: Vec::new(),
        drag: DragState::default(),
        text_at: None,
        last_out: Some(usize::MAX),
        focused: None,
        last_click: None,
    }));

    for (idx, monitor) in picks.iter().enumerate() {
        let win = make_window(app, &sess, idx, monitor);
        sess.borrow_mut().wins.push(Rc::new(win));
    }

    sess.borrow_mut().sync_ui();
    let color = sess.borrow().doc.borrow().color;
    for w in sess.borrow().wins.clone() {
        w.ui.sync_color(color);
    }
    for w in sess.borrow().wins.clone() {
        w.window.present();
    }

    // 供自动化验证使用：启动后按预设区域选择并落盘
    if let Ok(spec) = std::env::var("FOCUSSNAPS_TEST_SELECT") {
        let sess = sess.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
            let mut s = sess.borrow_mut();
            if let Some(rect) = parse_rect(&spec) {
                let idx = s.doc.borrow().sel.map(|x| x.out).unwrap_or(0);
                s.doc.borrow_mut().sel = Some(Selection {
                    out: idx,
                    x0: rect.0,
                    y0: rect.1,
                    x1: rect.0 + rect.2,
                    y1: rect.1 + rect.3,
                });
                if std::env::var("FOCUSSNAPS_TEST_ANNOTATE").is_ok() {
                    let color = s.doc.borrow().color;
                    let width = s.doc.borrow().width;
                    s.doc.borrow_mut().commit(Stroke {
                        tool: Tool::Rect,
                        color,
                        width,
                        points: vec![(rect.0 + 20.0, rect.1 + 20.0), (rect.0 + 160.0, rect.1 + 120.0)],
                        text: None,
                        number: None,
                        text_size: None,
                    });
                    s.doc.borrow_mut().commit(Stroke {
                        tool: Tool::Number,
                        color,
                        width,
                        points: vec![(rect.0 + 60.0, rect.1 + 60.0)],
                        text: None,
                        number: Some(7),
                        text_size: None,
                    });
                    let sample = "玲珑测试 文字";
                    let size = s.measure_text(idx, sample, crate::paint::text_px(width));
                    {
                        let mut d = s.doc.borrow_mut();
                        d.commit(Stroke {
                            tool: Tool::Text,
                            color,
                            width,
                            points: vec![(rect.0 + 30.0, rect.1 + 150.0)],
                            text: Some(sample.into()),
                            number: None,
                            text_size: size,
                        });
                        // 让文字框出现在截图里
                        d.text_sel = Some(d.strokes.len() - 1);
                    }
                }
            }
            s.sync_ui();
            s.redraw();
            // 模拟在 "x,y,文本[,线宽]" 处打开文字输入并已打上内容
            if let Ok(spec) = std::env::var("FOCUSSNAPS_TEST_TEXT") {
                let parts: Vec<&str> = spec.splitn(4, ',').collect();
                if parts.len() >= 3 {
                    if let (Ok(x), Ok(y)) = (parts[0].parse::<f64>(), parts[1].parse::<f64>()) {
                        let idx = s.doc.borrow().sel.map(|s| s.out).unwrap_or(0);
                        if let Some(w) = s.win_of(idx).cloned() {
                            let at = put_entry(&w, x, y);
                            s.text_at = Some((idx, at.0, at.1));
                            // 给了线宽就走真实的大小调节路径
                            if let Some(v) = parts.get(3).and_then(|v| v.trim().parse::<f64>().ok())
                            {
                                w.ui.width_spin.set_value(v);
                            }
                            let mut pos = -1i32;
                            w.ui.entry.insert_text(parts[2], &mut pos);
                            w.ui.entry.grab_focus();
                        }
                    }
                }
            }
            if std::env::var("FOCUSSNAPS_TEST_PALETTE").is_ok() {
                // 自动化检查调色板：连开两次，两次摆放位置应当一致
                let out = s.doc.borrow().sel.map(|x| x.out);
                if let Some(w) = out.and_then(|i| s.win_of(i)).cloned() {
                    for delay in [300, 600, 750] {
                        let w = w.clone();
                        glib::timeout_add_local_once(
                            std::time::Duration::from_millis(delay),
                            move || {
                                let area =
                                    (f64::from(w.area.width()), f64::from(w.area.height()));
                                w.ui.toggle_palette(area);
                            },
                        );
                    }
                }
            }
            if let Ok(path) = std::env::var("FOCUSSNAPS_TEST_SAVE") {
                let sess2 = sess.clone();
                glib::timeout_add_local_once(
                    std::time::Duration::from_millis(250),
                    move || {
                        let s = sess2.borrow_mut();
                        let mut doc = s.doc.borrow_mut();
                        match crate::export::render_selection(&mut doc).and_then(|mut surf| {
                            crate::export::save_to_path(&mut surf, &path)
                        }) {
                            Ok(p) => println!("{}", p.display()),
                            Err(e) => eprintln!("测试保存失败: {e}"),
                        }
                        s.app.quit();
                    },
                );
            }
        });
    }
    Ok(())
}

/// 建一个显示器的浮层窗口
fn make_window(
    app: &gtk::Application,
    sess: &SessionRef,
    idx: usize,
    monitor: &gdk::Monitor,
) -> Win {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .decorated(false)
        .build();
    window.set_title(Some("FocusSnapS"));

    let area = gtk::DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    area.set_cursor_from_name(Some("crosshair"));

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&area));

    let actions = Actions {
        on_copy: {
            let sess = sess.clone();
            Rc::new(move || {
                let mut s = sess.borrow_mut();
                s.do_copy(Some(idx));
            })
        },
        on_save: {
            let sess = sess.clone();
            Rc::new(move || {
                let mut s = sess.borrow_mut();
                s.do_save(Some(idx));
            })
        },
        on_cancel: {
            let sess = sess.clone();
            Rc::new(move || {
                let s = sess.borrow();
                s.app.quit();
            })
        },
    };
    let ui = Ui::build(&sess.borrow().doc, &area, &actions);
    overlay.add_overlay(&ui.root);
    overlay.add_overlay(&ui.palette);
    overlay.add_overlay(&ui.entry);
    overlay.add_overlay(&ui.hint);
    // 覆盖层不参与窗口尺寸计算，超出部分裁掉
    overlay.set_measure_overlay(&ui.root, false);
    overlay.set_measure_overlay(&ui.palette, false);
    overlay.set_measure_overlay(&ui.entry, false);
    overlay.set_clip_overlay(&ui.root, true);
    overlay.set_clip_overlay(&ui.palette, true);
    overlay.set_clip_overlay(&ui.entry, true);
    window.set_child(Some(&overlay));

    // 绘制
    {
        let sess = sess.clone();
        let entry = ui.entry.clone();
        area.set_draw_func(move |_a, cr, w, h| {
            let Ok(s) = sess.try_borrow() else { return };
            let Ok(mut doc) = s.doc.try_borrow_mut() else {
                return;
            };
            // 正在输入的文字直接取输入框内容，边打字边预览
            let preview = match s.text_at {
                Some((o, x, y)) if o == idx => Some((x, y, entry.text().to_string())),
                _ => None,
            };
            let preview = preview
                .as_ref()
                .map(|(x, y, t)| (*x, *y, t.as_str()));
            paint::paint(cr, &mut doc, idx, (f64::from(w), f64::from(h)), preview);
        });
    }

    // 拖拽：框选 / 调整 / 移动 / 画标注
    {
        let sess = sess.clone();
        let drag = gtk::GestureDrag::new();
        drag.set_button(1);
        drag.connect_drag_begin({
            let sess = sess.clone();
            move |_g, x, y| {
                let mut s = sess.borrow_mut();
                on_drag_begin(&mut s, idx, x, y);
                s.redraw();
            }
        });
        drag.connect_drag_update({
            let sess = sess.clone();
            move |_g, ox, oy| {
                let mut s = sess.borrow_mut();
                let anchor = s.drag.anchor;
                on_drag_update(&mut s, idx, anchor.0 + ox, anchor.1 + oy);
                s.redraw();
            }
        });
        drag.connect_drag_end({
            let sess = sess.clone();
            move |_g, ox, oy| {
                let mut s = sess.borrow_mut();
                let anchor = s.drag.anchor;
                on_drag_end(&mut s, idx, anchor.0 + ox, anchor.1 + oy);
                s.redraw();
            }
        });
        area.add_controller(drag);
    }

    // 指针进入哪块屏就把键盘焦点交给它；移动时更新悬停窗口
    {
        let sess = sess.clone();
        let motion = gtk::EventControllerMotion::new();
        motion.connect_enter({
            let sess = sess.clone();
            move |_c, x, y| {
                let mut s = sess.borrow_mut();
                s.focus_window(idx);
                if s.update_hover(idx, x, y) {
                    s.redraw();
                }
            }
        });
        motion.connect_motion(move |_c, x, y| {
            let mut s = sess.borrow_mut();
            if s.update_hover(idx, x, y) {
                s.redraw();
            }
        });
        // 调试用：把指针的逻辑坐标打到 stderr
        if std::env::var("FOCUSSNAPS_TEST_POINTER").is_ok() {
            motion.connect_motion(|_, x, y| eprintln!("POS {x:.1} {y:.1}"));
        }
        area.add_controller(motion);
    }

    // 右键：清掉选区 / 退出
    {
        let sess = sess.clone();
        let right = gtk::GestureClick::new();
        right.set_button(3);
        right.connect_released({
            let sess = sess.clone();
            move |_g, _n, _x, _y| {
                let mut s = sess.borrow_mut();
                if s.doc.borrow().sel.is_some() {
                    s.cancel_text();
                    s.doc.borrow_mut().clear_all();
                    s.sync_ui();
                    s.redraw();
                } else {
                    s.app.quit();
                }
            }
        });
        area.add_controller(right);
    }

    // 滚轮：原地调整文字字号
    {
        let sess = sess.clone();
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scroll.connect_scroll(move |_, _dx, dy| {
            let mut s = sess.borrow_mut();
            if s.text_at.is_none() {
                return glib::Propagation::Proceed;
            }
            let step = if dy < 0.0 { 1.0 } else { -1.0 };
            let next = (s.doc.borrow().width + step).clamp(1.0, 24.0);
            let spin = s.win_of(idx).map(|w| w.ui.width_spin.clone());
            if let Some(spin) = spin {
                spin.set_value(next);
            }
            s.redraw();
            glib::Propagation::Stop
        });
        area.add_controller(scroll);
    }

    // 文字输入：边打字边在画面上预览
    {
        let entry = ui.entry.clone();
        entry.connect_activate({
            let sess = sess.clone();
            move |_| {
                let mut s = sess.borrow_mut();
                s.commit_text();
                s.redraw();
            }
        });
        let esc = gtk::EventControllerKey::new();
        esc.connect_key_pressed({
            let sess = sess.clone();
            let entry = ui.entry.clone();
            move |_, key, _, _| {
                if key == gdk::Key::Escape {
                    entry.set_visible(false);
                    let mut s = sess.borrow_mut();
                    s.text_at = None;
                    s.doc.borrow_mut().active = None;
                    s.redraw();
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        });
        entry.add_controller(esc);
    }

    // 快捷键：capture 阶段处理，避免被输入框或按钮抢走
    {
        let sess = sess.clone();
        let entry = ui.entry.clone();
        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        key.connect_key_pressed(move |_, k, _, mods| {
            // 文字输入框开着时按键都归它
            if entry.is_mapped() {
                return glib::Propagation::Proceed;
            }
            let mut s = sess.borrow_mut();
            on_key(&mut s, idx, k, mods)
        });
        window.add_controller(key);
    }

    // --- layer shell 配置 ---
    window.init_layer_shell();
    window.set_namespace(Some("focussnaps"));
    window.set_layer(Layer::Overlay);
    window.set_monitor(Some(monitor));
    for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
        window.set_anchor(edge, true);
    }
    window.set_exclusive_zone(-1);
    window.set_keyboard_mode(KeyboardMode::Exclusive);

    Win {
        idx,
        window,
        area,
        ui,
    }
}

impl Session {
    /// 重绘：选区换了显示器时全部重绘，否则只重绘选区所在的窗口
    fn redraw(&mut self) {
        let (sel_out, hover_out) = {
            let d = self.doc.borrow();
            (d.sel.map(|s| s.out), d.hover.map(|(o, _)| o))
        };
        let owner = sel_out.or(hover_out);
        let all = owner != self.last_out;
        self.last_out = owner;
        for w in &self.wins {
            if all || Some(w.idx) == owner {
                w.area.queue_draw();
            }
        }
    }

    /// 更新悬停窗口，返回是否有变化
    fn update_hover(&mut self, idx: usize, x: f64, y: f64) -> bool {
        let next = {
            let d = self.doc.borrow();
            if d.sel.is_some() {
                None
            } else {
                d.window_at(idx, x, y).map(|r| (idx, r))
            }
        };
        if self.doc.borrow().hover == next {
            return false;
        }
        self.doc.borrow_mut().hover = next;
        true
    }

    /// 同步工具条位置、可见性、工具高亮
    fn sync_ui(&self) {
        let (sel, tool) = {
            let d = self.doc.borrow();
            (d.sel, d.tool)
        };
        // 框选、缩放、移动选区期间先不显示工具条，松手后再出现
        let selecting = matches!(
            self.drag.kind,
            DragKind::NewSel | DragKind::Resize(_) | DragKind::Move
        );
        for w in &self.wins {
            w.ui.sync_tool(tool);
            let on_sel = sel.map(|s| s.out == w.idx && !selecting).unwrap_or(false);
            if let (true, Some(s)) = (on_sel, sel) {
                let area = (f64::from(w.area.width()), f64::from(w.area.height()));
                // 先显示，measure() 才量得到自然尺寸
                w.ui.root.set_visible(true);
                w.ui.place(s.normalized(), area);
                w.ui.hint.set_visible(false);
            } else {
                w.ui.root.set_visible(false);
                w.ui.palette.set_visible(false);
                w.ui.hint.set_visible(sel.is_none());
            }
            let cursor = match tool {
                Tool::Select => "default",
                Tool::Text => "text",
                _ => "crosshair",
            };
            w.area.set_cursor_from_name(Some(cursor));
        }
    }

    fn win_of(&self, idx: usize) -> Option<&Rc<Win>> {
        self.wins.iter().find(|w| w.idx == idx)
    }

    /// 指针进入某块屏时确保该窗口重新申请独占键盘
    fn focus_window(&mut self, idx: usize) {
        let lost = self
            .wins
            .iter()
            .any(|w| w.idx == idx && !w.window.is_active());
        self.focused = Some(idx);
        if !lost {
            return;
        }
        // 每块屏的浮层都保持 Exclusive：合成器只把键给当前活动输出上的那一块
        for w in &self.wins {
            w.window.set_keyboard_mode(KeyboardMode::Exclusive);
        }
    }

    /// 结束文字输入，把输入框里的内容提交成一笔标注
    fn commit_text(&mut self) {
        let Some((out, x, y)) = self.text_at.take() else {
            return;
        };
        let text = match self.win_of(out) {
            Some(w) => {
                w.ui.entry.set_visible(false);
                w.ui.entry.text().to_string()
            }
            None => return,
        };
        let (color, width) = {
            let d = self.doc.borrow();
            (d.color, d.width)
        };
        let size = self.measure_text(out, &text, crate::paint::text_px(width));
        let mut d = self.doc.borrow_mut();
        if text.is_empty() {
            d.text_sel = None;
        } else {
            d.commit(Stroke {
                tool: Tool::Text,
                color,
                width,
                points: vec![(x, y)],
                text: Some(text),
                number: None,
                text_size: size,
            });
            d.text_sel = Some(d.strokes.len() - 1);
        }
    }

    /// 收起文字输入框
    fn cancel_text(&mut self) {
        if self.text_at.take().is_some() {
            for w in &self.wins {
                w.ui.entry.set_visible(false);
            }
        }
    }

    /// 量一段文字的排版尺寸，用该屏的画面建上下文
    fn measure_text(&self, out: usize, text: &str, px: f64) -> Option<(f64, f64)> {
        let surface = self.doc.borrow().outs.get(out).map(|o| o.phys.clone())?;
        crate::paint::text_size(&surface, text, px)
    }

    /// 收起调色板
    fn close_palette(&self) {
        for w in &self.wins {
            w.ui.palette.set_visible(false);
        }
    }

    /// 没有选区时把整块屏幕当成选区
    fn ensure_selection(&mut self, idx: Option<usize>) {
        if self.doc.borrow().sel.is_some() {
            return;
        }
        let Some(idx) = idx else { return };
        let size = self.doc.borrow().outs.get(idx).map(|o| o.size);
        if let Some(size) = size {
            self.doc.borrow_mut().sel = Some(Selection {
                out: idx,
                x0: 0.0,
                y0: 0.0,
                x1: size.0,
                y1: size.1,
            });
        }
    }

    /// 复制选区并退出
    fn do_copy(&mut self, idx: Option<usize>) {
        self.ensure_selection(idx);
        let clipboard = self
            .win_of(self.doc.borrow().sel.map(|s| s.out).unwrap_or(0))
            .or_else(|| self.wins.first())
            .map(|w| w.window.clipboard());
        let mut surface = {
            let mut doc = self.doc.borrow_mut();
            match export::render_selection(&mut doc) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("复制失败: {e}");
                    return;
                }
            }
        };
        let png = export::encode_png(&mut surface);
        if let Ok(png) = png {
            if export::copy_png_via_wl_copy(&png).is_ok() {
                export::notify("FocusSnapS", "已复制到剪贴板");
                self.app.quit();
                return;
            }
        }
        match clipboard {
            Some(cb) => match export::surface_texture(&mut surface) {
                Ok(texture) => {
                    cb.set_texture(&texture);
                    self.app.quit();
                }
                Err(e) => eprintln!("复制失败: {e}"),
            },
            None => eprintln!("复制失败: 没有可用窗口"),
        }
    }

    /// 保存选区并退出
    fn do_save(&mut self, idx: Option<usize>) {
        self.ensure_selection(idx);
        let dir = self.doc.borrow().dir.clone();
        let mut doc = self.doc.borrow_mut();
        let mut surface = match export::render_selection(&mut doc) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("保存失败: {e}");
                return;
            }
        };
        drop(doc);
        match export::save_to_dir(&mut surface, &dir) {
            Ok(path) => {
                println!("{}", path.display());
                export::notify("截图已保存", &path.display().to_string());
                self.app.quit();
            }
            Err(e) => eprintln!("保存失败: {e}"),
        }
    }
}

/// 列出所有显示器
fn list_monitors(display: &gdk::Display) -> Vec<gdk::Monitor> {
    let model = display.monitors();
    let mut out = Vec::new();
    for i in 0..model.n_items() {
        if let Some(obj) = model.item(i) {
            if let Ok(m) = obj.downcast::<gdk::Monitor>() {
                out.push(m);
            }
        }
    }
    out
}

/// 把文字输入框就地摆到落点上，返回实际用的落点
fn put_entry(w: &Win, x: f64, y: f64) -> (f64, f64) {
    let area_w = f64::from(w.area.width());
    let area_h = f64::from(w.area.height());
    // 贴边时往回收，别把输入框挤出屏
    let ax = x.clamp(0.0, (area_w - 40.0).max(0.0));
    let ay = y.clamp(0.0, (area_h - 30.0).max(0.0));
    w.ui.entry.set_text("");
    w.ui.entry.set_margin_start(ax.round() as i32);
    w.ui.entry.set_margin_top(ay.round() as i32);
    w.ui.entry.set_visible(true);
    (ax, ay)
}

/// 解析 "x,y,w,h"
fn parse_rect(spec: &str) -> Option<(f64, f64, f64, f64)> {
    let v: Vec<f64> = spec
        .split(',')
        .filter_map(|s| s.trim().parse::<f64>().ok())
        .collect();
    if v.len() != 4 {
        return None;
    }
    Some((v[0], v[1], v[2], v[3]))
}

// ---------- 交互 ----------

/// 选区几乎铺满整块屏幕
fn covers_most(r: (f64, f64, f64, f64), size: (f64, f64)) -> bool {
    let area = (r.2 - r.0) * (r.3 - r.1);
    area >= 0.98 * size.0 * size.1
}

fn contains(r: (f64, f64, f64, f64), x: f64, y: f64) -> bool {
    x >= r.0 && x <= r.2 && y >= r.1 && y <= r.3
}

fn handle_at(r: (f64, f64, f64, f64), x: f64, y: f64) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (i, (hx, hy)) in paint::handle_points(r).iter().enumerate() {
        let d = (x - hx).abs().max((y - hy).abs());
        if d <= HANDLE_HIT && best.map(|(_, bd)| d < bd).unwrap_or(true) {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}

fn on_drag_begin(s: &mut Session, idx: usize, x: f64, y: f64) {
    s.close_palette();
    s.commit_text();
    let (tool, sel, out_size) = {
        let d = s.doc.borrow();
        (
            d.tool,
            d.sel,
            d.outs.get(idx).map(|o| o.size).unwrap_or((0.0, 0.0)),
        )
    };
    s.drag = DragState {
        kind: DragKind::None,
        anchor: (x, y),
        saved_sel: sel,
        saved_strokes: s.doc.borrow().strokes.len(),
        saved_number: s.doc.borrow().next_number,
        text_from: (0.0, 0.0),
        moved: false,
    };

    let cur = sel.filter(|s| s.out == idx).map(|s| s.normalized());

    // 点在文字框上：选中它并拖动
    if tool == Tool::Select {
        let hit = s.doc.borrow().text_at(idx, x, y);
        if let Some(i) = hit {
            let from = s.doc.borrow().strokes[i].points[0];
            s.doc.borrow_mut().text_sel = Some(i);
            s.drag.kind = DragKind::MoveText;
            s.drag.text_from = from;
            return;
        }
        // 没点中文字就把文字框收掉
        s.doc.borrow_mut().text_sel = None;
    }

    match tool {
        // 单击放置的标注交给单击手势处理
        Tool::Text | Tool::Number => {}
        Tool::Select => match cur {
            Some(r) => {
                // 手柄伸出选框一半，先判手柄再判内部
                if let Some(h) = handle_at(r, x, y) {
                    s.drag.kind = DragKind::Resize(h);
                } else if contains(r, x, y) && !covers_most(r, out_size) {
                    s.drag.kind = DragKind::Move;
                } else {
                    s.drag.kind = DragKind::NewSel;
                    let mut d = s.doc.borrow_mut();
                    d.sel = Some(Selection {
                        out: idx,
                        x0: x,
                        y0: y,
                        x1: x,
                        y1: y,
                    });
                    d.active = None;
                }
            }
            None => {
                s.drag.kind = DragKind::NewSel;
                let mut d = s.doc.borrow_mut();
                d.sel = Some(Selection {
                    out: idx,
                    x0: x,
                    y0: y,
                    x1: x,
                    y1: y,
                });
                d.active = None;
            }
        },
        _ => {
            if let Some(_r) = cur.filter(|r| contains(*r, x, y)) {
                s.drag.kind = DragKind::Draw;
                let (color, width) = {
                    let d = s.doc.borrow();
                    (d.color, d.width)
                };
                s.doc.borrow_mut().active = Some(Stroke {
                    tool,
                    color,
                    width,
                    points: vec![(x, y)],
                    text: None,
                    number: None,
                    text_size: None,
                });
            } else {
                s.drag.kind = DragKind::NewSel;
                let mut d = s.doc.borrow_mut();
                d.sel = Some(Selection {
                    out: idx,
                    x0: x,
                    y0: y,
                    x1: x,
                    y1: y,
                });
                d.active = None;
            }
        }
    }
}

fn on_drag_update(s: &mut Session, idx: usize, x: f64, y: f64) {
    let (w, h) = {
        let d = s.doc.borrow();
        d.outs.get(idx).map(|o| o.size).unwrap_or((0.0, 0.0))
    };
    let (x, y) = (x.clamp(0.0, w), y.clamp(0.0, h));
    if (x - s.drag.anchor.0).abs() > 2.0 || (y - s.drag.anchor.1).abs() > 2.0 {
        s.drag.moved = true;
    }

    match s.drag.kind {
        DragKind::NewSel => {
            if let Some(sel) = s.doc.borrow_mut().sel.as_mut() {
                sel.x1 = x;
                sel.y1 = y;
            }
        }
        DragKind::Move => {
            let Some(orig) = s.drag.saved_sel else { return };
            let (x0, y0, x1, y1) = orig.normalized();
            let (dx, dy) = (x - s.drag.anchor.0, y - s.drag.anchor.1);
            let rw = x1 - x0;
            let rh = y1 - y0;
            let nx0 = (x0 + dx).clamp(0.0, (w - rw).max(0.0));
            let ny0 = (y0 + dy).clamp(0.0, (h - rh).max(0.0));
            let mut d = s.doc.borrow_mut();
            d.sel = Some(Selection {
                out: idx,
                x0: nx0,
                y0: ny0,
                x1: nx0 + rw,
                y1: ny0 + rh,
            });
        }
        DragKind::MoveText => {
            let Some(i) = s.doc.borrow().text_sel else { return };
            let (dx, dy) = (x - s.drag.anchor.0, y - s.drag.anchor.1);
            let (fx, fy) = s.drag.text_from;
            // 文字框整体留在本屏内
            let (bw, bh) = s
                .doc
                .borrow()
                .strokes
                .get(i)
                .and_then(|st| st.text_size)
                .unwrap_or((0.0, 0.0));
            let nx = (fx + dx).clamp(0.0, (w - bw).max(0.0));
            let ny = (fy + dy).clamp(0.0, (h - bh).max(0.0));
            let mut d = s.doc.borrow_mut();
            if let Some(st) = d.strokes.get_mut(i) {
                if let Some(p) = st.points.first_mut() {
                    *p = (nx, ny);
                }
            }
        }
        DragKind::Resize(handle) => {
            let Some(orig) = s.drag.saved_sel else { return };
            let (mut x0, mut y0, mut x1, mut y1) = orig.normalized();
            match handle {
                0 => {
                    x0 = x;
                    y0 = y;
                }
                1 => y0 = y,
                2 => {
                    x1 = x;
                    y0 = y;
                }
                3 => x1 = x,
                4 => {
                    x1 = x;
                    y1 = y;
                }
                5 => y1 = y,
                6 => {
                    x0 = x;
                    y1 = y;
                }
                7 => x0 = x,
                _ => {}
            }
            let mut d = s.doc.borrow_mut();
            d.sel = Some(Selection {
                out: idx,
                x0,
                y0,
                x1,
                y1,
            });
        }
        DragKind::Draw => {
            let mut d = s.doc.borrow_mut();
            if let Some(a) = d.active.as_mut() {
                if a.tool.is_freehand() {
                    a.points.push((x, y));
                } else {
                    a.points.truncate(1);
                    a.points.push((x, y));
                }
            }
        }
        DragKind::None => {}
    }
    s.sync_ui();
}

fn on_drag_end(s: &mut Session, idx: usize, x: f64, y: f64) {
    let kind = s.drag.kind;
    s.drag.kind = DragKind::None;

    match kind {
        DragKind::NewSel => {
            let sel = s.doc.borrow().sel;
            let keep = sel
                .map(|r| r.width() > 4.0 && r.height() > 4.0)
                .unwrap_or(false);
            if keep {
                // 换选区时旧标注已无意义
                let mut d = s.doc.borrow_mut();
                d.strokes.clear();
                d.redo.clear();
                d.next_number = 1;
                d.active = None;
                d.hover = None;
            } else {
                // 没拖动：先还原，再看是不是点了某个窗口
                let hover = s.doc.borrow().hover.filter(|(o, _)| *o == idx);
                let saved_sel = s.drag.saved_sel;
                let saved_n = s.drag.saved_strokes;
                let saved_num = s.drag.saved_number;
                let mut d = s.doc.borrow_mut();
                d.sel = saved_sel;
                d.strokes.truncate(saved_n);
                d.next_number = saved_num;
                if saved_sel.is_none() {
                    if let Some((_, r)) = hover {
                        d.sel = Some(Selection {
                            out: idx,
                            x0: r.x0,
                            y0: r.y0,
                            x1: r.x1,
                            y1: r.y1,
                        });
                        d.strokes.clear();
                        d.redo.clear();
                        d.next_number = 1;
                        d.hover = None;
                    }
                }
            }
        }
        DragKind::Resize(_) => {
            let mut d = s.doc.borrow_mut();
            if let Some(sel) = d.sel {
                let (x0, y0, x1, y1) = sel.normalized();
                if x1 - x0 <= 4.0 || y1 - y0 <= 4.0 {
                    d.sel = s.drag.saved_sel;
                } else {
                    d.sel = Some(Selection {
                        out: idx,
                        x0,
                        y0,
                        x1,
                        y1,
                    });
                }
            }
        }
        DragKind::Move | DragKind::MoveText => {}
        DragKind::Draw => {
            let mut d = s.doc.borrow_mut();
            if let Some(mut a) = d.active.take() {
                let moved = if a.tool.is_freehand() {
                    !a.points.is_empty()
                } else {
                    a.points.len() > 1
                        && a.points.iter().any(|p| {
                            (p.0 - a.points[0].0).abs() > 1.0
                                || (p.1 - a.points[0].1).abs() > 1.0
                        })
                };
                match a.tool {
                    Tool::Crop => {
                        if moved {
                            if let Some(sel) = d.sel {
                                let (sx0, sy0, sx1, sy1) = sel.normalized();
                                let p0 = a.points[0];
                                let p1 = *a.points.last().unwrap();
                                let nx0 = sx0.max(p0.0.min(p1.0));
                                let ny0 = sy0.max(p0.1.min(p1.1));
                                let nx1 = sx1.min(p0.0.max(p1.0));
                                let ny1 = sy1.min(p0.1.max(p1.1));
                                if nx1 - nx0 > 8.0 && ny1 - ny0 > 8.0 {
                                    d.sel = Some(Selection {
                                        out: sel.out,
                                        x0: nx0,
                                        y0: ny0,
                                        x1: nx1,
                                        y1: ny1,
                                    });
                                }
                            }
                        }
                    }
                    _ => {
                        if moved {
                            if !a.tool.is_freehand() {
                                a.points.truncate(2);
                            }
                            d.commit(a);
                        }
                    }
                }
            }
        }
        DragKind::None => {
            // 没有位移的单击：文字 / 序号在这里落点
            let tool = s.doc.borrow().tool;
            if !s.drag.moved && matches!(tool, Tool::Text | Tool::Number) {
                let (ax, ay) = s.drag.anchor;
                on_click(s, idx, ax, ay);
            }
        }
    }

    // 单击（没拖动）累计：和上一次点击挨得够近够快就是双击 → 完成截图
    if !s.drag.moved {
        let now = std::time::Instant::now();
        let close = s
            .last_click
            .map(|(t, p)| {
                now.duration_since(t) < std::time::Duration::from_millis(450)
                    && (p.0 - x).abs() < 8.0
                    && (p.1 - y).abs() < 8.0
            })
            .unwrap_or(false);
        s.last_click = Some((now, (x, y)));
        if close && s.doc.borrow().sel.is_some() {
            s.drag.moved = false;
            s.do_copy(Some(idx));
            return;
        }
    }

    s.drag.moved = false;
    s.sync_ui();
}

fn on_click(s: &mut Session, idx: usize, x: f64, y: f64) {
    let (tool, sel, color, width) = {
        let d = s.doc.borrow();
        (d.tool, d.sel, d.color, d.width)
    };
    let Some(sel) = sel else { return };
    let r = sel.normalized();
    if sel.out != idx || !contains(r, x, y) {
        return;
    }
    match tool {
        Tool::Number => {
            s.doc.borrow_mut().text_sel = None;
            let n = {
                let mut d = s.doc.borrow_mut();
                let n = d.next_number;
                d.next_number += 1;
                n
            };
            s.doc.borrow_mut().commit(Stroke {
                tool: Tool::Number,
                color,
                width,
                points: vec![(x, y)],
                text: None,
                number: Some(n),
                text_size: None,
            });
        }
        Tool::Text => {
            let win = s.win_of(idx).cloned();
            {
                let mut d = s.doc.borrow_mut();
                d.active = None;
                d.text_sel = None;
            }
            if let Some(w) = win {
                let at = put_entry(&w, x, y);
                s.text_at = Some((idx, at.0, at.1));
                w.window.present();
                w.ui.entry.grab_focus();
            }
        }
        _ => {}
    }
}

fn on_key(s: &mut Session, idx: usize, key: gdk::Key, mods: gdk::ModifierType) -> glib::Propagation {
    let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
    let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);

    // 选区在别的屏上时应作用在选区所属的显示器
    let sel_out = s.doc.borrow().sel.map(|x| x.out);
    let target = sel_out.unwrap_or(idx);

    if ctrl {
        match key {
            gdk::Key::c | gdk::Key::C => {
                s.do_copy(Some(target));
                return glib::Propagation::Stop;
            }
            gdk::Key::s | gdk::Key::S => {
                s.do_save(Some(target));
                return glib::Propagation::Stop;
            }
            gdk::Key::a | gdk::Key::A => {
                let size = {
                    let d = s.doc.borrow();
                    d.outs.get(idx).map(|o| o.size).unwrap_or((0.0, 0.0))
                };
                s.doc.borrow_mut().sel = Some(Selection {
                    out: idx,
                    x0: 0.0,
                    y0: 0.0,
                    x1: size.0,
                    y1: size.1,
                });
                s.sync_ui();
                s.redraw();
                return glib::Propagation::Stop;
            }
            gdk::Key::z | gdk::Key::Z => {
                let changed = if shift {
                    s.doc.borrow_mut().redo()
                } else {
                    let ok = s.doc.borrow_mut().undo();
                    if ok {
                        let mut d = s.doc.borrow_mut();
                        let next = d
                            .strokes
                            .iter()
                            .filter_map(|x| x.number)
                            .max()
                            .unwrap_or(0)
                            + 1;
                        d.next_number = next;
                    }
                    ok
                };
                if changed {
                    s.redraw();
                }
                return glib::Propagation::Stop;
            }
            _ => {}
        }
        return glib::Propagation::Proceed;
    }

    match key {
        gdk::Key::Escape => {
            s.app.quit();
            glib::Propagation::Stop
        }
        gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::space => {
            s.do_copy(Some(target));
            glib::Propagation::Stop
        }
        gdk::Key::Delete | gdk::Key::BackSpace => {
            let mut d = s.doc.borrow_mut();
            d.clear_all();
            drop(d);
            s.sync_ui();
            s.redraw();
            glib::Propagation::Stop
        }
        gdk::Key::Left | gdk::Key::Right | gdk::Key::Up | gdk::Key::Down => {
            let step = if shift { 10.0 } else { 1.0 };
            let (dx, dy) = match key {
                gdk::Key::Left => (-step, 0.0),
                gdk::Key::Right => (step, 0.0),
                gdk::Key::Up => (0.0, -step),
                _ => (0.0, step),
            };
            let size = {
                let d = s.doc.borrow();
                d.outs.get(target).map(|o| o.size).unwrap_or((0.0, 0.0))
            };
            let mut moved = false;
            {
                let mut d = s.doc.borrow_mut();
                if let Some(sel) = d.sel {
                    let (x0, y0, x1, y1) = sel.normalized();
                    let nx0 = (x0 + dx).clamp(0.0, (size.0 - (x1 - x0)).max(0.0));
                    let ny0 = (y0 + dy).clamp(0.0, (size.1 - (y1 - y0)).max(0.0));
                    d.sel = Some(Selection {
                        out: sel.out,
                        x0: nx0,
                        y0: ny0,
                        x1: nx0 + (x1 - x0),
                        y1: ny0 + (y1 - y0),
                    });
                    moved = true;
                }
            }
            if moved {
                s.sync_ui();
                s.redraw();
            }
            glib::Propagation::Stop
        }
        _ => {
            if let Some(ch) = key.to_unicode() {
                let lower = ch.to_ascii_lowercase();
                if let Some(tool) = Tool::ALL.iter().find(|t| t.shortcut() == lower) {
                    s.cancel_text();
                    s.close_palette();
                    let mut d = s.doc.borrow_mut();
                    d.tool = *tool;
                    d.text_sel = None;
                    drop(d);
                    s.sync_ui();
                    s.redraw();
                    return glib::Propagation::Stop;
                }
            }
            glib::Propagation::Proceed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_at_picks_nearest_handle() {
        let r = (10.0, 10.0, 110.0, 60.0);
        // 四角
        assert_eq!(handle_at(r, 10.0, 10.0), Some(0));
        assert_eq!(handle_at(r, 110.0, 60.0), Some(4));
        // 边中点
        assert_eq!(handle_at(r, 60.0, 10.0), Some(1));
        assert_eq!(handle_at(r, 110.0, 35.0), Some(3));
        // 靠外半个手柄也要能命中（手柄画在边线中心）
        assert_eq!(handle_at(r, 6.0, 6.0), Some(0));
        // 选区中心不该命中任何手柄
        assert_eq!(handle_at(r, 60.0, 35.0), None);
    }

    #[test]
    fn handle_at_small_selection_prefers_nearest() {
        // 小选区里八个手柄挤在一起，必须取最近的
        let r = (10.0, 10.0, 20.0, 16.0);
        assert_eq!(handle_at(r, 10.0, 10.0), Some(0));
        assert_eq!(handle_at(r, 20.0, 16.0), Some(4));
    }

    #[test]
    fn covers_most_detects_full_screen() {
        let size = (100.0, 50.0);
        assert!(covers_most((0.0, 0.0, 100.0, 50.0), size));
        assert!(covers_most((0.0, 0.0, 99.5, 50.0), size));
        assert!(!covers_most((0.0, 0.0, 50.0, 50.0), size));
    }

    #[test]
    fn parse_rect_reads_four_numbers() {
        assert_eq!(parse_rect("10,20,30,40"), Some((10.0, 20.0, 30.0, 40.0)));
        assert_eq!(parse_rect("1,2,3"), None);
        assert_eq!(parse_rect("a,b,c,d"), None);
    }
}
