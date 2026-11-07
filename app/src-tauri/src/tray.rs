//! Tray glyphs: the step strip on its own (no frame) — two finished steps,
//! the current one and a planned one, as in the app. Sources live in `icons/source/`; rebuild the PNGs
//! with `app/scripts/build-icons.sh`.
//!
//! On macOS the glyph also moves. While a project is working, the current step
//! slowly fills itself in; when something needs you, it turns amber and pulses.
//! The frames are drawn here (44×36 px, 22×18 pt @2x) so the animation needs no
//! extra assets. With Motion off the glyph holds still: the amber step stays,
//! steady.

use std::sync::Mutex;
use std::time::Duration;

use tauri::image::Image;
use tauri::{AppHandle, Runtime};

use crate::preferences::{Lang, active_lang, fill};

pub const TRAY_ID: &str = "agendar-tray";

// macOS menu bar icons are template images (black + alpha), so they follow the
// light/dark menu bar. Other platforms get small colour tiles instead.
#[cfg(target_os = "macos")]
const IDLE: &[u8] = include_bytes!("../icons/tray-idle.png");
#[cfg(target_os = "macos")]
const NEEDS: &[u8] = include_bytes!("../icons/tray-needs.png");
#[cfg(not(target_os = "macos"))]
const IDLE: &[u8] = include_bytes!("../icons/tray-idle-color.png");
#[cfg(not(target_os = "macos"))]
const NEEDS: &[u8] = include_bytes!("../icons/tray-needs-color.png");

/// Whether the glyphs are template images on this platform.
pub const TEMPLATE: bool = cfg!(target_os = "macos");

/// The glyph for the current attention state.
pub fn icon(needs: usize) -> Option<Image<'static>> {
    Image::from_bytes(if needs > 0 { NEEDS } else { IDLE }).ok()
}

/// Hover text for the tray icon.
pub fn tooltip(needs: usize, working: usize) -> String {
    tooltip_in(needs, working, active_lang())
}

/// Hover text in one language.
pub fn tooltip_in(needs: usize, working: usize, lang: Lang) -> String {
    let count = needs.to_string();
    let needs = match needs {
        0 => lang
            .pick("nothing needs you", "没有需要你的事", "沒有需要你的事")
            .to_owned(),
        1 => lang
            .pick("1 item needs you", "1 项需要你", "1 項需要你")
            .to_owned(),
        _ => fill(
            lang.pick("{n} items need you", "{n} 项需要你", "{n} 項需要你"),
            &[("n", &count)],
        ),
    };
    let working_count = working.to_string();
    let template = match working {
        0 => "Agendar — {needs}",
        1 => lang.pick(
            "Agendar — 1 project working, {needs}",
            "Agendar — 1 个项目进行中，{needs}",
            "Agendar — 1 個專案進行中，{needs}",
        ),
        _ => lang.pick(
            "Agendar — {n} projects working, {needs}",
            "Agendar — {n} 个项目进行中，{needs}",
            "Agendar — {n} 個專案進行中，{needs}",
        ),
    };
    fill(template, &[("n", &working_count), ("needs", &needs)])
}

/// The number beside the menu bar icon: projects with an agent at work.
pub fn title(working: usize) -> Option<String> {
    (working > 0).then(|| working.to_string())
}

/// The latest attention counts, read by the animator.
static COUNTS: Mutex<(usize, usize)> = Mutex::new((0, 0));

/// Switch the tray title and tooltip, and the glyph where it does not animate.
/// Call on the main thread.
pub fn apply<R: Runtime>(app: &AppHandle<R>, needs: usize, working: usize) {
    *COUNTS.lock().unwrap_or_else(|poison| poison.into_inner()) = (needs, working);
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    // macOS frames come from the animator; elsewhere the static tile switches.
    if !cfg!(target_os = "macos")
        && let Some(image) = icon(needs)
    {
        let _ = tray.set_icon(Some(image));
        let _ = tray.set_icon_as_template(TEMPLATE);
    }
    let _ = tray.set_title(title(working));
    let _ = tray.set_tooltip(Some(tooltip(needs, working)));
}

