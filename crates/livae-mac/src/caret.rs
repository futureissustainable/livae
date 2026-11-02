//! The glowing caret of the search fields (the macOS 27 Spotlight look): the system caret is hidden and a rounded
//! white bar with a soft glow stands at the insertion point, pulsing gently while you pause.

use objc2::rc::Retained;
use objc2::MainThreadOnly;
use objc2_app_kit::{NSColor, NSShadow, NSTextField, NSTextInputClient, NSTextView, NSView, NSWorkspace};
use objc2_foundation::{ns_string, MainThreadMarker, NSNumber, NSPoint, NSRange, NSRect, NSSize};
use objc2_quartz_core::{CABasicAnimation, CAMediaTiming, CAMediaTimingFunction, kCAMediaTimingFunctionEaseInEaseOut};

const WIDTH: f64 = 3.0;

pub struct Caret {
    bar: Retained<NSView>,
}

impl Caret {
    /// A caret `height` points tall, drawn inside `parent` (the view the field sits in).
    pub fn new(mtm: MainThreadMarker, parent: &NSView, height: f64) -> Caret {
        let bar = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, height)));
        bar.setWantsLayer(true);
        if let Some(layer) = bar.layer() {
            layer.setBackgroundColor(Some(&NSColor::whiteColor().CGColor()));
            layer.setCornerRadius(WIDTH / 2.0);
        }
        // The glow: AppKit keeps a view's shadow only when set through the view.
        let glow = NSShadow::new();
        glow.setShadowColor(Some(&NSColor::colorWithWhite_alpha(1.0, 0.9)));
        glow.setShadowBlurRadius(8.0);
        glow.setShadowOffset(NSSize::new(0.0, 0.0));
        bar.setShadow(Some(&glow));
        bar.setHidden(true);
        parent.addSubview(&bar);
        Caret { bar }
    }

    /// Stand at `field`'s insertion point; hidden while the field is not being edited or text is selected.
    pub fn place(&self, field: &NSTextField) {
        match insertion_x(field) {
            Some((x, mid)) => {
                let h = self.bar.frame().size.height;
                // Just after the last letter, or just before the hint text of an empty field, never touching a letter.
                let x = if field.stringValue().length() == 0 { x.round() - WIDTH - 2.0 } else { x.round() + 1.0 };
                self.bar.setFrame(NSRect::new(NSPoint::new(x, (mid - h / 2.0).round()), NSSize::new(WIDTH, h)));
                self.bar.setHidden(false);
                self.pulse();
            }
            None => self.bar.setHidden(true),
        }
    }

    /// Solid now (typing just happened), then a slow fade between full and faint; still under Reduce Motion.
    fn pulse(&self) {
        let Some(layer) = self.bar.layer() else { return };
        layer.removeAnimationForKey(ns_string!("pulse"));
        if NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion() {
            return;
        }
        let anim = CABasicAnimation::animationWithKeyPath(Some(ns_string!("opacity")));
        unsafe {
            anim.setFromValue(Some(&NSNumber::new_f64(1.0)));
            anim.setToValue(Some(&NSNumber::new_f64(0.2)));
        }
        anim.setDuration(0.6);
        anim.setAutoreverses(true);
        anim.setRepeatCount(f32::INFINITY);
        anim.setTimingFunction(Some(&CAMediaTimingFunction::functionWithName(unsafe { kCAMediaTimingFunctionEaseInEaseOut })));
        layer.addAnimation_forKey(&anim, Some(ns_string!("pulse")));
    }
}

/// The insertion point's x and the line's middle, in the coordinates of the field's superview; also hides the system
/// caret. None when the field is not being edited or text is selected.
fn insertion_x(field: &NSTextField) -> Option<(f64, f64)> {
    let editor = field.currentEditor()?.downcast::<NSTextView>().ok()?;
    editor.setInsertionPointColor(Some(&NSColor::clearColor()));
    let sel = editor.selectedRange();
    if sel.length > 0 {
        return None;
    }
    end_of(field, &editor, sel.location)
}

/// Where character `at` sits in the field: its x and the line's middle, in the field's superview.
fn end_of(field: &NSTextField, editor: &NSTextView, at: usize) -> Option<(f64, f64)> {
    let mut actual = NSRange::new(0, 0);
    let screen = unsafe { editor.firstRectForCharacterRange_actualRange(NSRange::new(at, 0), &mut actual) };
    let window = field.window()?;
    let in_window = window.convertRectFromScreen(screen);
    let parent = unsafe { field.superview() }?;
    let r = parent.convertRect_fromView(in_window, None);
    Some((r.origin.x, r.origin.y + r.size.height / 2.0))
}

/// The x just after the field's text, in the field's superview (for a hint written after it).
pub fn text_end(field: &NSTextField) -> Option<f64> {
    let editor = field.currentEditor()?.downcast::<NSTextView>().ok()?;
    end_of(field, &editor, editor.string().length()).map(|(x, _)| x)
}

