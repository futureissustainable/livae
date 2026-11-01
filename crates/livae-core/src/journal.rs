//! The evening note, written exactly as Vesper writes it (Vesper/Sources/LogWriter.swift, 13.09.2026),
//! so a journal Vesper started stays one continuous file.
//!
//! Format: a `# ` title line (Vesper's files say `# Daily Learnings`), then entries newest first:
//! ```text
//! ## 2026-10-31 - 20:05   (snoozed 2×)
//! Day: 🟢 Great
//! <note, at least 90 characters>
//! Exercise: Yes
//! Metacognitive distancing: No
//! ```
//! Saving again on the same day keeps that day's header line (time and snooze tag) and swaps the body.

pub use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime};
use chrono::TimeDelta;
use std::collections::BTreeMap;
use std::{fs, io, path::Path};
use unicode_segmentation::UnicodeSegmentation;

pub const MIN_CHARS: usize = 90;
pub const DAY_LABELS: [&str; 5] = ["Bad", "Ok", "Decent", "Great", "Perfect"];
pub const DAY_EMOJIS: [&str; 5] = ["🔴", "🟠", "🟡", "🟢", "🔵"];
/// Vesper's day colours, sRGB.
pub const DAY_COLORS: [(u8, u8, u8); 5] = [(255, 107, 99), (255, 173, 82), (245, 213, 74), (85, 206, 117), (105, 202, 250)];

/// The title line of a new journal file. An existing file keeps its own title (Vesper wrote "# Daily Learnings").
const HEADER: &str = "# Journal";

/// The file's title line (or the default for a new file) and everything after it. Tolerates CRLF files.
fn split_header(content: &str) -> (&str, &str) {
    match content.split_once('\n') {
        Some((first, rest)) if first.starts_with("# ") => (first.trim_end_matches('\r'), rest),
        None if content.starts_with("# ") => (content.trim_end(), ""),
        _ => (HEADER, content),
    }
}

/// The line names Vesper wrote before habits became editable; still the defaults.
pub const EXERCISE: &str = "Exercise";
pub const DISTANCING: &str = "Metacognitive distancing";

/// One day's answers. `None` means not answered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    /// 0 = Bad ... 4 = Perfect.
    pub day: Option<usize>,
    /// Yes/No check-ins in file order, by their line name ("Exercise: Yes"). Lines of habits no longer in
    /// Settings stay here, so editing a day never drops them.
    pub habits: Vec<(String, Option<bool>)>,
    /// Free-text exercise notes from before the Yes/No check-in; kept when editing.
    pub legacy_exercise: String,
    pub note: String,
}

impl Entry {
    /// Note length the way Vesper counts it: trimmed, in user-perceived characters.
    pub fn note_len(&self) -> usize {
        self.note.trim().graphemes(true).count()
    }

    pub fn habit(&self, key: &str) -> Option<bool> {
        self.habits.iter().find(|(k, _)| k == key).and_then(|(_, v)| *v)
    }

    /// Answer `key` in place, or add it after the others.
    pub fn set_habit(&mut self, key: &str, answer: Option<bool>) {
        match self.habits.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = answer,
            None => self.habits.push((key.to_string(), answer)),
        }
    }

    /// Saving needs the day, an answer for every habit in `keys`, and the note floor (as Vesper did).
    pub fn is_complete<K: AsRef<str>>(&self, keys: &[K]) -> bool {
        self.day.is_some_and(|d| d < DAY_LABELS.len()) && keys.iter().all(|k| self.habit(k.as_ref()).is_some()) && self.note_len() >= MIN_CHARS
    }

    fn body(&self) -> String {
        let day = match self.day {
            Some(d) if d < DAY_LABELS.len() => format!("{} {}", DAY_EMOJIS[d], DAY_LABELS[d]),
            _ => "—".to_string(),
        };
        let mut lines = vec![format!("Day: {day}"), self.note.trim().to_string()];
        let legacy = (!self.legacy_exercise.is_empty()).then(|| format!("Exercise notes: {}", self.legacy_exercise));
        for (key, answer) in &self.habits {
            let answer = match answer {
                Some(true) => "Yes",
                Some(false) => "No",
                None => "Unanswered",
            };
            lines.push(format!("{key}: {answer}"));
            if key == EXERCISE {
                lines.extend(legacy.clone());
            }
        }
        if !self.habits.iter().any(|(k, _)| k == EXERCISE) {
            lines.extend(legacy);
        }
        lines.join("\n")
    }
}

