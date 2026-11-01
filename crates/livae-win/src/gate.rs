//! Firefox/Zen native messaging. Browser calls exit before the app or its single-instance mutex starts.
use std::io::{Read, Write};

use windows::Win32::System::Registry::*;
use windows::core::{HSTRING, PCWSTR};

use crate::config;

const HOST: &str = "com.mateus.livae";
const ADDONS: [&str; 3] = ["livae-for-zen@livae", "zen-gate@livae", "zen-gate@god.local"];

pub fn is_host_call() -> bool {
    std::env::args().skip(1).any(|a| ADDONS.contains(&a.as_str()) || a.ends_with(&format!("{HOST}.json")))
}

/// JSON strings, including Windows paths and quotes containing control characters.
pub fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn strings(s: &[String]) -> String {
    format!("[{}]", s.iter().map(|s| quote(s)).collect::<Vec<_>>().join(","))
}

fn gate_config() -> String {
    let gate = config::gate();
    let sites = gate
        .sites
        .iter()
        .map(|s| {
            format!(
                "{{\"id\":{},\"label\":{},\"domains\":{},\"seconds\":{},\"step\":{}}}",
                quote(&s.id),
                quote(&s.label),
                strings(&s.domains),
                s.seconds,
                s.step,
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let quotes = livae_core::breaks::load_quotes(&config::data_dir().join("quotes.md"))
        .iter()
        .map(|q| format!("{{\"text\":{},\"author\":{}}}", quote(&q.text), quote(q.author.as_deref().unwrap_or_default())))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{\"version\":1,\"pass_minutes\":{},\"sites\":[{sites}],\"quotes\":[{quotes}]}}", gate.pass_minutes)
}

/// Only the request's top-level string fields matter. Validate the full JSON, including skipped values.
/// No JSON crate is exposed by this crate's allowed dependency set.
struct Json<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Json<'_> {
    fn space(&mut self) {
        while self.bytes.get(self.at).is_some_and(|b| b" \r\n\t".contains(b)) {
            self.at += 1;
        }
    }
    fn take(&mut self, b: u8) -> bool {
        self.space();
        if self.bytes.get(self.at) == Some(&b) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn hex(&mut self) -> Option<u32> {
        let raw = self.bytes.get(self.at..self.at + 4)?;
        if !raw.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        let n = u32::from_str_radix(std::str::from_utf8(raw).ok()?, 16).ok()?;
        self.at += 4;
        Some(n)
    }
    fn string(&mut self) -> Option<String> {
        if !self.take(b'"') {
            return None;
        }
        let mut out = String::new();
        loop {
            let b = *self.bytes.get(self.at)?;
            self.at += 1;
            match b {
                b'"' => return Some(out),
                b'\\' => {
                    let escaped = *self.bytes.get(self.at)?;
                    self.at += 1;
                    out.push(match escaped {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let mut n = self.hex()?;
                            if (0xd800..=0xdbff).contains(&n) {
                                if self.bytes.get(self.at..self.at + 2)? != b"\\u" {
                                    return None;
                                }
                                self.at += 2;
                                let low = self.hex()?;
                                if !(0xdc00..=0xdfff).contains(&low) {
                                    return None;
                                }
                                n = 0x10000 + ((n - 0xd800) << 10) + low - 0xdc00;
                            }
                            char::from_u32(n)?
                        }
                        _ => return None,
                    });
                }
                0..=31 => return None,
                _ => {
                    self.at -= 1;
                    let tail = std::str::from_utf8(&self.bytes[self.at..]).ok()?;
                    let c = tail.chars().next()?;
                    self.at += c.len_utf8();
                    out.push(c);
                }
            }
        }
    }
    fn value(&mut self, depth: usize) -> Option<Option<String>> {
        if depth > 64 {
            return None;
        }
        self.space();
        match *self.bytes.get(self.at)? {
            b'"' => return Some(Some(self.string()?)),
            b'{' => {
                self.object(depth + 1)?;
            }
            b'[' => {
                self.at += 1;
                if !self.take(b']') {
                    loop {
                        self.value(depth + 1)?;
                        if self.take(b']') {
                            break;
                        }
                        if !self.take(b',') {
                            return None;
                        }
                    }
                }
            }
            b't' | b'f' | b'n' => {
                let word: &[u8] = match self.bytes[self.at] {
                    b't' => b"true",
                    b'f' => b"false",
                    _ => b"null",
                };
                if self.bytes.get(self.at..self.at + word.len())? != word {
                    return None;
                }
                self.at += word.len();
            }
            _ => {
                let start = self.at;
                if self.bytes.get(self.at) == Some(&b'-') {
                    self.at += 1;
                }
                if self.bytes.get(self.at) == Some(&b'0') {
                    self.at += 1;
                } else {
                    if !self.bytes.get(self.at).is_some_and(|b| (b'1'..=b'9').contains(b)) {
                        return None;
                    }
                    while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
                        self.at += 1;
                    }
                }
                for marker in *b".e" {
                    let present = self.bytes.get(self.at).is_some_and(|b| *b == marker || marker == b'e' && *b == b'E');
                    if present {
                        self.at += 1;
                        if marker == b'e' && self.bytes.get(self.at).is_some_and(|b| b"+-".contains(b)) {
                            self.at += 1;
                        }
                        let digits = self.at;
                        while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
                            self.at += 1;
                        }
                        if self.at == digits {
                            return None;
                        }
                    }
                }
                if self.at == start {
                    return None;
                }
            }
        }
        Some(None)
    }
    fn object(&mut self, depth: usize) -> Option<Option<String>> {
        if !self.take(b'{') {
            return None;
        }
        let mut get = None;
        if !self.take(b'}') {
            loop {
                let key = self.string()?;
                if !self.take(b':') {
                    return None;
                }
                let value = self.value(depth)?;
                if key == "get" {
                    get = value;
                }
                if self.take(b'}') {
                    break;
                }
                if !self.take(b',') {
                    return None;
                }
            }
        }
        Some(get)
    }
}

