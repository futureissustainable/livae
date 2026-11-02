//! Time zones in the launcher: "time in tokyo", "nyc time", "10am london" (to your time), "15:00 in sydney" (your
//! time there), "9:30 pst to bucharest". Places are the cities of the time-zone database, common short names and
//! countries with one zone.

use std::collections::HashMap;
use std::sync::OnceLock;

use chrono::{DateTime, FixedOffset, Local, NaiveTime, Offset, TimeZone, Utc};
use chrono_tz::Tz;

use crate::answers::Answer;

/// Names that are not a city in the database: short names, abbreviations and one-zone countries.
const ALIASES: &[(&str, &str)] = &[
    ("utc", "UTC"), ("gmt", "UTC"), ("z", "UTC"),
    ("nyc", "America/New_York"), ("ny", "America/New_York"), ("boston", "America/New_York"), ("washington", "America/New_York"),
    ("miami", "America/New_York"), ("atlanta", "America/New_York"), ("est", "America/New_York"), ("edt", "America/New_York"),
    ("et", "America/New_York"), ("eastern", "America/New_York"),
    ("dallas", "America/Chicago"), ("houston", "America/Chicago"), ("austin", "America/Chicago"), ("cst", "America/Chicago"),
    ("cdt", "America/Chicago"), ("central", "America/Chicago"),
    ("mst", "America/Denver"), ("mdt", "America/Denver"), ("mountain", "America/Denver"),
    ("la", "America/Los_Angeles"), ("sf", "America/Los_Angeles"), ("san francisco", "America/Los_Angeles"),
    ("seattle", "America/Los_Angeles"), ("san diego", "America/Los_Angeles"), ("las vegas", "America/Los_Angeles"),
    ("silicon valley", "America/Los_Angeles"), ("pst", "America/Los_Angeles"), ("pdt", "America/Los_Angeles"),
    ("pt", "America/Los_Angeles"), ("pacific", "America/Los_Angeles"),
    ("montreal", "America/Toronto"), ("ottawa", "America/Toronto"), ("canada", "America/Toronto"),
    ("rio", "America/Sao_Paulo"), ("brazil", "America/Sao_Paulo"), ("mexico", "America/Mexico_City"),
    ("argentina", "America/Argentina/Buenos_Aires"),
    ("uk", "Europe/London"), ("england", "Europe/London"), ("britain", "Europe/London"), ("bst", "Europe/London"),
    ("ireland", "Europe/Dublin"), ("portugal", "Europe/Lisbon"), ("spain", "Europe/Madrid"), ("barcelona", "Europe/Madrid"),
    ("france", "Europe/Paris"), ("germany", "Europe/Berlin"), ("munich", "Europe/Berlin"), ("frankfurt", "Europe/Berlin"),
    ("hamburg", "Europe/Berlin"), ("cologne", "Europe/Berlin"), ("cet", "Europe/Paris"), ("cest", "Europe/Paris"),
    ("italy", "Europe/Rome"), ("milan", "Europe/Rome"), ("netherlands", "Europe/Amsterdam"), ("holland", "Europe/Amsterdam"),
    ("belgium", "Europe/Brussels"), ("switzerland", "Europe/Zurich"), ("geneva", "Europe/Zurich"), ("austria", "Europe/Vienna"),
    ("sweden", "Europe/Stockholm"), ("norway", "Europe/Oslo"), ("denmark", "Europe/Copenhagen"), ("finland", "Europe/Helsinki"),
    ("poland", "Europe/Warsaw"), ("czechia", "Europe/Prague"), ("hungary", "Europe/Budapest"), ("greece", "Europe/Athens"),
    ("romania", "Europe/Bucharest"), ("cluj", "Europe/Bucharest"), ("timisoara", "Europe/Bucharest"), ("iasi", "Europe/Bucharest"),
    ("moldova", "Europe/Chisinau"), ("bulgaria", "Europe/Sofia"), ("serbia", "Europe/Belgrade"), ("ukraine", "Europe/Kyiv"),
    ("kiev", "Europe/Kyiv"), ("eet", "Europe/Bucharest"), ("eest", "Europe/Bucharest"), ("turkey", "Europe/Istanbul"),
    ("russia", "Europe/Moscow"), ("msk", "Europe/Moscow"),
    ("israel", "Asia/Jerusalem"), ("tel aviv", "Asia/Jerusalem"), ("uae", "Asia/Dubai"), ("abu dhabi", "Asia/Dubai"),
    ("india", "Asia/Kolkata"), ("ist", "Asia/Kolkata"), ("delhi", "Asia/Kolkata"), ("new delhi", "Asia/Kolkata"),
    ("mumbai", "Asia/Kolkata"), ("bangalore", "Asia/Kolkata"), ("bengaluru", "Asia/Kolkata"), ("chennai", "Asia/Kolkata"),
    ("china", "Asia/Shanghai"), ("beijing", "Asia/Shanghai"), ("shenzhen", "Asia/Shanghai"), ("hk", "Asia/Hong_Kong"),
    ("hkt", "Asia/Hong_Kong"), ("japan", "Asia/Tokyo"), ("jst", "Asia/Tokyo"), ("osaka", "Asia/Tokyo"), ("korea", "Asia/Seoul"),
    ("kst", "Asia/Seoul"), ("sgt", "Asia/Singapore"), ("thailand", "Asia/Bangkok"), ("vietnam", "Asia/Ho_Chi_Minh"),
    ("hanoi", "Asia/Ho_Chi_Minh"), ("saigon", "Asia/Ho_Chi_Minh"), ("philippines", "Asia/Manila"), ("indonesia", "Asia/Jakarta"),
    ("bali", "Asia/Makassar"), ("taiwan", "Asia/Taipei"), ("pakistan", "Asia/Karachi"), ("bangladesh", "Asia/Dhaka"),
    ("egypt", "Africa/Cairo"), ("nigeria", "Africa/Lagos"), ("kenya", "Africa/Nairobi"), ("south africa", "Africa/Johannesburg"),
    ("cape town", "Africa/Johannesburg"), ("morocco", "Africa/Casablanca"),
    ("australia", "Australia/Sydney"), ("aest", "Australia/Sydney"), ("canberra", "Australia/Sydney"),
    ("new zealand", "Pacific/Auckland"), ("nz", "Pacific/Auckland"), ("wellington", "Pacific/Auckland"),
    ("hawaii", "Pacific/Honolulu"), ("iceland", "Atlantic/Reykjavik"),
];

