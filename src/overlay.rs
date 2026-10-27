// 截图浮层：多显示器窗口、选区交互、标注、导出
use std::cell::RefCell;
use std::rc::Rc;

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::capture;
use crate::doc::{intersect, union, Doc, OutputLayer, Rect4, Selection, Stroke, Tool};
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
    /// 本屏画面，铺满整屏；手势和光标都挂在它上面
    screen: gtk::Picture,
    /// 标注层，尺寸跟着选区走，通常只有屏幕的一小块
    annot: gtk::DrawingArea,
    /// 标注层左上角的全局逻辑坐标
    view: Rc<std::cell::Cell<(f64, f64)>>,
    ui: Ui,
    /// 本屏在全局布局里的左上角
    origin: (f64, f64),
    /// 本屏在全局布局里占的矩形
    rect: Rect4,
}

struct Session {
    app: gtk::Application,
    doc: Shared,
    wins: Vec<Rc<Win>>,
    drag: DragState,
    /// 文字工具待输入的落点（显示器下标 + 全局逻辑坐标）
    text_at: Option<(usize, f64, f64)>,
    /// 上次重绘覆盖的全局矩形
    dirty: Option<Rect4>,
    /// 主题色监听，丢掉就收不到通知
    theme_watch: Option<gio::Settings>,
    /// 当前持有键盘焦点的窗口
    focused: Option<usize>,
    /// 上次单击（时间 + 位置），用于识别双击
    last_click: Option<(std::time::Instant, (f64, f64))>,
    /// 已经甩给后台线程在导出，别再来一次
    exporting: bool,
    /// 画面还没抓回来时按下的完成动作，等最后一块屏补上再执行
    pending: Option<Export>,
    /// 上一次重绘的时刻，卡顿排查用
    last_redraw: Option<std::time::Instant>,
    /// 还没处理的最新指针位置（拖动用），同一个帧里的多个事件合并成一次
    pending_drag: Option<(usize, f64, f64)>,
    /// 还没处理的最新悬停位置
    pending_hover: Option<(f64, f64)>,
    /// 已经排了一帧，别重复排
    pointer_queued: bool,
}

/// 等画面就绪后再执行的完成动作
#[derive(Clone, Copy)]
enum Export {
    Copy(Option<usize>),
    Save(Option<usize>),
}

type SessionRef = Rc<RefCell<Session>>;

/// 启动阶段打点：设了 FOCUSSNAPS_TRACE 就往 stderr 打时间戳
///
/// 热键到浮层出现之间到底慢在哪，用这个能一眼看出来（进程启动之后的阶段）。
fn trace(msg: &str) {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let t = START.get_or_init(std::time::Instant::now);
    if std::env::var_os("FOCUSSNAPS_TRACE").is_some() {
        eprintln!("[trace] +{:>4} ms  {msg}", t.elapsed().as_millis());
    }
}

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
    // -o 指定的不是当前焦点显示器时先把焦点切过去：niri 只把键盘交给当前焦点输出
    // 上的独占层表面，不然截图期间打字会落到别的应用里。退出时切回原来的焦点。
    let restore_focus = match opts.only.as_deref() {
        Some(want) => {
            let prev = crate::niri::focused_output();
            if prev.as_deref() != Some(want) {
                crate::niri::focus_output(want);
                prev
            } else {
                None
            }
        }
        None => None,
    };
    let opts = Rc::new(opts);

    let app = gtk::Application::builder()
        .application_id("dev.yunluo.FocusSnapS")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
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
    if let Some(prev) = restore_focus {
        // 这时层表面还没销毁，直接切焦点会被弹回来；交给脱离的小进程多试几次
        let name = prev.replace('\'', "");
        let _ = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!(
                "for i in 1 2 3 4 5 6; do sleep 0.25; niri msg action focus-monitor '{name}' \
                 >/dev/null 2>&1; done"
            ))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    if failed.get() {
        1
    } else {
        0
    }
}