/// The first thing Save still needs, as the line under the note says it.
pub fn missing<K: AsRef<str>>(e: &Entry, keys: &[K]) -> Option<&'static str> {
    if e.day.is_none() {
        Some("Pick how your day was")
    } else if keys.iter().any(|k| e.habit(k.as_ref()).is_none()) {
        Some("Answer every habit")
    } else if e.note_len() < MIN_CHARS {
        Some("Write at least 90 characters")
    } else {
        None
    }
}

/// Line names the file already uses for something else.
const RESERVED: [&str; 2] = ["Day", "Exercise notes"];

/// A habit name the file can hold as a line ("Name: Yes"), not taken by another habit.
pub fn habit_name(text: &str, taken: &[&str]) -> Option<String> {
    let t = text.trim();
    let clash = |s: &str| s.eq_ignore_ascii_case(t);
    let ok = !t.is_empty() && !t.contains(':') && !t.contains('\n') && !RESERVED.iter().any(|r| clash(r)) && !taken.iter().any(|s| clash(s));
    ok.then(|| t.to_string())
}

/// A press on a habit: unanswered → Yes → No → Yes.
pub fn toggled(answer: Option<bool>) -> Option<bool> {
    Some(answer != Some(true))
}

/// A habit's streak shown while editing: the run up to yesterday, plus today when Yes, ended by No.
pub fn streak_with(before: u32, today: Option<bool>) -> u32 {
    match today {
        Some(true) => before + 1,
        Some(false) => 0,
        None => before,
    }
}

/// Whether a day has an entry, read again only when the file or the day changed.
#[derive(Debug, Default)]
pub struct DoneCache(Option<(std::path::PathBuf, NaiveDate, Option<std::time::SystemTime>, bool)>);

impl DoneCache {
    pub fn get(&mut self, path: &Path, day: NaiveDate) -> bool {
        let modified = fs::metadata(path).and_then(|m| m.modified()).ok();
        if let Some((p, d, m, hit)) = &self.0 {
            if p == path && *d == day && *m == modified {
                return *hit;
            }
        }
        let hit = has_entry(path, day).unwrap_or(false);
        self.0 = Some((path.to_path_buf(), day, modified, hit));
        hit
    }

    pub fn clear(&mut self) {
        self.0 = None;
    }
}