// ---------------------------------------------------------------------
// Animated glyph (drawn on every platform, shown on macOS)
// ---------------------------------------------------------------------

/// Frames of the working sweep and how long each one shows.
pub const WORK_FRAMES: u8 = 12;
pub const WORK_FRAME_MS: u64 = 600;
/// Frames of the needs-you pulse and how long each one shows.
pub const PULSE_FRAMES: u8 = 8;
pub const PULSE_FRAME_MS: u64 = 300;

/// One frame of the menu bar glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    Idle,
    /// The current step filling itself in.
    Working(u8),
    /// The amber current step at one pulse phase.
    Needs(u8),
}

/// The frame to show `elapsed_ms` into the animation.
pub fn glyph(needs: usize, working: usize, motion: bool, elapsed_ms: u64) -> Glyph {
    if needs > 0 {
        if !motion {
            return Glyph::Needs(0);
        }
        let frame = (elapsed_ms / PULSE_FRAME_MS) % u64::from(PULSE_FRAMES);
        return Glyph::Needs(u8::try_from(frame).unwrap_or(0));
    }
    if working > 0 && motion {
        let frame = (elapsed_ms / WORK_FRAME_MS) % u64::from(WORK_FRAMES);
        return Glyph::Working(u8::try_from(frame).unwrap_or(0));
    }
    Glyph::Idle
}

/// How often the animator should look again for this frame.
pub fn next_check(glyph: Glyph) -> Duration {
    match glyph {
        Glyph::Idle => Duration::from_millis(500),
        Glyph::Working(_) => Duration::from_millis(WORK_FRAME_MS / 2),
        Glyph::Needs(_) => Duration::from_millis(PULSE_FRAME_MS / 2),
    }
}

pub const WIDTH: u32 = 44;
pub const HEIGHT: u32 = 36;
// Four 8 px steps, 3 px apart, centred; no frame around them.
const CELL: f32 = 8.0;
const PITCH: f32 = 11.0;
const FIRST_LEFT: f32 = 1.5;
const TOP: f32 = 14.0;
const CORNER: f32 = 2.0;
const OUTLINE: f32 = 1.5;
/// The current step (the third square).
const CURRENT: usize = 2;
const AMBER: [u8; 3] = [0xE8, 0x8E, 0x14];

