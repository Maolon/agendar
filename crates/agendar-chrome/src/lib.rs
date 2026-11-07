//! Window chrome. On macOS 26 and later the app's floating sidebar and toolbar
//! capsules are real Liquid Glass: `NSGlassEffectView`s placed behind the
//! transparent webview, at rectangles the page reports. Everywhere else (older
//! macOS, Reduce Transparency, Windows, Linux) the page paints solid surfaces
//! from its own tokens and this crate does nothing.
//!
//! The API is safe; the one `unsafe` block lives in the macOS module (the
//! second of the two allowed `unsafe` exceptions).

#![deny(unsafe_op_in_unsafe_fn)]

use serde::{Deserialize, Serialize};

/// The most regions a page may ask for (sidebar plus a few toolbar capsules).
pub const MAX_REGIONS: usize = 8;

/// How the page should paint its chrome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Material {
    /// Native glass shows through transparent regions.
    Glass,
    /// The page paints every surface itself.
    Solid,
}

/// Which window edges a region follows while the window resizes, before the
/// page reports fresh rectangles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    /// Pinned left, top and bottom: grows with the window height (sidebar).
    Fill,
    TopLeft,
    TopCenter,
    TopRight,
}

/// One glass rectangle in page coordinates (CSS pixels from the top left of
/// the webview).
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Region {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub radius: f64,
    pub anchor: Anchor,
}

/// The theme colour the glass leans toward (sRGB, 0..=1), so text on it keeps
/// its contrast whatever is behind the window.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tint {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Tint {
    fn sane(&self) -> bool {
        [self.r, self.g, self.b, self.a]
            .iter()
            .all(|value| value.is_finite() && (0.0..=1.0).contains(value))
    }
}

impl Region {
    fn sane(&self) -> bool {
        [self.x, self.y, self.width, self.height, self.radius]
            .iter()
            .all(|value| value.is_finite() && *value >= 0.0 && *value <= 100_000.0)
            && self.width > 0.0
            && self.height > 0.0
    }
}

/// Keep at most `MAX_REGIONS` well-formed regions.
pub fn sanitize(regions: Vec<Region>) -> Vec<Region> {
    regions
        .into_iter()
        .filter(Region::sane)
        .take(MAX_REGIONS)
        .collect()
}

/// The material this window can use right now.
pub fn material() -> Material {
    #[cfg(target_os = "macos")]
    {
        macos::material()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Material::Solid
    }
}