/// Swift's `.newlines` set, used for its `trimmingCharacters(in: .newlines)`.
fn is_newline(c: char) -> bool {
    matches!(c, '\n' | '\u{0B}' | '\u{0C}' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

fn date_key(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

fn read(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e),
    }
}

/// The body lines of the entry for `date`, or None when that day has no entry.
fn body_lines(content: &str, date: NaiveDate) -> Option<Vec<&str>> {
    let prefix = format!("## {}", date_key(date));
    let lines: Vec<&str> = content.split('\n').collect();
    let i = lines.iter().position(|l| l.starts_with(&prefix))?;
    Some(lines[i + 1..].iter().take_while(|l| !l.starts_with("## ")).copied().collect())
}

/// Every dated entry, newest first as in the file.
fn entries(content: &str) -> Vec<(NaiveDate, Vec<&str>)> {
    let mut out: Vec<(NaiveDate, Vec<&str>)> = Vec::new();
    let mut open = false;
    for line in content.split('\n') {
        if let Some(rest) = line.strip_prefix("## ") {
            let mut ymd = rest.get(..10).unwrap_or("").split('-').map(|p| p.parse::<u32>().ok());
            let date = match (ymd.next().flatten(), ymd.next().flatten(), ymd.next().flatten()) {
                (Some(y), Some(m), Some(d)) => NaiveDate::from_ymd_opt(y as i32, m, d),
                _ => None,
            };
            open = date.is_some();
            out.extend(date.map(|d| (d, Vec::new())));
        } else if open {
            if let Some((_, body)) = out.last_mut() {
                body.push(line);
            }
        }
    }
    out
}

/// "Yes", "No", "Unanswered" (and Vesper's old "—") as an answer; anything else is not a check-in line.
fn answer(value: &str) -> Option<Option<bool>> {
    match value.trim_matches(|c: char| c == ' ' || c == '\t') {
        "Yes" => Some(Some(true)),
        "No" => Some(Some(false)),
        "Unanswered" | "—" => Some(None),
        _ => None,
    }
}

/// A body as written: "Day:" first, then the note, then the trailing block of "Key: Yes|No" lines. Vesper's
/// older entries hold free text in "Exercise:" or an "Exercise notes:" line; both stay as `legacy_exercise`.
fn parse_body(body: &[&str]) -> Entry {
    let mut lines = body.to_vec();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let mut e = Entry::default();
    let start = match lines.first().and_then(|l| l.strip_prefix("Day: ")) {
        Some(v) => {
            e.day = DAY_LABELS.iter().position(|l| v.contains(l));
            1
        }
        None => 0,
    };
    let mut end = lines.len();
    while end > start {
        let line = lines[end - 1];
        if let Some(v) = line.strip_prefix("Exercise notes: ") {
            e.legacy_exercise = v.to_string();
        } else if let Some((key, value)) = line.split_once(": ") {
            match answer(value) {
                Some(a) => e.habits.push((key.to_string(), a)),
                None if key == EXERCISE => {
                    e.legacy_exercise = value.trim_matches(|c: char| c == ' ' || c == '\t').to_string();
                    e.habits.push((key.to_string(), None));
                }
                None => break,
            }
        } else {
            break;
        }
        end -= 1;
    }
    e.habits.reverse();
    e.note = lines[start..end].join("\n").trim().to_string();
    e
}

/// True when the journal already holds an entry for `date` (this is what "done today" means).
pub fn has_entry(path: &Path, date: NaiveDate) -> io::Result<bool> {
    Ok(body_lines(&read(path)?, date).is_some())
}

/// Parse the entry for `date` back into its parts, so the editor can prefill it.
pub fn parse(path: &Path, date: NaiveDate) -> io::Result<Option<Entry>> {
    Ok(body_lines(&read(path)?, date).map(|b| parse_body(&b)))
}

/// Every day's entry by date; a date written twice keeps the upper (newer) one.
fn by_date(path: &Path) -> BTreeMap<NaiveDate, Entry> {
    let content = read(path).unwrap_or_default();
    let mut out = BTreeMap::new();
    for (date, body) in entries(&content) {
        out.entry(date).or_insert_with(|| parse_body(&body));
    }
    out
}

/// Consecutive days answered Yes, ending the day before `today`.
fn run_before(all: &BTreeMap<NaiveDate, Entry>, key: &str, today: NaiveDate) -> u32 {
    let mut n = 0;
    let mut day = today.pred_opt();
    while let Some(d) = day.filter(|d| all.get(d).and_then(|e| e.habit(key)) == Some(true)) {
        n += 1;
        day = d.pred_opt();
    }
    n
}

/// Each habit's run of Yes days ending yesterday, whatever today says: the editor adds today's answer live.
pub fn streaks_before<K: AsRef<str>>(path: &Path, habits: &[K], today: NaiveDate) -> Vec<(String, u32)> {
    let all = by_date(path);
    habits.iter().map(|k| (k.as_ref().to_string(), run_before(&all, k.as_ref(), today))).collect()
}

/// Each habit's current streak: through today when today is Yes; still alive up to yesterday while today is
/// unanswered; 0 once today is No. Unreadable files count as no history.
pub fn streaks<K: AsRef<str>>(path: &Path, habits: &[K], today: NaiveDate) -> Vec<(String, u32)> {
    let all = by_date(path);
    habits
        .iter()
        .map(|k| {
            let k = k.as_ref();
            let n = match all.get(&today).and_then(|e| e.habit(k)) {
                Some(true) => run_before(&all, k, today) + 1,
                Some(false) => 0,
                None => run_before(&all, k, today),
            };
            (k.to_string(), n)
        })
        .collect()
}

/// One day for the year view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaySummary {
    pub date: NaiveDate,
    pub rating: Option<usize>,
    /// Answered check-ins only, in file order.
    pub habits: Vec<(String, bool)>,
    pub note: String,
}

