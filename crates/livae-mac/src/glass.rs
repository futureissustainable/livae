//! Livae's glass: Apple's Liquid Glass (blur and refraction) plus a black gradient inside the shape. On an OLED black background it stays pure black (no grey lift); over a bright
//! background it darkens from the top so text stays readable. `darkness` (0 clear … 1 dark) is the Settings slider.

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::MainThreadOnly;
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSColor, NSGlassEffectView, NSGlassEffectViewStyle, NSView, NSVisualEffectBlendingMode,
    NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView,
};
use objc2_foundation::{MainThreadMarker, NSArray, NSPoint, NSRect, NSSize};
use objc2_quartz_core::CAGradientLayer;

/// How the glass is tinted from top to bottom.
#[derive(Clone, Copy, PartialEq)]
pub enum Tint {
    /// Frosted glass, black at the top fading to a dark veil: windows full of text.
    Frosted,
    /// The macOS 27 Spotlight capsule: clear Liquid Glass, dark behind the text, fully clear at the bottom so the
    /// refraction and the lit edge show.
    Capsule,
    /// Clear Liquid Glass under a steadier dark tint, so result rows stay readable over a busy desktop.
    Card,
}

pub struct Glass {
    /// Add this to the window.
    pub root: Retained<NSView>,
    /// Put the shape's controls in here.
    pub content: Retained<NSView>,
    shade: Retained<CAGradientLayer>,
    tint: Tint,
    _glass: Option<Retained<NSGlassEffectView>>,
}

impl Glass {
    pub fn new(mtm: MainThreadMarker, radius: f64, darkness: f64) -> Self {
        Self::tinted(mtm, radius, darkness, Tint::Frosted)
    }

    pub fn tinted(mtm: MainThreadMarker, radius: f64, darkness: f64, tint: Tint) -> Self {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, 100.0));
        let fill = NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable;
        let inner = NSView::initWithFrame(NSView::alloc(mtm), frame);
        inner.setAutoresizingMask(fill);
        // The black gradient sits under the controls.
        let shade_view = NSView::initWithFrame(NSView::alloc(mtm), frame);
        shade_view.setAutoresizingMask(fill);
        shade_view.setWantsLayer(true);
        let shade = CAGradientLayer::new();
        shade.setStartPoint(objc2_core_foundation::CGPoint::new(0.5, 1.0));
        shade.setEndPoint(objc2_core_foundation::CGPoint::new(0.5, 0.0));
        shade.setCornerRadius(radius);
        shade.setMasksToBounds(true);
        shade_view.setLayer(Some(&shade));
        inner.addSubview(&shade_view);
        let content = NSView::initWithFrame(NSView::alloc(mtm), frame);
        content.setAutoresizingMask(fill);
        inner.addSubview(&content);

        let (root, glass) = if AnyClass::get(c"NSGlassEffectView").is_some() {
            // Start at the same frame as the views inside: from a zero frame, autoresizing leaves them offset
            // after a resize (a one-row card had its row 12 pt low and clipped).
            let g = NSGlassEffectView::initWithFrame(NSGlassEffectView::alloc(mtm), frame);
            // Regular blurs what is behind enough to read over busy screens; Clear keeps it sharp and refracted,
            // the way the Spotlight capsule looks. The gradient removes the grey and carries the text.
            g.setStyle(if tint == Tint::Frosted { NSGlassEffectViewStyle::Regular } else { NSGlassEffectViewStyle::Clear });
            g.setCornerRadius(radius);
            g.setContentView(Some(&inner));
            (Retained::into_super(g.clone()), Some(g))
        } else {
            let v = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), frame);
            v.setMaterial(NSVisualEffectMaterial::HUDWindow);
            v.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
            v.setState(NSVisualEffectState::Active);
            v.addSubview(&inner);
            (Retained::into_super(v), None)
        };
        let g = Self { root, content, shade, tint, _glass: glass };
        g.set_darkness(darkness);
        g
    }

    /// 0 = clear glass, 1 = the full tint (see `Tint`), top to bottom.
    pub fn set_darkness(&self, darkness: f64) {
        let d = darkness.clamp(0.0, 1.0);
        let stops: &[(f64, f64)] = match self.tint {
            Tint::Frosted => &[(0.0, 0.92), (1.0, 0.45)],
            Tint::Capsule => &[(0.0, 0.88), (0.55, 0.62), (1.0, 0.0)],
            Tint::Card => &[(0.0, 0.90), (1.0, 0.66)],
        };
        let colors: Vec<_> = stops.iter().map(|&(_, a)| NSColor::colorWithWhite_alpha(0.0, a * d).CGColor()).collect();
        // CGColor is toll-free bridged to an object, which is what the layer's colour array holds.
        let as_object = |c: &objc2_core_graphics::CGColor| -> &AnyObject { unsafe { &*(c as *const objc2_core_graphics::CGColor as *const AnyObject) } };
        let objects: Vec<&AnyObject> = colors.iter().map(|c| as_object(c)).collect();
        unsafe { self.shade.setColors(Some(&NSArray::from_slice(&objects))) };
        let at: Vec<Retained<objc2_foundation::NSNumber>> = stops.iter().map(|&(p, _)| objc2_foundation::NSNumber::new_f64(p)).collect();
        self.shade.setLocations(Some(&NSArray::from_retained_slice(&at)));
    }
}
