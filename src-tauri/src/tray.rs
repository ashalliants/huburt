//! The menu bar icon: a glyph with a red dot when something is unread, a hover summary, and the right-click menu.
use crate::{hub::Hub, menus, webviews};
use std::sync::Arc;
use tauri::{
    image::Image,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

const SIZE: usize = 44; // 22pt @2x

/// Anti-aliased coverage (0..1) of a shape given its signed distance, via 4x4 supersampling.
fn render(badge: bool, dark: bool) -> Vec<u8> {
    let nodes = [(7.0f32, 13.0f32), (33.0, 13.0), (20.0, 36.0)];   // an inverted triangle, like the Swift app's symbol
    // distance to a segment, plus how far along it the nearest point is (0..1)
    let seg = |p: (f32, f32), a: (f32, f32), b: (f32, f32)| {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        (((p.0 - a.0 - t * dx).powi(2) + (p.1 - a.1 - t * dy).powi(2)).sqrt(), t)
    };
    let badge_c = (38.0f32, 7.0f32);
    let mut px = vec![0u8; SIZE * SIZE * 4];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (mut glyph, mut ring, mut dot) = (0.0f32, 0.0f32, 0.0f32);
            for sy in 0..4 {
                for sx in 0..4 {
                    let p = (x as f32 + (sx as f32 + 0.5) / 4.0, y as f32 + (sy as f32 + 0.5) / 4.0);
                    // three hollow rings joined by dotted lines
                    let in_rings = nodes.iter().any(|n| { let r = ((p.0 - n.0).powi(2) + (p.1 - n.1).powi(2)).sqrt(); (3.6..=5.6).contains(&r) });
                    let in_lines = [(0, 1), (1, 2), (2, 0)].iter().any(|(i, j)| {
                        let (d, t) = seg(p, nodes[*i], nodes[*j]);
                        // dotted, and never drawn through the inside of a ring
                        d <= 1.0 && (t * 7.0).fract() < 0.5 && nodes.iter().all(|n| ((p.0 - n.0).powi(2) + (p.1 - n.1).powi(2)).sqrt() > 6.0)
                    });
                    let (in_nodes, in_lines) = (in_rings, in_lines);
                    let d = ((p.0 - badge_c.0).powi(2) + (p.1 - badge_c.1).powi(2)).sqrt();
                    if in_nodes || in_lines { glyph += 1.0; }
                    if badge && d <= 8.6 { ring += 1.0; }
                    if badge && d <= 7.0 { dot += 1.0; }
                }
            }
            let (g, r, d) = (glyph / 16.0, ring / 16.0, dot / 16.0);
            let i = (y * SIZE + x) * 4;
            // the glyph follows the menu bar (white on dark, black on light); the badge dot is red, set off by a thin gap
            let ga = g * (1.0 - r);
            let a = ga + d * (1.0 - ga);
            if a > 0.0 {
                let wr = d * (1.0 - ga) / a;
                let ink = if dark { 255.0 } else { 0.0 };
                px[i] = (255.0 * wr + ink * (1.0 - wr)) as u8;
                px[i + 1] = (59.0 * wr + ink * (1.0 - wr)) as u8;
                px[i + 2] = (48.0 * wr + ink * (1.0 - wr)) as u8;
                px[i + 3] = (a * 255.0) as u8;
            }
        }
    }
    px
}

pub fn init(hub: &Arc<Hub>) -> tauri::Result<()> {
    let h = hub.clone();
    let tray = TrayIconBuilder::with_id("hub")
        .icon(Image::new_owned(render(false, is_dark(hub)), SIZE as u32, SIZE as u32))
        .icon_as_template(false)
        .tooltip("Huburt")
        .show_menu_on_left_click(false)
        .on_tray_icon_event(move |_t, e| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                webviews::toggle_window(&h);
            }
        })
        .build(&hub.app)?;
    let _ = tray.set_menu(Some(menus::tray_menu(hub)?));
    Ok(())
}

/// Red dot when anything needs you, and a hover summary of every service.
pub fn update(hub: &Arc<Hub>, total: u32, summary: &str) {
    let Some(tray) = hub.app.tray_by_id("hub") else { return };
    let _ = tray.set_icon(Some(Image::new_owned(render(total > 0, is_dark(hub)), SIZE as u32, SIZE as u32)));
    let _ = tray.set_tooltip(Some(fit_tooltip(&format!("Huburt\n{summary}"))));
    if let Ok(m) = menus::tray_menu(hub) {
        let _ = tray.set_menu(Some(m));
    }
    crate::log(&format!("summary: {}", summary.replace('\n', " | ")));
}

/// Whether the system is in dark mode (decides the glyph colour).
pub fn is_dark(hub: &Arc<Hub>) -> bool {
    use tauri::Manager;
    hub.app.get_window("main").and_then(|w| w.theme().ok()).map(|t| t == tauri::Theme::Dark).unwrap_or(false)
}

/// Windows limits a notification-area tooltip to 127 characters; macOS has no such limit.
fn fit_tooltip(text: &str) -> String {
    const WINDOWS_MAX: usize = 120;
    if cfg!(windows) { shorten(text, WINDOWS_MAX) } else { text.to_string() }
}

/// Cuts `text` to at most `max` characters, ending in an ellipsis, preferably at a line break.
fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max - 1).collect();
    let cut = cut.rsplit_once('\n').map(|(whole_lines, _)| whole_lines.to_string()).filter(|s| s.chars().count() > 20).unwrap_or(cut);
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_tooltips_are_left_alone() {
        assert_eq!(shorten("Huburt\nSlack: all caught up", 120), "Huburt\nSlack: all caught up");
    }

    #[test]
    fn long_tooltips_are_cut_at_a_line_break_within_the_limit() {
        let text = "Huburt\nSlack: 12 DMs/mentions, 3 unread channels\nGmail: 150 unread emails\nWhatsApp: 20 unread chats\nNotifications paused until 18:00";
        let out = shorten(text, 100);
        assert!(out.chars().count() <= 100, "{out}");
        assert!(out.ends_with('…') && out.starts_with("Huburt\nSlack: 12"), "{out}");
        assert!(!out.contains("paused"));
    }
}
