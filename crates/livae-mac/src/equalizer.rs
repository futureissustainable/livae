//! The Equaliser window: choose every device or one output, see the curve it plays through, set the 10-band EQ and
//! the boost, and type or import parametric filters (AutoEq text). A change is heard at once and saved a moment
//! later, to the same `settings.toml` as everything else.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;

use block2::RcBlock;
use livae_core::eq::{Design, BANDS, BAND_RANGE, BOOST_MAX};
use livae_core::sound::Sound;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, MainThreadOnly};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSApplication, NSBezierPath, NSButton, NSColor, NSFont,
    NSFontWeightRegular, NSModalResponseOK, NSOpenPanel, NSPopUpButton, NSScrollView, NSSlider, NSTextAlignment, NSTextDidChangeNotification,
    NSTextField, NSTextView, NSView, NSWindow, NSWindowDelegate, NSWindowTitleVisibility,
};
use objc2_foundation::{ns_string, MainThreadMarker, NSNotification, NSNotificationCenter, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use crate::config;
use crate::glass::Glass;
use crate::ui::{self, SECONDARY, TERTIARY, TEXT};

const W: f64 = 620.0;
const PAD: f64 = 24.0;
const TITLEBAR: f64 = 44.0;
const GRAPH_H: f64 = 170.0;
const SLIDER_H: f64 = 110.0;
const FILTERS_H: f64 = 140.0;
/// The graph's sample rate: what the curve shows does not depend on the device's.
const RATE: f64 = 48000.0;
const BAND_NAMES: [&str; 10] = ["32", "64", "128", "250", "500", "1k", "2k", "4k", "8k", "16k"];
/// The graph's dB scale: at least ±12, in steps of 6 so its halves are whole numbers.
const SCALE_DB: f64 = 12.0;

struct Ui {
    window: Retained<NSWindow>,
    glass: Glass,
    device: Retained<NSPopUpButton>,
    graph: Retained<Graph>,
    scale: [Retained<NSTextField>; 3],
    bands: Vec<Retained<NSSlider>>,
    values: Vec<Retained<NSTextField>>,
    boost: Retained<NSSlider>,
    boost_value: Retained<NSTextField>,
    filters: Retained<NSTextView>,
    note: Retained<NSTextField>,
    _target: Retained<Target>,
    _observer: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

/// The settings being edited, and which profile ("" for every device).
struct Edit {
    draft: Sound,
    scope: String,
    /// The device popup's names after "All devices".
    devices: Vec<String>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    static EDIT: RefCell<Option<Edit>> = const { RefCell::new(None) };
    /// Bumped by every change; a save runs only if no change came after it.
    static CHANGES: Cell<u64> = const { Cell::new(0) };
}

define_class!(
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[name = "LivaeEqGraph"]
    struct Graph;

    impl Graph {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let size = self.bounds().size;
            let Some((curve, top)) = EDIT.with(|e| e.borrow().as_ref().map(|e| curve(&e.draft, &e.scope))) else { return };
            let x_of = |f: f64| (f / 20.0).log10() / 3.0 * size.width;
            let y_of = |db: f64| size.height / 2.0 + db / top * (size.height / 2.0 - 6.0);
            let line = |path: &NSBezierPath, color: &NSColor, width: f64| {
                path.setLineWidth(width);
                color.setStroke();
                path.stroke();
            };
            // The grid: 100 Hz, 1 kHz, 10 kHz across; 0 dB and the scale's halves up and down.
            let grid = NSBezierPath::bezierPath();
            for f in [100.0, 1000.0, 10000.0] {
                grid.moveToPoint(NSPoint::new(x_of(f).round() + 0.5, 0.0));
                grid.lineToPoint(NSPoint::new(x_of(f).round() + 0.5, size.height));
            }
            for db in [-top / 2.0, top / 2.0] {
                grid.moveToPoint(NSPoint::new(0.0, y_of(db).round() + 0.5));
                grid.lineToPoint(NSPoint::new(size.width, y_of(db).round() + 0.5));
            }
            line(&grid, &ui::white(0.08), 1.0);
            let zero = NSBezierPath::bezierPath();
            zero.moveToPoint(NSPoint::new(0.0, y_of(0.0).round() + 0.5));
            zero.lineToPoint(NSPoint::new(size.width, y_of(0.0).round() + 0.5));
            line(&zero, &ui::white(0.22), 1.0);
            let path = NSBezierPath::bezierPath();
            for (i, (f, db)) in curve.iter().enumerate() {
                let p = NSPoint::new(x_of(*f), y_of(*db));
                if i == 0 { path.moveToPoint(p) } else { path.lineToPoint(p) }
            }
            line(&path, &ui::rgb(TEXT), 2.0);
        }
    }
);

impl Graph {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

/// The curve `scope` plays through (every device's profile, then its own), 20 Hz to 20 kHz, and the dB scale that
/// holds it.
fn curve(draft: &Sound, scope: &str) -> (Vec<(f64, f64)>, f64) {
    let design = Design::of(&draft.chain(scope)).unwrap_or_default();
    let points: Vec<(f64, f64)> = (0..=240).map(|i| 20.0 * 1000f64.powf(i as f64 / 240.0)).map(|f| (f, design.db_at(f, RATE))).collect();
    let peak = points.iter().fold(0f64, |m, (_, db)| m.max(db.abs()));
    (points, SCALE_DB.max(((peak + 1.0) / 6.0).ceil() * 6.0))
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "LivaeEqTarget"]
    struct Target;

    unsafe impl NSObjectProtocol for Target {}

    unsafe impl NSWindowDelegate for Target {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _n: &NSNotification) {
            save_now();
        }
    }

    impl Target {
        #[unsafe(method(deviceChanged:))]
        fn device_changed(&self, sender: Option<&AnyObject>) {
            let Some(i) = sender.and_then(|s| s.downcast_ref::<NSPopUpButton>()).map(|p| p.indexOfSelectedItem()) else { return };
            edit(|e| {
                e.scope = if i <= 0 { String::new() } else { e.devices.get(i as usize - 1).cloned().unwrap_or_default() };
            });
            show_scope();
        }

        #[unsafe(method(bandChanged:))]
        fn band_changed(&self, sender: Option<&AnyObject>) {
            let Some(s) = sender.and_then(|s| s.downcast_ref::<NSSlider>()) else { return };
            let (band, db) = (s.tag() as usize, (s.doubleValue() * 10.0).round() / 10.0);
            edit(|e| {
                let scope = e.scope.clone();
                e.draft.profile_mut(&scope).bands[band.min(9)] = db;
            });
            changed();
        }

        #[unsafe(method(boostChanged:))]
        fn boost_changed(&self, sender: Option<&AnyObject>) {
            let Some(s) = sender.and_then(|s| s.downcast_ref::<NSSlider>()) else { return };
            let db = (s.doubleValue() * 2.0).round() / 2.0;
            edit(|e| {
                let scope = e.scope.clone();
                e.draft.profile_mut(&scope).boost = db;
            });
            changed();
        }

        /// An AutoEq or Equalizer APO "ParametricEQ.txt": its filters replace the ones shown.
        #[unsafe(method(importFilters:))]
        fn import_filters(&self, _s: Option<&AnyObject>) {
            let mtm = MainThreadMarker::new().unwrap();
            let panel = NSOpenPanel::openPanel(mtm);
            panel.setMessage(Some(ns_string!("Choose a ParametricEQ.txt from AutoEq or Equalizer APO")));
            if panel.runModal() != NSModalResponseOK {
                return;
            }
            let Some(text) = panel.URL().and_then(|u| u.path()).and_then(|p| std::fs::read_to_string(p.to_string()).ok()) else { return };
            UI.with(|u| {
                if let Some(ui) = u.borrow().as_ref() {
                    ui.filters.setString(&NSString::from_str(&text));
                }
            });
            filters_typed();
        }

        /// The chosen profile back to flat (nothing changed on that output).
        #[unsafe(method(resetProfile:))]
        fn reset_profile(&self, _s: Option<&AnyObject>) {
            edit(|e| {
                let scope = e.scope.clone();
                e.draft.profiles.retain(|p| p.device != scope);
            });
            show_scope();
            changed();
        }
    }
);

