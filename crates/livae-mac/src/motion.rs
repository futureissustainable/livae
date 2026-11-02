//! In and out motion for every Livae window, from the shared tokens. A window fades in while its content settles from
//! a slightly smaller, offset pose (240 ms, ease-out); it fades out and shrinks a touch (150 ms). Under Reduce Motion
//! only the fades remain. Opening again during an exit picks up from where the exit is.

use block2::RcBlock;
use livae_core::tokens::motion as m;
use objc2::rc::Retained;
use objc2::Message;
use objc2_app_kit::{NSAnimatablePropertyContainer, NSAnimationContext, NSView, NSWindow, NSWorkspace};
use objc2_foundation::{ns_string, NSRect, NSValue};
use objc2_quartz_core::{CABasicAnimation, CAMediaTiming, CAMediaTimingFunction, CATransaction, CATransform3D, NSValueCATransform3DAdditions};

/// Where content starts (enter) or ends (exit): its scale about `anchor` and a vertical shift in points (up is +).
#[derive(Clone, Copy)]
pub struct Pose {
    pub scale: f64,
    pub dy: f64,
    /// The fixed point as fractions of the view's width and height from its bottom left; (0.5, 0.5) is the centre.
    pub anchor: (f64, f64),
}

impl Pose {
    pub const fn new(scale: f64, dy: f64, anchor: (f64, f64)) -> Pose {
        Pose { scale, dy, anchor }
    }
}

/// A panel opening below its field: from a little smaller and a little higher, fixed at its top edge.
pub const DROP: Pose = Pose::new(0.96, 6.0, (0.5, 1.0));
/// A window rising into place from its centre.
pub const RISE: Pose = Pose::new(0.98, -10.0, (0.5, 0.5));
/// A small confirmation popping from its centre.
pub const POP: Pose = Pose::new(0.9, 0.0, (0.5, 0.5));
/// Leaving: a touch smaller, in place.
pub const SHRINK: Pose = Pose::new(0.98, 0.0, (0.5, 0.5));
/// Only the fade (a full-screen backdrop).
pub const FADE: Pose = Pose::new(1.0, 0.0, (0.5, 0.5));

/// On screen and not leaving: what a toggle should treat as open.
pub fn shown(window: &NSWindow) -> bool {
    window.isVisible() && window.alphaValue() > 0.0
}

/// `enter` with the window's own content view.
pub fn show(window: &NSWindow, from: Pose) {
    if let Some(view) = window.contentView() {
        enter(window, &view, from);
    }
}

/// `exit` with the window's own content view.
pub fn hide(window: &NSWindow, to: Pose) {
    match window.contentView() {
        Some(view) => exit(window, &view, to, || {}),
        None => window.orderOut(None),
    }
}

