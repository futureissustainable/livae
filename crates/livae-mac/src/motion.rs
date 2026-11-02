//! In and out motion for every Livae window, with livae-core's motion values (`livae_core::motion`), so the Mac moves
//! exactly like Windows and Livae for Zen:
//! - the search and the clipboard drop into focus (`drop_in`, `drop_out`): the capsule falls onto its place on a
//!   spring, the list unfolds under it and the rows fall in one after another, all sharpening from a blur;
//! - everything else blooms into focus (`show`, `hide`): it grows from a little smaller with a soft overshoot while
//!   it sharpens.
//!
//! Apple's glass cannot be blurred, so the blur runs on what the glass holds. Reduce Motion keeps only the fades.
//! Opening again during an exit carries on from where the exit is.

use block2::RcBlock;
use livae_core::motion::{self as m, Spring};
use objc2::rc::Retained;
use objc2::Message;
use objc2_app_kit::{NSAnimatablePropertyContainer, NSAnimationContext, NSGlassEffectView, NSView, NSWindow, NSWorkspace};
use objc2_core_image::CIFilter;
use objc2::runtime::AnyObject;
use objc2_foundation::{ns_string, NSArray, NSNumber, NSObjectNSKeyValueCoding, NSRect, NSString, NSValue};
use objc2_quartz_core::{
    kCAFillModeBackwards, CABasicAnimation, CACurrentMediaTime, CAMediaTiming, CAMediaTimingFunction, CASpringAnimation, CATransaction, CATransform3D,
    NSValueCATransform3DAdditions,
};

/// How a window other than the search and the clipboard comes and goes.
#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    /// Grows from a little smaller into focus (windows, the toast, the break warning).
    Bloom,
    /// Only fades (a full-screen backdrop).
    Fade,
}

/// The parts of the search or the clipboard that move: the capsule, the list's card, and each row's views (top first).
pub struct Parts {
    pub capsule: Retained<NSView>,
    pub card: Retained<NSView>,
    pub rows: Vec<Vec<Retained<NSView>>>,
}

/// Where the search or the clipboard falls from: a drop in points, a starting size, and the fixed point of that
/// size as fractions of the panel from its bottom left.
#[derive(Clone, Copy)]
pub struct From {
    pub drop: f64,
    pub scale: f64,
    pub anchor: (f64, f64),
}

/// The search: falls `DROP_Y` at full size.
pub const SEARCH: From = From { drop: m::DROP_Y, scale: 1.0, anchor: (0.5, 1.0) };

/// The clipboard at the text cursor: falls `CLIP_DROP_Y` from `CARET_SCALE`, fixed at the edge nearest the cursor.
pub fn at_caret(above: bool) -> From {
    From { drop: m::CLIP_DROP_Y, scale: m::CARET_SCALE, anchor: (0.5, if above { 0.0 } else { 1.0 }) }
}

/// On screen and not leaving: what a toggle should treat as open.
pub fn shown(window: &NSWindow) -> bool {
    window.isVisible() && window.alphaValue() > 0.0
}