fn edit(f: impl FnOnce(&mut Edit)) {
    EDIT.with(|e| {
        if let Some(e) = e.borrow_mut().as_mut() {
            f(e);
        }
    });
}

fn draft() -> Option<(Sound, String)> {
    EDIT.with(|e| e.borrow().as_ref().map(|e| (e.draft.clone(), e.scope.clone())))
}

/// The text in the filters box changed: keep it when it reads, say why when not.
fn filters_typed() {
    let Some(text) = UI.with(|u| u.borrow().as_ref().map(|ui| ui.filters.string().to_string())) else { return };
    edit(|e| {
        let scope = e.scope.clone();
        e.draft.profile_mut(&scope).filters = text;
    });
    changed();
}

/// Something was edited: show it, hear it, save it shortly.
fn changed() {
    paint();
    if let Some((sound, _)) = draft() {
        crate::sound::preview(&sound);
    }
    let n = CHANGES.with(|c| {
        c.set(c.get() + 1);
        c.get()
    });
    let _ = dispatch2::DispatchQueue::main().after(dispatch2::DispatchTime::NOW.time(500_000_000), move || {
        if CHANGES.with(|c| c.get()) == n {
            save_now();
        }
    });
}

fn save_now() {
    if let Some((sound, _)) = draft() {
        config::update(|s| s.sound = sound);
    }
}