/// Every day of `year` that has an entry, oldest first.
pub fn year(path: &Path, year: i32) -> Vec<DaySummary> {
    by_date(path)
        .into_iter()
        .filter(|(d, _)| d.year() == year)
        .map(|(date, e)| DaySummary {
            date,
            rating: e.day,
            habits: e.habits.iter().filter_map(|(k, v)| v.map(|v| (k.clone(), v))).collect(),
            note: e.note,
        })
        .collect()
}

/// Create the entry for `now`'s date (prepended, newest on top) or replace its body if it exists.
/// On replace the original header line (time and snooze tag) is kept. Writes atomically.
pub fn save(path: &Path, entry: &Entry, snoozes: u32, now: NaiveDateTime) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let body = entry.body();
    let content = read(path)?;
    let (header, rest) = split_header(&content);
    let lines: Vec<&str> = rest.split('\n').collect();
    let prefix = format!("## {}", date_key(now.date()));

    let out = if let Some(i) = lines.iter().position(|l| l.starts_with(&prefix)) {
        let end = lines[i + 1..].iter().position(|l| l.starts_with("## ")).map_or(lines.len(), |j| i + 1 + j);
        let mut out: Vec<&str> = lines[..=i].to_vec();
        out.extend(body.split('\n'));
        if end < lines.len() {
            out.push("");
            out.extend(&lines[end..]);
        }
        format!("{header}\n\n{}\n", out.join("\n").trim_matches(is_newline))
    } else {
        let tag = if snoozes > 0 { format!("   (snoozed {snoozes}×)") } else { String::new() };
        let block = format!("## {} - {}{tag}\n{body}\n", date_key(now.date()), now.format("%H:%M"));
        let existing = rest.trim_matches(is_newline);
        if existing.is_empty() { format!("{header}\n\n{block}") } else { format!("{header}\n\n{block}\n{existing}\n") }
    };

    let tmp = path.with_extension("md.livae-tmp");
    fs::write(&tmp, out)?;
    fs::rename(&tmp, path)
}

/// Local wall-clock time.
pub fn now() -> NaiveDateTime {
    chrono::Local::now().naive_local()
}

/// "Saturday, 31.10.2026".
pub fn date_label(date: NaiveDate) -> String {
    date.format("%A, %d.%m.%Y").to_string()
}

/// When the evening prompt asks, and how long a snooze and a nag last (Vesper/Sources/PromptController.swift).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule {
    pub at: NaiveTime,
    pub snooze: TimeDelta,
    pub nag: TimeDelta,
}

/// How an open prompt may be dismissed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Past the hour and not done: comes back to the front until saved; dismissing snoozes.
    Due,
    /// Opened before the hour: dismissing closes it until the hour.
    Early,
    /// Today is done: today's entry, prefilled; dismissing cancels.
    Edit,
}