/// 建好所有显示器的浮层并显示
fn build(app: &gtk::Application, opts: &Options) -> Result<(), String> {
    trace("build 开始");
    // 这里已在 gtk::init 之后，可以设默认图标
    gtk::Window::set_default_icon_name("focus-snaps");
    if !gtk4_layer_shell::is_supported() {
        return Err("当前合成器不支持 wlr-layer-shell".into());
    }
    let accent = crate::theme::accent();
    crate::ui::set_accent(accent);

    let display = gdk::Display::default().ok_or("拿不到 GDK display")?;
    let monitors = list_monitors(&display);
    if monitors.is_empty() {
        return Err("没有找到任何显示器".into());
    }

    let metas = capture::probe().unwrap_or_default();
    let logical = crate::niri::outputs_logical().unwrap_or_default();
    trace("probe 完");
    // 先把每块屏的图层按「已知的位置和逻辑尺寸」建出来，不等抓屏。
    // 抓屏走的是合成器把整屏画面读回共享内存，GPU 忙的时候能卡到秒级；
    // 拦在启动路径上就是「按了热键半天没反应」。画面由 apply_capture 后补。
    let mut outs = Vec::new();
    let mut picks: Vec<gdk::Monitor> = Vec::new();
    let mut targets: Vec<(usize, String, gdk::Rectangle, (f64, f64))> = Vec::new();
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
        // GDK 给的显示器位置就是合成器的全局逻辑布局坐标，跨屏拼接全靠它
        let origin = (f64::from(geo.x()), f64::from(geo.y()));
        // 尺寸用合成器自己报的逻辑尺寸：GDK 那边可能是四舍五入过的，
        // 差一个逻辑像素就会让整块浮层被缩放一次（见 niri::outputs_logical）
        let size = match logical.iter().find(|(n, _)| n == &connector) {
            Some((_, (w, h))) => (f64::from(*w), f64::from(*h)),
            None => (f64::from(geo.width()), f64::from(geo.height())),
        };
        targets.push((outs.len(), connector, geo, size));
        outs.push(OutputLayer::pending(origin, size));
        picks.push(m);
    }

    if outs.is_empty() {
        return Err("没有一个显示器能抓取画面".into());
    }

    let doc: Shared = Rc::new(RefCell::new(Doc::new(outs, crate::doc::default_dir())));
    doc.borrow_mut().accent = accent;
    let sess: SessionRef = Rc::new(RefCell::new(Session {
        app: app.clone(),
        doc: doc.clone(),
        wins: Vec::new(),
        drag: DragState::default(),
        text_at: None,
        dirty: None,
        theme_watch: None,
        focused: None,
        last_click: None,
        exporting: false,
        pending: None,
        last_redraw: None,
        pending_drag: None,
        pending_hover: None,
        pointer_queued: false,
    }));

    for (idx, monitor) in picks.iter().enumerate() {
        let win = make_window(app, &sess, idx, monitor);
        sess.borrow_mut().wins.push(Rc::new(win));
    }

    // 系统强调色变了就跟着换（设置对象要留在 Session 里才收得到通知）
    {
        let sess2 = sess.clone();
        let watch = crate::theme::watch(move |c| {
            let s = sess2.borrow_mut();
            s.doc.borrow_mut().accent = c;
            crate::ui::set_accent(c);
            for w in &s.wins {
                w.annot.queue_draw();
            }
        });
        sess.borrow_mut().theme_watch = watch;
    }

    // 只看一屏时盯着焦点，被切走就抢回来，避免键漏给别的应用
    if let Some(only) = opts.only.clone() {
        crate::niri::keep_focus_in_background(only);
    }

    sess.borrow_mut().sync_ui();

    sess.borrow().sync_annot();
    let color = sess.borrow().doc.borrow().color;
    for w in sess.borrow().wins.clone() {
        w.ui.sync_color(color);
    }
    trace("窗口都建好");
    for w in sess.borrow().wins.clone() {
        w.window.present();
    }
    trace("present 完");
    if std::env::var_os("FOCUSSNAPS_TRACE").is_some() {
        // 帧时钟间隔看门狗：连续两次 tick 之间隔太久，是合成器/系统卡的
        let last = std::cell::Cell::new(None::<std::time::Instant>);
        if let Some(w) = sess.borrow().wins.first() {
            let sess3 = sess.clone();
            w.annot.add_tick_callback(move |_w, _c| {
                let now = std::time::Instant::now();
                let dragging = sess3
                    .try_borrow()
                    .map(|s| s.drag.kind != DragKind::None)
                    .unwrap_or(false);
                if let Some(prev) = last.get() {
                    let gap = now.duration_since(prev).as_millis();
                    // 只在拖拽期间报：空闲时没有重绘，帧时钟本来就不跳
                    if dragging && gap > 40 {
                        trace(&format!("帧时钟间隔 {gap} ms（合成器没给帧）"));
                    }
                }
                last.set(Some(now));
                glib::ControlFlow::Continue
            });
        }
    }
    start_captures(&sess, targets, opts.cursor);
    {
        let ticks = std::cell::Cell::new(0u32);
        if let Some(w) = sess.borrow().wins.first() {
            let w2 = w.clone();
            w.annot.add_tick_callback(move |_w, _c| {
                let n = ticks.get() + 1;
                ticks.set(n);
                if n <= 2 {
                    trace(&format!("第 {n} 个帧时钟 tick"));
                }
                if n >= 2 {
                    // 只用来打点，之后把窗口交还给正常流程
                    let _ = &w2;
                    return glib::ControlFlow::Break;
                }
                glib::ControlFlow::Continue
            });
        }
    }

    // 性能回归用：连续重绘若干帧，打印实际帧间隔
    //
    // FOCUSSNAPS_BENCH=帧数[:drag[:每帧事件数]]
    //   drag 模式下每帧还会挪一下选区，走真实的拖拽路径；
    //   每帧事件数模拟高回报率鼠标（1000 Hz 在 120 Hz 屏上约 8 个/帧）
    if let Ok(spec) = std::env::var("FOCUSSNAPS_BENCH") {
        let mut parts = spec.split(':');
        let frames: u32 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(120);
        let drag = parts.next().map(|m| m.contains("drag")).unwrap_or(false);
        let steps: u32 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(1);
        let sess = sess.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
            let Some(w) = sess.borrow().wins.first().cloned() else {
                return;
            };
            let start = std::time::Instant::now();
            let done = std::cell::Cell::new(0u32);
            let quit = sess.clone();
            w.annot.add_tick_callback(move |w, _clock| {
                if drag {
                    for _ in 0..steps {
                        if let Ok(mut s) = quit.try_borrow_mut() {
                            let bbox = s.doc.borrow().bbox();
                            let cur = s.doc.borrow().sel;
                            if let Some(mut sel) = cur {
                                let step = if (done.get() / 30) % 2 == 0 { 0.5 } else { -0.5 };
                                sel.x1 = (sel.x1 + step).clamp(sel.x0 + 40.0, bbox.2);
                                sel.y1 = (sel.y1 + step).clamp(sel.y0 + 40.0, bbox.3);
                                s.doc.borrow_mut().sel = Some(sel);
                                s.redraw();
                            }
                        }
                    }
                }
                w.queue_draw();
                let n = done.get() + 1;
                done.set(n);
                if n >= frames {
                    let dt = start.elapsed().as_secs_f64();
                    eprintln!(
                        "BENCH {} 帧{}，{:.2} ms/帧（{:.0} fps）",
                        frames,
                        if drag {
                            format!("（模拟拖拽，每帧 {steps} 个指针事件）")
                        } else {
                            String::new()
                        },
                        dt * 1000.0 / f64::from(frames),
                        f64::from(frames) / dt
                    );
                    quit.borrow().app.quit();
                    return glib::ControlFlow::Break;
                }
                glib::ControlFlow::Continue
            });
        });
    }

    // 供自动化验证使用：启动后按预设区域选择并落盘
    if let Ok(spec) = std::env::var("FOCUSSNAPS_TEST_SELECT") {
        let sess = sess.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
            let mut s = sess.borrow_mut();
            if let Some(rect) = parse_rect(&spec) {
                let idx = s.doc.borrow().sel_output().unwrap_or(0);
                s.doc.borrow_mut().sel = Some(Selection {
                    x0: rect.0,
                    y0: rect.1,
                    x1: rect.0 + rect.2,
                    y1: rect.1 + rect.3,
                });
                // 宣传图用：一套更完整的示例标注
                if std::env::var("FOCUSSNAPS_TEST_ANNOTATE").as_deref() == Ok("promo") {
                    let color = s.doc.borrow().color;
                    let width = s.doc.borrow().width;
                    let (x, y) = (rect.0, rect.1);
                    let label = "这段要改";
                    // 文字比其它标注大一号，宣传图里才看得清
                    let text_width = 6.5;
                    let size = s.measure_text(idx, label, crate::paint::text_px(text_width));
                    let mut d = s.doc.borrow_mut();
                    // 高亮一行
                    d.commit(Stroke {
                        tool: Tool::Highlighter,
                        color: (1.0, 0.78, 0.2),
                        width: 4.0,
                        points: vec![(x + 82.0, y + 152.0), (x + 355.0, y + 152.0)],
                        text: None,
                        number: None,
                        text_size: None,
                    });
                    // 框住一块区域
                    d.commit(Stroke {
                        tool: Tool::Rect,
                        color,
                        width,
                        points: vec![(x + 106.0, y + 254.0), (x + 372.0, y + 351.0)],
                        text: None,
                        number: None,
                        text_size: None,
                    });
                    // 箭头指向它
                    d.commit(Stroke {
                        tool: Tool::Arrow,
                        color,
                        width,
                        points: vec![(x + 510.0, y + 350.0), (x + 384.0, y + 312.0)],
                        text: None,
                        number: None,
                        text_size: None,
                    });
                    // 序号
                    d.commit(Stroke {
                        tool: Tool::Number,
                        color,
                        width,
                        points: vec![(x + 524.0, y + 362.0)],
                        text: None,
                        number: Some(1),
                        text_size: None,
                    });
                    // 文字说明
                    d.commit(Stroke {
                        tool: Tool::Text,
                        color,
                        width: text_width,
                        points: vec![(x + 110.0, y + 380.0)],
                        text: Some(label.into()),
                        number: None,
                        text_size: size,
                    });
                    d.text_sel = None;
                    drop(d);
                } else if std::env::var("FOCUSSNAPS_TEST_ANNOTATE").is_ok() {
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
                        let idx = s.doc.borrow().sel_output().unwrap_or(0);
                        if let Some(w) = s.win_of(idx).cloned() {
                            let at = put_entry(&w, x, y);
                            s.text_at = Some((idx, at.0 + w.origin.0, at.1 + w.origin.1));
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
                let out = s.doc.borrow().sel_output();
                if let Some(w) = out.and_then(|i| s.win_of(i)).cloned() {
                    for delay in [300, 600, 750] {
                        let w = w.clone();
                        glib::timeout_add_local_once(
                            std::time::Duration::from_millis(delay),
                            move || {
                                let area = (f64::from(w.screen.width()), f64::from(w.screen.height()));
                                w.ui.toggle_palette(area);
                            },
                        );
                    }
                }
            }
            if let Ok(path) = std::env::var("FOCUSSNAPS_TEST_SAVE") {
                test_save(sess.clone(), path, 0);
            }
        });
    }
    Ok(())
}