fn asks_gate(body: &[u8]) -> bool {
    let mut json = Json { bytes: body, at: 0 };
    let get = json.object(0);
    json.space();
    json.at == body.len() && get == Some(Some("gate".into()))
}

pub fn serve() {
    let (mut input, mut output) = (std::io::stdin().lock(), std::io::stdout().lock());
    let mut len = [0u8; 4];
    while input.read_exact(&mut len).is_ok() {
        let n = u32::from_le_bytes(len) as usize;
        if n > 1 << 20 {
            return;
        }
        let mut body = vec![0u8; n];
        if input.read_exact(&mut body).is_err() {
            return;
        }
        let out = if asks_gate(&body) { gate_config() } else { "{\"error\":\"unknown request\"}".into() };
        if output.write_all(&(out.len() as u32).to_le_bytes()).and_then(|_| output.write_all(out.as_bytes())).and_then(|_| output.flush()).is_err() {
            return;
        }
    }
}

/// Normal starts only. Firefox and Zen use the same per-user Mozilla host registration.
pub fn install_manifest() {
    if config::test_mode() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let file = config::data_dir().join(format!("{HOST}.json"));
    let manifest = format!(
        "{{\"name\":{},\"description\":\"Livae: Livae for Zen sites, waits and quotes\",\"path\":{},\"type\":\"stdio\",\"allowed_extensions\":{}}}",
        quote(HOST),
        quote(&exe.to_string_lossy()),
        strings(&ADDONS.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    );
    if std::fs::read_to_string(&file).ok().as_deref() != Some(&manifest) && livae_core::write_atomic(&file, manifest.as_bytes()).is_err() {
        return;
    }
    let path = HSTRING::from(format!("Software\\Mozilla\\NativeMessagingHosts\\{HOST}"));
    let value: Vec<u16> = file.as_os_str().to_string_lossy().encode_utf16().chain(Some(0)).collect();
    unsafe {
        let mut current = vec![0u16; value.len()];
        let mut size = (current.len() * 2) as u32;
        let got = RegGetValueW(HKEY_CURRENT_USER, &path, PCWSTR::null(), RRF_RT_REG_SZ, None, Some(current.as_mut_ptr().cast()), Some(&mut size));
        if got.is_ok() && size as usize == value.len() * 2 && current == value {
            return;
        }
        let mut key = HKEY::default();
        if RegCreateKeyExW(HKEY_CURRENT_USER, &path, None, PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, None, &mut key, None).is_ok() {
            let bytes = std::slice::from_raw_parts(value.as_ptr().cast::<u8>(), value.len() * 2);
            let _ = RegSetValueExW(key, PCWSTR::null(), None, REG_SZ, Some(bytes));
            let _ = RegCloseKey(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_requests_validate_json_and_only_read_the_top_level_get() {
        for request in [r#"{"get":"gate"}"#, r#" { "get" : "g\u0061te", "extra": [null, true, -2.5e+3, {"x":"\ud83d\ude00"}] } "#] {
            assert!(asks_gate(request.as_bytes()), "{request}");
        }
        for request in [
            r#"{"nested":{"get":"gate"}}"#,
            r#"{"get":"g ate"}"#,
            r#"{"get":"gate",}"#,
            r#"{"get":"gate"}junk"#,
            r#"{"get":"gate","n":01}"#,
            r#"{"get":"gate","s":"\ud800"}"#,
        ] {
            assert!(!asks_gate(request.as_bytes()), "{request}");
        }
        assert_eq!(quote("C:\\name\n\"é\""), "\"C:\\\\name\\u000a\\\"é\\\"\"");
    }
}