impl Schedule {
    /// From the settings: `at` is "HH:MM" (anything unreadable means 20:00); minutes under 1 count as 1.
    pub fn new(at: &str, snooze_min: u32, nag_min: u32) -> Self {
        let parsed = at.trim().split_once(':').and_then(|(h, m)| NaiveTime::from_hms_opt(h.trim().parse().ok()?, m.trim().parse().ok()?, 0));
        Self {
            at: parsed.unwrap_or(NaiveTime::from_hms_opt(20, 0, 0).unwrap()),
            snooze: TimeDelta::minutes(snooze_min.max(1).into()),
            nag: TimeDelta::minutes(nag_min.max(1).into()),
        }
    }

    pub fn target(&self, day: NaiveDate) -> NaiveDateTime {
        day.and_time(self.at)
    }

    /// The mode of a prompt opened at `now`.
    pub fn mode(&self, now: NaiveDateTime, done: bool) -> Mode {
        if done {
            Mode::Edit
        } else if now >= self.target(now.date()) {
            Mode::Due
        } else {
            Mode::Early
        }
    }
}

/// Snoozes since the last save. Kept in memory, as Vesper does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tracker {
    until: Option<NaiveDateTime>,
    snoozes: u32,
    /// The day the snoozes were counted on, so a missed evening never tags the next one.
    day: Option<NaiveDate>,
}

impl Tracker {
    /// The prompt should open by itself: past the hour, not snoozed, today not done.
    pub fn due(&self, s: &Schedule, now: NaiveDateTime, done: bool) -> bool {
        !done && now >= s.target(now.date()) && self.until.is_none_or(|u| now >= u)
    }

    pub fn snooze(&mut self, s: &Schedule, now: NaiveDateTime) {
        if self.day != Some(now.date()) {
            self.snoozes = 0;
            self.day = Some(now.date());
        }
        self.snoozes += 1;
        self.until = Some(now + s.snooze);
    }

    /// An early prompt was closed: quiet until the hour.
    pub fn close(&mut self, s: &Schedule, now: NaiveDateTime) {
        self.until = Some(s.target(now.date()));
    }

    pub fn saved(&mut self) {
        *self = Self::default();
    }

    /// Snoozes to write in a new entry's header ("(snoozed 2×)").
    pub fn snoozes(&self, now: NaiveDateTime) -> u32 {
        if self.day == Some(now.date()) { self.snoozes } else { 0 }
    }

    /// One line for the menu.
    pub fn status(&self, s: &Schedule, now: NaiveDateTime, done: bool) -> String {
        let hm = |t: NaiveDateTime| t.format("%H:%M").to_string();
        if done {
            "Done today".into()
        } else if now < s.target(now.date()) {
            format!("Opens at {}", hm(s.target(now.date())))
        } else if let Some(u) = self.until.filter(|u| now < *u) {
            format!("Snoozed until {}", hm(u))
        } else {
            "Due now".into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, d).unwrap().and_hms_opt(h, m, 0).unwrap()
    }
    fn entry(note: &str) -> Entry {
        let habits = vec![(EXERCISE.into(), Some(true)), (DISTANCING.into(), Some(false))];
        Entry { day: Some(3), habits, legacy_exercise: String::new(), note: note.into() }
    }
    fn day(d: u32) -> NaiveDate {
        at(d, 0, 0).date()
    }

    #[test]
    fn new_file_gets_the_plain_title_and_crlf_titles_are_kept_once() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("Journal.md");
        save(&p, &entry("first day"), 0, at(30, 20, 5)).unwrap();
        assert!(fs::read_to_string(&p).unwrap().starts_with("# Journal\n\n## 2026-10-30 - 20:05\n"));