fn reduce() -> bool {
    NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

/// `LIVAE_MOTION_SLOW=20` stretches every animation 20 times, springs too, so a check can capture it mid-way.
fn slow() -> f64 {
    std::env::var("LIVAE_MOTION_SLOW").ok().and_then(|s| s.parse::<f64>().ok()).filter(|s| *s >= 1.0).unwrap_or(1.0)
}

fn secs(ms: u64) -> f64 {
    ms as f64 / 1000.0 * slow()
}

fn curve(c: [f64; 4]) -> Retained<CAMediaTimingFunction> {
    CAMediaTimingFunction::functionWithControlPoints(c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32)
}

fn identity() -> CATransform3D {
    CATransform3D::new_scale(1.0, 1.0, 1.0)
}

/// Scale `sx`, `sy` about `anchor` (fractions of `view` from its bottom left), then move up `dy` points.
fn pose(view: &NSView, sx: f64, sy: f64, anchor: (f64, f64), dy: f64) -> CATransform3D {
    let size = view.bounds().size;
    let (ax, ay) = (size.width * anchor.0, size.height * anchor.1);
    CATransform3D::new_translation(-ax, -ay, 0.0).concat(CATransform3D::new_scale(sx, sy, 1.0)).concat(CATransform3D::new_translation(ax, ay + dy, 0.0))
}

/// Set a layer value at once, without an implicit animation.
fn now(f: impl FnOnce()) {
    CATransaction::begin();
    CATransaction::setDisableActions(true);
    f();
    CATransaction::commit();
}

/// Run `anim` on `view` after `delay` seconds, showing its start value until then.
fn add(view: &NSView, anim: &CABasicAnimation, key: &NSString, delay: f64) {
    let Some(layer) = view.layer() else { return };
    if delay > 0.0 {
        anim.setBeginTime(layer.convertTime_fromLayer(CACurrentMediaTime(), None) + delay);
        anim.setFillMode(unsafe { kCAFillModeBackwards });
    }
    layer.addAnimation_forKey(anim, Some(key));
}

fn transform_value(t: CATransform3D) -> Retained<NSValue> {
    unsafe { NSValue::valueWithCATransform3D(t) }
}

/// Where the view's transform is on screen now (mid-animation included).
fn current(view: &NSView) -> CATransform3D {
    view.layer().map_or_else(identity, |l| unsafe { l.presentationLayer() }.map_or_else(|| l.transform(), |p| p.transform()))
}

/// Move the view's transform from `from` to `to` on `spring`, starting after `delay` seconds.
fn spring_to(view: &NSView, from: CATransform3D, to: CATransform3D, spring: Spring, delay: f64) {
    let Some(layer) = view.layer() else { return };
    now(|| layer.setTransform(to));
    // Slowed s times, a spring keeps its shape with stiffness / s² and damping / s.
    let s = slow();
    let anim = CASpringAnimation::animationWithKeyPath(Some(ns_string!("transform")));
    anim.setStiffness(spring.stiffness / (s * s));
    anim.setDamping(spring.damping / s);
    anim.setMass(1.0);
    anim.setInitialVelocity(0.0);
    unsafe {
        anim.setFromValue(Some(&transform_value(from)));
        anim.setToValue(Some(&transform_value(to)));
    }
    anim.setDuration(anim.settlingDuration());
    add(view, &anim, ns_string!("livae.move"), delay);
}

/// Move the view's transform to `to` over `ms` with `ease`, from where it is now.
fn ease_to(view: &NSView, to: CATransform3D, ms: u64, ease: [f64; 4]) {
    let from = current(view);
    let Some(layer) = view.layer() else { return };
    now(|| layer.setTransform(to));
    let anim = CABasicAnimation::animationWithKeyPath(Some(ns_string!("transform")));
    unsafe {
        anim.setFromValue(Some(&transform_value(from)));
        anim.setToValue(Some(&transform_value(to)));
    }
    anim.setDuration(secs(ms));
    anim.setTimingFunction(Some(&curve(ease)));
    add(view, &anim, ns_string!("livae.move"), 0.0);
}

/// Animate `view`'s layer value at `path` from `from` to `to` over `ms` with `ease`, after `delay` seconds.
fn tween(view: &NSView, path: &NSString, from: f64, to: f64, ms: u64, ease: [f64; 4], delay: f64) {
    let Some(layer) = view.layer() else { return };
    now(|| unsafe { layer.setValue_forKeyPath(Some(&NSNumber::new_f64(to)), path) });
    let anim = CABasicAnimation::animationWithKeyPath(Some(path));
    unsafe {
        anim.setFromValue(Some(&NSNumber::new_f64(from)));
        anim.setToValue(Some(&NSNumber::new_f64(to)));
    }
    anim.setDuration(secs(ms));
    anim.setTimingFunction(Some(&curve(ease)));
    add(view, &anim, path, delay);
}

/// What a window's glass holds (the glass itself cannot be blurred), or the view itself.
fn inside(view: &NSView) -> Retained<NSView> {
    view.downcast_ref::<NSGlassEffectView>().and_then(|g| g.contentView()).unwrap_or_else(|| view.retain())
}

/// The blur on `view`, as a radius in points: a Core Image blur put on for the motion and taken off once sharp, so
/// typing never pays for it.
fn blur(view: &NSView, from: f64, to: f64, ms: u64, ease: [f64; 4]) {
    view.setWantsLayer(true);
    view.setLayerUsesCoreImageFilters(true);
    let Some(layer) = view.layer() else { return };
    if layer.filters().is_none_or(|f| f.count() == 0) {
        let Some(filter) = (unsafe { CIFilter::filterWithName(ns_string!("CIGaussianBlur")) }) else { return };
        unsafe {
            filter.setName(ns_string!("blur"));
            filter.setValue_forKey(Some(&NSNumber::new_f64(from)), ns_string!("inputRadius"));
        }
        let filters = NSArray::<AnyObject>::from_slice(&[&*filter as &AnyObject]);
        now(|| unsafe { layer.setFilters(Some(&filters)) });
    }
    tween(view, ns_string!("filters.blur.inputRadius"), from, to, ms, ease, 0.0);
}

fn unblur(view: &NSView) {
    if let Some(layer) = view.layer() {
        now(|| unsafe { layer.setFilters(None) });
    }
}

/// The blur on `view` now (mid-animation included), 0 when it has none.
fn blur_now(view: &NSView) -> f64 {
    let Some(layer) = view.layer() else { return 0.0 };
    let shown = unsafe { layer.presentationLayer() }.unwrap_or(layer);
    shown.valueForKeyPath(ns_string!("filters.blur.inputRadius")).and_then(|v| v.downcast::<NSNumber>().ok()).map_or(0.0, |n| n.doubleValue())
}

/// Fade the window to `alpha` over `ms` with `ease`; `done` runs when it ends.
fn fade(window: &NSWindow, alpha: f64, ms: u64, ease: [f64; 4], done: impl Fn() + 'static) {
    let w = window.retain();
    let c = curve(ease);
    let group = RcBlock::new(move |ctx: std::ptr::NonNull<NSAnimationContext>| {
        let ctx = unsafe { ctx.as_ref() };
        ctx.setDuration(secs(ms));
        ctx.setTimingFunction(Some(&c));
        w.animator().setAlphaValue(alpha);
    });
    let end = RcBlock::new(done);
    NSAnimationContext::runAnimationGroup_completionHandler(&group, Some(&end));
}

/// On screen but already fading to nothing: leaving.
fn leaving(window: &NSWindow) -> bool {
    window.isVisible() && window.alphaValue() <= 0.0
}

/// Start an exit: hand the keyboard back at once (ordering out and straight back in drops key status without a
/// visible gap) and let clicks through while it fades, so typing after Esc lands in the app below.
fn begin_exit(window: &NSWindow) {
    if window.isKeyWindow() {
        window.orderOut(None);
        window.orderFrontRegardless();
    }
    window.setIgnoresMouseEvents(true);
}

/// End an exit unless the window was opened again meanwhile: order it out and put every part back at rest.
fn end_exit(window: &NSWindow, views: &[Retained<NSView>]) -> bool {
    if window.alphaValue() > 0.0 {
        return false;
    }
    window.orderOut(None);
    window.setAlphaValue(1.0);
    window.setIgnoresMouseEvents(false);
    for v in views {
        if let Some(l) = v.layer() {
            now(|| {
                l.removeAllAnimations();
                l.setTransform(identity());
                l.setOpacity(1.0);
            });
        }
        unblur(v);
    }
    true
}

/// Show `window` (placed and ordered front by the caller).
pub fn show(window: &NSWindow, kind: Kind) {
    let Some(view) = window.contentView() else { return };
    view.setWantsLayer(true);
    window.setIgnoresMouseEvents(false);
    let fresh = !leaving(window);
    if fresh {
        window.setAlphaValue(0.0);
    }
    let focus = inside(&view);
    if kind == Kind::Bloom && !reduce() {
        let from = if fresh { pose(&view, m::BLOOM_FROM, m::BLOOM_FROM, (0.5, 0.5), 0.0) } else { current(&view) };
        spring_to(&view, from, identity(), m::BLOOM, 0.0);
        let start = if fresh { m::FOCUS_BLUR } else { blur_now(&focus) };
        blur(&focus, start, 0.0, m::FOCUS_MS, m::EASE_FOCUS);
    }
    fade(window, 1.0, m::FOCUS_MS, m::EASE_FOCUS, move || unblur(&focus));
}

/// Fade `window` out, shrinking a little into a blur, then order it out (unless it was opened again meanwhile).
pub fn hide(window: &NSWindow, kind: Kind) {
    hide_then(window, kind, || {});
}

/// `hide`, then `done` once the window is gone.
pub fn hide_then(window: &NSWindow, kind: Kind, done: impl Fn() + 'static) {
    let Some(view) = window.contentView() else { return window.orderOut(None) };
    if !window.isVisible() {
        return done();
    }
    begin_exit(window);
    let focus = inside(&view);
    if kind == Kind::Bloom && !reduce() {
        ease_to(&view, pose(&view, m::EXIT_SCALE, m::EXIT_SCALE, (0.5, 0.5), 0.0), m::EXIT_MS, m::EASE_EXIT);
        blur(&focus, blur_now(&focus), m::EXIT_BLUR, m::EXIT_MS, m::EASE_EXIT);
    }
    let w = window.retain();
    fade(window, 0.0, m::EXIT_MS, m::EASE_EXIT, move || {
        if end_exit(&w, &[view.clone(), focus.clone()]) {
            done();
        }
    });
}

/// The search or the clipboard dropping into focus: the capsule falls onto its place on a spring, the card unfolds
/// down from its top edge, each row falls in `STAGGER_MS` after the one above, and everything sharpens.
pub fn drop_in(window: &NSWindow, parts: &Parts, from: From) {
    window.setIgnoresMouseEvents(false);
    let fresh = !leaving(window);
    if fresh {
        window.setAlphaValue(0.0);
    }
    let insides = [inside(&parts.capsule), inside(&parts.card)];
    if !reduce() {
        for v in [&parts.capsule, &parts.card] {
            v.setWantsLayer(true);
        }
        let stagger = secs(m::STAGGER_MS);
        let capsule_from = if fresh { pose(&parts.capsule, from.scale, from.scale, from.anchor, from.drop) } else { current(&parts.capsule) };
        spring_to(&parts.capsule, capsule_from, identity(), m::DROP, 0.0);
        let card_from = if fresh { pose(&parts.card, from.scale, m::UNFOLD_FROM * from.scale, (0.5, 1.0), from.drop) } else { current(&parts.card) };
        spring_to(&parts.card, card_from, identity(), m::DROP, if fresh { 3.0 * stagger } else { 0.0 });
        for v in &insides {
            let start = if fresh { m::FOCUS_BLUR } else { blur_now(v) };
            blur(v, start, 0.0, m::FOCUS_MS, m::EASE_FOCUS);
        }
        if fresh {
            for (i, row) in parts.rows.iter().enumerate() {
                let delay = (4 + i) as f64 * stagger;
                for v in row {
                    v.setWantsLayer(true);
                    spring_to(v, CATransform3D::new_translation(0.0, m::ROW_DROP, 0.0), identity(), m::ROW, delay);
                    tween(v, ns_string!("opacity"), 0.0, 1.0, m::FOCUS_MS, m::EASE_FOCUS, delay);
                }
            }
        }
    }
    fade(window, 1.0, m::FOCUS_MS, m::EASE_FOCUS, move || insides.iter().for_each(|v| unblur(v)));
}

/// The search or the clipboard leaving: the capsule lifts `EXIT_LIFT` and the card folds up a little, blurring as
/// they fade; then the window is ordered out (unless it was opened again meanwhile).
pub fn drop_out(window: &NSWindow, parts: &Parts) {
    if !window.isVisible() {
        return;
    }
    begin_exit(window);
    let insides = [inside(&parts.capsule), inside(&parts.card)];
    if !reduce() {
        ease_to(&parts.capsule, CATransform3D::new_translation(0.0, m::EXIT_LIFT, 0.0), m::EXIT_MS, m::EASE_EXIT);
        ease_to(&parts.card, pose(&parts.card, 1.0, 0.85, (0.5, 1.0), 0.0), m::EXIT_MS, m::EASE_EXIT);
        for v in &insides {
            blur(v, blur_now(v), m::EXIT_BLUR, m::EXIT_MS, m::EASE_EXIT);
        }
    }
    let w = window.retain();
    let mut views: Vec<Retained<NSView>> = vec![parts.capsule.clone(), parts.card.clone()];
    views.extend(insides);
    views.extend(parts.rows.iter().flatten().cloned());
    fade(window, 0.0, m::EXIT_MS, m::EASE_EXIT, move || {
        end_exit(&w, &views);
    });
}

/// Put the selection band on `to`: a slide from the row it was on (`PRESS`, Livae's ease-out), or at once when
/// `animate` is false (the list was laid out again or scrolled), when it was hidden, or under Reduce Motion.
pub fn slide(band: &NSView, to: NSRect, animate: bool) {
    if !animate || band.isHidden() || reduce() {
        band.setFrame(to);
        band.setHidden(false);
        return;
    }
    let b = band.retain();
    let c = curve(livae_core::tokens::motion::EASE_OUT);
    let group = RcBlock::new(move |ctx: std::ptr::NonNull<NSAnimationContext>| {
        let ctx = unsafe { ctx.as_ref() };
        ctx.setDuration(secs(livae_core::tokens::motion::PRESS));
        ctx.setTimingFunction(Some(&c));
        b.animator().setFrame(to);
    });
    NSAnimationContext::runAnimationGroup(&group);
}
