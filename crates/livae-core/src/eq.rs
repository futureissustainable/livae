//! The equaliser: a 10-band EQ, parametric filters in the AutoEq / Equalizer APO text form, a boost, and a limiter
//! that keeps a boosted signal from clipping. The same chain as the CamillaDSP setup it replaces (itself a port of
//! SoundSource's Output chain), sample for sample: RBJ cookbook biquads in transposed direct form II, in f64.
//!
//! `Design` is what a device's sound goes through (the profile for every device, then the device's own);
//! `Chain` runs it on audio without allocating, for the real-time audio thread.

use serde::{Deserialize, Serialize};

/// The 10-band EQ's centres in Hz, one octave apart, each a peak filter of `BAND_Q` (SoundSource's bands).
pub const BANDS: [f64; 10] = [32.0, 64.0, 128.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0];
pub const BAND_Q: f64 = 1.414;
/// The 10-band EQ and the boost go this far each way, in dB.
pub const BAND_RANGE: f64 = 12.0;
pub const BOOST_MAX: f64 = 18.0;

/// One equaliser: for one output device by name, or for every device when `device` is empty.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Profile {
    /// The output device's name as the system shows it; empty: every device (under the device's own profile).
    pub device: String,
    /// The 10-band EQ in dB, 32 Hz to 16 kHz.
    pub bands: [f64; 10],
    /// Parametric filters as AutoEq writes them: "Preamp: -4.9 dB", "Filter 1: ON PK Fc 133 Hz Gain -3.6 dB Q 0.36".
    pub filters: String,
    /// Gain after the equaliser in dB; the limiter keeps it from clipping.
    pub boost: f64,
}

