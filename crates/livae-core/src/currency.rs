//! Money in the launcher ("100 eur to ron", "$20", "50 lei in usd") at the European Central Bank's daily reference
//! rates. They are fetched only after a money question, at most twice a day, and kept on disk, so answers also work
//! offline.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::answers::Answer;

pub const ECB_URL: &str = "https://www.ecb.europa.eu/stats/eurofxref/eurofxref-daily.xml";
const FILE: &str = "rates.json";
const STALE_SECONDS: i64 = 12 * 3600;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rates {
    /// The ECB's date for these rates, "2026-10-30".
    pub date: String,
    /// Units of each currency per euro (EUR itself is 1).
    pub per_eur: HashMap<String, f64>,
    /// When they were fetched, in Unix seconds.
    pub fetched: i64,
}

/// Words and signs for a currency code. Codes themselves ("chf", "sek") always work.
const NAMES: &[(&str, &str)] = &[
    ("€", "EUR"), ("euro", "EUR"), ("euros", "EUR"),
    ("$", "USD"), ("us$", "USD"), ("dollar", "USD"), ("dollars", "USD"),
    ("£", "GBP"), ("pound", "GBP"), ("pounds", "GBP"),
    ("¥", "JPY"), ("yen", "JPY"),
    ("lei", "RON"), ("leu", "RON"),
    ("franc", "CHF"), ("francs", "CHF"),
    ("yuan", "CNY"), ("rmb", "CNY"),
    ("₹", "INR"), ("rupee", "INR"), ("rupees", "INR"),
    ("forint", "HUF"), ("zloty", "PLN"), ("złoty", "PLN"), ("zł", "PLN"), ("kč", "CZK"),
    ("₩", "KRW"), ("won", "KRW"), ("rand", "ZAR"), ("₪", "ILS"), ("shekel", "ILS"), ("shekels", "ILS"),
    ("baht", "THB"), ("₺", "TRY"), ("lira", "TRY"), ("r$", "BRL"), ("reais", "BRL"),
    ("c$", "CAD"), ("a$", "AUD"), ("hk$", "HKD"), ("s$", "SGD"),
];

const CONNECTORS: &[&str] = &["to", "in", "into", "=", ">", "->", "as"];

/// The ECB's currencies, to tell a money question before any rates are saved.
const CODES: &[&str] = &[
    "EUR", "USD", "JPY", "BGN", "CZK", "DKK", "GBP", "HUF", "PLN", "RON", "SEK", "CHF", "ISK", "NOK", "TRY", "AUD", "BRL",
    "CAD", "CNY", "HKD", "IDR", "ILS", "INR", "KRW", "MXN", "MYR", "NZD", "PHP", "SGD", "THB", "ZAR",
];

/// The rates in an ECB daily file (`<Cube currency='USD' rate='1.0812'/>` under `<Cube time='2026-10-30'>`).
pub fn parse_ecb(xml: &str) -> Option<Rates> {
    fn attr<'a>(s: &'a str, name: &str) -> Option<&'a str> {
        let at = s.find(&format!("{name}="))? + name.len() + 1;
        let quote = s[at..].chars().next()?;
        let body = &s[at + 1..];
        Some(&body[..body.find(quote)?])
    }
    let date = attr(xml, "time")?.to_string();
    let mut per_eur = HashMap::from([("EUR".to_string(), 1.0)]);
    for cube in xml.split("<Cube").filter(|c| c.contains("currency=")) {
        let code = attr(cube, "currency")?.to_ascii_uppercase();
        let rate: f64 = attr(cube, "rate")?.parse().ok()?;
        if rate > 0.0 {
            per_eur.insert(code, rate);
        }
    }
    (per_eur.len() > 1).then_some(Rates { date, per_eur, fetched: 0 })
}

fn store() -> &'static Mutex<Option<Rates>> {
    static RATES: OnceLock<Mutex<Option<Rates>>> = OnceLock::new();
    RATES.get_or_init(|| Mutex::new(None))
}

/// Where the rates are kept; set by `load`. None: never fetch (a test copy, or `load` not called).
static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