/// 自动化用：等画面都就绪了再存（最多等 5 秒）
fn test_save(sess: SessionRef, path: String, tries: u32) {
    glib::timeout_add_local_once(std::time::Duration::from_millis(100), move || {
        let sess2 = sess.clone();
        let mut s = sess.borrow_mut();
        if !s.all_ready() && tries < 50 {
            drop(s);
            test_save(sess2, path, tries + 1);
            return;
        }
        // 「@」表示走真实的保存路径（后台线程 + 默认目录），
        // 否则就按给定路径同步存一份，方便脚本直接拿文件
        if path == "@" {
            let idx = s.doc.borrow().sel_output();
            s.do_save(idx);
            return;
        }
        let s = s;
        let mut doc = s.doc.borrow_mut();
        let saved = crate::export::render_selection(&mut doc)
            .and_then(|mut surf| crate::export::save_to_path(&mut surf, &path));
        match saved {
            Ok(p) => println!("{}", p.display()),
            Err(e) => eprintln!("测试保存失败: {e}"),
        }
        s.app.quit();
    });
}

/// 后台抓屏：每块屏一个线程，抓回来一块就在主线程补一块画面
fn start_captures(
    sess: &SessionRef,
    targets: Vec<(usize, String, gdk::Rectangle, (f64, f64))>,
    cursor: bool,
) {
    if targets.is_empty() {
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel::<(
        usize,
        String,
        gdk::Rectangle,
        (f64, f64),
        Result<capture::Capture, String>,
    )>();
    for (idx, name, geo, logical) in targets {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let r = capture::capture_output(&name, cursor);
            let _ = tx.send((idx, name, geo, logical, r));
        });
    }
    drop(tx);
    let sess2 = sess.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(20), move || loop {
        match rx.try_recv() {
            Ok((idx, name, geo, logical, r)) => apply_capture(&sess2, idx, &name, geo, logical, r),
            Err(std::sync::mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return glib::ControlFlow::Break,
        }
    });
}