impl Default for Profile {
    fn default() -> Self {
        Self { device: String::new(), bands: [0.0; 10], filters: String::new(), boost: 0.0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Peak,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Filter {
    pub kind: Kind,
    pub freq: f64,
    /// dB; unused by the passes.
    pub gain: f64,
    pub q: f64,
}

/// A parametric filter list with its preamp, as read from (and written to) the AutoEq text form.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Parametric {
    pub preamp: f64,
    pub filters: Vec<Filter>,
}

/// Read AutoEq / Equalizer APO text: `Preamp: x dB` and `Filter n: ON <type> Fc x Hz [Gain x dB] [Q x | BW Oct x]`
/// with types PK, LSC, HSC, LS, HS, LP, HP, LPQ, HPQ (LS, HS, LP and HP without Q take 0.707). Filters marked OFF,
/// blank lines and `#` comments are skipped; anything else is an error naming its line.
pub fn parse(text: &str) -> Result<Parametric, String> {
    let mut out = Parametric::default();
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let bad = |why: &str| Err(format!("Line {}: {why}", n + 1));
        let Some((head, rest)) = line.split_once(':') else { return bad("expected \"Preamp:\" or \"Filter:\"") };
        let words: Vec<&str> = rest.split_whitespace().collect();
        let head = head.trim().to_ascii_lowercase();
        if head == "preamp" {
            match words.first().and_then(|w| w.parse::<f64>().ok()) {
                Some(db) => out.preamp += db,
                None => return bad("the preamp needs a number of dB"),
            }
            continue;
        }
        if !head.starts_with("filter") {
            return bad("expected \"Preamp:\" or \"Filter:\"");
        }
        let mut i = 0;
        match words.first().map(|w| w.to_ascii_uppercase()) {
            Some(w) if w == "OFF" => continue,
            Some(w) if w == "ON" => i = 1,
            _ => {}
        }
        let Some(kind) = words.get(i).map(|w| w.to_ascii_uppercase()) else { return bad("missing the filter type") };
        let value = |key: &str| -> Option<f64> {
            let at = words.iter().position(|w| w.eq_ignore_ascii_case(key))?;
            words.get(at + 1)?.parse().ok()
        };
        let Some(freq) = value("Fc").filter(|f| *f > 0.0) else { return bad("missing \"Fc\" in Hz") };
        let gain = value("Gain").unwrap_or(0.0);
        let q = match (value("Q"), words.iter().position(|w| w.eq_ignore_ascii_case("BW"))) {
            (Some(q), _) => q,
            // "BW Oct 1.0": the bandwidth in octaves.
            (None, Some(at)) => match words.get(at + 2).and_then(|w| w.parse::<f64>().ok()) {
                Some(bw) => (2f64.powf(bw)).sqrt() / (2f64.powf(bw) - 1.0),
                None => return bad("\"BW Oct\" needs a number"),
            },
            (None, None) => std::f64::consts::FRAC_1_SQRT_2,
        };
        if q <= 0.0 {
            return bad("Q must be above 0");
        }
        let kind = match kind.as_str() {
            "PK" | "PEQ" => Kind::Peak,
            "LSC" | "LS" => Kind::LowShelf,
            "HSC" | "HS" => Kind::HighShelf,
            "LP" | "LPQ" => Kind::LowPass,
            "HP" | "HPQ" => Kind::HighPass,
            other => return bad(&format!("unknown filter type \"{other}\" (PK, LSC, HSC, LP or HP)")),
        };
        out.filters.push(Filter { kind, freq, gain, q });
    }
    Ok(out)
}

/// Write `p` in the AutoEq text form `parse` reads.
pub fn format(p: &Parametric) -> String {
    let mut s = format!("Preamp: {} dB\n", round(p.preamp));
    for (n, f) in p.filters.iter().enumerate() {
        let kind = match f.kind {
            Kind::Peak => "PK",
            Kind::LowShelf => "LSC",
            Kind::HighShelf => "HSC",
            Kind::LowPass => "LPQ",
            Kind::HighPass => "HPQ",
        };
        let gain = if matches!(f.kind, Kind::LowPass | Kind::HighPass) { String::new() } else { format!(" Gain {} dB", round(f.gain)) };
        s.push_str(&format!("Filter {}: ON {kind} Fc {} Hz{gain} Q {}\n", n + 1, round(f.freq), round(f.q)));
    }
    s
}

fn round(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// Normalised biquad coefficients (a0 = 1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coeffs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Filter {
    /// RBJ Audio EQ Cookbook coefficients at `rate`; none at or above Nyquist, where the filter cannot exist.
    pub fn coeffs(&self, rate: f64) -> Option<Coeffs> {
        if !(self.freq > 0.0 && self.freq < rate / 2.0 && self.q > 0.0) {
            return None;
        }
        let w = 2.0 * std::f64::consts::PI * self.freq / rate;
        let (sn, cs) = w.sin_cos();
        let alpha = sn / (2.0 * self.q);
        let a = 10f64.powf(self.gain / 40.0);
        let (b0, b1, b2, a0, a1, a2) = match self.kind {
            Kind::Peak => (1.0 + alpha * a, -2.0 * cs, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cs, 1.0 - alpha / a),
            Kind::LowShelf => {
                let beta = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cs + beta),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cs),
                    a * ((a + 1.0) - (a - 1.0) * cs - beta),
                    (a + 1.0) + (a - 1.0) * cs + beta,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cs),
                    (a + 1.0) + (a - 1.0) * cs - beta,
                )
            }
            Kind::HighShelf => {
                let beta = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cs + beta),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cs),
                    a * ((a + 1.0) + (a - 1.0) * cs - beta),
                    (a + 1.0) - (a - 1.0) * cs + beta,
                    2.0 * ((a - 1.0) - (a + 1.0) * cs),
                    (a + 1.0) - (a - 1.0) * cs - beta,
                )
            }
            Kind::LowPass => ((1.0 - cs) / 2.0, 1.0 - cs, (1.0 - cs) / 2.0, 1.0 + alpha, -2.0 * cs, 1.0 - alpha),
            Kind::HighPass => ((1.0 + cs) / 2.0, -(1.0 + cs), (1.0 + cs) / 2.0, 1.0 + alpha, -2.0 * cs, 1.0 - alpha),
        };
        Some(Coeffs { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 })
    }
}

