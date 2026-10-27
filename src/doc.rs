// 截图文档：选区、标注笔画、工具与画笔参数
use gtk::cairo;

use crate::windows::Rect;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Select,
    Brush,
    Highlighter,
    Line,
    Arrow,
    Rect,
    Ellipse,
    Text,
    Number,
    Mosaic,
    Crop,
}

impl Tool {
    pub const ALL: [Tool; 11] = [
        Tool::Select,
        Tool::Brush,
        Tool::Highlighter,
        Tool::Line,
        Tool::Arrow,
        Tool::Rect,
        Tool::Ellipse,
        Tool::Text,
        Tool::Number,
        Tool::Mosaic,
        Tool::Crop,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tool::Select => "选择",
            Tool::Brush => "画笔",
            Tool::Highlighter => "高亮",
            Tool::Line => "直线",
            Tool::Arrow => "箭头",
            Tool::Rect => "矩形",
            Tool::Ellipse => "椭圆",
            Tool::Text => "文字",
            Tool::Number => "序号",
            Tool::Mosaic => "马赛克",
            Tool::Crop => "裁剪",
        }
    }

    pub fn tip(self) -> &'static str {
        match self {
            Tool::Select => "拖动选区边缘可缩放，拖动内部可移动",
            Tool::Brush => "自由绘制",
            Tool::Highlighter => "半透明粗笔",
            Tool::Line => "拖出一条直线",
            Tool::Arrow => "拖出一个箭头",
            Tool::Rect => "拖出一个矩形框",
            Tool::Ellipse => "拖出一个椭圆框",
            Tool::Text => "点击后输入文字",
            Tool::Number => "点击放置递增序号",
            Tool::Mosaic => "拖出区域并打码",
            Tool::Crop => "把选区裁剪成框出的范围",
        }
    }

    /// 边拖边记录轨迹的自由笔
    pub fn is_freehand(self) -> bool {
        matches!(self, Tool::Brush | Tool::Highlighter)
    }

    pub fn shortcut(self) -> char {
        match self {
            Tool::Select => 'v',
            Tool::Brush => 'b',
            Tool::Highlighter => 'h',
            Tool::Line => 'l',
            Tool::Arrow => 'a',
            Tool::Rect => 'r',
            Tool::Ellipse => 'e',
            Tool::Text => 't',
            Tool::Number => 'n',
            Tool::Mosaic => 'm',
            Tool::Crop => 'c',
        }
    }
}

#[derive(Clone, Debug)]
pub struct Stroke {
    pub tool: Tool,
    pub color: (f64, f64, f64),
    pub width: f64,
    pub points: Vec<(f64, f64)>,
    pub text: Option<String>,
    pub number: Option<u32>,
}

/// 选区，坐标是所在显示器的本地逻辑坐标
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Selection {
    pub out: usize,
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Selection {
    pub fn normalized(&self) -> (f64, f64, f64, f64) {
        (
            self.x0.min(self.x1),
            self.y0.min(self.y1),
            self.x0.max(self.x1),
            self.y0.max(self.y1),
        )
    }
    pub fn width(&self) -> f64 {
        (self.x1 - self.x0).abs()
    }
    pub fn height(&self) -> f64 {
        (self.y1 - self.y0).abs()
    }
}

/// 一个显示器的画面与坐标换算
pub struct OutputLayer {
    /// 逻辑尺寸
    pub size: (f64, f64),
    /// 物理像素 / 逻辑像素
    pub sx: f64,
    pub sy: f64,
    /// 全分辨率画面，屏幕观感方向
    pub phys: cairo::ImageSurface,
    /// 这一屏上识别出来的窗口矩形（本屏逻辑坐标）
    pub windows: Vec<Rect>,
}

pub struct Doc {
    pub outs: Vec<OutputLayer>,
    pub sel: Option<Selection>,
    pub strokes: Vec<Stroke>,
    pub redo: Vec<Stroke>,
    pub tool: Tool,
    pub color: (f64, f64, f64),
    pub width: f64,
    pub next_number: u32,
    pub dir: String,
    /// 正在拖拽的临时笔画
    pub active: Option<Stroke>,
    /// 指针悬停的窗口（显示器下标 + 矩形）
    pub hover: Option<(usize, Rect)>,
}

impl Doc {
    pub fn new(outs: Vec<OutputLayer>, dir: String) -> Self {
        Self {
            outs,
            sel: None,
            strokes: Vec::new(),
            redo: Vec::new(),
            tool: Tool::Select,
            color: (1.0, 0.45, 0.35),
            width: 4.0,
            next_number: 1,
            dir,
            active: None,
            hover: None,
        }
    }

    /// 指针位置落在哪个窗口上
    pub fn window_at(&self, out: usize, x: f64, y: f64) -> Option<Rect> {
        self.outs
            .get(out)?
            .windows
            .iter()
            .find(|r| r.contains(x, y))
            .copied()
    }

    /// 提交一笔标注
    pub fn commit(&mut self, stroke: Stroke) {
        self.strokes.push(stroke);
        self.redo.clear();
    }

    pub fn undo(&mut self) -> bool {
        match self.strokes.pop() {
            Some(s) => {
                self.redo.push(s);
                self.recount();
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self) -> bool {
        match self.redo.pop() {
            Some(s) => {
                self.strokes.push(s);
                self.recount();
                true
            }
            None => false,
        }
    }

    /// 序号工具的当前值始终等于已有最大序号加一
    fn recount(&mut self) {
        self.next_number = self
            .strokes
            .iter()
            .filter_map(|x| x.number)
            .max()
            .unwrap_or(0)
            + 1;
    }

    /// 重置选区与标注
    pub fn clear_all(&mut self) {
        self.sel = None;
        self.strokes.clear();
        self.redo.clear();
        self.active = None;
        self.hover = None;
        self.next_number = 1;
    }
}

/// 保存目录，优先 FOCUSSNAPS_DIR
pub fn default_dir() -> String {
    if let Ok(dir) = std::env::var("FOCUSSNAPS_DIR") {
        return dir;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let pictures = std::env::var("XDG_PICTURES_DIR")
        .ok()
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| format!("{home}/Pictures"));
    format!("{pictures}/Screenshots")
}