/// Values, curve and note for what is in the draft.
fn paint() {
    let Some((sound, scope)) = draft() else { return };
    UI.with(|u| {
        let Some(ui) = u.borrow().as_ref().map(|ui| (ui.values.clone(), ui.boost_value.clone(), ui.note.clone(), ui.graph.clone(), ui.scale.clone())) else { return };
        let (values, boost_value, note, graph, scale) = ui;
        let own = sound.profiles.iter().find(|p| p.device == scope).cloned().unwrap_or_default();
        for (label, db) in values.iter().zip(own.bands) {
            label.setStringValue(&NSString::from_str(&signed(db)));
        }
        boost_value.setStringValue(&NSString::from_str(&format!("{} dB", signed(own.boost))));
        let (_, top) = curve(&sound, &scope);
        for (label, db) in scale.iter().zip([top / 2.0, 0.0, -top / 2.0]) {
            label.setStringValue(&NSString::from_str(&if db == 0.0 { "0 dB".to_string() } else if db > 0.0 { format!("+{db:.0}") } else { format!("−{:.0}", -db) }));
        }
        graph.setNeedsDisplay(true);
        let line = match livae_core::eq::parse(&own.filters) {
            Err(e) => e,
            Ok(p) => {
                let peak = Design::of(&sound.chain(&scope)).map(|d| d.peak_db(RATE)).unwrap_or(0.0);
                let filters = match p.filters.len() {
                    0 => "No filters".to_string(),
                    1 => "1 filter".to_string(),
                    n => format!("{n} filters"),
                };
                let preamp = if p.preamp != 0.0 { format!(", preamp {} dB", signed(p.preamp)) } else { String::new() };
                let limit = if peak > 0.0 { ". The limiter keeps the peaks at 0 dB." } else { "" };
                let status = if sound.eq { String::new() } else { " The equaliser is off in Settings.".into() };
                format!("{filters}{preamp}{limit}{status}")
            }
        };
        note.setStringValue(&NSString::from_str(&line));
    });
}

fn signed(db: f64) -> String {
    if db > 0.0 { format!("+{db:.1}") } else if db < 0.0 { format!("−{:.1}", -db) } else { "0".into() }
}