impl Coeffs {
    /// The gain in dB at `freq` Hz.
    pub fn db_at(&self, freq: f64, rate: f64) -> f64 {
        let w = 2.0 * std::f64::consts::PI * freq / rate;
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        let (nr, ni) = (self.b0 + self.b1 * c1 + self.b2 * c2, -(self.b1 * s1 + self.b2 * s2));
        let (dr, di) = (1.0 + self.a1 * c1 + self.a2 * c2, -(self.a1 * s1 + self.a2 * s2));
        10.0 * ((nr * nr + ni * ni) / (dr * dr + di * di)).log10()
    }
}

/// Everything one device's sound goes through: filters in order, then one gain (preamps and boosts together),
/// then the limiter when the result can pass 0 dB.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Design {
    pub filters: Vec<Filter>,
    pub gain_db: f64,
}

impl Design {
    /// The profiles in order (the one for every device first); an error names the profile's bad line.
    pub fn of(profiles: &[&Profile]) -> Result<Self, String> {
        let mut d = Design::default();
        for p in profiles {
            for (freq, gain) in BANDS.iter().zip(p.bands) {
                if gain != 0.0 {
                    d.filters.push(Filter { kind: Kind::Peak, freq: *freq, gain, q: BAND_Q });
                }
            }
            let parametric = parse(&p.filters)?;
            d.filters.extend(parametric.filters);
            d.gain_db += parametric.preamp + p.boost;
        }
        Ok(d)
    }

    /// Nothing to do: no filter and no gain.
    pub fn is_flat(&self) -> bool {
        self.filters.iter().all(|f| matches!(f.kind, Kind::Peak | Kind::LowShelf | Kind::HighShelf) && f.gain == 0.0) && self.gain_db == 0.0
    }

    /// The whole chain's gain in dB at `freq` Hz, before the limiter.
    pub fn db_at(&self, freq: f64, rate: f64) -> f64 {
        self.gain_db + self.filters.iter().filter_map(|f| f.coeffs(rate)).map(|c| c.db_at(freq, rate)).sum::<f64>()
    }

    /// The highest gain from 20 Hz to 20 kHz, on a 1/48-octave grid.
    pub fn peak_db(&self, rate: f64) -> f64 {
        (0..=480).map(|i| 20.0 * 2f64.powf(i as f64 / 48.0)).filter(|f| *f < rate / 2.0).map(|f| self.db_at(f, rate)).fold(f64::MIN, f64::max)
    }
}

/// CamillaDSP's compressor as the replaced setup ran it: loudness of the channels' sum, attack 2 ms, release
/// 200 ms, above -2 dB a ratio of 20, then a hard ceiling at 0 dB.
#[derive(Clone, Debug)]
struct Limiter {
    attack: f64,
    release: f64,
    loudness: f64,
}

const LIMIT_THRESHOLD: f64 = -2.0;
const LIMIT_RATIO: f64 = 20.0;

impl Limiter {
    fn new(rate: f64) -> Self {
        // It starts from silence, so the first sound is not turned down.
        Self { attack: (-1.0 / (rate * 0.002)).exp(), release: (-1.0 / (rate * 0.2)).exp(), loudness: -100.0 }
    }

    /// The gain for one frame whose channels sum to `sum`.
    fn gain(&mut self, sum: f64) -> f64 {
        let level = 20.0 * (sum.abs() + 1.0e-9).log10();
        let k = if level >= self.loudness { self.attack } else { self.release };
        self.loudness = k * self.loudness + (1.0 - k) * level;
        if self.loudness > LIMIT_THRESHOLD {
            10f64.powf(-(self.loudness - LIMIT_THRESHOLD) * (LIMIT_RATIO - 1.0) / LIMIT_RATIO / 20.0)
        } else {
            1.0
        }
    }
}

/// The most channels a chain processes; any others pass through untouched.
pub const MAX_CHANNELS: usize = 8;

/// A `Design` ready to run at one sample rate. `process` never allocates or locks.
#[derive(Clone, Debug)]
pub struct Chain {
    coeffs: Vec<Coeffs>,
    /// Per stage and channel: the two delays of the transposed direct form II.
    state: Vec<[[f64; 2]; MAX_CHANNELS]>,
    gain: f64,
    limiter: Option<Limiter>,
    pub rate: f64,
}

