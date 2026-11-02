//! In and out motion for every Livae window, with livae-core's motion values (`livae_core::motion`), so the Mac moves
//! like Windows and Livae for Zen:
//! - the search and the clipboard drop into focus (`drop_in`, `drop_out`): the panel falls onto its place on a spring
//!   and its rows fall in one after another, all sharpening from a blur;
//! - everything else blooms into focus (`show`, `hide`): what the glass holds grows from a little smaller with a soft
//!   overshoot while it sharpens.
//!
//! Apple's glass is never transformed: on screen it does not follow a layer transform, so its rim showed at the final
//! place while the content was still on its way (02.11.2026). The panel window moves instead (frame by frame, see
//! `place`), and the bloom and the blur act on what the glass holds. Reduce Motion keeps only the fades. Opening again
//! during an exit carries on from where the exit is.

use std::cell::RefCell;
use std::ptr::NonNull;

use block2::RcBlock;
use livae_core::motion::{self as m, Spring};
use objc2::rc::Retained;
use objc2::Message;
use objc2_app_kit::{NSAnimatablePropertyContainer, NSAnimationContext, NSGlassEffectView, NSView, NSWindow, NSWorkspace};
use objc2_core_image::CIFilter;
use objc2::runtime::AnyObject;
use objc2_foundation::{ns_string, NSArray, NSNumber, NSObjectNSKeyValueCoding, NSPoint, NSRect, NSRunLoop, NSRunLoopCommonModes, NSString, NSTimer, NSValue};
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

/// The search or the clipboard: the capsule and the card (their contents sharpen), and each row's views (top first,
/// they fall in).
pub struct Parts {
    pub capsule: Retained<NSView>,
    pub card: Retained<NSView>,
    pub rows: Vec<Vec<Retained<NSView>>>,
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

/// What a window's glass holds, without its tint (the glass and its tint must stay put), or the view itself.
fn inside(view: &NSView) -> Retained<NSView> {
    let held = view.downcast_ref::<NSGlassEffectView>().and_then(|g| g.contentView());
    held.and_then(|h| h.subviews().lastObject()).unwrap_or_else(|| view.retain())
}

// The panel window falling or lifting, frame by frame on the main run loop.

enum How {
    Spring(Spring),
    Ease(u64, [f64; 4]),
}

/// A window moving up or down from where it rests (`target`): `from` points above it to `to` points above it.
struct Move {
    window: Retained<NSWindow>,
    target: NSRect,
    from: f64,
    to: f64,
    start: f64,
    how: How,
}

impl Move {
    /// Points above the resting place now, and whether the move is over.
    fn at(&self, now: f64) -> (f64, bool) {
        let t = (now - self.start) / slow();
        let (v, end) = match self.how {
            How::Spring(s) => (s.at(t), t >= s.settle()),
            How::Ease(ms, c) => {
                let p = t / (ms as f64 / 1000.0);
                (m::ease(c, p), p >= 1.0)
            }
        };
        (if end { self.to } else { self.from + (self.to - self.from) * v }, end)
    }
}

thread_local! {
    static MOVES: RefCell<Vec<Move>> = const { RefCell::new(Vec::new()) };
    static TICKER: RefCell<Option<Retained<NSTimer>>> = const { RefCell::new(None) };
}

fn up(target: NSRect, dy: f64) -> NSPoint {
    NSPoint::new(target.origin.x, target.origin.y + dy)
}

fn same(a: &NSWindow, b: &NSWindow) -> bool {
    std::ptr::eq(a, b)
}

/// Where `window` is now, above its resting place, while it moves.
fn lifted(window: &NSWindow) -> Option<f64> {
    let now = CACurrentMediaTime();
    MOVES.with(|m| m.borrow().iter().find(|mv| same(&mv.window, window)).map(|mv| mv.at(now).0))
}

fn tick() {
    let now = CACurrentMediaTime();
    let empty = MOVES.with(|m| {
        let mut moves = m.borrow_mut();
        moves.retain(|mv| {
            let (dy, end) = mv.at(now);
            mv.window.setFrameOrigin(up(mv.target, dy));
            !end
        });
        moves.is_empty()
    });
    if empty && let Some(t) = TICKER.with(|t| t.borrow_mut().take()) {
        t.invalidate();
    }
}

/// Move `window` from `from` to `to` points above where it rests, replacing a move already under way.
fn move_window(window: &NSWindow, from: f64, to: f64, how: How) {
    let target = MOVES.with(|m| {
        let mut moves = m.borrow_mut();
        let old = moves.iter().position(|mv| same(&mv.window, window)).map(|i| moves.remove(i));
        old.map_or_else(|| window.frame(), |mv| mv.target)
    });
    window.setFrameOrigin(up(target, from));
    MOVES.with(|m| m.borrow_mut().push(Move { window: window.retain(), target, from, to, start: CACurrentMediaTime(), how }));
    TICKER.with(|t| {
        if t.borrow().is_none() {
            let block = RcBlock::new(|_: NonNull<NSTimer>| tick());
            let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(1.0 / 120.0, true, &block) };
            unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
            *t.borrow_mut() = Some(timer);
        }
    });
}

