// 工具条上的工具图标，全部用 cairo 画，不依赖图标主题
use std::f64::consts::TAU;

use gtk::cairo;

use crate::doc::Tool;

/// 图标前景色
const FG: (f64, f64, f64) = (0.93, 0.89, 0.86);
/// 图标按这个边长的坐标系绘制
const UNIT: f64 = 16.0;

/// 在给定的显示尺寸里画一个工具图标
pub fn draw_tool(cr: &cairo::Context, tool: Tool, area: (f64, f64)) {
    let s = area.0.min(area.1);
    let _ = cr.save();
    cr.translate((area.0 - s) / 2.0, (area.1 - s) / 2.0);
    cr.scale(s / UNIT, s / UNIT);
    cr.set_line_width(1.6);
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    fg(cr);
    match tool {
        Tool::Select => {
            cr.move_to(4.6, 2.4);
            cr.line_to(4.6, 13.0);
            cr.line_to(7.4, 9.9);
            cr.line_to(9.2, 13.8);
            cr.line_to(11.1, 12.9);
            cr.line_to(9.3, 9.2);
            cr.line_to(12.8, 8.8);
            cr.close_path();
            let _ = cr.fill();
        }
        Tool::Brush => {
            cr.set_line_width(2.4);
            cr.move_to(12.6, 3.4);
            cr.line_to(7.0, 9.0);
            let _ = cr.stroke();
            cr.move_to(7.4, 9.4);
            cr.line_to(2.8, 13.4);
            cr.line_to(6.6, 11.0);
            cr.close_path();
            let _ = cr.fill();
        }
        Tool::Highlighter => {
            let _ = cr.save();
            cr.set_line_width(5.0);
            cr.set_source_rgba(FG.0, FG.1, FG.2, 0.32);
            cr.move_to(4.6, 11.4);
            cr.line_to(10.6, 5.4);
            let _ = cr.stroke();
            let _ = cr.restore();
            fg(cr);
            cr.set_line_width(1.8);
            cr.move_to(12.8, 3.2);
            cr.line_to(10.6, 5.4);
            let _ = cr.stroke();
        }
        Tool::Line => {
            cr.move_to(3.6, 12.4);
            cr.line_to(12.4, 3.6);
            let _ = cr.stroke();
        }
        Tool::Arrow => {
            cr.move_to(3.6, 12.4);
            cr.line_to(12.4, 3.6);
            let _ = cr.stroke();
            fg(cr);
            cr.move_to(13.0, 3.0);
            cr.line_to(7.4, 4.6);
            cr.line_to(11.4, 8.6);
            cr.close_path();
            let _ = cr.fill();
        }
        Tool::Rect => {
            cr.rectangle(2.8, 4.6, 10.4, 6.8);
            let _ = cr.stroke();
        }
        Tool::Ellipse => {
            let _ = cr.save();
            cr.translate(8.0, 8.0);
            cr.scale(5.2, 3.5);
            cr.arc(0.0, 0.0, 1.0, 0.0, TAU);
            let _ = cr.restore();
            let _ = cr.stroke();
        }
        Tool::Text => {
            cr.set_line_width(1.8);
            cr.move_to(3.4, 4.0);
            cr.line_to(12.6, 4.0);
            cr.move_to(8.0, 4.0);
            cr.line_to(8.0, 12.8);
            let _ = cr.stroke();
        }
        Tool::Number => {
            cr.set_line_width(1.5);
            cr.arc(8.0, 8.0, 5.6, 0.0, TAU);
            let _ = cr.stroke();
            cr.set_line_width(1.7);
            cr.move_to(6.6, 6.6);
            cr.line_to(8.2, 5.4);
            cr.line_to(8.2, 11.2);
            let _ = cr.stroke();
        }
        Tool::Mosaic => {
            for (x, y) in [(2.4, 2.4), (9.2, 2.4), (5.8, 5.8), (2.4, 9.2), (9.2, 9.2)] {
                cr.rectangle(x, y, 4.0, 4.0);
            }
            let _ = cr.fill();
        }
        Tool::Crop => {
            cr.set_line_width(1.5);
            cr.move_to(5.8, 2.4);
            cr.line_to(5.8, 10.2);
            cr.line_to(13.6, 10.2);
            let _ = cr.stroke();
            cr.move_to(2.4, 5.8);
            cr.line_to(10.2, 5.8);
            cr.line_to(10.2, 13.6);
            let _ = cr.stroke();
        }
    }
    let _ = cr.restore();
}

fn fg(cr: &cairo::Context) {
    cr.set_source_rgba(FG.0, FG.1, FG.2, 1.0);
}

