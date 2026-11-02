//! Test aids for design checks: what a window shows (every visible text and control, with its frame, font and
//! colour) and what a menu holds, as tab-separated lines a script can score. Only `--test` copies call these.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSButton, NSControl, NSFontWeightTrait, NSMenu, NSPopUpButton, NSScrollView, NSSlider, NSSwitch, NSTextField, NSTextView, NSView,
    NSWindow,
};
use objc2_foundation::{NSDictionary, NSNumber, NSRect};

fn hex(color: Option<Retained<objc2_app_kit::NSColor>>) -> String {
    let Some(c) = color.and_then(|c| c.colorUsingColorSpace(&objc2_app_kit::NSColorSpace::sRGBColorSpace())) else { return "-".into() };
    let v = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", v(c.redComponent()), v(c.greenComponent()), v(c.blueComponent()))
}

fn weight(font: &objc2_app_kit::NSFont) -> f64 {
    let traits = font.fontDescriptor().objectForKey(unsafe { objc2_app_kit::NSFontTraitsAttribute });
    traits
        .and_then(|t| t.downcast::<NSDictionary>().ok())
        .and_then(|d| unsafe { d.objectForKey(NSFontWeightTrait) })
        .and_then(|w| w.downcast::<NSNumber>().ok())
        .map(|n| (n.doubleValue() * 100.0).round() / 100.0)
        .unwrap_or(0.0)
}

fn clean(s: &str) -> String {
    s.replace(['\t', '\n'], " ")
}

/// Every visible text and control in `window`, top-left coordinates in points, then the window line:
/// `kind x y w h size weight color fit_w text action`.
pub fn window(window: &NSWindow) -> String {
    let Some(root) = window.contentView() else { return String::new() };
    let mut out = String::new();
    let height = root.frame().size.height;
    walk(&root, &root, height, &mut out);
    let r = window.frame();
    out.push_str(&format!("window\t0\t0\t{:.0}\t{:.0}\t-\t-\t-\t-\t{}\t-\n", r.size.width, r.size.height, clean(&window.title().to_string())));
    out
}

fn walk(view: &NSView, root: &NSView, height: f64, out: &mut String) {
    if view.isHidden() {
        return;
    }
    let f: NSRect = view.convertRect_toView(view.bounds(), Some(root));
    let (x, y, w, h) = (f.origin.x, height - f.origin.y - f.size.height, f.size.width, f.size.height);
    let any: &AnyObject = view.as_ref();
    let line = |kind: &str, size: f64, wt: f64, color: String, fit: f64, text: String, action: String| {
        format!("{kind}\t{x:.0}\t{y:.0}\t{w:.0}\t{h:.0}\t{size}\t{wt}\t{color}\t{fit:.0}\t{}\t{action}\n", clean(&text))
    };
    let action = |c: &NSControl| c.action().map(|s| s.name().to_string_lossy().into_owned()).unwrap_or_else(|| "-".into());
    if let Some(sv) = any.downcast_ref::<NSScrollView>() {
        // How tall the content is against what shows: more than shows means scrolling.
        let doc = sv.documentView().map(|d| d.frame().size.height).unwrap_or(0.0);
        out.push_str(&line("scroll", 0.0, 0.0, "-".into(), doc, String::new(), "-".into()));
        if let Some(d) = sv.documentView()
            && let Some(tv) = d.downcast_ref::<NSTextView>()
        {
            let size = tv.font().map(|f| f.pointSize()).unwrap_or(0.0);
            out.push_str(&line("textview", size, 0.0, hex(tv.textColor()), 0.0, tv.string().to_string(), "-".into()));
            return;
        }
    } else if let Some(t) = any.downcast_ref::<NSTextField>() {
        let text = t.stringValue().to_string();
        if !text.is_empty() {
            let font = t.font();
            let size = font.as_ref().map(|f| f.pointSize()).unwrap_or(0.0);
            let wt = font.as_ref().map(|f| weight(f)).unwrap_or(0.0);
            let kind = if t.isEditable() { "field" } else { "label" };
            out.push_str(&line(kind, size, wt, hex(t.textColor()), t.fittingSize().width, text, "-".into()));
        }
        return;
    } else if let Some(p) = any.downcast_ref::<NSPopUpButton>() {
        let title = p.titleOfSelectedItem().map(|s| s.to_string()).unwrap_or_default();
        out.push_str(&line("popup", 0.0, 0.0, "-".into(), p.fittingSize().width, title, action(p)));
        return;
    } else if let Some(s) = any.downcast_ref::<NSSwitch>() {
        out.push_str(&line("switch", 0.0, 0.0, "-".into(), 0.0, String::new(), action(s)));
        return;
    } else if let Some(s) = any.downcast_ref::<NSSlider>() {
        out.push_str(&line("slider", 0.0, 0.0, "-".into(), 0.0, String::new(), action(s)));
        return;
    } else if let Some(b) = any.downcast_ref::<NSButton>() {
        out.push_str(&line("button", 0.0, 0.0, "-".into(), b.fittingSize().width, b.title().to_string(), action(b)));
        return;
    }
    for sub in view.subviews().iter() {
        walk(&sub, root, height, out);
    }
}

/// Every item of `menu` and its submenus: `item depth title enabled state submenu key image subtitle separator`.
pub fn menu(menu: &NSMenu) -> String {
    let mut out = String::new();
    items(menu, 0, &mut out);
    out
}

fn items(menu: &NSMenu, depth: usize, out: &mut String) {
    for item in menu.itemArray().iter() {
        if item.isHidden() {
            continue;
        }
        let subtitle = item.subtitle().map(|s| s.to_string()).unwrap_or_default();
        let key = item.keyEquivalent().to_string();
        out.push_str(&format!(
            "item\t{depth}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            clean(&item.title().to_string()),
            u8::from(item.isEnabled()),
            item.state(),
            u8::from(item.hasSubmenu()),
            clean(&key),
            u8::from(item.image().is_some()),
            clean(&subtitle),
            u8::from(item.isSeparatorItem()),
        ));
        if let Some(sub) = item.submenu() {
            items(&sub, depth + 1, out);
        }
    }
}