        let w = dir.path().join("windows.md");
        fs::write(&w, "# Daily Learnings\r\n\r\n## 2026-10-29 - 20:00\r\nDay: 🟢 Great\r\nold\r\n").unwrap();
        save(&w, &entry("new"), 0, at(30, 20, 5)).unwrap();
        let s = fs::read_to_string(&w).unwrap();
        assert!(s.starts_with("# Daily Learnings\n\n## 2026-10-30 - 20:05\n"), "{s:?}");
        assert_eq!(s.matches("# Daily Learnings").count(), 1);
        assert!(s.contains("## 2026-10-29 - 20:00"));
    }

    #[test]
    fn creates_prepends_and_edits_like_vesper() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("3 - Personal/Daily Learnings.md");
        // Vesper's file: its title stays on every save.
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "# Daily Learnings\n").unwrap();

        save(&p, &entry("  first day  "), 0, at(30, 20, 5)).unwrap();
        assert_eq!(
            fs::read_to_string(&p).unwrap(),
            "# Daily Learnings\n\n## 2026-10-30 - 20:05\nDay: 🟢 Great\nfirst day\nExercise: Yes\nMetacognitive distancing: No\n"
        );

        save(&p, &entry("second day"), 2, at(31, 20, 40)).unwrap();
        assert_eq!(
            fs::read_to_string(&p).unwrap(),
            "# Daily Learnings\n\n## 2026-10-31 - 20:40   (snoozed 2×)\nDay: 🟢 Great\nsecond day\nExercise: Yes\nMetacognitive distancing: No\n\n\
             ## 2026-10-30 - 20:05\nDay: 🟢 Great\nfirst day\nExercise: Yes\nMetacognitive distancing: No\n"
        );

        // Editing today keeps its header line, swaps the body, keeps older entries.
        let mut e = entry("second day, edited");
        e.day = None;
        e.set_habit(DISTANCING, None);
        save(&p, &e, 0, at(31, 22, 0)).unwrap();
        let s = fs::read_to_string(&p).unwrap();
        assert!(s.starts_with("# Daily Learnings\n\n## 2026-10-31 - 20:40   (snoozed 2×)\nDay: —\nsecond day, edited\nExercise: Yes\nMetacognitive distancing: Unanswered\n\n## 2026-10-30"));
        assert!(s.ends_with("Metacognitive distancing: No\n"));
    }

    #[test]
    fn parses_back_including_legacy_exercise() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("j.md");
        fs::write(&p, "# Daily Learnings\n\n## 2026-10-31 - 20:00\nDay: 🔵 Perfect\nline one\nline two\nExercise: 5 km run\nMetacognitive distancing: Yes\n\n## 2026-10-30 - 20:00\nDay: 🔴 Bad\nold\n").unwrap();
        let e = parse(&p, at(31, 0, 0).date()).unwrap().unwrap();
        assert_eq!(e.day, Some(4));
        assert_eq!(e.habits, vec![(EXERCISE.to_string(), None), (DISTANCING.to_string(), Some(true))]);
        assert_eq!(e.legacy_exercise, "5 km run");
        assert_eq!(e.note, "line one\nline two");
        // Saved again it keeps Vesper's lines, the old exercise text after the Exercise line.
        let mut edited = e.clone();
        edited.set_habit(EXERCISE, Some(true));
        save(&p, &edited, 0, at(31, 21, 0)).unwrap();
        assert!(fs::read_to_string(&p).unwrap().contains("line two\nExercise: Yes\nExercise notes: 5 km run\nMetacognitive distancing: Yes\n"));
        assert!(has_entry(&p, at(30, 0, 0).date()).unwrap());
        assert!(!has_entry(&p, at(29, 0, 0).date()).unwrap());
        assert_eq!(parse(&dir.path().join("missing.md"), at(31, 0, 0).date()).unwrap(), None);
    }

    #[test]
    fn custom_habits_round_trip_and_note_lines_stay_note() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("j.md");
        let mut e = entry(&"x".repeat(90));
        e.note = format!("{}\nRule: keep calls short\n{}", "a".repeat(50), "b".repeat(50));
        e.set_habit("Read 20 pages", None);
        assert!(!e.is_complete(&[EXERCISE, DISTANCING, "Read 20 pages"]));
        assert!(e.is_complete(&[EXERCISE, DISTANCING]));
        e.set_habit("Read 20 pages", Some(true));
        assert!(e.is_complete(&[EXERCISE, DISTANCING, "Read 20 pages"]));
        save(&p, &e, 0, at(31, 20, 0)).unwrap();
        assert!(fs::read_to_string(&p).unwrap().ends_with("Exercise: Yes\nMetacognitive distancing: No\nRead 20 pages: Yes\n"));
        assert_eq!(parse(&p, day(31)).unwrap().unwrap(), e);
    }

    #[test]
    fn streaks_count_yes_days_up_to_today() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("j.md");
        let write = |d: u32, ex: Option<bool>, read: Option<bool>| {
            let mut e = entry(&"x".repeat(90));
            e.set_habit(EXERCISE, ex);
            e.set_habit("Read", read);
            save(&p, &e, 0, at(d, 20, 0)).unwrap();
        };
        // Exercise: 25 Yes, 26 missing (gap), 27-29 Yes. Read: 28 No, 29 Yes.
        write(25, Some(true), None);
        for d in 27..=29 {
            write(d, Some(true), Some(d == 29));
        }
        let keys = [EXERCISE, "Read", "Never"];
        // Today (30) has no entry yet: the streak ending yesterday is still alive.
        assert_eq!(streaks(&p, &keys, day(30)), vec![(EXERCISE.into(), 3), ("Read".into(), 1), ("Never".into(), 0)]);
        // Today Yes extends it; today No ends it.
        write(30, Some(true), Some(false));
        assert_eq!(streaks(&p, &keys, day(30)), vec![(EXERCISE.into(), 4), ("Read".into(), 0), ("Never".into(), 0)]);
        assert_eq!(streaks_before(&p, &keys, day(30)), vec![(EXERCISE.into(), 3), ("Read".into(), 1), ("Never".into(), 0)]);
        // Unanswered today keeps yesterday's run; a missed yesterday breaks it.
        write(31, None, None);
        assert_eq!(streaks(&p, &[EXERCISE], day(31)), vec![(EXERCISE.into(), 4)]);
        assert_eq!(streaks(&p, &[EXERCISE], NaiveDate::from_ymd_opt(2026, 11, 2).unwrap()), vec![(EXERCISE.into(), 0)]);
    }

    #[test]
    fn year_lists_answered_days_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("j.md");
        fs::write(
            &p,
            "# Daily Learnings\n\n## 2027-01-01 - 20:00\nDay: 🔴 Bad\nnew year\nExercise: No\n\n\
             ## 2026-10-31 - 20:00\nDay: 🔵 Perfect\nlast\nExercise: Yes\nMetacognitive distancing: Unanswered\n\n\
             ## 2026-10-30 - 20:00\nold vesper note\nExercise: 5 km run\n",
        )
        .unwrap();
        let y = year(&p, 2026);
        assert_eq!(y.len(), 2);
        assert_eq!((y[0].date, y[0].rating, y[0].habits.clone(), y[0].note.as_str()), (day(30), None, vec![], "old vesper note"));
        assert_eq!((y[1].rating, y[1].habits.clone()), (Some(4), vec![(EXERCISE.to_string(), true)]));
        assert_eq!(year(&p, 2027)[0].habits, vec![(EXERCISE.to_string(), false)]);
    }

    #[test]
    fn schedule_asks_at_the_hour_snoozes_and_stops_when_done() {
        let s = Schedule::new("20:00", 15, 3);
        let mut t = Tracker::default();
        assert!(!t.due(&s, at(31, 19, 59), false));
        assert!(t.due(&s, at(31, 20, 0), false));
        assert!(!t.due(&s, at(31, 20, 0), true));

        t.snooze(&s, at(31, 20, 1));
        assert!(!t.due(&s, at(31, 20, 15), false));
        assert!(t.due(&s, at(31, 20, 16), false));
        assert_eq!(t.status(&s, at(31, 20, 10), false), "Snoozed until 20:16");
        t.snooze(&s, at(31, 20, 16));
        assert_eq!(t.snoozes(at(31, 20, 40)), 2);

        // A missed evening never tags the next one.
        assert_eq!(t.snoozes(at(31, 23, 59)), 2);
        t.snooze(&s, NaiveDate::from_ymd_opt(2026, 11, 1).unwrap().and_hms_opt(20, 5, 0).unwrap());
        assert_eq!(t.snoozes(NaiveDate::from_ymd_opt(2026, 11, 1).unwrap().and_hms_opt(20, 30, 0).unwrap()), 1);

        t.saved();
        assert_eq!(t, Tracker::default());

        // Opened early and closed: quiet until the hour, then due.
        t.close(&s, at(30, 18, 0));
        assert!(!t.due(&s, at(30, 19, 0), false));
        assert!(t.due(&s, at(30, 20, 0), false));
    }

    #[test]
    fn mode_status_and_settings_fallbacks() {
        let s = Schedule::new("21:30", 0, 0);
        assert_eq!((s.snooze, s.nag), (TimeDelta::minutes(1), TimeDelta::minutes(1)));
        assert_eq!(Schedule::new("soon", 15, 3).at, NaiveTime::from_hms_opt(20, 0, 0).unwrap());
        assert_eq!(s.mode(at(31, 21, 29), false), Mode::Early);
        assert_eq!(s.mode(at(31, 21, 30), false), Mode::Due);
        assert_eq!(s.mode(at(31, 9, 0), true), Mode::Edit);
        let t = Tracker::default();
        assert_eq!(t.status(&s, at(31, 9, 0), false), "Opens at 21:30");
        assert_eq!(t.status(&s, at(31, 22, 0), false), "Due now");
        assert_eq!(t.status(&s, at(31, 22, 0), true), "Done today");
        assert_eq!(date_label(at(31, 0, 0).date()), "Saturday, 31.10.2026");
    }

    #[test]
    fn the_prompt_says_what_save_needs_and_checks_habit_names() {
        let keys = [EXERCISE, DISTANCING];
        let mut e = Entry::default();
        assert_eq!(missing(&e, &keys), Some("Pick how your day was"));
        e.day = Some(2);
        assert_eq!(missing(&e, &keys), Some("Answer every habit"));
        e.set_habit(EXERCISE, Some(false));
        e.set_habit(DISTANCING, Some(true));
        assert_eq!(missing(&e, &keys), Some("Write at least 90 characters"));
        e.note = "x".repeat(90);
        assert_eq!(missing(&e, &keys), None);

        // Names become "Name: Yes" lines: no colon, no line the file uses for something else, no repeats.
        assert_eq!(habit_name("  Read 20 pages ", &["Worked out"]), Some("Read 20 pages".into()));
        for bad in ["", "  ", "Sleep: 8 h", "day", "Exercise notes", "worked OUT"] {
            assert_eq!(habit_name(bad, &["Worked out"]), None, "{bad:?}");
        }

        // A press cycles unanswered → Yes → No → Yes; the shown streak follows today's answer.
        assert_eq!((toggled(None), toggled(Some(true)), toggled(Some(false))), (Some(true), Some(false), Some(true)));
        assert_eq!((streak_with(4, Some(true)), streak_with(4, Some(false)), streak_with(4, None)), (5, 0, 4));
    }

    #[test]
    fn done_cache_reads_again_after_a_save_or_on_a_new_day() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("j.md");
        let mut done = DoneCache::default();
        assert!(!done.get(&p, day(31)));
        save(&p, &entry(&"x".repeat(90)), 0, at(31, 20, 0)).unwrap();
        assert!(done.get(&p, day(31)));
        assert!(!done.get(&p, day(30)));
    }

    #[test]
    fn completeness_needs_every_answer_and_90_characters() {
        let keys = [EXERCISE, DISTANCING];
        let mut e = entry(&"x".repeat(89));
        assert!(!e.is_complete(&keys));
        e.note.push('é');
        assert!(e.is_complete(&keys));
        e.set_habit(EXERCISE, None);
        assert!(!e.is_complete(&keys));
    }
}