fn stop_moving(window: &NSWindow) {
    MOVES.with(|m| m.borrow_mut().retain(|mv| !same(&mv.window, window)));
}

/// Put a panel at its resting `frame` (its layout). While it falls or lifts, the frame becomes the place it moves to.
pub fn place(window: &NSWindow, frame: NSRect) {
    let dy = MOVES.with(|m| {
        let mut moves = m.borrow_mut();
        let mv = moves.iter_mut().find(|mv| same(&mv.window, window))?;
        mv.target = frame;
        Some(mv.at(CACurrentMediaTime()).0)
    });
    let shown = NSRect::new(up(frame, dy.unwrap_or(0.0)), frame.size);
    window.setFrame_display(shown, true);
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
    stop_moving(window);
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
        focus.setWantsLayer(true);
        let from = if fresh { pose(&focus, m::BLOOM_FROM, m::BLOOM_FROM, (0.5, 0.5), 0.0) } else { current(&focus) };
        spring_to(&focus, from, identity(), m::BLOOM, 0.0);
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
        ease_to(&focus, pose(&focus, m::EXIT_SCALE, m::EXIT_SCALE, (0.5, 0.5), 0.0), m::EXIT_MS, m::EASE_EXIT);
        blur(&focus, blur_now(&focus), m::EXIT_BLUR, m::EXIT_MS, m::EASE_EXIT);
    }
    let w = window.retain();
    fade(window, 0.0, m::EXIT_MS, m::EASE_EXIT, move || {
        if end_exit(&w, &[view.clone(), focus.clone()]) {
            done();
        }
    });
}

/// The search or the clipboard dropping into focus: the panel falls onto its place on a spring, each row falls in
/// `STAGGER_MS` after the one above, and the contents sharpen. `drop` is how far it falls (`DROP_Y`, the clipboard
/// `CLIP_DROP_Y`). The caller has placed and ordered it front.
pub fn drop_in(window: &NSWindow, parts: &Parts, drop: f64) {
    window.setIgnoresMouseEvents(false);
    let fresh = !leaving(window);
    if fresh {
        window.setAlphaValue(0.0);
    }
    let insides = [inside(&parts.capsule), inside(&parts.card)];
    if !reduce() {
        move_window(window, if fresh { drop } else { lifted(window).unwrap_or(0.0) }, 0.0, How::Spring(m::DROP));
        for v in &insides {
            let start = if fresh { m::FOCUS_BLUR } else { blur_now(v) };
            blur(v, start, 0.0, m::FOCUS_MS, m::EASE_FOCUS);
        }
        if fresh {
            for (i, row) in parts.rows.iter().enumerate() {
                let delay = m::row_delay(i) * slow();
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

/// The search or the clipboard leaving: the panel lifts `EXIT_LIFT` while its contents blur and it fades; then it
/// is ordered out (unless it was opened again meanwhile).
pub fn drop_out(window: &NSWindow, parts: &Parts) {
    if !window.isVisible() {
        return;
    }
    begin_exit(window);
    let insides = [inside(&parts.capsule), inside(&parts.card)];
    if !reduce() {
        move_window(window, lifted(window).unwrap_or(0.0), m::EXIT_LIFT, How::Ease(m::EXIT_MS, m::EASE_EXIT));
        for v in &insides {
            blur(v, blur_now(v), m::EXIT_BLUR, m::EXIT_MS, m::EASE_EXIT);
        }
    }
    let w = window.retain();
    let mut views: Vec<Retained<NSView>> = insides.to_vec();
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