/// Replace the window's glass regions. A no-op where the material is solid.
pub fn set_regions<R: tauri::Runtime>(
    window: &tauri::WebviewWindow<R>,
    regions: Vec<Region>,
    tint: Option<Tint>,
) -> tauri::Result<()> {
    let regions = sanitize(regions);
    let tint = tint.filter(Tint::sane);
    #[cfg(target_os = "macos")]
    {
        macos::set_regions(window, regions, tint)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (window, regions, tint);
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2::runtime::AnyClass;
    use objc2_app_kit::{
        NSAutoresizingMaskOptions, NSColor, NSGlassEffectView, NSView, NSWindowOrderingMode,
        NSWorkspace,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    use super::{Anchor, Material, Region, Tint};

    thread_local! {
        // Main-thread only: AppKit views are created and moved there. One list
        // per window label (the main window and the menu bar popover).
        static VIEWS: RefCell<HashMap<String, Vec<Retained<NSGlassEffectView>>>> =
            RefCell::new(HashMap::new());
    }

    pub fn material() -> Material {
        if AnyClass::get(c"NSGlassEffectView").is_none() {
            return Material::Solid;
        }
        if NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceTransparency() {
            return Material::Solid;
        }
        Material::Glass
    }

    pub fn set_regions<R: tauri::Runtime>(
        window: &tauri::WebviewWindow<R>,
        regions: Vec<Region>,
        tint: Option<Tint>,
    ) -> tauri::Result<()> {
        let regions = if material() == Material::Glass {
            regions
        } else {
            Vec::new()
        };
        let label = window.label().to_owned();
        window.with_webview(move |platform| {
            let Some(mtm) = MainThreadMarker::new() else {
                return;
            };
            let pointer = platform.inner().cast::<NSView>();
            if pointer.is_null() {
                return;
            }
            // SAFETY: Tauri runs this closure on the main thread and hands us
            // its live WKWebView (an NSView subclass) for the closure's
            // duration; we only borrow it, and `superview` is read on the main
            // thread while nothing else mutates the hierarchy.
            let (webview, parent) = unsafe {
                let webview: &NSView = &*pointer;
                (webview, webview.superview())
            };
            let Some(parent) = parent else {
                return;
            };
            apply(mtm, &label, &parent, webview, &regions, tint);
        })
    }

    fn apply(
        mtm: MainThreadMarker,
        label: &str,
        parent: &NSView,
        webview: &NSView,
        regions: &[Region],
        tint: Option<Tint>,
    ) {
        let tint = tint
            .map(|tint| NSColor::colorWithSRGBRed_green_blue_alpha(tint.r, tint.g, tint.b, tint.a));
        VIEWS.with(|windows| {
            let mut windows = windows.borrow_mut();
            let views = windows.entry(label.to_owned()).or_default();
            while views.len() > regions.len() {
                if let Some(view) = views.pop() {
                    view.removeFromSuperview();
                }
            }
            while views.len() < regions.len() {
                let view = NSGlassEffectView::new(mtm);
                parent.addSubview_positioned_relativeTo(
                    &view,
                    NSWindowOrderingMode::Below,
                    Some(webview),
                );
                views.push(view);
            }
            let origin = webview.frame().origin;
            let height = parent.bounds().size.height;
            let flipped = parent.isFlipped();
            for (view, region) in views.iter().zip(regions) {
                let x = origin.x + region.x;
                let y = if flipped {
                    origin.y + region.y
                } else {
                    height - origin.y - region.y - region.height
                };
                view.setFrame(NSRect::new(
                    NSPoint::new(x, y),
                    NSSize::new(region.width, region.height),
                ));
                view.setCornerRadius(region.radius);
                view.setTintColor(tint.as_deref());
                view.setAutoresizingMask(mask(region.anchor, flipped));
            }
        });
    }

    /// Keep a region on its edges while the window resizes.
    fn mask(anchor: Anchor, flipped: bool) -> NSAutoresizingMaskOptions {
        // "Pinned to the top" is a flexible bottom margin, which is MaxY when
        // the parent is flipped and MinY when it is not.
        let top = if flipped {
            NSAutoresizingMaskOptions::ViewMaxYMargin
        } else {
            NSAutoresizingMaskOptions::ViewMinYMargin
        };
        match anchor {
            Anchor::Fill => {
                NSAutoresizingMaskOptions::ViewHeightSizable
                    | NSAutoresizingMaskOptions::ViewMaxXMargin
            }
            Anchor::TopLeft => top | NSAutoresizingMaskOptions::ViewMaxXMargin,
            Anchor::TopCenter => {
                top | NSAutoresizingMaskOptions::ViewMinXMargin
                    | NSAutoresizingMaskOptions::ViewMaxXMargin
            }
            Anchor::TopRight => top | NSAutoresizingMaskOptions::ViewMinXMargin,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(width: f64) -> Region {
        Region {
            x: 8.0,
            y: 8.0,
            width,
            height: 100.0,
            radius: 14.0,
            anchor: Anchor::Fill,
        }
    }

    #[test]
    fn malformed_regions_are_dropped_and_the_count_is_capped() {
        let mut regions = vec![region(0.0), region(f64::NAN), region(-1.0)];
        regions.extend(std::iter::repeat_n(region(240.0), 12));
        let kept = sanitize(regions);
        assert_eq!(kept.len(), MAX_REGIONS);
        assert!(kept.iter().all(|region| region.width == 240.0));
    }

    #[test]
    fn regions_decode_from_the_page_shape() {
        let region: Region = serde_json::from_str(
            r#"{"x":8,"y":8,"width":240,"height":600,"radius":14,"anchor":"fill"}"#,
        )
        .expect("decodes");
        assert_eq!(region.anchor, Anchor::Fill);
        assert!(serde_json::from_str::<Region>(r#"{"x":8}"#).is_err());
    }
}