impl Chain {
    pub fn new(design: &Design, rate: f64) -> Self {
        let coeffs: Vec<Coeffs> = design.filters.iter().filter_map(|f| f.coeffs(rate)).collect();
        let state = vec![[[0.0; 2]; MAX_CHANNELS]; coeffs.len()];
        let limiter = (design.peak_db(rate) > 0.0).then(|| Limiter::new(rate));
        Self { coeffs, state, gain: 10f64.powf(design.gain_db / 20.0), limiter, rate }
    }

    /// Carry on from `old` (the same filters, edited): its delays and limiter level, so a change never clicks.
    pub fn continue_from(&mut self, old: &Chain) {
        if old.state.len() == self.state.len() && old.rate == self.rate {
            self.state.copy_from_slice(&old.state);
        }
        if let (Some(new), Some(old)) = (self.limiter.as_mut(), old.limiter.as_ref()) {
            new.loudness = old.loudness;
        }
    }

    /// Process interleaved `frames` of `channels` channels in place.
    pub fn process(&mut self, frames: &mut [f32], channels: usize) {
        let used = channels.min(MAX_CHANNELS);
        if channels == 0 {
            return;
        }
        for frame in frames.chunks_exact_mut(channels) {
            let mut sum = 0.0;
            for (ch, sample) in frame.iter_mut().enumerate().take(used) {
                let mut x = *sample as f64;
                for (c, s) in self.coeffs.iter().zip(self.state.iter_mut()) {
                    let z = &mut s[ch];
                    let y = c.b0 * x + z[0];
                    z[0] = c.b1 * x - c.a1 * y + z[1];
                    z[1] = c.b2 * x - c.a2 * y;
                    x = y;
                }
                x *= self.gain;
                sum += x;
                *sample = x as f32;
            }
            if let Some(l) = self.limiter.as_mut() {
                let g = l.gain(sum);
                for sample in frame.iter_mut().take(used) {
                    *sample = (*sample as f64 * g).clamp(-1.0, 1.0) as f32;
                }
            }
        }
        // Silence decays the delays into subnormal numbers, which some CPUs handle very slowly.
        for z in self.state.iter_mut().flatten().flatten() {
            if z.abs() < 1.0e-25 {
                *z = 0.0;
            }
        }
    }
}

