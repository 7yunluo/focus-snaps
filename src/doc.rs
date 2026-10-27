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
    /// 文字排版后的尺寸（逻辑像素）
    pub text_size: Option<(f64, f64)>,
}

/// 全局逻辑坐标下的矩形：左、上、右、下
pub type Rect4 = (f64, f64, f64, f64);

/// 两个矩形的交集，没有交集（或只贴边）时返回 None
pub fn intersect(a: Rect4, b: Rect4) -> Option<Rect4> {
    let r = (a.0.max(b.0), a.1.max(b.1), a.2.min(b.2), a.3.min(b.3));
    if r.2 > r.0 && r.3 > r.1 {
        Some(r)
    } else {
        None
    }
}

/// 两个矩形的并集
pub fn union(a: Rect4, b: Rect4) -> Rect4 {
    (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))
}

/// 选区，坐标是全局逻辑坐标（所有显示器拼起来的布局空间）
///
/// 跨屏框选就是一块比单屏更大的矩形，各块屏自己跟它求交集，不用为「跨屏」单开一条路径。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Selection {
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
    /// 在全局布局里的左上角（逻辑坐标）
    pub origin: (f64, f64),
    /// 逻辑尺寸
    pub size: (f64, f64),
    /// 物理像素 / 逻辑像素
    pub sx: f64,
    pub sy: f64,
    /// 全分辨率画面，屏幕观感方向
    pub phys: cairo::ImageSurface,
    /// 这一屏上识别出来的窗口矩形（全局逻辑坐标）
    pub windows: Vec<Rect>,
    /// 打码用的块平均小图，第一次用到时才建（见 paint::mosaic_image）
    pub mosaic: Option<cairo::ImageSurface>,
}

impl OutputLayer {
    pub fn new(
        origin: (f64, f64),
        size: (f64, f64),
        sx: f64,
        sy: f64,
        phys: cairo::ImageSurface,
        windows: Vec<Rect>,
    ) -> Self {
        Self {
            origin,
            size,
            sx,
            sy,
            phys,
            windows,
            mosaic: None,
        }
    }

    /// 本屏在全局布局里占的矩形
    pub fn rect(&self) -> Rect4 {
        (
            self.origin.0,
            self.origin.1,
            self.origin.0 + self.size.0,
            self.origin.1 + self.size.1,
        )
    }
}

/// 默认标注颜色
pub const DEFAULT_COLOR: (f64, f64, f64) = (1.0, 0.32, 0.28);

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
    /// 指针悬停的窗口（全局逻辑坐标）
    pub hover: Option<Rect>,
    /// 正在带框显示、可拖动的文字笔画下标
    pub text_sel: Option<usize>,
    /// 界面强调色（跟随系统主题色）
    pub accent: (f64, f64, f64),
}

impl Doc {
    pub fn new(outs: Vec<OutputLayer>, dir: String) -> Self {
        Self {
            outs,
            sel: None,
            strokes: Vec::new(),
            redo: Vec::new(),
            tool: Tool::Select,
            color: DEFAULT_COLOR,
            width: 4.0,
            next_number: 1,
            dir,
            active: None,
            hover: None,
            text_sel: None,
            accent: crate::theme::FALLBACK,
        }
    }

    /// 全局坐标落在哪块屏上
    pub fn out_at(&self, x: f64, y: f64) -> Option<usize> {
        self.outs.iter().position(|o| {
            let r = o.rect();
            x >= r.0 && x <= r.2 && y >= r.1 && y <= r.3
        })
    }

    /// 所有屏拼起来的包围盒
    pub fn bbox(&self) -> Rect4 {
        self.outs
            .iter()
            .map(|o| o.rect())
            .reduce(union)
            .unwrap_or((0.0, 0.0, 0.0, 0.0))
    }

    /// 选区左上角所在的显示器，用于决定工具条 / 剪贴板挂在哪个窗口上
    pub fn sel_output(&self) -> Option<usize> {
        let s = self.sel?.normalized();
        self.out_at(s.0, s.1).or_else(|| {
            // 左上角正好落在屏与屏之间的缝隙里，退回到有交集的那块
            self.outs
                .iter()
                .position(|o| intersect(s, o.rect()).is_some())
        })
    }

    /// 指针位置落在哪块屏的哪个窗口上（全局坐标）
    pub fn window_at(&self, x: f64, y: f64) -> Option<Rect> {
        self.outs
            .iter()
            .flat_map(|o| o.windows.iter())
            .find(|r| r.contains(x, y))
            .copied()
    }

    /// 一笔文字的包围框（全局逻辑坐标）
    pub fn text_box(&self, i: usize) -> Option<Rect4> {
        let s = self.strokes.get(i)?;
        let (w, h) = s.text_size?;
        let (x, y) = *s.points.first()?;
        Some((x, y, x + w, y + h))
    }

