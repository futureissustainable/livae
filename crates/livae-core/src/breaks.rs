//! Break reminders with the Stretchly Zen rules (Stretchly 1.20.0 `breaksPlanner.js` and the Zen overrides):
//! a mini break after every stretch of work, a long break in place of every Nth one, a notice before each,
//! natural breaks (time away starts the timer over), weekends off, pauses, and the quotes shown during a break.
//!
//! Platform-free. The app calls `Scheduler::tick` once a second and does what the returned `Action` says.

use chrono::{Datelike, Duration, Local, NaiveDate, Timelike, Weekday};

pub use chrono::NaiveDateTime;

/// "Until tomorrow morning" resumes at this hour (Stretchly's `morningHour`).
pub const MORNING_HOUR: u32 = 6;
/// Skips allowed per day for breaks that can be skipped (Stretchly Zen's limit).
pub const SKIPS_PER_DAY: u32 = 3;
/// A longer silence between two ticks means the computer slept: timers stop while it sleeps.
const GAP: f64 = 10.0;

/// The schedule, in seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct Rules {
    pub enabled: bool,
    /// Work between two breaks.
    pub every: f64,
    pub mini_seconds: f64,
    /// A mini break cannot be skipped.
    pub mini_strict: bool,
    pub long_enabled: bool,
    /// Every Nth break is a long one.
    pub long_every: u32,
    pub long_seconds: f64,
    /// A long break can be put off once, by this long; 0 = never.
    pub long_postpone: f64,
    pub weekends_off: bool,
    /// Away this long counts as a break and starts the timer over; 0 = off.
    pub natural_reset: f64,
    /// Warning before a break; 0 = none.
    pub notice: f64,
    /// How far "Delay" in the warning moves the coming break.
    pub delay: f64,
}

impl Default for Rules {
    /// Stretchly's rhythm: 20 s every 20 min.
    fn default() -> Self {
        Self {
            enabled: true,
            every: 20.0 * 60.0,
            mini_seconds: 20.0,
            mini_strict: true,
            long_enabled: false,
            long_every: 2,
            long_seconds: 5.0 * 60.0,
            long_postpone: 5.0 * 60.0,
            weekends_off: true,
            natural_reset: 5.0 * 60.0,
            notice: 10.0,
            delay: 30.0 * 60.0,
        }
    }
}

/// What the app sees once a second.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tick {
    /// Wall-clock seconds (Unix time). Unlike a monotonic clock it keeps running while the computer sleeps.
    pub now: f64,
    /// Local time, for weekends and "until tomorrow morning".
    pub local: NaiveDateTime,
    /// Seconds since the last keyboard, mouse or trackpad input.
    pub idle: f64,
    /// Screen locked, displays asleep or the computer asleep.
    pub away: bool,
    /// Focus or Do Not Disturb is on.
    pub focus: bool,
    /// Another break app (Stretchly Zen) is running; Livae stays silent.
    pub other_app: bool,
}