/// Copy interleaved `src` (`m` channels) into interleaved `dst` (`n` channels) for the equaliser's output: a mono
/// source feeds every channel, a wider one its own channels, the rest are silent; frames past the shorter of the two
/// are silent too. Returns the frames copied. For the real-time thread: no allocation.
pub fn route(src: &[f32], m: usize, dst: &mut [f32], n: usize) -> usize {
    if m == 0 || n == 0 {
        dst.fill(0.0);
        return 0;
    }
    let frames = (src.len() / m).min(dst.len() / n);
    for f in 0..frames {
        for c in 0..n {
            dst[f * n + c] = if m == 1 { src[f] } else if c < m { src[f * m + c] } else { 0.0 };
        }
    }
    dst[frames * n..].fill(0.0);
    frames
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48000.0;

    #[test]
    fn the_tap_reaches_every_output_channel_it_should() {
        let mut out = [9.0f32; 8];
        assert_eq!(route(&[0.1, 0.2, 0.3, 0.4], 2, &mut out, 4), 2);
        assert_eq!(out, [0.1, 0.2, 0.0, 0.0, 0.3, 0.4, 0.0, 0.0], "stereo into four channels");
        let mut out = [9.0f32; 6];
        assert_eq!(route(&[0.5, 0.6], 1, &mut out, 2), 2);
        assert_eq!(out, [0.5, 0.5, 0.6, 0.6, 0.0, 0.0], "mono feeds both, the frames it lacks are silent");
        let mut out = [9.0f32; 2];
        assert_eq!(route(&[0.1, 0.2, 0.3], 3, &mut out, 2), 1);
        assert_eq!(out, [0.1, 0.2]);
    }

    #[test]
    fn autoeq_text_reads_and_writes_back() {
        let text = "Preamp: -4.9 dB\n# Meze\nFilter 1: ON LSC Fc 105 Hz Gain 3.9 dB Q 0.70\nFilter 2: OFF PK Fc 1 Hz Gain 9 dB Q 1\n\
                    Filter 3: ON PK Fc 1792 Hz Gain 5.1 dB Q 2.29\nFilter 4: ON HPQ Fc 20 Hz Q 0.5\nFilter 5: ON PK Fc 1000 Hz Gain 1 dB BW Oct 1\n";
        let p = parse(text).unwrap();
        assert_eq!(p.preamp, -4.9);
        assert_eq!(p.filters.len(), 4);
        assert_eq!(p.filters[0], Filter { kind: Kind::LowShelf, freq: 105.0, gain: 3.9, q: 0.7 });
        assert!((p.filters[3].q - 1.414).abs() < 0.001);
        assert_eq!(parse(&format(&p)).unwrap(), Parametric { filters: p.filters.iter().map(|f| Filter { q: round(f.q), ..*f }).collect(), ..p.clone() });
        assert_eq!(parse("Filter 1: ON XX Fc 100 Hz").unwrap_err(), "Line 1: unknown filter type \"XX\" (PK, LSC, HSC, LP or HP)");
        assert!(parse("Preamp: loud").unwrap_err().starts_with("Line 1:"));
    }

    #[test]
    fn filters_shape_the_response_as_drawn() {
        let peak = Filter { kind: Kind::Peak, freq: 1000.0, gain: 6.0, q: 1.0 }.coeffs(RATE).unwrap();
        assert!((peak.db_at(1000.0, RATE) - 6.0).abs() < 1e-9 && peak.db_at(20.0, RATE).abs() < 0.01);
        let shelf = Filter { kind: Kind::LowShelf, freq: 105.0, gain: 3.9, q: 0.7 }.coeffs(RATE).unwrap();
        assert!((shelf.db_at(10.0, RATE) - 3.9).abs() < 0.05 && shelf.db_at(10000.0, RATE).abs() < 0.01);
        // Above Nyquist a filter cannot exist: skipped, not a broken chain.
        assert!(Filter { kind: Kind::Peak, freq: 30000.0, gain: 3.0, q: 1.0 }.coeffs(RATE).is_none());
    }

    #[test]
    fn a_flat_profile_leaves_the_sound_as_it_is() {
        let d = Design::of(&[&Profile::default()]).unwrap();
        assert!(d.is_flat());
        let mut chain = Chain::new(&d, RATE);
        let mut audio: Vec<f32> = (0..960).map(|i| ((i as f32) * 0.05).sin() * 0.9).collect();
        let before = audio.clone();
        chain.process(&mut audio, 2);
        assert_eq!(audio, before);
    }

    #[test]
    fn a_boost_never_clips() {
        let profile = Profile { boost: 12.0, ..Profile::default() };
        let d = Design::of(&[&profile]).unwrap();
        let mut chain = Chain::new(&d, RATE);
        // One second of a full-scale 1 kHz tone, boosted 12 dB: the limiter holds it at 0 dB at most.
        let mut audio: Vec<f32> = (0..48000).flat_map(|i| {
            let s = (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 48000.0).sin();
            [s, s]
        }).collect();
        chain.process(&mut audio, 2);
        assert!(audio.iter().all(|s| s.abs() <= 1.0));
        let tail = &audio[48000..];
        let peak = tail.iter().fold(0f32, |m, s| m.max(s.abs()));
        // The channels' sum sets the level (+6 dB for a centred tone), so it settles near -7 dB: lower, not muted.
        assert!(peak > 0.3, "the limiter rides the level, it does not mute ({peak})");
    }

    #[test]
    fn profiles_stack_for_every_device_then_the_device() {
        let all = Profile { bands: [0.0, 0.0, 0.0, 0.0, 0.0, 3.0, 0.0, 0.0, 0.0, 0.0], ..Profile::default() };
        let meze = Profile { device: "External Headphones".into(), filters: "Preamp: -4 dB\nFilter 1: ON PK Fc 1000 Hz Gain 3 dB Q 1.414".into(), boost: 2.0, ..Profile::default() };
        let d = Design::of(&[&all, &meze]).unwrap();
        assert_eq!(d.filters.len(), 2);
        assert_eq!(d.gain_db, -2.0);
        assert!((d.db_at(1000.0, RATE) - 4.0).abs() < 1e-9);
    }
}