/// 把一块屏抓回来的画面接上：建画布、建纹理、识别窗口，再把图挂到窗口上
fn apply_capture(
    sess: &SessionRef,
    idx: usize,
    connector: &str,
    geo: gdk::Rectangle,
    size: (f64, f64),
    cap: Result<capture::Capture, String>,
) {
    let mut cap = match cap {
        Ok(c) => c,
        Err(e) => {
            eprintln!("抓取显示器 {connector} 失败: {e}");
            return;
        }
    };
    trace(&format!("{connector} 抓屏完（{}x{}）", cap.width, cap.height));
    let (width, height) = (cap.width as i32, cap.height as i32);
    // 共享内存那条路像素就在手边，先拷进画布（零拷贝的像素要等后台读）
    let phys = if cap.pixels.is_empty() {
        None
    } else {
        export::surface_from_bgra(cap.width, cap.height, &cap.pixels).ok()
    };
    // 优先零拷贝：合成器把画面画在显存里，纹理直接引它，
    // 既不用等 GPU 读回，也不用再往显存上传 44 MB
    let (texture, kept) = match cap.dmabuf {
        Some(buf) => match export::dmabuf_texture(&buf) {
            Ok(t) => {
                trace("走零拷贝（显存缓冲直接当纹理）");
                (t, Some(buf))
            }
            Err(e) => {
                // 导不进纹理就退回普通纹理（慢，但总比没画面强）
                eprintln!("{connector} 零拷贝纹理失败（{e}），改用像素上传");
                let pixels = std::mem::take(&mut cap.pixels);
                if pixels.is_empty() {
                    return;
                }
                (export::texture_from_bgra_owned(pixels, width, height), None)
            }
        },
        None => {
            trace("走共享内存（像素拷回 CPU）");
            (
                export::texture_from_bgra_owned(std::mem::take(&mut cap.pixels), width, height),
                None,
            )
        }
    };
    let sx = cap.width as f64 / size.0;
    let sy = cap.height as f64 / size.1;
    if (sx / sy - 1.0).abs() > 0.02 {
        eprintln!("显示器 {connector} 横纵缩放不一致（{sx:.3} / {sy:.3}），画面可能轻微拉伸");
    }
    let origin = (f64::from(geo.x()), f64::from(geo.y()));
    // 零拷贝的画面还要在后台读一份像素出来（导出、打码、窗口识别要用）
    let reader = kept.as_ref().map(|b| b.reader());
    let mut s = sess.borrow_mut();
    {
        let mut d = s.doc.borrow_mut();
        if let Some(o) = d.outs.get_mut(idx) {
            o.sx = sx;
            o.sy = sy;
            o.origin = origin;
            o.size = size;
            o.texture = Some(texture.clone());
            o.keep = kept;
            o.phys = phys;
        }
    }
    if let Some(w) = s.win_of(idx).cloned() {
        w.screen.set_paintable(Some(&texture));
        // 画面有了就不用再让窗口透明
        w.window.remove_css_class("fs-pending");
    }
    trace(&format!("{connector} 画面已挂上"));
    s.redraw();

    // 剩下的（读像素、识别窗口）放到后台，别拦着画面出现
    let sess2 = sess.clone();
    let name = connector.to_string();
    match reader {
        Some(reader) => {
            let (tx, rx) = std::sync::mpsc::channel::<Result<Vec<u8>, String>>();
            std::thread::spawn(move || {
                // 读显存是一大坨内存操作，降到最低优先级，别跟拖拽抢带宽
                // SAFETY: 只是把本线程的 nice 值调高
                unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 19) };
                let _ = tx.send(reader.read_pixels());
            });
            glib::timeout_add_local(std::time::Duration::from_millis(20), move || {
                match rx.try_recv() {
                    Ok(pixels) => {
                        finish_layer(&sess2, idx, &name, pixels.ok());
                        glib::ControlFlow::Break
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
                }
            });
        }
        // 共享内存：像素已经用上了，只差识别
        None => {
            glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
                finish_layer(&sess2, idx, &name, None)
            });
        }
    }

    // 画面没齐的时候按过的完成动作，等最后一块屏补齐再执行
    if s.all_ready() {
        if let Some(p) = s.pending.take() {
            match p {
                Export::Copy(i) => s.do_copy(i),
                Export::Save(i) => s.do_save(i),
            }
        }
    }
}

/// 画面挂上之后再做的那部分：备好像素、识别窗口
fn finish_layer(sess: &SessionRef, idx: usize, connector: &str, pixels: Option<Vec<u8>>) {
    let doc = sess.borrow().doc.clone();
    {
        let mut d = doc.borrow_mut();
        let Some(o) = d.outs.get_mut(idx) else {
            return;
        };
        let (sx, sy, size, origin) = (o.sx, o.sy, o.size, o.origin);
        // 零拷贝那条路的像素在后台读回来，这里只是拷进画布
        if let Some(pixels) = pixels {
            if let Some(t) = o.texture.as_ref() {
                let (w, h) = (t.width() as u32, t.height() as u32);
                match crate::export::surface_from_bgra(w, h, &pixels) {
                    Ok(surface) => o.phys = Some(surface),
                    Err(e) => eprintln!("建画布失败: {e}"),
                }
            }
        }
        let Some(phys) = o.ensure_phys() else {
            return;
        };
        let mut rects = crate::windows::detect_for_output(connector, phys, sx, sy, size);
        trace(&format!("{connector} 窗口识别完（{} 块）", rects.len()));
        // 窗口矩形跟着换成全局坐标，交互就只用一套坐标系
        for r in rects.iter_mut() {
            r.x0 += origin.0;
            r.x1 += origin.0;
            r.y0 += origin.1;
            r.y1 += origin.1;
        }
        o.windows = rects;
    }
    let mut s = sess.borrow_mut();
    s.redraw();
    // 画面齐了就把之前按下的完成动作做掉
    if s.all_ready() {
        if let Some(p) = s.pending.take() {
            match p {
                Export::Copy(i) => s.do_copy(i),
                Export::Save(i) => s.do_save(i),
            }
        }
    }
}