/// The current time as a tick needs it: Unix seconds and local time.
pub fn clock() -> (f64, NaiveDateTime) {
    let now = Local::now();
    (now.timestamp_millis() as f64 / 1000.0, now.naive_local())
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Break {
    pub long: bool,
    pub started: f64,
    pub seconds: f64,
    /// Cannot be skipped.
    pub strict: bool,
}

impl Break {
    pub fn left(&self, now: f64) -> f64 {
        (self.started + self.seconds - now).max(0.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    None,
    /// Show or update "Break in N s".
    Notice { seconds: u32, long: bool },
    /// Open the break screen.
    Start(Break),
    /// Close the break screen.
    End,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Status {
    Off,
    OtherApp,
    Paused(NaiveDateTime),
    Away,
    Weekend,
    Focus,
    Next { seconds: f64, long: bool },
    OnBreak { long: bool, left: f64 },
}

impl Status {
    /// One line for the menu: "Next break in 12 min", "Paused until 06:00".
    pub fn line(&self) -> String {
        match *self {
            Status::Off => "Breaks are off".into(),
            Status::OtherApp => "Stretchly is running".into(),
            Status::Paused(until) => format!("Paused until {}", until.format("%H:%M")),
            Status::Away => "Paused while away".into(),
            Status::Weekend => "Off for the weekend".into(),
            Status::Focus => "Waiting for Focus to end".into(),
            Status::Next { seconds, long } => {
                let name = if long { "Long break" } else { "Next break" };
                if seconds < 60.0 {
                    format!("{name} in {} s", seconds.ceil().max(1.0) as u64)
                } else {
                    format!("{name} in {} min", (seconds / 60.0).ceil() as u64)
                }
            }
            Status::OnBreak { long, .. } => if long { "On a long break" } else { "On a break" }.into(),
        }
    }
}

/// Seconds left in a break: "18 s", "4:32".
pub fn countdown(seconds: f64) -> String {
    let s = seconds.max(0.0).ceil() as u64;
    if s < 60 { format!("{s} s") } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// Time left in a long break: "4:32", "0:45".
pub fn minutes_seconds(seconds: f64) -> String {
    let s = seconds.max(0.0).ceil() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// The next `MORNING_HOUR`:00 after `local`.
pub fn next_morning(local: NaiveDateTime) -> NaiveDateTime {
    let day = if local.hour() >= MORNING_HOUR { local.date() + Duration::days(1) } else { local.date() };
    day.and_hms_opt(MORNING_HOUR, 0, 0).unwrap_or(local)
}

#[derive(Debug, Default)]
pub struct Scheduler {
    /// Seconds worked since the last break.
    worked: f64,
    /// Mini breaks since the last long one.
    minis: u32,
    /// The coming long break was put off: it comes after `long_postpone` and cannot be put off again.
    postponed: bool,
    running: Option<Break>,
    paused_until: Option<NaiveDateTime>,
    /// Seconds away (locked, asleep) in the current stretch.
    away_for: f64,
    last: Option<Tick>,
    /// Skips used, and on which day.
    skips: Option<(NaiveDate, u32)>,
}

impl Scheduler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn running(&self) -> Option<Break> {
        self.running
    }

    pub fn tick(&mut self, r: &Rules, t: Tick) -> Action {
        let step = self.last.map_or(0.0, |l| (t.now - l.now).max(0.0));
        self.last = Some(t);
        let (dt, gap) = if step > GAP { (0.0, step) } else { (step, 0.0) };

        if self.paused_until.is_some_and(|u| t.local >= u) {
            self.paused_until = None;
            self.restart();
        }
        if !r.enabled || t.other_app || self.paused_until.is_some() {
            self.restart();
            return self.close();
        }

        // Locked or asleep: the timer waits; long enough away counts as a break.
        if t.away || gap > 0.0 {
            self.away_for += dt + gap;
            // A break the lock or sleep cut short still counts as taken.
            let ended = self.finish();
            if r.natural_reset > 0.0 && self.away_for >= r.natural_reset {
                self.restart();
            }
            if t.away {
                return ended;
            }
            self.away_for = 0.0;
            if ended != Action::None {
                return ended;
            }
        }
        self.away_for = 0.0;

        if let Some(b) = self.running {
            return if t.now >= b.started + b.seconds { self.finish() } else { Action::None };
        }
        if r.weekends_off && matches!(t.local.weekday(), Weekday::Sat | Weekday::Sun) {
            self.restart();
            return Action::None;
        }
        // A natural break: no input for long enough.
        if r.natural_reset > 0.0 && t.idle >= r.natural_reset {
            self.restart();
            return Action::None;
        }
        // Focus holds the timer; breaks continue where they were once it ends.
        if t.focus {
            return Action::None;
        }

        self.worked += dt;
        let long = self.next_is_long(r);
        let target = self.target(r);
        if self.worked >= target {
            return self.begin(r, t.now, long);
        }
        let left = target - self.worked;
        if r.notice > 0.0 && left <= r.notice {
            return Action::Notice { seconds: left.ceil() as u32, long };
        }
        Action::None
    }

    pub fn status(&self, r: &Rules) -> Status {
        if let Some(b) = self.running {
            return Status::OnBreak { long: b.long, left: self.last.map_or(b.seconds, |t| b.left(t.now)) };
        }
        let Some(t) = self.last else {
            return if r.enabled { Status::Next { seconds: r.every, long: self.next_is_long(r) } } else { Status::Off };
        };
        if !r.enabled {
            Status::Off
        } else if t.other_app {
            Status::OtherApp
        } else if let Some(until) = self.paused_until {
            Status::Paused(until)
        } else if t.away {
            Status::Away
        } else if r.weekends_off && matches!(t.local.weekday(), Weekday::Sat | Weekday::Sun) {
            Status::Weekend
        } else if t.focus {
            Status::Focus
        } else {
            Status::Next { seconds: (self.target(r) - self.worked).max(0.0), long: self.next_is_long(r) }
        }
    }

    /// Start the coming break now; `long` forces its kind. Never while breaks are off or another break app runs.
    pub fn start_now(&mut self, r: &Rules, now: f64, long: Option<bool>) -> Action {
        if !r.enabled || self.running.is_some() || self.last.is_some_and(|t| t.other_app) {
            return Action::None;
        }
        self.paused_until = None;
        let long = long.unwrap_or_else(|| self.next_is_long(r));
        self.begin(r, now, long)
    }

    pub fn skips_left(&self, day: NaiveDate) -> u32 {
        match self.skips {
            Some((d, used)) if d == day => SKIPS_PER_DAY.saturating_sub(used),
            _ => SKIPS_PER_DAY,
        }
    }

    pub fn can_skip(&self, local: NaiveDateTime) -> bool {
        self.running.is_some_and(|b| !b.strict) && self.skips_left(local.date()) > 0
    }

    /// End the running break early, if it is not strict and skips are left today.
    pub fn skip(&mut self, local: NaiveDateTime) -> Action {
        if !self.can_skip(local) {
            return Action::None;
        }
        let day = local.date();
        self.skips = Some((day, SKIPS_PER_DAY - self.skips_left(day) + 1));
        self.finish()
    }

    pub fn can_postpone(&self, r: &Rules) -> bool {
        self.running.is_some_and(|b| b.long) && !self.postponed && r.long_postpone > 0.0
    }

    /// Put the running long break off, once.
    pub fn postpone(&mut self, r: &Rules) -> Action {
        if !self.can_postpone(r) {
            return Action::None;
        }
        self.running = None;
        self.worked = 0.0;
        self.postponed = true;
        Action::End
    }

    /// Move the coming break `delay` of work from now (the warning's Delay). Nothing once a break has begun.
    pub fn delay(&mut self, r: &Rules) {
        if self.running.is_none() {
            self.worked = self.target(r) - r.delay;
        }
    }

    /// Skip the coming break: a running break ends if it may be skipped; otherwise the timer starts over.
    pub fn skip_to_next(&mut self, local: NaiveDateTime) -> Action {
        if self.running.is_some() {
            return self.skip(local);
        }
        self.worked = 0.0;
        self.postponed = false;
        Action::None
    }

    pub fn pause_for(&mut self, local: NaiveDateTime, minutes: u32) -> Action {
        self.pause_until(local + Duration::minutes(minutes as i64))
    }

    pub fn pause_until_morning(&mut self, local: NaiveDateTime) -> Action {
        self.pause_until(next_morning(local))
    }

    fn pause_until(&mut self, until: NaiveDateTime) -> Action {
        self.paused_until = Some(until);
        self.restart();
        self.close()
    }

    pub fn resume(&mut self) {
        self.paused_until = None;
        self.restart();
    }

    fn next_is_long(&self, r: &Rules) -> bool {
        self.postponed || (r.long_enabled && self.minis + 1 >= r.long_every.max(1))
    }

    fn target(&self, r: &Rules) -> f64 {
        if self.postponed { r.long_postpone } else { r.every }
    }

    fn begin(&mut self, r: &Rules, now: f64, long: bool) -> Action {
        let b = Break { long, started: now, seconds: if long { r.long_seconds } else { r.mini_seconds }, strict: !long && r.mini_strict };
        self.running = Some(b);
        Action::Start(b)
    }

    /// The running break is over and counts toward the long-break cycle.
    fn finish(&mut self) -> Action {
        let Some(b) = self.running.take() else { return Action::None };
        if b.long { self.minis = 0 } else { self.minis += 1 }
        self.worked = 0.0;
        self.postponed = false;
        Action::End
    }

    /// Close a running break without counting it.
    fn close(&mut self) -> Action {
        if self.running.take().is_some() { Action::End } else { Action::None }
    }

    /// A fresh timer and a fresh long-break cycle (Stretchly's `clear`).
    fn restart(&mut self) {
        self.worked = 0.0;
        self.minis = 0;
        self.postponed = false;
    }
}

/// One quote: its text with line and paragraph breaks kept, and its author when it has one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quote {
    pub text: String,
    pub author: Option<String>,
}

/// The built-in quotes and break prompts, written to `quotes.md` on first run; edit that file to use your own.
pub const DEFAULT_QUOTES: &str = include_str!("break-quotes.md");

pub fn default_quotes() -> Vec<Quote> {
    parse_quotes(DEFAULT_QUOTES)
}

/// The quotes in `path` (`quotes.md`); the built-in 13 when the file is missing or holds none.
pub fn load_quotes(path: &std::path::Path) -> Vec<Quote> {
    let quotes = std::fs::read_to_string(path).map(|t| parse_quotes(&t)).unwrap_or_default();
    if quotes.is_empty() { default_quotes() } else { quotes }
}

/// Read the `Active Quotes.md` format: quotes separated by `---` lines, Markdown headings ignored. The last
/// paragraph is the author when there is more than one paragraph and it is one short line without closing
/// punctuation ("Thomas Keller, The Bear"); a sentence ending in `.` or `”` stays part of the quote.
pub fn parse_quotes(md: &str) -> Vec<Quote> {
    let md = md.replace("\r\n", "\n");
    let mut out = Vec::new();
    for block in md.split('\n').collect::<Vec<_>>().split(|l| l.trim() == "---") {
        let mut paragraphs: Vec<Vec<&str>> = vec![Vec::new()];
        for line in block.iter().map(|l| l.trim_end()).filter(|l| !is_heading(l)) {
            if line.trim().is_empty() {
                if paragraphs.last().is_some_and(|p| !p.is_empty()) {
                    paragraphs.push(Vec::new());
                }
            } else if let Some(p) = paragraphs.last_mut() {
                p.push(line);
            }
        }
        paragraphs.retain(|p| !p.is_empty());
        if paragraphs.is_empty() {
            continue;
        }
        let author = match paragraphs.last() {
            Some(last) if paragraphs.len() > 1 && is_author(last) => Some(last[0].trim().to_string()),
            _ => None,
        };
        if author.is_some() {
            paragraphs.pop();
        }
        let text = paragraphs.iter().map(|p| p.join("\n")).collect::<Vec<_>>().join("\n\n");
        out.push(Quote { text, author });
    }
    out
}

/// A Markdown heading ("# Active quotes"), not a quote line that starts with "#".
fn is_heading(line: &str) -> bool {
    let rest = line.trim_start_matches('#');
    (1..=6).contains(&(line.len() - rest.len())) && rest.starts_with(' ')
}

fn is_author(paragraph: &[&str]) -> bool {
    let [line] = paragraph else { return false };
    let line = line.trim();
    line.chars().count() <= 60 && !line.ends_with(['.', ',', ';', ':', '!', '?', '…', '"', '”', '’', '\'', ')'])
}

/// Hands out quotes in a shuffled order, each once before any repeats, never the same one twice in a row
/// (Stretchly's `Shuffled`).
#[derive(Debug)]
pub struct Deck {
    left: Vec<usize>,
    size: usize,
    last: Option<usize>,
    seed: u64,
}

impl Deck {
    pub fn new(seed: u64) -> Self {
        Self { left: Vec::new(), size: 0, last: None, seed: if seed == 0 { 0x9e37_79b9_7f4a_7c15 } else { seed } }
    }

    /// Index of the next quote out of `n`; `None` when there are none.
    pub fn next(&mut self, n: usize) -> Option<usize> {
        if n == 0 {
            return None;
        }
        if self.size != n {
            // The file changed: a new round over the new list.
            self.size = n;
            self.left.clear();
            self.last = self.last.filter(|&l| l < n);
        }
        if self.left.is_empty() {
            self.left = (0..n).collect();
        }
        let mut i = (self.random() % self.left.len() as u64) as usize;
        if self.left.len() > 1 && Some(self.left[i]) == self.last {
            i = (i + 1) % self.left.len();
        }
        let q = self.left.swap_remove(i);
        self.last = Some(q);
        Some(q)
    }

    /// xorshift64: enough to shuffle quotes.
    fn random(&mut self) -> u64 {
        let mut x = self.seed;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.seed = x;
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Monday 02.11.2026, 09:00.
    fn monday() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, 2).unwrap().and_hms_opt(9, 0, 0).unwrap()
    }

    struct Sim {
        s: Scheduler,
        r: Rules,
        now: f64,
        local: NaiveDateTime,
        idle: f64,
        away: bool,
        focus: bool,
        other_app: bool,
    }

    impl Sim {
        fn new(r: Rules) -> Self {
            Self { s: Scheduler::new(), r, now: 1_000_000.0, local: monday(), idle: 0.0, away: false, focus: false, other_app: false }
        }

        fn step(&mut self, seconds: f64) -> Action {
            self.now += seconds;
            self.local += Duration::milliseconds((seconds * 1000.0) as i64);
            let t = Tick { now: self.now, local: self.local, idle: self.idle, away: self.away, focus: self.focus, other_app: self.other_app };
            self.s.tick(&self.r, t)
        }

        /// Tick once a second for `seconds`; the actions other than `None`, with the second they came on.
        fn run(&mut self, seconds: u32) -> Vec<(u32, Action)> {
            (1..=seconds).filter_map(|i| Some((i, self.step(1.0))).filter(|(_, a)| *a != Action::None)).collect()
        }

        fn starts(&mut self, seconds: u32) -> Vec<(u32, bool)> {
            self.run(seconds).into_iter().filter_map(|(i, a)| if let Action::Start(b) = a { Some((i, b.long)) } else { None }).collect()
        }

        fn status(&self) -> Status {
            self.s.status(&self.r)
        }
    }

    fn short() -> Rules {
        Rules { every: 60.0, mini_seconds: 5.0, notice: 3.0, natural_reset: 30.0, long_seconds: 10.0, long_postpone: 20.0, ..Rules::default() }
    }

    #[test]
    fn notice_then_strict_mini_break_then_end() {
        let mut sim = Sim::new(short());
        sim.step(0.0);
        let actions = sim.run(66);
        assert_eq!(actions[0], (57, Action::Notice { seconds: 3, long: false }));
        assert_eq!(actions[2], (59, Action::Notice { seconds: 1, long: false }));
        assert!(matches!(actions[3], (60, Action::Start(Break { long: false, strict: true, .. }))));
        assert_eq!(actions[4], (65, Action::End));
        assert_eq!(actions.len(), 5);
    }

    #[test]
    fn strict_break_cannot_be_skipped() {
        let mut sim = Sim::new(short());
        sim.step(0.0);
        sim.run(60);
        assert!(!sim.s.can_skip(sim.local));
        assert_eq!(sim.s.skip(sim.local), Action::None);
        assert_eq!(sim.s.skip_to_next(sim.local), Action::None);
        assert!(sim.s.running().is_some());
    }

    #[test]
    fn every_nth_break_is_long() {
        let mut sim = Sim::new(Rules { long_enabled: true, long_every: 2, ..short() });
        sim.step(0.0);
        // mini at 60, ends 65; long at 125, ends 135; mini at 195.
        assert_eq!(sim.starts(200), vec![(60, false), (125, true), (195, false)]);
    }

    #[test]
    fn skips_stop_after_three_a_day_and_return_the_next_day() {
        let mut sim = Sim::new(Rules { mini_strict: false, ..short() });
        sim.step(0.0);
        for _ in 0..SKIPS_PER_DAY {
            sim.run(60);
            assert_eq!(sim.s.skip(sim.local), Action::End);
        }
        sim.run(60);
        assert!(!sim.s.can_skip(sim.local));
        let tomorrow = sim.local + Duration::days(1);
        assert!(sim.s.can_skip(tomorrow));
    }

    #[test]
    fn delay_from_the_warning_moves_the_break() {
        let mut sim = Sim::new(Rules { delay: 120.0, ..short() });
        sim.step(0.0);
        assert_eq!(sim.run(57).last(), Some(&(57, Action::Notice { seconds: 3, long: false })));
        sim.s.delay(&sim.r);
        assert_eq!(sim.status(), Status::Next { seconds: 120.0, long: false });
        assert_eq!(sim.starts(125), vec![(120, false)]);
    }

    #[test]
    fn long_break_postpones_once() {
        let mut sim = Sim::new(Rules { long_enabled: true, long_every: 1, ..short() });
        sim.step(0.0);
        assert_eq!(sim.starts(60), vec![(60, true)]);
        assert_eq!(sim.s.postpone(&sim.r), Action::End);
        assert_eq!(sim.status(), Status::Next { seconds: 20.0, long: true });
        assert_eq!(sim.starts(20), vec![(20, true)]);
        assert!(!sim.s.can_postpone(&sim.r));
        assert_eq!(sim.s.postpone(&sim.r), Action::None);
    }

    #[test]
    fn idle_past_the_reset_restarts_the_timer_and_the_cycle() {
        let mut sim = Sim::new(Rules { long_enabled: true, long_every: 2, ..short() });
        sim.step(0.0);
        sim.run(65); // one mini break taken
        sim.run(40);
        sim.idle = 30.0;
        sim.step(1.0);
        sim.idle = 0.0;
        // A full interval again, and the next break is a mini one: the cycle started over.
        assert_eq!(sim.status(), Status::Next { seconds: 60.0, long: false });
    }

    #[test]
    fn weekends_are_off() {
        let mut sim = Sim::new(short());
        sim.local = NaiveDate::from_ymd_opt(2026, 10, 31).unwrap().and_hms_opt(12, 0, 0).unwrap(); // Saturday
        sim.step(0.0);
        assert!(sim.run(600).is_empty());
        assert_eq!(sim.status().line(), "Off for the weekend");
    }

    #[test]
    fn a_short_lock_holds_the_timer_and_a_long_one_restarts_it() {
        let mut sim = Sim::new(short());
        sim.step(0.0);
        sim.run(40);
        sim.away = true;
        sim.run(10);
        sim.away = false;
        sim.step(1.0);
        assert_eq!(sim.status(), Status::Next { seconds: 19.0, long: false });
        sim.away = true;
        sim.run(30);
        sim.away = false;
        sim.step(1.0);
        assert_eq!(sim.status(), Status::Next { seconds: 59.0, long: false });
    }

    #[test]
    fn locking_during_a_break_ends_it() {
        let mut sim = Sim::new(short());
        sim.step(0.0);
        sim.run(61);
        sim.away = true;
        assert_eq!(sim.step(1.0), Action::End);
    }

    #[test]
    fn sleep_shows_as_a_gap_between_ticks() {
        let mut sim = Sim::new(short());
        sim.step(0.0);
        sim.run(40);
        sim.step(15.0); // slept 15 s: not work, not a natural break
        assert_eq!(sim.status(), Status::Next { seconds: 20.0, long: false });
        sim.step(600.0); // slept 10 min: a natural break
        assert_eq!(sim.status(), Status::Next { seconds: 60.0, long: false });
    }

    #[test]
    fn focus_holds_the_timer() {
        let mut sim = Sim::new(short());
        sim.step(0.0);
        sim.run(50);
        sim.focus = true;
        assert!(sim.run(300).is_empty());
        assert_eq!(sim.status().line(), "Waiting for Focus to end");
        sim.focus = false;
        assert_eq!(sim.starts(10), vec![(10, false)]);
    }

    #[test]
    fn pause_ends_a_break_and_resumes_with_a_fresh_timer() {
        let mut sim = Sim::new(short());
        sim.step(0.0);
        sim.run(61);
        assert_eq!(sim.s.pause_for(sim.local, 2), Action::End);
        assert_eq!(sim.status().line(), "Paused until 09:03");
        assert!(sim.run(119).is_empty());
        sim.step(1.0); // 09:03:01, the pause is over and this second already counts
        assert_eq!(sim.status(), Status::Next { seconds: 59.0, long: false });
        sim.s.pause_for(sim.local, 60);
        sim.s.resume();
        sim.step(1.0);
        assert_eq!(sim.status(), Status::Next { seconds: 59.0, long: false });
    }

    #[test]
    fn until_morning_means_the_next_six_oclock() {
        let day = NaiveDate::from_ymd_opt(2026, 11, 2).unwrap();
        let at = |h| day.and_hms_opt(h, 30, 0).unwrap();
        assert_eq!(next_morning(at(22)), (day + Duration::days(1)).and_hms_opt(6, 0, 0).unwrap());
        assert_eq!(next_morning(at(2)), day.and_hms_opt(6, 0, 0).unwrap());
    }

    #[test]
    fn stretchly_running_keeps_livae_silent() {
        let mut sim = Sim::new(short());
        sim.other_app = true;
        sim.step(0.0);
        assert!(sim.run(300).is_empty());
        assert_eq!(sim.s.start_now(&sim.r, sim.now, None), Action::None);
        assert_eq!(sim.status().line(), "Stretchly is running");
    }

    #[test]
    fn skip_to_next_starts_the_timer_over() {
        let mut sim = Sim::new(short());
        sim.step(0.0);
        sim.run(50);
        sim.s.skip_to_next(sim.local);
        assert_eq!(sim.status().line(), "Next break in 1 min");
        assert_eq!(sim.starts(60), vec![(60, false)]);
    }

    #[test]
    fn status_lines() {
        assert_eq!(Status::Next { seconds: 11.0 * 60.0 + 5.0, long: false }.line(), "Next break in 12 min");
        assert_eq!(Status::Next { seconds: 44.2, long: false }.line(), "Next break in 45 s");
        assert_eq!(countdown(17.2), "18 s");
        assert_eq!(countdown(272.0), "4:32");
        assert_eq!((minutes_seconds(44.2), minutes_seconds(272.0)), ("0:45".into(), "4:32".into()));
    }

    #[test]
    fn quotes_file_falls_back_to_the_built_in_list() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("quotes.md");
        assert_eq!(load_quotes(&p).len(), 10);
        std::fs::write(&p, "# Active quotes\n").unwrap();
        assert_eq!(load_quotes(&p).len(), 10);
        std::fs::write(&p, "Breathe.\n").unwrap();
        assert_eq!(load_quotes(&p), vec![Quote { text: "Breathe.".into(), author: None }]);
    }

    #[test]
    fn built_in_quotes_keep_breaks_and_authors() {
        let q = default_quotes();
        assert_eq!(q.len(), 10);
        assert_eq!((q[0].text.as_str(), q[0].author.as_deref()), ("Look away from the screen.\nLet your eyes rest on something far away.", None));
        assert_eq!((q[2].text.as_str(), q[2].author.as_deref()), ("Rest is not idleness.", Some("John Lubbock")));
        assert_eq!(q[5].text, "Nature does not hurry,\nyet everything is accomplished.");
        assert_eq!(q.iter().filter(|q| q.author.is_some()).count(), 6);
    }

    #[test]
    fn quotes_from_windows_line_endings() {
        let q = parse_quotes("One.\r\n\r\nAuthor\r\n---\r\nTwo.\r\n");
        assert_eq!(q, vec![Quote { text: "One.".into(), author: Some("Author".into()) }, Quote { text: "Two.".into(), author: None }]);
    }

    #[test]
    fn deck_shows_every_quote_once_per_round_and_never_twice_in_a_row() {
        let mut deck = Deck::new(42);
        let mut prev = None;
        for _ in 0..20 {
            let mut round: Vec<usize> = (0..5).map(|_| deck.next(5).unwrap()).collect();
            assert_ne!(Some(round[0]), prev);
            prev = round.last().copied();
            round.sort();
            assert_eq!(round, vec![0, 1, 2, 3, 4]);
        }
        assert_eq!(deck.next(0), None);
        assert!(deck.next(2).unwrap() < 2);
    }
}