/// Every place name, lower case, to its zone: the database's cities ("new york") and the aliases.
fn places() -> &'static HashMap<String, Tz> {
    static PLACES: OnceLock<HashMap<String, Tz>> = OnceLock::new();
    PLACES.get_or_init(|| {
        let areas = ["Africa/", "America/", "Asia/", "Atlantic/", "Australia/", "Europe/", "Indian/", "Pacific/"];
        let mut map = HashMap::new();
        for tz in chrono_tz::TZ_VARIANTS {
            let name = tz.name();
            if areas.iter().any(|a| name.starts_with(a)) && let Some(city) = name.rsplit('/').next() {
                map.entry(city.replace('_', " ").to_lowercase()).or_insert(tz);
            }
        }
        for (alias, zone) in ALIASES {
            if let Ok(tz) = zone.parse::<Tz>() {
                map.insert(alias.to_string(), tz);
            }
        }
        map
    })
}

/// A place's label as typed ("New York", "PST") and its zone.
type Place = (String, Tz);

/// A place as typed ("New York", "pst") and its zone; the label keeps the typed words, capitalised.
fn place(words: &str) -> Option<Place> {
    let key = words.trim().trim_end_matches('?').trim().to_lowercase();
    let tz = *places().get(&key)?;
    let label = if key.len() <= 4 && !key.contains(' ') && ALIASES.iter().any(|(a, _)| *a == key) {
        key.to_uppercase()
    } else {
        key.split(' ').map(|w| w.chars().next().map(|c| c.to_uppercase().chain(w.chars().skip(1)).collect::<String>()).unwrap_or_default()).collect::<Vec<_>>().join(" ")
    };
    Some((label, tz))
}