/// The rates saved by an earlier run, if any. `fetch` false (a test copy) never goes online.
pub fn load_from(dir: &Path, fetch: bool) {
    let _ = DIR.set(fetch.then(|| dir.to_path_buf()));
    let saved = std::fs::read_to_string(dir.join(FILE)).ok().and_then(|t| serde_json::from_str::<Rates>(&t).ok());
    if let (Some(r), Ok(mut slot)) = (saved, store().lock()) {
        *slot = Some(r);
    }
}

/// Fetch fresh rates in the background when there are none or they are half a day old. Uses the system's curl, so
/// Livae needs no network library.
fn refresh(dir: &Path) {
    static BUSY: AtomicBool = AtomicBool::new(false);
    let now = chrono::Utc::now().timestamp();
    let fresh = store().lock().ok().and_then(|s| s.as_ref().map(|r| now - r.fetched < STALE_SECONDS)).unwrap_or(false);
    if fresh || BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    let dir = dir.to_path_buf();
    std::thread::spawn(move || {
        if let Some(mut r) = fetch().and_then(|xml| parse_ecb(&xml)) {
            r.fetched = now;
            if let Ok(json) = serde_json::to_vec(&r) {
                let _ = crate::write_atomic(&dir.join(FILE), &json);
            }
            if let Ok(mut slot) = store().lock() {
                *slot = Some(r);
            }
        }
        BUSY.store(false, Ordering::SeqCst);
    });
}

