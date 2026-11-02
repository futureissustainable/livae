//! Sound: the microphone and the output the system uses, chosen from a list in order, and the equaliser each output
//! gets. The platform code lists the devices, switches them and runs the equaliser; the choices live here.

use serde::{Deserialize, Serialize};

use crate::eq::{Design, Profile};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Sound {
    /// Output devices by name, first choice first. Whenever a device comes or goes, the first one connected becomes
    /// the system's output; with none connected (or the list empty), the system's own choice stays.
    pub outputs: Vec<String>,
    /// Microphones the same way.
    pub inputs: Vec<String>,
    /// The equaliser, on or off for every device at once.
    pub eq: bool,
    /// One per output device, plus one for every device (`device` empty) that comes first.
    pub profiles: Vec<Profile>,
}

/// The device to switch to: the first of `wanted` that is connected, or none (then nothing changes).
pub fn pick<'a>(wanted: &'a [String], connected: &[String]) -> Option<&'a str> {
    wanted.iter().find(|w| connected.iter().any(|c| c == *w)).map(String::as_str)
}

impl Sound {
    /// What `device` plays through: the profile for every device, then its own; none when the equaliser is off or
    /// both are flat. An error names the profile and its bad line.
    pub fn design_for(&self, device: &str) -> Result<Option<Design>, String> {
        if !self.eq {
            return Ok(None);
        }
        let chosen = self.chain(device);
        let design = Design::of(&chosen).map_err(|e| {
            let name = chosen.iter().find(|p| crate::eq::parse(&p.filters).is_err()).map(|p| p.device.as_str()).unwrap_or("");
            if name.is_empty() { format!("All devices: {e}") } else { format!("{name}: {e}") }
        })?;
        Ok((!design.is_flat()).then_some(design))
    }

    /// The profiles `device` plays through, in order: the one for every device, then its own ("" alone: the first).
    pub fn chain(&self, device: &str) -> Vec<&Profile> {
        let mut chosen: Vec<&Profile> = self.profiles.iter().filter(|p| p.device.is_empty()).take(1).collect();
        if !device.is_empty() {
            chosen.extend(self.profiles.iter().filter(|p| p.device == device).take(1));
        }
        chosen
    }

    /// The profile for `device` ("" for every device), made when missing.
    pub fn profile_mut(&mut self, device: &str) -> &mut Profile {
        if let Some(i) = self.profiles.iter().position(|p| p.device == device) {
            return &mut self.profiles[i];
        }
        self.profiles.push(Profile { device: device.to_string(), ..Profile::default() });
        self.profiles.last_mut().unwrap()
    }
}

/// The Equalizer APO config for the outputs in `devices` (name, endpoint GUID): each output its whole chain (the
/// profile for every device, then its own) under its own `Device:` line, matched by GUID, never by a name that
/// another device could share. Equalizer APO has no limiter, so the gain is held where the curve peaks at 0 dB:
/// a boost uses only the headroom the filters leave, and nothing clips.
pub fn equalizer_apo(sound: &Sound, devices: &[(String, String)]) -> String {
    let mut out = String::from("# Written by Livae from its settings ([sound]); edits here are replaced. Livae, Settings, Equaliser.\n");
    for (name, guid) in devices {
        let Ok(Some(mut design)) = sound.design_for(name) else { continue };
        let peak = design.peak_db(48000.0);
        if peak > 0.0 {
            design.gain_db -= peak;
        }
        let parametric = crate::eq::Parametric { preamp: design.gain_db, filters: design.filters };
        out.push_str(&format!("Device: {guid}\n"));
        for line in crate::eq::format(&parametric).lines() {
            // Equalizer APO numbers nothing: "Filter: ON PK ...".
            let line = match line.split_once(": ON ") {
                Some((head, rest)) if head.starts_with("Filter") => format!("Filter: ON {rest}"),
                _ => line.to_string(),
            };
            out.push_str(&line);
            out.push('\n');
        }
    }
    out.push_str("Device: all\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_first_connected_choice_wins_and_none_leaves_the_system_alone() {
        let wanted = names(&["Shure MV7", "MacBook Pro Microphone"]);
        assert_eq!(pick(&wanted, &names(&["MacBook Pro Microphone", "Shure MV7", "AirPods"])), Some("Shure MV7"));
        assert_eq!(pick(&wanted, &names(&["AirPods", "MacBook Pro Microphone"])), Some("MacBook Pro Microphone"));
        assert_eq!(pick(&wanted, &names(&["AirPods"])), None);
        assert_eq!(pick(&[], &names(&["AirPods"])), None);
    }

    #[test]
    fn each_device_gets_the_shared_profile_then_its_own() {
        let mut s = Sound { eq: true, ..Sound::default() };
        s.profile_mut("External Headphones").boost = 6.0;
        assert_eq!(s.design_for("External Headphones").unwrap().unwrap().gain_db, 6.0);
        assert!(s.design_for("MacBook Pro Speakers").unwrap().is_none(), "no profile: the sound is left alone");
        s.profile_mut("").boost = -1.0;
        assert_eq!(s.design_for("External Headphones").unwrap().unwrap().gain_db, 5.0);
        assert_eq!(s.design_for("MacBook Pro Speakers").unwrap().unwrap().gain_db, -1.0);
        s.profile_mut("MacBook Pro Speakers").filters = "Filter: ON ZZ Fc 1 Hz".into();
        assert!(s.design_for("MacBook Pro Speakers").unwrap_err().starts_with("MacBook Pro Speakers: Line 1"));
        s.eq = false;
        assert!(s.design_for("External Headphones").unwrap().is_none());
    }

    #[test]
    fn equalizer_apo_gets_each_chain_by_guid_and_never_clips() {
        let mut s = Sound { eq: true, ..Sound::default() };
        let hp = s.profile_mut("Headphones (Realtek(R) Audio)");
        hp.boost = 12.0;
        hp.filters = "Preamp: -3 dB\nFilter 1: ON PK Fc 1000 Hz Gain 4 dB Q 1".into();
        let devices = [("Headphones (Realtek(R) Audio)".to_string(), "{guid-hp}".to_string()), ("Speakers".to_string(), "{guid-sp}".to_string())];
        let text = equalizer_apo(&s, &devices);
        assert!(text.contains("Device: {guid-hp}\nPreamp: -4 dB\nFilter: ON PK Fc 1000 Hz Gain 4 dB Q 1\n"), "{text}");
        assert!(!text.contains("{guid-sp}") && text.ends_with("Device: all\n"));
        s.eq = false;
        assert!(!equalizer_apo(&s, &devices).contains("Device: {"));
    }
}