/// "10am", "10 am", "10:30", "9:30pm", "22:15", "noon", "midnight". A bare hour needs am or pm.
fn clock(words: &[&str]) -> Option<(NaiveTime, usize)> {
    let first = words.first()?.to_lowercase();
    match first.as_str() {
        "noon" | "midday" => return Some((NaiveTime::from_hms_opt(12, 0, 0)?, 1)),
        "midnight" => return Some((NaiveTime::from_hms_opt(0, 0, 0)?, 1)),
        _ => {}
    }
    let (text, used) = match words.get(1).map(|w| w.to_lowercase()) {
        Some(ap) if ap == "am" || ap == "pm" => (format!("{first}{ap}"), 2),
        _ => (first, 1),
    };
    let (body, half) = if let Some(b) = text.strip_suffix("am") {
        (b, Some(false))
    } else if let Some(b) = text.strip_suffix("pm") {
        (b, Some(true))
    } else {
        (text.as_str(), None)
    };
    let (h, m) = match body.split_once(':') {
        Some((h, m)) if m.len() == 2 => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None if half.is_some() => (body.parse::<u32>().ok()?, 0),
        _ => return None,
    };
    let h = match half {
        Some(pm) if (1..=12).contains(&h) => h % 12 + if pm { 12 } else { 0 },
        Some(_) => return None,
        None => h,
    };
    Some((NaiveTime::from_hms_opt(h, m, 0)?, used))
}

/// Where "here" is: the system's zone, or a fixed zone in tests.
#[derive(Clone, Copy)]
pub enum Here {
    Local,
    Zone(Tz),
}

impl Here {
    fn offset(self, at: DateTime<Utc>) -> FixedOffset {
        match self {
            Here::Local => Local.offset_from_utc_datetime(&at.naive_utc()).fix(),
            Here::Zone(tz) => tz.offset_from_utc_datetime(&at.naive_utc()).fix(),
        }
    }

    /// The UTC moment of a wall-clock time today here.
    fn moment(self, t: NaiveTime, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let day = now.with_timezone(&self.offset(now)).date_naive();
        let local = day.and_time(t);
        Some(match self {
            Here::Local => Local.from_local_datetime(&local).earliest()?.with_timezone(&Utc),
            Here::Zone(tz) => tz.from_local_datetime(&local).earliest()?.with_timezone(&Utc),
        })
    }
}

fn hm(at: DateTime<FixedOffset>) -> String {
    at.format("%H:%M").to_string()
}

/// "8 h ahead", "5 h 30 min behind", "same time as you".
fn gap(there: FixedOffset, here: FixedOffset) -> String {
    let d = (there.local_minus_utc() - here.local_minus_utc()) / 60;
    if d == 0 {
        return "same time as you".into();
    }
    let (h, m) = (d.abs() / 60, d.abs() % 60);
    let size = if m == 0 { format!("{h} h") } else if h == 0 { format!("{m} min") } else { format!("{h} h {m} min") };
    format!("{size} {}", if d > 0 { "ahead" } else { "behind" })
}

/// ", next day" when `at` falls on another day than `base`.
fn day_note(at: DateTime<FixedOffset>, base: DateTime<FixedOffset>) -> &'static str {
    match at.date_naive().signed_duration_since(base.date_naive()).num_days() {
        d if d > 0 => ", next day",
        d if d < 0 => ", previous day",
        _ => "",
    }
}

fn utc_label(o: FixedOffset) -> String {
    let s = o.local_minus_utc();
    let (h, m) = (s.abs() / 3600, s.abs() % 3600 / 60);
    let sign = if s < 0 { '-' } else { '+' };
    if m == 0 { format!("UTC{sign}{h}") } else { format!("UTC{sign}{h}:{m:02}") }
}

pub fn answers(q: &str) -> Vec<Answer> {
    answers_at(q, Utc::now(), Here::Local)
}

pub fn answers_at(q: &str, now: DateTime<Utc>, here: Here) -> Vec<Answer> {
    answer_at(q, now, here).into_iter().collect()
}