/// Draw a frame as straight RGBA, `WIDTH`×`HEIGHT`. Returns whether it is a
/// template image (black + alpha). The amber needs frames are not: their
/// squares take the menu bar's ink, `dark` for a dark menu bar.
pub fn render(glyph: Glyph, dark: bool) -> (Vec<u8>, bool) {
    // How far the current step is filled, 0..=1 of its width.
    let fill = match glyph {
        Glyph::Idle => 0.0,
        Glyph::Needs(_) => 1.0,
        // Fill over the first two thirds of the cycle, then hold full.
        Glyph::Working(frame) => {
            let growing = f32::from(WORK_FRAMES) * 2.0 / 3.0;
            (f32::from(frame) / (growing - 1.0)).min(1.0)
        }
    };
    let amber_alpha = match glyph {
        Glyph::Needs(frame) => {
            // A slow cosine between full and 35%.
            let phase = f32::from(frame) / f32::from(PULSE_FRAMES) * std::f32::consts::TAU;
            0.675 + 0.325 * phase.cos()
        }
        _ => 0.0,
    };
    let template = !matches!(glyph, Glyph::Needs(_));
    let ink: [u8; 3] = if template || !dark {
        [0, 0, 0]
    } else {
        [255, 255, 255]
    };
    let ink_alpha = if template { 1.0 } else { 0.85 };
    let mut pixels = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let mut ink_cover: f32 = 0.0;
            let mut amber_cover: f32 = 0.0;
            for cell in 0..4 {
                let left = FIRST_LEFT + PITCH * cell as f32;
                let half = CELL / 2.0;
                let distance = rounded_box(px, py, (left + half, TOP + half), (half, half), CORNER);
                let solid = (0.5 - distance).clamp(0.0, 1.0);
                let ring = ring(distance, OUTLINE);
                match cell {
                    0 | 1 => ink_cover = ink_cover.max(solid),
                    CURRENT if matches!(glyph, Glyph::Needs(_)) => amber_cover = solid,
                    CURRENT => {
                        let edge = left + CELL * fill;
                        let filled = solid * (edge - px + 0.5).clamp(0.0, 1.0);
                        ink_cover = ink_cover.max(ring).max(filled);
                    }
                    _ => ink_cover = ink_cover.max(ring),
                }
            }
            // The squares never overlap, so a pixel is either amber or ink.
            let (color, alpha) = if amber_cover > 0.0 {
                (AMBER, amber_cover * amber_alpha)
            } else {
                (ink, ink_cover * ink_alpha)
            };
            pixels.extend_from_slice(&color);
            pixels.push((alpha.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
    }
    (pixels, template)
}

/// Signed distance from a pixel centre to a rounded box (negative inside).
fn rounded_box(px: f32, py: f32, (cx, cy): (f32, f32), (hw, hh): (f32, f32), corner: f32) -> f32 {
    let qx = (px - cx).abs() - (hw - corner);
    let qy = (py - cy).abs() - (hh - corner);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - corner
}

/// Coverage of a `width` line drawn just inside a shape's edge.
fn ring(distance: f32, width: f32) -> f32 {
    (0.5 - (distance + width / 2.0).abs() + width / 2.0).clamp(0.0, 1.0)
}

/// Drive the macOS menu bar glyph: pick the frame from the latest counts and
/// the Motion preference, and swap the image only when the frame changes.
#[cfg(target_os = "macos")]
pub fn spawn_animator(app: AppHandle, motion: impl Fn() -> bool + Send + 'static) {
    use tauri::Manager;
    std::thread::spawn(move || {
        let start = std::time::Instant::now();
        let mut shown: Option<(Glyph, bool)> = None;
        // The menu bar ink only matters for the amber frames; ask the system
        // appearance at most once a second.
        let mut dark = true;
        let mut theme_checked: Option<std::time::Instant> = None;
        loop {
            let (needs, working) = *COUNTS.lock().unwrap_or_else(|poison| poison.into_inner());
            let elapsed = u64::try_from(start.elapsed().as_millis()).unwrap_or(0);
            let frame = glyph(needs, working, motion(), elapsed);
            if matches!(frame, Glyph::Needs(_))
                && theme_checked.is_none_or(|at| at.elapsed() >= Duration::from_secs(1))
            {
                theme_checked = Some(std::time::Instant::now());
                dark = app
                    .get_webview_window("main")
                    .and_then(|window| window.theme().ok())
                    .is_none_or(|theme| theme == tauri::Theme::Dark);
            }
            if shown != Some((frame, dark)) {
                shown = Some((frame, dark));
                let handle = app.clone();
                let _ = app.run_on_main_thread(move || {
                    let Some(tray) = handle.tray_by_id(TRAY_ID) else {
                        return;
                    };
                    let (rgba, template) = render(frame, dark);
                    let _ = tray.set_icon(Some(Image::new_owned(rgba, WIDTH, HEIGHT)));
                    let _ = tray.set_icon_as_template(template);
                });
            }
            std::thread::sleep(next_check(frame));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_glyphs_decode() {
        assert!(Image::from_bytes(IDLE).is_ok());
        assert!(Image::from_bytes(NEEDS).is_ok());
    }

    #[test]
    fn tooltip_counts_items() {
        assert_eq!(tooltip_in(0, 0, Lang::En), "Agendar — nothing needs you");
        assert_eq!(tooltip_in(1, 0, Lang::En), "Agendar — 1 item needs you");
        assert_eq!(tooltip_in(3, 0, Lang::En), "Agendar — 3 items need you");
        assert_eq!(
            tooltip_in(0, 1, Lang::En),
            "Agendar — 1 project working, nothing needs you"
        );
        assert_eq!(
            tooltip_in(2, 3, Lang::En),
            "Agendar — 3 projects working, 2 items need you"
        );
    }

    #[test]
    fn title_shows_working_projects_only_when_any() {
        assert_eq!(title(0), None);
        assert_eq!(title(2), Some("2".to_owned()));
    }

    #[test]
    fn the_frame_follows_attention_work_and_motion() {
        assert_eq!(glyph(0, 0, true, 5_000), Glyph::Idle);
        assert_eq!(glyph(0, 2, true, 0), Glyph::Working(0));
        assert_eq!(glyph(0, 2, true, WORK_FRAME_MS * 3), Glyph::Working(3));
        assert_eq!(
            glyph(0, 2, true, WORK_FRAME_MS * u64::from(WORK_FRAMES)),
            Glyph::Working(0),
            "the sweep loops"
        );
        assert_eq!(
            glyph(0, 2, false, 9_999),
            Glyph::Idle,
            "Motion off holds still"
        );
        assert_eq!(
            glyph(1, 2, true, PULSE_FRAME_MS * 2),
            Glyph::Needs(2),
            "needs wins"
        );
        assert_eq!(
            glyph(1, 0, false, 9_999),
            Glyph::Needs(0),
            "a steady amber step"
        );
    }

    fn alpha_at(rgba: &[u8], x: u32, y: u32) -> u8 {
        rgba[((y * WIDTH + x) * 4 + 3) as usize]
    }

    fn rgb_at(rgba: &[u8], x: u32, y: u32) -> [u8; 3] {
        let i = ((y * WIDTH + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2]]
    }

    // Pixel centres: a finished square (5,18), inside the current square near
    // its left (26,18) and right (29,18) sides, its outline (24,18) and empty
    // middle (27,18), and the planned square's empty middle (38,18).

    #[test]
    fn idle_and_working_frames_are_template_ink() {
        let (idle, template) = render(Glyph::Idle, true);
        assert!(template);
        assert_eq!(idle.len(), (WIDTH * HEIGHT * 4) as usize);
        assert!(
            idle.chunks(4).all(|px| px[..3] == [0, 0, 0]),
            "black + alpha"
        );
        assert_eq!(alpha_at(&idle, 5, 18), 255, "finished steps are solid");
        assert_eq!(alpha_at(&idle, 24, 18), 255, "the current step is outlined");
        assert_eq!(alpha_at(&idle, 27, 18), 0, "and empty while nobody works");
        assert_eq!(alpha_at(&idle, 0, 4), 0, "no frame around the steps");
        assert_eq!(alpha_at(&idle, 38, 18), 0, "a planned step is hollow");

        let (start, _) = render(Glyph::Working(0), true);
        assert_eq!(alpha_at(&start, 29, 18), 0, "the fill starts empty");
        let (half, _) = render(Glyph::Working(WORK_FRAMES / 3), true);
        assert_eq!(alpha_at(&half, 26, 18), 255, "it fills from the left");
        assert_eq!(alpha_at(&half, 29, 18), 0);
        let (held, _) = render(Glyph::Working(WORK_FRAMES - 1), true);
        assert_eq!(alpha_at(&held, 29, 18), 255, "then holds full");
        assert_eq!(alpha_at(&held, 38, 18), 0, "the planned step never fills");
    }

    #[test]
    fn the_needs_step_is_amber_and_pulses_on_the_menu_bar_ink() {
        let (bright, template) = render(Glyph::Needs(0), true);
        assert!(!template);
        assert_eq!(rgb_at(&bright, 27, 18), AMBER);
        assert_eq!(alpha_at(&bright, 27, 18), 255);
        assert_eq!(
            rgb_at(&bright, 5, 18),
            [255, 255, 255],
            "white ink on a dark bar"
        );
        let (dim, _) = render(Glyph::Needs(PULSE_FRAMES / 2), true);
        assert!(
            alpha_at(&dim, 27, 18) < 100,
            "half a cycle later the step is dim"
        );
        let (light, _) = render(Glyph::Needs(0), false);
        assert_eq!(rgb_at(&light, 5, 18), [0, 0, 0], "black ink on a light bar");
    }
}