    /// 点到哪一笔文字上，返回笔画下标
    pub fn text_at(&self, x: f64, y: f64) -> Option<usize> {
        self.sel?;
        (0..self.strokes.len()).rev().find(|i| {
            self.text_box(*i)
                .map(|(x0, y0, x1, y1)| x >= x0 && x <= x1 && y >= y0 && y <= y1)
                .unwrap_or(false)
        })
    }

    /// 提交一笔标注
    pub fn commit(&mut self, stroke: Stroke) {
        self.strokes.push(stroke);
        self.redo.clear();
    }

    pub fn undo(&mut self) -> bool {
        self.text_sel = None;
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
        self.text_sel = None;
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
        self.text_sel = None;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一块逻辑尺寸 lw*lh、缩放 1 的显示器画面
    fn doc_with_sel(lw: i32, lh: i32) -> Doc {
        let phys = cairo::ImageSurface::create(cairo::Format::ARgb32, lw, lh).unwrap();
        phys.mark_dirty();
        let mut doc = Doc::new(
            vec![OutputLayer::new(
                (0.0, 0.0),
                (f64::from(lw), f64::from(lh)),
                1.0,
                1.0,
                phys,
                Vec::new(),
            )],
            "/tmp".into(),
        );
        doc.sel = Some(Selection {
            x0: 0.0,
            y0: 0.0,
            x1: f64::from(lw),
            y1: f64::from(lh),
        });
        doc
    }

    fn push_text(doc: &mut Doc, x: f64, y: f64, size: (f64, f64), text: &str) {
        doc.commit(Stroke {
            tool: Tool::Text,
            color: doc.color,
            width: doc.width,
            points: vec![(x, y)],
            text: Some(text.into()),
            number: None,
            text_size: Some(size),
        });
    }

    #[test]
    fn text_box_follows_anchor_and_size() {
        let mut doc = doc_with_sel(400, 300);
        push_text(&mut doc, 50.0, 40.0, (100.0, 20.0), "hello");
        let (x0, y0, x1, y1) = doc.text_box(0).unwrap();
        assert_eq!((x0, x1), (50.0, 150.0));
        // 文字框以落点为左上角
        assert_eq!((y0, y1), (40.0, 60.0));
    }

    #[test]
    fn text_at_picks_topmost_text() {
        let mut doc = doc_with_sel(400, 300);
        push_text(&mut doc, 50.0, 40.0, (100.0, 20.0), "lower");
        push_text(&mut doc, 60.0, 45.0, (100.0, 20.0), "upper");
        // 两块重叠的地方取后画的那一笔
        assert_eq!(doc.text_at(100.0, 60.0), Some(1));
        // 只有下面那块覆盖到的地方
        assert_eq!(doc.text_at(55.0, 44.0), Some(0));
        // 都不在
        assert_eq!(doc.text_at(300.0, 200.0), None);
    }

    #[test]
    fn text_at_ignores_untouched_text() {
        let mut doc = doc_with_sel(400, 300);
        push_text(&mut doc, 50.0, 40.0, (100.0, 20.0), "hello");
        assert_eq!(doc.text_at(60.0, 45.0), Some(0));
        // 没有选区时也不算命中
        doc.sel = None;
        assert_eq!(doc.text_at(60.0, 45.0), None);
    }

    #[test]
    fn text_at_works_across_outputs() {
        // 两块屏并排：第二块从 x=400 开始，文字落在第二块上
        let mut doc = doc_with_sel(400, 300);
        let phys = cairo::ImageSurface::create(cairo::Format::ARgb32, 400, 300).unwrap();
        doc.outs.push(OutputLayer::new(
            (400.0, 0.0),
            (400.0, 300.0),
            1.0,
            1.0,
            phys,
            Vec::new(),
        ));
        doc.sel = Some(Selection {
            x0: 100.0,
            y0: 40.0,
            x1: 500.0,
            y1: 80.0,
        });
        push_text(&mut doc, 450.0, 40.0, (100.0, 20.0), "跨屏");
        // 全局坐标直接命中第二块屏上的文字
        assert_eq!(doc.text_at(460.0, 45.0), Some(0));
        assert_eq!(doc.out_at(460.0, 45.0), Some(1));
        assert_eq!(doc.out_at(60.0, 45.0), Some(0));
        // 选区左上角在第一块屏，工具条就挂在第一块上
        assert_eq!(doc.sel_output(), Some(0));
    }

    #[test]
    fn clear_all_drops_text_frame() {
        let mut doc = doc_with_sel(400, 300);
        push_text(&mut doc, 50.0, 40.0, (100.0, 20.0), "hello");
        doc.text_sel = Some(0);
        doc.clear_all();
        assert_eq!(doc.text_sel, None);
        assert!(doc.text_box(0).is_none());
    }
}