impl Session {
    /// 记下最新的指针位置；拖动和悬停各自只保留最后一个
    fn queue_pointer(&mut self, idx: usize, gx: f64, gy: f64) {
        if self.drag.kind != DragKind::None {
            self.pending_drag = Some((idx, gx, gy));
        } else {
            self.pending_hover = Some((gx, gy));
        }
    }
}

/// 排一次「下一帧」来消费最新的指针位置
///
/// 一定要挂帧时钟而不是空闲回调：空闲回调在指针密集时一秒能跑上千次，
/// 等于没合并；帧时钟保证每个屏幕上真正要画的帧最多更新一次。
fn schedule_pointer(sess: &SessionRef) {
    let Ok(mut s) = sess.try_borrow_mut() else {
        return;
    };
    if s.pointer_queued {
        return;
    }
    let Some(w) = s.wins.first().cloned() else {
        return;
    };
    s.pointer_queued = true;
    drop(s);
    let sess2 = sess.clone();
    w.screen.add_tick_callback(move |_w, _clock| {
        let Ok(mut s) = sess2.try_borrow_mut() else {
            // 借用不到就等下一个事件再排
            return glib::ControlFlow::Break;
        };
        s.pointer_queued = false;
        if let Some((i, x, y)) = s.pending_drag.take() {
            on_drag_update(&mut s, i, x, y);
            s.redraw();
        }
        if let Some((x, y)) = s.pending_hover.take() {
            if s.update_hover(x, y) {
                s.redraw();
            }
        }
        glib::ControlFlow::Break
    });
}

/// 建一个显示器的浮层窗口
fn make_window(app: &gtk::Application, sess: &SessionRef, idx: usize, monitor: &gdk::Monitor) -> Win {
    let (origin, rect) = {
        let s = sess.borrow();
        let doc = s.doc.borrow();
        match doc.outs.get(idx) {
            Some(o) => (o.origin, o.rect()),
            None => ((0.0, 0.0), (0.0, 0.0, 0.0, 0.0)),
        }
    };
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .decorated(false)
        .build();
    window.set_title(Some("FocusSnapS"));

    // 屏幕画面单独放一层 GtkPicture：纹理只在启动时上传一次，之后每帧由
    // GPU 合成。以前是每帧用 cairo 把整屏图像重画一遍（5120x2160 就是 44 MB），
    // 负载一高就掉帧。
    let picture = gtk::Picture::new();
    picture.set_hexpand(true);
    picture.set_vexpand(true);
    // 纹理就是物理分辨率，按需拉满整块屏，避免二次采样发虚
    picture.set_content_fit(gtk::ContentFit::Fill);
    // 画面抓到之前窗口是透明的，让桌面透出来（见 ui.rs 里的 .fs-pending）
    window.add_css_class("fs-pending");
    // 手势和光标就挂在画面这一层：下面没有别的东西了
    picture.set_cursor_from_name(Some("crosshair"));

    // 标注层尺寸跟着选区走（见 sync_annot），平时只覆盖屏幕的一小块
    let annot = gtk::DrawingArea::new();
    annot.set_can_target(false);
    annot.set_halign(gtk::Align::Start);
    annot.set_valign(gtk::Align::Start);
    annot.set_visible(false);
    let view: Rc<std::cell::Cell<(f64, f64)>> = Rc::new(std::cell::Cell::new(origin));

    // 压暗是一整块固定的 CSS 实心矩形：位置尺寸都不变，拖拽时不用重画它，
    // 合成器也就不必每帧搬整屏
    let dim = gtk::Box::new(gtk::Orientation::Vertical, 0);
    dim.add_css_class("fs-dim");
    dim.set_can_target(false);

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&picture));
    overlay.add_overlay(&dim);
    overlay.set_measure_overlay(&dim, false);
    overlay.add_overlay(&annot);
    overlay.set_measure_overlay(&annot, false);
    overlay.set_clip_overlay(&annot, true);

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
    let ui = Ui::build(&sess.borrow().doc, &picture, &annot, &actions);
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
        let view = view.clone();
        let drawn = std::cell::Cell::new(false);
        let overlay2 = overlay.clone();
        annot.set_draw_func(move |a, cr, _w, _h| {
            if !drawn.get() {
                drawn.set(true);
                trace("首帧光栅化");
            }
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
            // 画布原点用控件「实际」落在哪，而不是上一帧算出来的位置：
            // 拖拽时标注层跟着选区走，GTK 的布局要到下一帧才落地，
            // 用算出来的位置会让洞里的画面和边框错开。
            let view = a
                .compute_bounds(&overlay2)
                .map(|b| (origin.0 + f64::from(b.x()), origin.1 + f64::from(b.y())))
                .unwrap_or_else(|| view.get());
            paint::paint(cr, &mut doc, idx, view, preview);
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
                on_drag_begin(&mut s, idx, x + origin.0, y + origin.1);
                s.redraw();
            }
        });
        drag.connect_drag_update({
            let sess = sess.clone();
            move |_g, ox, oy| {
                // 高回报率鼠标一秒能报上千个位置，同一帧里只处理最新那个：
                // 每个事件都跑一遍几何计算和 widget 更新纯属浪费
                let anchor = sess.borrow().drag.anchor;
                sess.borrow_mut().queue_pointer(idx, anchor.0 + ox, anchor.1 + oy);
                schedule_pointer(&sess);
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
        picture.add_controller(drag);
    }

    // 左键抬起兜底：拖到另一块屏上松手时，结束事件可能落在那块屏的窗口上
    {
        let sess = sess.clone();
        let click = gtk::GestureClick::new();
        click.set_button(1);
        click.connect_released({
            let sess = sess.clone();
            move |_g, _n, x, y| {
                let mut s = sess.borrow_mut();
                if s.drag.kind == DragKind::None {
                    return;
                }
                on_drag_end(&mut s, idx, x + origin.0, y + origin.1);
                s.redraw();
            }
        });
        picture.add_controller(click);
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
                let (gx, gy) = (x + origin.0, y + origin.1);
                let changed = s.update_hover(gx, gy);
                if s.drag.kind != DragKind::None {
                    // 指针跨到另一块屏上，拖拽交给这块屏接着算
                    on_drag_update(&mut s, idx, gx, gy);
                    s.redraw();
                } else if changed {
                    s.redraw();
                }
            }
        });
        motion.connect_motion(move |_c, x, y| {
            // 同样按帧合并：拖动时这个处理器和 gesture 的 drag_update 都会收到
            // 同一个位置，一个个处理等于干两遍
            sess.borrow_mut().queue_pointer(idx, x + origin.0, y + origin.1);
            schedule_pointer(&sess);
        });
        // 调试用：把指针的逻辑坐标打到 stderr
        if std::env::var("FOCUSSNAPS_TEST_POINTER").is_ok() {
            motion.connect_motion(|_, x, y| eprintln!("POS {x:.1} {y:.1}"));
        }
        picture.add_controller(motion);
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
        picture.add_controller(right);
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
        picture.add_controller(scroll);
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
    // 尺寸对齐到合成器自己报的逻辑尺寸：GDK 的几何可能是四舍五入过的，
    // 差一个逻辑像素会让整块浮层被缩放一次（DP-1：GDK 3793 → 缓冲 5121，
    // 而输出是 5120）。对不上就自己定尺寸，只锚左上。
    let gdk_geo = monitor.geometry();
    let own_size = {
        let s = sess.borrow();
        let doc = s.doc.borrow();
        doc.outs.get(idx).map(|o| {
            (
                o.size.0.round() as i32,
                o.size.1.round() as i32,
            )
        })
    }
    .filter(|(w, h)| *w > 0 && *h > 0 && (*w != gdk_geo.width() || *h != gdk_geo.height()));
    if let Some((w, h)) = own_size {
        window.set_default_size(w, h);
    }
    window.set_namespace(Some("focussnaps"));
    window.set_layer(Layer::Overlay);
    window.set_monitor(Some(monitor));
    for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
        // 自己定尺寸时只锚左上，否则合成器会按它自己的尺寸把表面拉满
        let anchor = own_size.is_none() || matches!(edge, Edge::Top | Edge::Left);
        window.set_anchor(edge, anchor);
    }
    window.set_exclusive_zone(-1);
    window.set_keyboard_mode(KeyboardMode::Exclusive);

    Win {
        idx,
        window,
        screen: picture.clone(),
        annot,
        view,
        ui,
        origin,
        rect,
    }
}