fn fetch() -> Option<String> {
    let mut cmd = std::process::Command::new(if cfg!(windows) { "curl.exe" } else { "/usr/bin/curl" });
    cmd.args(["-fsSL", "--max-time", "15", ECB_URL]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flashes up
    }
    let out = cmd.output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A number or a second currency next to a currency word: worth fetching rates for.
fn looks_like_money(q: &str) -> bool {
    let words = split(q);
    let currency = |w: &String| {
        let l = w.to_lowercase();
        NAMES.iter().any(|(n, _)| *n == l) || CODES.contains(&w.to_uppercase().as_str())
    };
    let n = words.iter().filter(|w| currency(w)).count();
    n >= 2 || (n == 1 && words.iter().any(|w| number(w).is_some()))
}

/// Answers for a money query with the saved rates. `home` is the currency to convert to when none is named. A money
/// question with no or old rates starts a fetch; its answer shows from the next keystroke.
pub fn answers(q: &str, home: &str) -> Vec<Answer> {
    if let Some(Some(dir)) = DIR.get()
        && looks_like_money(q)
    {
        refresh(dir);
    }
    match store().lock() {
        Ok(slot) => slot.as_ref().map(|r| answers_with(q, home, r)).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn code(word: &str, rates: &Rates) -> Option<String> {
    let w = word.to_lowercase();
    let code = NAMES.iter().find(|(n, _)| *n == w).map(|(_, c)| c.to_string()).unwrap_or_else(|| w.to_uppercase());
    rates.per_eur.contains_key(&code).then_some(code)
}

/// "€20" and "20eur" as "€ 20" and "20 eur": numbers and words apart.
fn split(q: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    for raw in q.split_whitespace() {
        let mut cur = String::new();
        let mut digits = None;
        for c in raw.chars() {
            let is_num = c.is_ascii_digit() || ((c == '.' || c == ',') && digits == Some(true));
            if digits.is_some_and(|d| d != is_num) && !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            digits = Some(is_num);
            cur.push(c);
        }
        if !cur.is_empty() {
            words.push(cur);
        }
    }
    words
}

fn number(word: &str) -> Option<f64> {
    if !word.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    // "1,5" is one and a half (as in the calculator); "1,000.50" drops the grouping.
    let w = if word.contains('.') { word.replace(',', "") } else { word.replace(',', ".") };
    w.parse().ok().filter(|v: &f64| v.is_finite())
}

fn format_money(v: f64) -> String {
    if v.abs() >= 1.0 || v == 0.0 {
        format!("{v:.2}")
    } else {
        let s = format!("{v:.6}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

pub fn answers_with(q: &str, home: &str, rates: &Rates) -> Vec<Answer> {
    let words = split(q.trim());
    // [sign] amount [currency] [to] [currency], or "eur to usd" for one unit.
    let mut i = 0;
    let mut from = None;
    if let Some(c) = words.first().and_then(|w| code(w, rates)) {
        from = Some(c);
        i = 1;
    }
    let amount = match words.get(i).and_then(|w| number(w)) {
        Some(v) => {
            i += 1;
            v
        }
        None if from.is_some() => 1.0,
        None => return Vec::new(),
    };
    if from.is_none() {
        from = words.get(i).and_then(|w| code(w, rates));
        if from.is_some() {
            i += 1;
        }
    }
    let Some(from) = from else { return Vec::new() };
    if words.get(i).is_some_and(|w| CONNECTORS.contains(&w.to_lowercase().as_str())) {
        i += 1;
    }
    let to = words.get(i).and_then(|w| code(w, rates));
    if to.is_some() {
        i += 1;
    }
    // Anything left over means this was not a money question ("5 usd cables").
    if i != words.len() || (to.is_none() && amount == 1.0 && number(&words[0]).is_none()) {
        return Vec::new();
    }
    let targets: Vec<String> = match to {
        Some(t) => vec![t],
        None => {
            let home = home.to_uppercase();
            let mut t: Vec<String> = Vec::new();
            for c in [home.as_str(), "USD", "EUR"] {
                if c != from && rates.per_eur.contains_key(c) && !t.iter().any(|x| x == c) {
                    t.push(c.to_string());
                }
            }
            t.truncate(2);
            t
        }
    };
    let date = chrono::NaiveDate::parse_from_str(&rates.date, "%Y-%m-%d").map(|d| d.format("%d.%m.%Y").to_string()).unwrap_or_else(|_| rates.date.clone());
    targets
        .into_iter()
        .map(|t| {
            let v = amount / rates.per_eur[&from] * rates.per_eur[&t];
            let value = format_money(v);
            Answer { title: format!("{value} {t}"), detail: format!("{} {from}, ECB rate of {date}", format_money(amount)), copy: value }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = "<gesmes:Envelope><Cube><Cube time='2026-10-30'><Cube currency='USD' rate='1.0800'/>\
        <Cube currency='RON' rate='4.9700'/><Cube currency='JPY' rate='160.00'/></Cube></Cube></gesmes:Envelope>";

    fn titles(q: &str) -> Vec<String> {
        answers_with(q, "RON", &parse_ecb(XML).unwrap()).into_iter().map(|a| a.title).collect()
    }

    #[test]
    fn parses_the_ecb_file() {
        let r = parse_ecb(XML).unwrap();
        assert_eq!((r.date.as_str(), r.per_eur["USD"], r.per_eur["EUR"], r.per_eur.len()), ("2026-10-30", 1.08, 1.0, 4));
        assert!(parse_ecb("<html>maintenance</html>").is_none());
    }

    #[test]
    fn money_queries() {
        assert_eq!(titles("100 eur to ron"), ["497.00 RON"]);
        assert_eq!(titles("100eur in usd"), ["108.00 USD"]);
        assert_eq!(titles("$20"), ["92.04 RON", "18.52 EUR"]);
        assert_eq!(titles("50 lei"), ["10.87 USD", "10.06 EUR"]);
        assert_eq!(titles("eur to jpy"), ["160.00 JPY"]);
        assert_eq!(titles("1,5 eur ron"), ["7.46 RON"]);
        let a = &answers_with("10 usd to eur", "RON", &parse_ecb(XML).unwrap())[0];
        assert_eq!((a.copy.as_str(), a.detail.as_str()), ("9.26", "10.00 USD, ECB rate of 30.10.2026"));
    }

    #[test]
    fn other_queries_are_not_money() {
        for q in ["usd", "5 km to mi", "5 usd cables", "notes", "2+2", "eur"] {
            assert!(titles(q).is_empty(), "{q}");
        }
    }

    #[test]
    fn only_money_questions_go_online() {
        for q in ["100 eur", "$5", "eur to usd", "20 lei in chf"] {
            assert!(looks_like_money(q), "{q}");
        }
        for q in ["eur", "time in tokyo", "5 km to mi", "notes 2026", "gh livae"] {
            assert!(!looks_like_money(q), "{q}");
        }
    }
}
