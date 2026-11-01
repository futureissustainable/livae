//! The launcher's calculator: a query with digits and an operator is evaluated as maths.

/// The answer as text ("14", "2.5"), or None when the query is not maths.
pub fn evaluate(q: &str) -> Option<String> {
    let q = q.trim().replace('×', "*").replace('÷', "/").replace(',', ".");
    if !q.chars().any(|c| c.is_ascii_digit()) || !q.chars().any(|c| "+-*/^%()".contains(c)) {
        return None;
    }
    let value = evalexpr::eval(&q).ok()?;
    match value {
        evalexpr::Value::Int(n) => Some(n.to_string()),
        evalexpr::Value::Float(f) if f.is_finite() => {
            let s = format!("{f:.10}");
            Some(s.trim_end_matches('0').trim_end_matches('.').to_string())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::evaluate;

    #[test]
    fn maths_only() {
        assert_eq!(evaluate("2+3*4").as_deref(), Some("14"));
        assert_eq!(evaluate("10/4.0").as_deref(), Some("2.5"));
        assert_eq!(evaluate("3 × 4").as_deref(), Some("12"));
        assert_eq!(evaluate("1,5*2").as_deref(), Some("3"));
        // A plain number or a word is a search, not maths.
        assert_eq!(evaluate("2024"), None);
        assert_eq!(evaluate("notepad"), None);
        assert_eq!(evaluate("1/0.0"), None);
    }
}
