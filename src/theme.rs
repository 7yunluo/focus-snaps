// 系统主题色：跟着桌面的强调色走
use gtk::gio;
use gtk::gio::prelude::*;

/// 取不到系统强调色时的默认暖橙
pub const FALLBACK: (f64, f64, f64) = (1.0, 0.72, 0.55);

const INTERFACE: &str = "org.gnome.desktop.interface";
const ACCENT: &str = "accent-color";

/// 系统强调色
pub fn accent() -> (f64, f64, f64) {
    name_rgb(&accent_name().unwrap_or_default()).unwrap_or(FALLBACK)
}

/// 强调色变化时回调，返回的设置对象要留着（丢掉就收不到通知了）
pub fn watch<F: Fn((f64, f64, f64)) + 'static>(cb: F) -> Option<gio::Settings> {
    let s = settings()?;
    s.connect_changed(Some(ACCENT), move |_, _| cb(accent()));
    Some(s)
}

fn accent_name() -> Option<String> {
    Some(settings()?.string(ACCENT).to_string())
}

fn settings() -> Option<gio::Settings> {
    let source = gio::SettingsSchemaSource::default()?;
    let schema = source.lookup(INTERFACE, true)?;
    if !schema.has_key(ACCENT) {
        return None;
    }
    Some(gio::Settings::new_full(
        &schema,
        gio::SettingsBackend::NONE,
        None::<&str>,
    ))
}

/// 强调色名字转 RGB（GNOME 那套九种）
fn name_rgb(name: &str) -> Option<(f64, f64, f64)> {
    let hex = match name {
        "blue" => 0x3584e4,
        "teal" => 0x2190a4,
        "green" => 0x3a944a,
        "yellow" => 0xc64600,
        "orange" => 0xed5b00,
        "red" => 0xe62d42,
        "pink" => 0xd56199,
        "purple" => 0x9141ac,
        "slate" => 0x6f8396,
        _ => return None,
    };
    Some(hex_rgb(hex))
}

/// 0xRRGGBB → 0..1 的三元组
pub fn hex_rgb(hex: u32) -> (f64, f64, f64) {
    (
        f64::from((hex >> 16) & 0xff) / 255.0,
        f64::from((hex >> 8) & 0xff) / 255.0,
        f64::from(hex & 0xff) / 255.0,
    )
}

/// 0..1 三元组 → #rrggbb
pub fn rgb_hex(c: (f64, f64, f64)) -> String {
    format!(
        "#{:02x}{:02x}{:02x}",
        (c.0 * 255.0).round().clamp(0.0, 255.0) as u8,
        (c.1 * 255.0).round().clamp(0.0, 255.0) as u8,
        (c.2 * 255.0).round().clamp(0.0, 255.0) as u8
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_accent_names() {
        // GNOME 的蓝色就是 #3584e4
        let c = name_rgb("blue").unwrap();
        assert!((c.2 - 0xe4 as f64 / 255.0).abs() < 1e-6);
        assert!((c.0 - 0x35 as f64 / 255.0).abs() < 1e-6);
        assert!(name_rgb("purple").is_some());
        assert!(name_rgb("没有这个").is_none());
        assert!(name_rgb("").is_none());
    }

    #[test]
    fn hex_roundtrip() {
        assert_eq!(rgb_hex(hex_rgb(0x3584e4)), "#3584e4");
        assert_eq!(rgb_hex(FALLBACK), "#ffb88c");
    }
}