impl Session {
    /// 需要重画的范围（全局）：有选区就是选区，否则是悬停窗口
    fn dirty_rect(&self) -> Option<Rect4> {
        let d = self.doc.borrow();
        if let Some(s) = d.sel {
            return Some(s.normalized());
        }
        d.hover.as_ref().map(|r| (r.x0, r.y0, r.x1, r.y1))
    }

    /// 让标注层只覆盖会画出东西的那一块：选区（或悬停窗口）加余量
    ///
    /// 以前这层铺满整屏，每帧都要把整块屏（5120x2160 是 44 MB）重新光栅化
    /// 并上传成纹理；缩到选区大小之后，拖拽时每帧只有几个像素的活要干。
    fn sync_annot(&self) {
        let live = {
            let d = self.doc.borrow();
            match d.sel {
                Some(s) => Some(s.normalized()),
                None => d.hover.as_ref().map(|r| (r.x0, r.y0, r.x1, r.y1)),
            }
        };
        for w in &self.wins {
            let Some((x0, y0, x1, y1)) = paint::annot_rect(live, w.rect) else {
                if w.annot.is_visible() {
                    w.annot.set_visible(false);
                }
                continue;
            };
            let (x, y) = (x0.round() as i32, y0.round() as i32);
            let (bw, bh) = ((x1 - x0).round() as i32, (y1 - y0).round() as i32);
            if bw <= 0 || bh <= 0 {
                if w.annot.is_visible() {
                    w.annot.set_visible(false);
                }
                continue;
            }
            let view = (w.rect.0 + x0.round(), w.rect.1 + y0.round());
            if w.view.get() != view {
                w.view.set(view);
                // 平移变了，整层内容都要跟着挪
                w.annot.queue_draw();
            }
            if w.annot.margin_start() != x {
                w.annot.set_margin_start(x);
            }
            if w.annot.margin_top() != y {
                w.annot.set_margin_top(y);
            }
            if w.annot.width_request() != bw || w.annot.height_request() != bh {
                w.annot.set_size_request(bw, bh);
            }
            if !w.annot.is_visible() {
                w.annot.set_visible(true);
            }
        }
    }

