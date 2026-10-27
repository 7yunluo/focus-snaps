// niri IPC：取当前输出可见工作区的窗口尺寸，用来给画面识别做先验
use std::process::Command;

#[derive(Clone, Debug)]
pub struct WinInfo {
    pub floating: bool,
    /// 窗口实际逻辑尺寸
    pub size: (f64, f64),
    /// 所在列的宽度
    pub tile: (f64, f64),
    /// 列的序号（同一列里的多个窗口是 tab）
    pub col: u32,
    /// 浮动窗口在工作区视图里的位置
    pub tile_pos: Option<(f64, f64)>,
    /// 窗口相对自己所在 tile 的偏移（窗口比 tile 小时不为 0）
    pub offset: (f64, f64),
    /// 是否在滚动布局里；全屏/最大化的窗口不在
    pub tiled: bool,
    /// 所在 tile 的尺寸
    pub tile_size: (f64, f64),
}

fn run_json(args: &[&str]) -> Option<serde_json::Value> {
    let out = Command::new("niri").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// 取某块显示器上当前活动工作区的窗口列表
pub fn active_workspace_windows(output: &str) -> Option<Vec<WinInfo>> {
    let workspaces = run_json(&["msg", "--json", "workspaces"])?;
    let ws_id = workspaces
        .as_array()?
        .iter()
        .find(|w| {
            w["output"].as_str() == Some(output) && w["is_active"].as_bool().unwrap_or(false)
        })
        .and_then(|w| w["id"].as_u64())?;

    let windows = run_json(&["msg", "--json", "windows"])?;
    let mut out = Vec::new();
    for w in windows.as_array()? {
        if w["workspace_id"].as_u64() != Some(ws_id) {
            continue;
        }
        let layout = &w["layout"];
        let num = |v: &serde_json::Value, i: usize| v[i].as_f64().unwrap_or(0.0);
        let size = (num(&layout["window_size"], 0), num(&layout["window_size"], 1));
        let tile = (num(&layout["tile_size"], 0), num(&layout["tile_size"], 1));
        let tiled = layout["pos_in_scrolling_layout"].is_array();
        let col = layout["pos_in_scrolling_layout"][0]
            .as_u64()
            .unwrap_or(0) as u32;
        let offset = (
            layout["window_offset_in_tile"][0].as_f64().unwrap_or(0.0),
            layout["window_offset_in_tile"][1].as_f64().unwrap_or(0.0),
        );
        let tile_pos = {
            let p = &layout["tile_pos_in_workspace_view"];
            if p.is_array() {
                Some((num(p, 0), num(p, 1)))
            } else {
                None
            }
        };
        out.push(WinInfo {
            floating: w["is_floating"].as_bool().unwrap_or(false),
            size,
            tile,
            col,
            tile_pos,
            offset,
            tiled,
            tile_size: tile,
        });
    }
    // 空工作区也返回 Some（空列表），与「拿不到 IPC」区分开
    Some(out)
}

/// 当前焦点显示器的名字
pub fn focused_output() -> Option<String> {
    let v = run_json(&["msg", "--json", "focused-output"])?;
    Some(v["name"].as_str()?.to_string())
}

/// 在后台盯着焦点显示器，被切到别处就抢回来
///
/// 每次轮询都要 fork/exec 一次 `niri msg`（实测 4 ms 上下，负载高时更久），
/// 放在主线程上等于每 500 ms 吃掉一帧的预算，整段必须丢到独立线程里。
/// 进程退出时线程跟着没，不需要收拾。
pub fn keep_focus_in_background(want: String) {
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if let Some(cur) = focused_output() {
            if cur != want {
                focus_output(&want);
            }
        }
    });
}

/// 把键盘焦点切到指定显示器
pub fn focus_output(name: &str) -> bool {
    Command::new("niri")
        .args(["msg", "action", "focus-monitor", name])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 读 niri 配置里的窗口间隙，读不到就用默认值
pub fn gaps() -> f64 {
    let home = std::env::var("HOME").unwrap_or_default();
    let base = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| format!("{home}/.config"));
    for name in ["layout.kdl", "config.kdl"] {
        let path = format!("{base}/niri/{name}");
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            let l = line.trim();
            if let Some(rest) = l.strip_prefix("gaps") {
                if let Ok(n) = rest.trim().parse::<f64>() {
                    return n;
                }
            }
        }
    }
    10.0
}