/// Load the chosen profile into the controls.
fn show_scope() {
    let Some((sound, scope)) = draft() else { return };
    let own = sound.profiles.iter().find(|p| p.device == scope).cloned().unwrap_or_default();
    UI.with(|u| {
        if let Some(ui) = u.borrow().as_ref() {
            for (s, db) in ui.bands.iter().zip(own.bands) {
                s.setDoubleValue(db);
            }
            ui.boost.setDoubleValue(own.boost);
            if ui.filters.string().to_string() != own.filters {
                ui.filters.setString(&NSString::from_str(&own.filters));
            }
        }
    });
    paint();
}

/// The device popup: every device, the outputs connected now and any output that has a profile.
fn fill_devices() {
    let mut names: Vec<String> = crate::sound::devices().into_iter().filter(|d| d.output).map(|d| d.name).collect();
    if let Some((sound, _)) = draft() {
        names.extend(sound.profiles.iter().filter(|p| !p.device.is_empty()).map(|p| p.device.clone()));
    }
    names.sort();
    names.dedup();
    let scope = draft().map(|(_, s)| s).unwrap_or_default();
    UI.with(|u| {
        if let Some(ui) = u.borrow().as_ref() {
            ui.device.removeAllItems();
            ui.device.addItemWithTitle(ns_string!("All devices"));
            for n in &names {
                ui.device.addItemWithTitle(&NSString::from_str(n));
            }
            let at = names.iter().position(|n| *n == scope).map_or(0, |i| i + 1);
            ui.device.selectItemAtIndex(at as isize);
        }
    });
    edit(|e| e.devices = names);
}

/// Settings changed outside the window (the file, or Settings): show them, unless an edit is still being saved.
pub fn reload() {
    if UI.with(|u| u.borrow().is_none()) {
        return;
    }
    let fresh = config::get().sound;
    let mut differs = false;
    edit(|e| {
        if e.draft != fresh {
            e.draft = fresh;
            differs = true;
        }
    });
    if differs {
        fill_devices();
        show_scope();
    } else {
        paint();
    }
}

pub fn show() {
    let mtm = MainThreadMarker::new().unwrap();
    // The output in use first: that is what one usually tunes.
    let in_use = crate::sound::default_device(true).and_then(|id| crate::sound::devices().into_iter().find(|d| d.id == id)).map(|d| d.name);
    let sound = config::get().sound;
    let scope = in_use.filter(|n| sound.profiles.iter().any(|p| p.device == *n) || !sound.profiles.iter().any(|p| p.device.is_empty())).unwrap_or_default();
    EDIT.with(|e| *e.borrow_mut() = Some(Edit { draft: sound, scope, devices: Vec::new() }));
    if UI.with(|u| u.borrow().is_none()) {
        build(mtm);
    }
    fill_devices();
    show_scope();
    UI.with(|u| {
        if let Some(ui) = u.borrow().as_ref() {
            ui.window.center();
            ui::present(&ui.window);
            crate::motion::show(&ui.window, crate::motion::Kind::Bloom);
        }
    });
    if !crate::app::offscreen() {
        #[allow(deprecated)]
        NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    }
}

pub fn set_glass(darkness: f64) {
    UI.with(|u| {
        if let Some(ui) = u.borrow().as_ref() {
            ui.glass.set_darkness(darkness.max(0.85));
        }
    });
}

fn text(mtm: MainThreadMarker, s: &str, size: f64, color: [u8; 3]) -> Retained<NSTextField> {
    let t = ui::label(mtm, size, color);
    t.setStringValue(&NSString::from_str(s));
    t
}

