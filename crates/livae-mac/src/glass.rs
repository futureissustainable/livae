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

pub struct Glass {
    /// Add this to the window.
    pub root: Retained<NSView>,
    /// Put the shape's controls in here.
    pub content: Retained<NSView>,
    shade: Retained<CAGradientLayer>,
    _glass: Option<Retained<NSGlassEffectView>>,
}

impl Glass {
    pub fn new(mtm: MainThreadMarker, radius: f64, darkness: f64) -> Self {
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
            // Regular blurs what is behind enough to read over busy screens; the gradient removes its grey.
            g.setStyle(NSGlassEffectViewStyle::Regular);
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
        let g = Self { root, content, shade, _glass: glass };
        g.set_darkness(darkness);
        g
    }

    /// 0 = clear glass, 1 = black at the top fading to a dark veil at the bottom.
    pub fn set_darkness(&self, darkness: f64) {
        let d = darkness.clamp(0.0, 1.0);
        let top = NSColor::colorWithWhite_alpha(0.0, 0.92 * d).CGColor();
        let bottom = NSColor::colorWithWhite_alpha(0.0, 0.45 * d).CGColor();
        // CGColor is toll-free bridged to an object, which is what the layer's colour array holds.
        let as_object = |c: &objc2_core_graphics::CGColor| -> &AnyObject { unsafe { &*(c as *const objc2_core_graphics::CGColor as *const AnyObject) } };
        let colors = NSArray::from_slice(&[as_object(&top), as_object(&bottom)]);
        unsafe { self.shade.setColors(Some(&colors)) };
    }
}