    /// 重绘：只刷「新旧脏区」碰到的屏，跨屏拖动时相关的那几块一起刷
    fn redraw(&mut self) {
        let t = std::time::Instant::now();
        if let Some(prev) = self.last_redraw {
            let gap = t.duration_since(prev).as_millis();
            // 拖拽期间两次重绘隔太久，说明卡在等系统（合成器没给帧、被别的进程抢）
            if gap > 40 && self.drag.kind != DragKind::None {
                trace(&format!("重绘间隔 {gap} ms（卡在等系统）"));
            }
        }
        self.last_redraw = Some(t);
        self.sync_annot();
        let next = self.dirty_rect();
        let dirty = match (self.dirty, next) {
            (Some(a), Some(b)) => Some(union(a, b)),
            // 脏区消失（比如清空了选区）就全刷，避免留下压暗的旧画面
            _ => None,
        };
        self.dirty = next;
        for w in &self.wins {
            if dirty.map(|d| intersect(d, w.rect).is_some()).unwrap_or(true) {
                w.annot.queue_draw();
            }
        }
        let cost = t.elapsed().as_millis();
        // 我们自己的这一趟要是一直很慢就是代码问题
        if cost > 10 {
            trace(&format!("redraw 本身花了 {cost} ms"));
        }
    }

    /// 所有屏的画面都抓回来了？
    fn all_ready(&self) -> bool {
        self.doc.borrow().outs.iter().all(|o| o.ready())
    }

    /// 更新悬停窗口，返回是否有变化
    fn update_hover(&mut self, gx: f64, gy: f64) -> bool {
        let next = {
            let d = self.doc.borrow();
            if d.sel.is_some() {
                None
            } else {
                d.window_at(gx, gy)
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
        let owner = self.doc.borrow().sel_output();
        for w in &self.wins {
            w.ui.sync_tool(tool);
            let on_sel = owner == Some(w.idx) && sel.is_some() && !selecting;
            if let (true, Some(s)) = (on_sel, sel) {
                let area = (f64::from(w.screen.width()), f64::from(w.screen.height()));
                let r = s.normalized();
                // 工具条是窗口里的控件，摆位得用本屏局部坐标
                let local = (
                    r.0 - w.origin.0,
                    r.1 - w.origin.1,
                    r.2 - w.origin.0,
                    r.3 - w.origin.1,
                );
                // 先显示，measure() 才量得到自然尺寸
                w.ui.root.set_visible(true);
                w.ui.place(local, area);
                w.ui.hint.set_visible(false);
            } else {
                w.ui.root.set_visible(false);
                w.ui.palette.set_visible(false);
                w.ui.hint.set_visible(sel.is_none());
            }
            // 截图时指针始终是十字准星（只有打字时换成 I 形）
            let cursor = match tool {
                Tool::Text => "text",
                _ => "crosshair",
            };
            w.screen.set_cursor_from_name(Some(cursor));
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
        let surface = self.doc.borrow().outs.get(out).and_then(|o| o.phys.clone())?;
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
        let rect = self.doc.borrow().outs.get(idx).map(|o| o.rect());
        if let Some((x0, y0, x1, y1)) = rect {
            self.doc.borrow_mut().sel = Some(Selection { x0, y0, x1, y1 });
        }
    }

    /// 复制选区并退出
    fn do_copy(&mut self, idx: Option<usize>) {
        if self.exporting {
            return;
        }
        self.ensure_selection(idx);
        if !self.all_ready() {
            trace("画面还没就绪，复制等一会儿");
            self.pending = Some(Export::Copy(idx));
            return;
        }
        let Some((rgba, w, h)) = self.report(self.render_rgba(), "复制失败") else {
            return;
        };
        self.exporting = true;
        let rgba = std::sync::Arc::new(rgba);
        let worker = rgba.clone();
        let (tx, rx) = std::sync::mpsc::channel::<bool>();
        std::thread::spawn(move || {
            let copied = match export::encode_png_rgba(&worker, w, h) {
                Ok(png) => export::copy_png_via_wl_copy(&png).is_ok(),
                Err(e) => {
                    eprintln!("复制失败: {e}");
                    false
                }
            };
            let _ = tx.send(copied);
        });
        let app = self.app.clone();
        when_done(rx, move |copied| {
            if copied {
                export::notify("FocusSnapS", "已复制到剪贴板");
                app.quit();
                return;
            }
            // wl-copy 用不了就退回 GTK 剪贴板，这一步只能在主线程做
            let clipboard = app
                .windows()
                .first()
                .and_then(|w| w.clone().downcast::<gtk::ApplicationWindow>().ok())
                .map(|w| w.clipboard());
            match (clipboard, export::texture_from_rgba(&rgba[..], w, h)) {
                (Some(cb), Ok(texture)) => cb.set_texture(&texture),
                (Some(_), Err(e)) => eprintln!("复制失败: {e}"),
                (None, _) => eprintln!("复制失败: 没有可用窗口"),
            }
            app.quit();
        });
    }

    /// 保存选区并退出
    fn do_save(&mut self, idx: Option<usize>) {
        if self.exporting {
            return;
        }
        self.ensure_selection(idx);
        if !self.all_ready() {
            trace("画面还没就绪，保存等一会儿");
            self.pending = Some(Export::Save(idx));
            return;
        }
        let dir = self.doc.borrow().dir.clone();
        let Some((rgba, w, h)) = self.report(self.render_rgba(), "保存失败") else {
            return;
        };
        self.exporting = true;
        let (tx, rx) = std::sync::mpsc::channel::<Result<std::path::PathBuf, String>>();
        std::thread::spawn(move || {
            let _ = tx.send(export::save_rgba_png(&rgba, w, h, &dir));
        });
        let app = self.app.clone();
        when_done(rx, move |saved| {
            match saved {
                Ok(path) => {
                    println!("{}", path.display());
                    export::notify("截图已保存", &path.display().to_string());
                }
                Err(e) => eprintln!("保存失败: {e}"),
            }
            app.quit();
        });
    }

    /// 把选区渲染成 RGBA 像素
    ///
    /// 这一步要用 cairo 画布，只能在主线程做；后面的编码、写盘、喂给 wl-copy
    /// 都是几百毫秒的活，全甩给后台线程。
    fn render_rgba(&self) -> Result<(Vec<u8>, i32, i32), String> {
        let mut doc = self.doc.borrow_mut();
        let mut surface = export::render_selection(&mut doc)?;
        export::surface_rgba(&mut surface)
    }

    /// 出错就打一行日志并返回 None
    fn report<T>(&self, r: Result<T, String>, what: &str) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                eprintln!("{what}: {e}");
                None
            }
        }
    }
}

