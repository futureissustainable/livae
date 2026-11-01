//! Livae Sans, the app's own typeface (SIL Open Font License; the bundle carries it in Resources/Fonts and
//! Info.plist's ATSApplicationFontsPath registers it at launch). Outside the bundle (cargo run) the system font
//! stands in, so nothing breaks.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSFont, NSFontDescriptor, NSFontFeatureSelectorIdentifierKey, NSFontFeatureSettingsAttribute, NSFontFeatureTypeIdentifierKey,
    NSFontWeight, NSFontWeightBold, NSFontWeightMedium, NSFontWeightSemibold,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};

/// The face for an AppKit weight: Regular, Medium, SemiBold or Bold.
fn face(weight: NSFontWeight) -> &'static str {
    let (medium, semibold, bold) = unsafe { (NSFontWeightMedium, NSFontWeightSemibold, NSFontWeightBold) };
    if weight >= bold {
        "LivaeSans-Bold"
    } else if weight >= semibold {
        "LivaeSans-SemiBold"
    } else if weight >= medium {
        "LivaeSans-Medium"
    } else {
        "LivaeSans-Regular"
    }
}

/// Livae Sans at `size` and `weight` (an AppKit weight such as `NSFontWeightMedium`; 0.0 is Regular).
pub fn sans(size: f64, weight: NSFontWeight) -> Retained<NSFont> {
    NSFont::fontWithName_size(&NSString::from_str(face(weight)), size).unwrap_or_else(|| NSFont::systemFontOfSize_weight(size, weight))
}

/// Livae Sans Regular.
pub fn regular(size: f64) -> Retained<NSFont> {
    sans(size, 0.0)
}

/// Livae Sans with figures of one width (its tabular figures), for times and counters that change in place.
pub fn digits(size: f64, weight: NSFontWeight) -> Retained<NSFont> {
    let Some(base) = NSFont::fontWithName_size(&NSString::from_str(face(weight)), size) else {
        return NSFont::monospacedDigitSystemFontOfSize_weight(size, weight);
    };
    unsafe {
        // Number spacing (type 6), monospaced numbers (selector 0): CoreText turns it into the font's `tnum`.
        let kind = NSNumber::new_isize(6);
        let selector = NSNumber::new_isize(0);
        let keys: [&NSString; 2] = [NSFontFeatureTypeIdentifierKey, NSFontFeatureSelectorIdentifierKey];
        let values: [&AnyObject; 2] = [kind.as_ref(), selector.as_ref()];
        let feature = NSDictionary::<NSString, AnyObject>::from_slices(&keys, &values);
        let features = NSArray::from_retained_slice(&[feature]);
        let attrs = NSDictionary::<NSString, AnyObject>::from_slices(&[NSFontFeatureSettingsAttribute], &[features.as_ref()]);
        let desc: Retained<NSFontDescriptor> = base.fontDescriptor().fontDescriptorByAddingAttributes(&attrs);
        NSFont::fontWithDescriptor_size(&desc, size).unwrap_or(base)
    }
}