fn build(mtm: MainThreadMarker) {
    let s = config::get();
    let this = Target::alloc(mtm).set_ivars(());
    let target: Retained<Target> = unsafe { msg_send![super(this), init] };
    let tgt = Some(&*target as &AnyObject);
    let bands_h = 16.0 + 6.0 + SLIDER_H + 6.0 + 16.0;
    let h = TITLEBAR + 28.0 + 20.0 + GRAPH_H + 28.0 + bands_h + 20.0 + 32.0 + 20.0 + 28.0 + 8.0 + FILTERS_H + 8.0 + 36.0 + PAD;
    let window = ui::titled_window(mtm, NSSize::new(W, h));
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(ns_string!("Livae Equaliser"));
    window.setTitlebarAppearsTransparent(true);
    window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    window.setOpaque(false);
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    window.setAppearance(NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }).as_deref());
    window.setMovableByWindowBackground(true);
    let glass = Glass::new(mtm, 0.0, s.glass.max(0.85));
    window.setContentView(Some(&glass.root));
    let v: &NSView = &glass.content;
    let place = |view: &NSView, x: f64, y: f64, w: f64, h: f64| {
        view.setFrame(NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)));
        v.addSubview(view);
    };

    let mut y = h - TITLEBAR - 28.0;
    let title = text(mtm, "Equaliser", 20.0, TEXT);
    title.setFont(Some(&crate::font::sans(20.0, unsafe { objc2_app_kit::NSFontWeightMedium })));
    place(&title, PAD, y, 200.0, 26.0);
    let device = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
    unsafe {
        device.setTarget(tgt);
        device.setAction(Some(sel!(deviceChanged:)));
    }
    place(&device, W - PAD - 260.0, y - 2.0, 260.0, 28.0);

    // The curve, with its scale on the left and 100 Hz, 1 kHz, 10 kHz below.
    y -= 20.0 + GRAPH_H;
    let gx = PAD + 44.0;
    let gw = W - PAD - gx;
    let graph = Graph::new(mtm, NSRect::new(NSPoint::new(gx, y), NSSize::new(gw, GRAPH_H)));
    v.addSubview(&graph);
    // Beside the grid lines the graph draws: half the scale up, 0 dB, half down.
    let half = (GRAPH_H / 2.0 - 6.0) / 2.0;
    let scale = [half, 0.0, -half].map(|dy| {
        let t = text(mtm, "", 12.0, TERTIARY);
        t.setFont(Some(&crate::font::digits(12.0, unsafe { NSFontWeightRegular })));
        t.setAlignment(NSTextAlignment::Right);
        place(&t, PAD - 8.0, y + GRAPH_H / 2.0 + dy - 8.0, 44.0, 16.0);
        t
    });
    for (f, name) in [(100.0, "100"), (1000.0, "1k"), (10000.0, "10k")] {
        let t = text(mtm, name, 12.0, TERTIARY);
        t.setFont(Some(&crate::font::digits(12.0, unsafe { NSFontWeightRegular })));
        let x = gx + (f / 20.0f64).log10() / 3.0 * gw;
        place(&t, x + 4.0, y - 18.0, 40.0, 16.0);
    }

    // The 10-band EQ: the value above each slider, the band below.
    y -= 28.0 + bands_h;
    let step = (W - 2.0 * PAD) / 10.0;
    let mut bands = Vec::new();
    let mut values = Vec::new();
    for (i, name) in BAND_NAMES.iter().enumerate() {
        let cx = PAD + step * (i as f64 + 0.5);
        let value = text(mtm, "0", 12.0, SECONDARY);
        value.setFont(Some(&crate::font::digits(12.0, unsafe { NSFontWeightRegular })));
        value.setAlignment(NSTextAlignment::Center);
        place(&value, cx - step / 2.0, y + bands_h - 16.0, step, 16.0);
        let slider = unsafe { NSSlider::sliderWithValue_minValue_maxValue_target_action(0.0, -BAND_RANGE, BAND_RANGE, tgt, Some(sel!(bandChanged:)), mtm) };
        slider.setVertical(true);
        slider.setContinuous(true);
        slider.setTag(i as isize);
        place(&slider, cx - 12.0, y + 22.0, 24.0, SLIDER_H);
        let label = text(mtm, name, 12.0, TERTIARY);
        label.setFont(Some(&crate::font::digits(12.0, unsafe { NSFontWeightRegular })));
        label.setAlignment(NSTextAlignment::Center);
        place(&label, cx - step / 2.0, y, step, 16.0);
        bands.push(slider);
        values.push(value);
    }
    debug_assert_eq!(BANDS.len(), BAND_NAMES.len());

    // The boost, after the equaliser; the limiter keeps it from clipping.
    y -= 20.0 + 32.0;
    let boost_label = text(mtm, "Boost", 16.0, TEXT);
    place(&boost_label, PAD, y + 6.0, 120.0, 22.0);
    let boost = unsafe { NSSlider::sliderWithValue_minValue_maxValue_target_action(0.0, 0.0, BOOST_MAX, tgt, Some(sel!(boostChanged:)), mtm) };
    boost.setContinuous(true);
    place(&boost, PAD + 120.0, y + 4.0, W - 2.0 * PAD - 120.0 - 80.0, 24.0);
    let boost_value = text(mtm, "0 dB", 14.0, SECONDARY);
    boost_value.setFont(Some(&crate::font::digits(14.0, unsafe { NSFontWeightRegular })));
    boost_value.setAlignment(NSTextAlignment::Right);
    place(&boost_value, W - PAD - 72.0, y + 7.0, 72.0, 18.0);

    // Parametric filters as text, with the AutoEq import.
    y -= 20.0 + 28.0;
    let filters_label = text(mtm, "Filters", 16.0, TEXT);
    place(&filters_label, PAD, y + 3.0, 120.0, 22.0);
    let import = unsafe { NSButton::buttonWithTitle_target_action(ns_string!("Import…"), tgt, Some(sel!(importFilters:)), mtm) };
    place(&import, W - PAD - 100.0, y, 100.0, 28.0);
    y -= 8.0 + FILTERS_H;
    // A faint field, so it reads as text one can edit.
    let scroll: Retained<NSScrollView> = NSTextView::scrollableTextView(mtm);
    scroll.setDrawsBackground(true);
    scroll.setBackgroundColor(&ui::white(0.05));
    scroll.setWantsLayer(true);
    if let Some(layer) = scroll.layer() {
        layer.setCornerRadius(8.0);
        layer.setMasksToBounds(true);
    }
    scroll.setFrame(NSRect::new(NSPoint::new(PAD, y), NSSize::new(W - 2.0 * PAD, FILTERS_H)));
    v.addSubview(&scroll);
    let filters: Retained<NSTextView> = scroll.documentView().and_then(|d| d.downcast::<NSTextView>().ok()).expect("a scrollable text view holds a text view");
    filters.setDrawsBackground(false);
    filters.setTextContainerInset(NSSize::new(8.0, 8.0));
    filters.setRichText(false);
    filters.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(12.0, unsafe { NSFontWeightRegular })));
    filters.setTextColor(Some(&ui::rgb(TEXT)));
    filters.setInsertionPointColor(Some(&ui::rgb(TEXT)));
    filters.setAutomaticQuoteSubstitutionEnabled(false);
    filters.setAutomaticDashSubstitutionEnabled(false);
    filters.setAutomaticTextReplacementEnabled(false);
    filters.setAutomaticSpellingCorrectionEnabled(false);
    let typed = RcBlock::new(|_n: NonNull<NSNotification>| filters_typed());
    let observer = unsafe { NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(Some(NSTextDidChangeNotification), Some(&filters), None, &typed) };

    // What the profile does, or why its filters do not read; the reset beside it.
    y -= 8.0 + 36.0;
    let note = text(mtm, "", 14.0, SECONDARY);
    note.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByWordWrapping);
    note.setMaximumNumberOfLines(2);
    place(&note, PAD, y, W - 2.0 * PAD - 110.0, 36.0);
    let reset = unsafe { NSButton::buttonWithTitle_target_action(ns_string!("Reset"), tgt, Some(sel!(resetProfile:)), mtm) };
    place(&reset, W - PAD - 100.0, y + 6.0, 100.0, 28.0);

    window.setDelegate(Some(ProtocolObject::from_ref(&*target)));
    UI.with(|u| {
        *u.borrow_mut() = Some(Ui { window, glass, device, graph, scale, bands, values, boost, boost_value, filters, note, _target: target, _observer: observer })
    });
}
