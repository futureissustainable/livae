//! What the launcher answers for a query, copied on Enter: maths, units, money and the time somewhere.

#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    /// The answer, shown large ("497.00 RON").
    pub title: String,
    /// What it is from ("100 EUR, ECB rate of 30.10.2026").
    pub detail: String,
    /// What Enter copies ("497.00").
    pub copy: String,
}

/// Every answer for `q`, best first. `currency` is the one to convert money to when none is named.
pub fn answers(q: &str, currency: &str) -> Vec<Answer> {
    let mut out = Vec::new();
    if let Some(v) = crate::calc::evaluate(q) {
        out.push(Answer { title: v.clone(), detail: String::new(), copy: v });
    }
    if let Some(results) = crate::units::convert_query(q) {
        out.extend(results.iter().take(3).map(|r| {
            let v = crate::units::format_number(r.target_value);
            Answer { title: format!("{v} {}", r.target_unit.name), detail: String::new(), copy: v }
        }));
    }
    out.extend(crate::currency::answers(q, currency));
    out.extend(crate::timezones::answers(q));
    out
}