fn answer_at(q: &str, now: DateTime<Utc>, here: Here) -> Option<Answer> {
    let q = q.trim().trim_end_matches('?').trim().to_lowercase();
    let here_now = now.with_timezone(&here.offset(now));
    // The time somewhere now: "time in tokyo", "tokyo time", "what time is it in tokyo".
    let asked = ["what time is it in ", "what's the time in ", "current time in ", "time in ", "time at ", "now in ", "time "]
        .iter()
        .find_map(|p| q.strip_prefix(p))
        .or_else(|| q.strip_suffix(" time").or_else(|| q.strip_suffix(" now")));
    if let Some((label, tz)) = asked.and_then(place) {
        let there = now.with_timezone(&tz.offset_from_utc_datetime(&now.naive_utc()).fix());
        let title = format!("{} in {label}", hm(there));
        let detail = format!("{}, {}, {}", there.format("%a %-d %b"), utc_label(*there.offset()), gap(*there.offset(), *here_now.offset()));
        return Some(Answer { title, detail, copy: hm(there) });
    }
    // A time to convert: "10am london" (there to here), "15:00 in sydney" (here to there), "9:30 pst to tokyo".
    let words: Vec<&str> = q.split_whitespace().collect();
    let (t, used) = clock(&words)?;
    let rest = words[used..].join(" ");
    let (from, to): (Option<Place>, Option<Place>) = if let Some(dest) = rest.strip_prefix("in ").or_else(|| rest.strip_prefix("to ")) {
        (None, Some(place(dest)?))
    } else {
        let split = [" to ", " in ", " into "].iter().find_map(|c| rest.split_once(c));
        match split {
            Some((a, b)) => (Some(place(a)?), Some(place(b)?)),
            None => (Some(place(&rest)?), None),
        }
    };
    // The moment the time names, then that moment where it is asked for.
    let moment = match &from {
        Some((_, tz)) => {
            let day = now.with_timezone(tz).date_naive();
            tz.from_local_datetime(&day.and_time(t)).earliest()?.with_timezone(&Utc)
        }
        None => here.moment(t, now)?,
    };
    let source = match &from {
        Some((_, tz)) => moment.with_timezone(&tz.offset_from_utc_datetime(&moment.naive_utc()).fix()),
        None => moment.with_timezone(&here.offset(moment)),
    };
    let (target, label) = match &to {
        Some((label, tz)) => (moment.with_timezone(&tz.offset_from_utc_datetime(&moment.naive_utc()).fix()), format!("in {label}")),
        None => (moment.with_timezone(&here.offset(moment)), "here".to_string()),
    };
    let source_label = from.as_ref().map_or("here".to_string(), |(l, _)| format!("in {l}"));
    let title = format!("{} {label}{}", hm(target), day_note(target, source));
    let detail = format!("{} {source_label}", hm(source));
    Some(Answer { title, detail, copy: hm(target) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(q: &str) -> Vec<(String, String)> {
        // 02.11.2026 09:05 UTC; "here" is Bucharest (UTC+2 in winter).
        let now = Utc.with_ymd_and_hms(2026, 11, 2, 9, 5, 0).unwrap();
        answers_at(q, now, Here::Zone(chrono_tz::Europe::Bucharest)).into_iter().map(|a| (a.title, a.detail)).collect()
    }

    #[test]
    fn time_somewhere_now() {
        assert_eq!(at("time in tokyo"), [("18:05 in Tokyo".into(), "Mon 2 Nov, UTC+9, 7 h ahead".into())]);
        assert_eq!(at("New York time?")[0].0, "04:05 in New York");
        assert_eq!(at("what time is it in pst")[0], ("01:05 in PST".into(), "Mon 2 Nov, UTC-8, 10 h behind".into()));
        assert_eq!(at("time in india")[0].1, "Mon 2 Nov, UTC+5:30, 3 h 30 min ahead");
        assert_eq!(at("time in bucharest")[0].1, "Mon 2 Nov, UTC+2, same time as you");
    }

    #[test]
    fn converting_a_time() {
        assert_eq!(at("10am london"), [("12:00 here".into(), "10:00 in London".into())]);
        assert_eq!(at("15:00 in sydney"), [("00:00 in Sydney, next day".into(), "15:00 here".into())]);
        assert_eq!(at("9:30 pm pst to tokyo")[0].0, "14:30 in Tokyo, next day");
        assert_eq!(at("noon nyc in berlin")[0].0, "18:00 in Berlin");
    }

    #[test]
    fn other_queries_are_not_time() {
        for q in ["tokyo", "10 london", "time machine", "25:00 london", "13pm paris", "notes", "10am atlantis", "5 km to mi"] {
            assert!(at(q).is_empty(), "{q}");
        }
    }
}