fn reduce() -> bool {
    NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

/// `LIVAE_MOTION_SLOW=20` stretches every animation 20 times, so a check can capture it mid-way.
fn seconds(ms: u64) -> f64 {
    let slow = std::env::var("LIVAE_MOTION_SLOW").ok().and_then(|s| s.parse::<f64>().ok()).filter(|s| *s >= 1.0).unwrap_or(1.0);
    ms as f64 / 1000.0 * slow
}

fn transform(view: &NSView, pose: Pose) -> CATransform3D {
    let size = view.bounds().size;
    let (ax, ay) = (size.width * pose.anchor.0, size.height * pose.anchor.1);
    CATransform3D::new_translation(-ax, -ay, 0.0).concat(CATransform3D::new_scale(pose.scale, pose.scale, 1.0)).concat(CATransform3D::new_translation(ax, ay + pose.dy, 0.0))
}

fn set_now(view: &NSView, t: CATransform3D) {
    if let Some(layer) = view.layer() {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        layer.setTransform(t);
        CATransaction::commit();
    }
}

fn curve() -> Retained<CAMediaTimingFunction> {
    let [a, b, c, d] = m::EASE_OUT;
    CAMediaTimingFunction::functionWithControlPoints(a as f32, b as f32, c as f32, d as f32)
}

/// Move the content's transform to `to` over `ms`, from `from` or else from where it is on screen now. AppKit does not
/// animate a view's own layer by itself, so this is an explicit Core Animation.
fn move_content(view: &NSView, from: Option<CATransform3D>, to: CATransform3D, ms: u64) {
    let Some(layer) = view.layer() else { return };
    let from = from.unwrap_or_else(|| unsafe { layer.presentationLayer() }.map_or_else(|| layer.transform(), |p| p.transform()));
    set_now(view, to);
    if reduce() {
        return;
    }
    let anim = CABasicAnimation::animationWithKeyPath(Some(ns_string!("transform")));
    unsafe {
        anim.setFromValue(Some(&NSValue::valueWithCATransform3D(from)));
        anim.setToValue(Some(&NSValue::valueWithCATransform3D(to)));
    }
    anim.setDuration(seconds(ms));
    anim.setTimingFunction(Some(&curve()));
    layer.addAnimation_forKey(&anim, Some(ns_string!("livae.motion")));
}

/// Fade the window to `alpha` over `ms` with Livae's ease-out curve; `done` runs when it ends.
fn fade(window: &NSWindow, alpha: f64, ms: u64, done: impl Fn() + 'static) {
    let w = window.retain();
    let curve = curve();
    let group = RcBlock::new(move |ctx: std::ptr::NonNull<NSAnimationContext>| {
        let ctx = unsafe { ctx.as_ref() };
        ctx.setDuration(seconds(ms));
        ctx.setTimingFunction(Some(&curve));
        w.animator().setAlphaValue(alpha);
    });
    let end = RcBlock::new(done);
    NSAnimationContext::runAnimationGroup_completionHandler(&group, Some(&end));
}

/// Show `window` (already placed and ordered front by the caller) with its content `view` settling from `from`.
pub fn enter(window: &NSWindow, view: &NSView, from: Pose) {
    view.setWantsLayer(true);
    window.setIgnoresMouseEvents(false);
    // Hidden or invisible: start from nothing. Mid-exit: carry on from the current look.
    let fresh = !window.isVisible() || window.alphaValue() >= 1.0;
    if fresh {
        window.setAlphaValue(0.0);
    }
    move_content(view, fresh.then(|| transform(view, from)), CATransform3D::new_scale(1.0, 1.0, 1.0), m::PANEL);
    fade(window, 1.0, m::PANEL, || {});
}

/// Fade `window` out with its content easing to `to`, then order it out and run `done`, unless it was opened again
/// meanwhile.
pub fn exit(window: &NSWindow, view: &NSView, to: Pose, done: impl Fn() + 'static) {
    if !window.isVisible() {
        done();
        return;
    }
    // Hand the keyboard back at once (ordering out and straight back in drops key status without a visible gap), and
    // let clicks through while it fades: typing right after Esc lands in the app below, not in the fading window.
    if window.isKeyWindow() {
        window.orderOut(None);
        window.orderFrontRegardless();
    }
    window.setIgnoresMouseEvents(true);
    let (w2, v2): (Retained<NSWindow>, Retained<NSView>) = (window.retain(), view.retain());
    move_content(view, None, transform(view, to), m::EXIT);
    fade(
        window,
        0.0,
        m::EXIT,
        move || {
            // Opened again while leaving: `enter` set the alpha back to 1, so stay.
            if w2.alphaValue() <= 0.0 {
                w2.orderOut(None);
                w2.setAlphaValue(1.0);
                w2.setIgnoresMouseEvents(false);
                set_now(&v2, CATransform3D::new_scale(1.0, 1.0, 1.0));
                done();
            }
        },
    );
}

/// Put the selection band on `to`: a 160 ms slide from the row it was on, or at once when `animate` is false (the list
/// was laid out again or scrolled), when it was hidden, or under Reduce Motion.
pub fn slide(band: &NSView, to: NSRect, animate: bool) {
    if !animate || band.isHidden() || reduce() {
        band.setFrame(to);
        band.setHidden(false);
        return;
    }
    let b = band.retain();
    let curve = curve();
    let group = RcBlock::new(move |ctx: std::ptr::NonNull<NSAnimationContext>| {
        let ctx = unsafe { ctx.as_ref() };
        ctx.setDuration(seconds(m::PRESS));
        ctx.setTimingFunction(Some(&curve));
        b.animator().setFrame(to);
    });
    NSAnimationContext::runAnimationGroup(&group);
}