/// 等后台线程把结果送回来，再在主线程上收尾
fn when_done<T: Send + 'static>(rx: std::sync::mpsc::Receiver<T>, done: impl Fn(T) + 'static) {
    glib::timeout_add_local(std::time::Duration::from_millis(30), move || {
        match rx.try_recv() {
            Ok(v) => {
                done(v);
                glib::ControlFlow::Break
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            // 线程没了却没送回结果，只能作罢
            Err(std::sync::mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
        }
    });
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
    let area_w = f64::from(w.screen.width());
    let area_h = f64::from(w.screen.height());
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
    // x、y 是全局逻辑坐标，跨屏框选时同一套坐标直接就能用
    s.drag = DragState {
        kind: DragKind::None,
        anchor: (x, y),
        saved_sel: sel,
        saved_strokes: s.doc.borrow().strokes.len(),
        saved_number: s.doc.borrow().next_number,
        text_from: (0.0, 0.0),
        moved: false,
    };

    let cur = sel.map(|s| s.normalized());

    // 点在文字框上：选中它并拖动
    if tool == Tool::Select {
        let hit = s.doc.borrow().text_at(x, y);
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

fn on_drag_update(s: &mut Session, _idx: usize, x: f64, y: f64) {
    // 选区可以横跨多块屏，所以夹在整个布局的包围盒里，而不是当前这块屏
    let bbox = s.doc.borrow().bbox();
    let (x, y) = (x.clamp(bbox.0, bbox.2), y.clamp(bbox.1, bbox.3));
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
            let nx0 = (x0 + dx).clamp(bbox.0, (bbox.2 - rw).max(bbox.0));
            let ny0 = (y0 + dy).clamp(bbox.1, (bbox.3 - rh).max(bbox.1));
            let mut d = s.doc.borrow_mut();
            d.sel = Some(Selection {
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
            // 文字框整体留在选区内
            let area = s.doc.borrow().sel.map(|s| s.normalized()).unwrap_or(bbox);
            let nx = (fx + dx).clamp(area.0, (area.2 - bw).max(area.0));
            let ny = (fy + dy).clamp(area.1, (area.3 - bh).max(area.1));
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
            d.sel = Some(Selection { x0, y0, x1, y1 });
        }
        DragKind::Draw => {
            let mut d = s.doc.borrow_mut();
            if let Some(a) = d.active.as_mut() {
                if a.tool.is_freehand() {
                    // 高回报率鼠标一次拖动能报上千个点，轨迹画起来是线性的，
                    // 间距不到 1 个逻辑像素的点直接丢掉，画出来看不出差别
                    let far = a
                        .points
                        .last()
                        .map(|p| (p.0 - x).abs() >= 1.0 || (p.1 - y).abs() >= 1.0)
                        .unwrap_or(true);
                    if far {
                        a.points.push((x, y));
                    }
                } else {
                    a.points.truncate(1);
                    a.points.push((x, y));
                }
            }
        }
        DragKind::None => {}
    }
    // 这里不调 sync_ui：拖动期间工具条本来就藏起来了，位置和可见性都不会变，
    // 每个指针事件跑一遍只是白做 GTK 布局和光标设置。松手时 on_drag_end 会同步。
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
                let hover = s.doc.borrow().hover.filter(|r| r.contains(x, y));
                let saved_sel = s.drag.saved_sel;
                let saved_n = s.drag.saved_strokes;
                let saved_num = s.drag.saved_number;
                let mut d = s.doc.borrow_mut();
                d.sel = saved_sel;
                d.strokes.truncate(saved_n);
                d.next_number = saved_num;
                if saved_sel.is_none() {
                    if let Some(r) = hover {
                        d.sel = Some(Selection {
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
                    d.sel = Some(Selection { x0, y0, x1, y1 });
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
    if !contains(r, x, y) {
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
                // 输入框是窗口里的控件，摆位用本屏局部坐标，落点仍按全局坐标记
                let (lx, ly) = (x - w.origin.0, y - w.origin.1);
                let at = put_entry(&w, lx, ly);
                s.text_at = Some((idx, at.0 + w.origin.0, at.1 + w.origin.1));
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
    let sel_out = s.doc.borrow().sel_output();
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
                let rect = {
                    let d = s.doc.borrow();
                    d.outs.get(idx).map(|o| o.rect())
                };
                if let Some((x0, y0, x1, y1)) = rect {
                    s.doc.borrow_mut().sel = Some(Selection { x0, y0, x1, y1 });
                }
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
            let bbox = s.doc.borrow().bbox();
            let mut moved = false;
            {
                let mut d = s.doc.borrow_mut();
                if let Some(sel) = d.sel {
                    let (x0, y0, x1, y1) = sel.normalized();
                    let nx0 = (x0 + dx).clamp(bbox.0, (bbox.2 - (x1 - x0)).max(bbox.0));
                    let ny0 = (y0 + dy).clamp(bbox.1, (bbox.3 - (y1 - y0)).max(bbox.1));
                    d.sel = Some(Selection {
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
