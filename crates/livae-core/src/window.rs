//! Window placement (halves, thirds, quarters, centre, next display) as plain geometry, the same on both systems.
//! Each system reads the front window and the screen's usable area, asks `place`, and moves the window.

/// A rectangle with its origin at the top left and y growing down (both systems' window coordinates).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    fn centre(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    fn contains(&self, (x, y): (f64, f64)) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    /// Whole pixels, so a window placed twice lands exactly where it was.
    fn rounded(self) -> Rect {
        Rect::new(self.x.round(), self.y.round(), self.w.round(), self.h.round())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    LeftHalf,
    RightHalf,
    TopHalf,
    BottomHalf,
    Maximize,
    AlmostMaximize,
    Center,
    LeftThird,
    CenterThird,
    RightThird,
    LeftTwoThirds,
    RightTwoThirds,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    /// The same place on the next screen, scaled to its size.
    NextDisplay,
    /// Back to where the window was before Livae first moved it.
    Restore,
}

/// Where `action` puts a window that is now at `window` on a screen whose usable area is `area`. None for
/// `NextDisplay` and `Restore`, which need more than one screen's area.
pub fn place(action: Action, area: Rect, window: Rect) -> Option<Rect> {
    let Rect { x, y, w, h } = area;
    let r = match action {
        Action::LeftHalf => Rect::new(x, y, w / 2.0, h),
        Action::RightHalf => Rect::new(x + w / 2.0, y, w / 2.0, h),
        Action::TopHalf => Rect::new(x, y, w, h / 2.0),
        Action::BottomHalf => Rect::new(x, y + h / 2.0, w, h / 2.0),
        Action::Maximize => area,
        Action::AlmostMaximize => Rect::new(x + w * 0.05, y + h * 0.05, w * 0.9, h * 0.9),
        Action::Center => {
            let (cw, ch) = (window.w.min(w), window.h.min(h));
            Rect::new(x + (w - cw) / 2.0, y + (h - ch) / 2.0, cw, ch)
        }
        Action::LeftThird => Rect::new(x, y, w / 3.0, h),
        Action::CenterThird => Rect::new(x + w / 3.0, y, w / 3.0, h),
        Action::RightThird => Rect::new(x + w * 2.0 / 3.0, y, w / 3.0, h),
        Action::LeftTwoThirds => Rect::new(x, y, w * 2.0 / 3.0, h),
        Action::RightTwoThirds => Rect::new(x + w / 3.0, y, w * 2.0 / 3.0, h),
        Action::TopLeft => Rect::new(x, y, w / 2.0, h / 2.0),
        Action::TopRight => Rect::new(x + w / 2.0, y, w / 2.0, h / 2.0),
        Action::BottomLeft => Rect::new(x, y + h / 2.0, w / 2.0, h / 2.0),
        Action::BottomRight => Rect::new(x + w / 2.0, y + h / 2.0, w / 2.0, h / 2.0),
        Action::NextDisplay | Action::Restore => return None,
    };
    Some(r.rounded())
}

/// The screen (index into `areas`) a window is on: the one holding its centre, else the nearest one.
pub fn screen_of(window: Rect, areas: &[Rect]) -> Option<usize> {
    let c = window.centre();
    areas.iter().position(|a| a.contains(c)).or_else(|| {
        let dist = |a: &Rect| {
            let (ax, ay) = a.centre();
            (ax - c.0).powi(2) + (ay - c.1).powi(2)
        };
        (0..areas.len()).min_by(|&i, &j| dist(&areas[i]).total_cmp(&dist(&areas[j])))
    })
}

/// The window moved to the next screen in `areas` (in their order, wrapping), keeping its place and size relative to
/// the screen. None with one screen.
pub fn next_display(window: Rect, areas: &[Rect]) -> Option<Rect> {
    if areas.len() < 2 {
        return None;
    }
    let from = areas[screen_of(window, areas)?];
    let to = areas[(screen_of(window, areas)? + 1) % areas.len()];
    let (sx, sy) = (to.w / from.w, to.h / from.h);
    let r = Rect::new(to.x + (window.x - from.x) * sx, to.y + (window.y - from.y) * sy, (window.w * sx).min(to.w), (window.h * sy).min(to.h));
    // Keep it on the new screen even when it hung over the old one's edge.
    let x = r.x.clamp(to.x, to.x + to.w - r.w);
    let y = r.y.clamp(to.y, to.y + to.h - r.h);
    Some(Rect::new(x, y, r.w, r.h).rounded())
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect::new(0.0, 25.0, 1440.0, 875.0);

    #[test]
    fn halves_thirds_quarters_fill_the_area() {
        let win = Rect::new(100.0, 100.0, 800.0, 600.0);
        assert_eq!(place(Action::LeftHalf, AREA, win), Some(Rect::new(0.0, 25.0, 720.0, 875.0)));
        assert_eq!(place(Action::RightHalf, AREA, win), Some(Rect::new(720.0, 25.0, 720.0, 875.0)));
        assert_eq!(place(Action::BottomHalf, AREA, win), Some(Rect::new(0.0, 463.0, 1440.0, 438.0)));
        assert_eq!(place(Action::RightThird, AREA, win), Some(Rect::new(960.0, 25.0, 480.0, 875.0)));
        assert_eq!(place(Action::LeftTwoThirds, AREA, win), Some(Rect::new(0.0, 25.0, 960.0, 875.0)));
        assert_eq!(place(Action::BottomRight, AREA, win), Some(Rect::new(720.0, 463.0, 720.0, 438.0)));
        assert_eq!(place(Action::Maximize, AREA, win), Some(AREA));
        assert_eq!(place(Action::Restore, AREA, win), None);
    }

    #[test]
    fn centre_keeps_the_size_that_fits() {
        let win = Rect::new(0.0, 0.0, 800.0, 2000.0);
        assert_eq!(place(Action::Center, AREA, win), Some(Rect::new(320.0, 25.0, 800.0, 875.0)));
    }

    #[test]
    fn next_display_keeps_the_relative_place_and_wraps() {
        let screens = [AREA, Rect::new(1440.0, 0.0, 2560.0, 1440.0)];
        let left_half = Rect::new(0.0, 25.0, 720.0, 875.0);
        assert_eq!(next_display(left_half, &screens), Some(Rect::new(1440.0, 0.0, 1280.0, 1440.0)));
        assert_eq!(next_display(Rect::new(1500.0, 10.0, 600.0, 400.0), &screens).map(|r| r.x), Some(34.0));
        assert_eq!(next_display(left_half, &screens[..1]), None);
        // Off every screen: the nearest one counts as its screen.
        assert_eq!(screen_of(Rect::new(-900.0, 0.0, 100.0, 100.0), &screens), Some(0));
    }
}
