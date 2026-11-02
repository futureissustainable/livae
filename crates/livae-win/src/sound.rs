//! Sound on Windows: keeps the chosen output and microphone in use, and hands the equaliser to Equalizer APO.
//!
//! Windows has no public way to change the default device; like SoundSwitch and EarTrumpet, Livae uses the
//! interface Windows' own Sound settings use (IPolicyConfig), for every role (console, media, communications).
//! Windows runs no equaliser over a whole output without an audio driver, so Livae writes its profiles for
//! Equalizer APO (free, open source) when it is installed: each output by its endpoint GUID, with the gain held so
//! the curve never passes 0 dB (Equalizer APO has no limiter).

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::path::PathBuf;

use livae_core::settings::Settings;
use livae_core::sound::{equalizer_apo, pick};
use windows::core::{implement, interface, IUnknown, IUnknown_Vtbl, GUID, HRESULT, HSTRING, PCWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    eCapture, eCommunications, eConsole, eMultimedia, eRender, EDataFlow, ERole, IMMDevice, IMMDeviceEnumerator, IMMNotificationClient,
    IMMNotificationClient_Impl, MMDeviceEnumerator, DEVICE_STATE, DEVICE_STATE_ACTIVE, DEVICE_STATE_UNPLUGGED,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, STGM_READ};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6464KEY};

use crate::{app, config};

/// Windows' own default-device switch, undocumented but unchanged since Windows 7; its methods keep their COM names.
#[allow(non_snake_case)]
mod policy {
    use super::*;

    #[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
    pub unsafe trait IPolicyConfig: IUnknown {
        fn GetMixFormat(&self, id: PCWSTR, format: *mut *mut c_void) -> HRESULT;
        fn GetDeviceFormat(&self, id: PCWSTR, default: i32, format: *mut *mut c_void) -> HRESULT;
        fn ResetDeviceFormat(&self, id: PCWSTR) -> HRESULT;
        fn SetDeviceFormat(&self, id: PCWSTR, endpoint: *mut c_void, mix: *mut c_void) -> HRESULT;
        fn GetProcessingPeriod(&self, id: PCWSTR, default: i32, period: *mut i64, minimum: *mut i64) -> HRESULT;
        fn SetProcessingPeriod(&self, id: PCWSTR, period: *mut i64) -> HRESULT;
        fn GetShareMode(&self, id: PCWSTR, mode: *mut c_void) -> HRESULT;
        fn SetShareMode(&self, id: PCWSTR, mode: *mut c_void) -> HRESULT;
        fn GetPropertyValue(&self, id: PCWSTR, key: *const PROPERTYKEY, value: *mut c_void) -> HRESULT;
        fn SetPropertyValue(&self, id: PCWSTR, key: *const PROPERTYKEY, value: *mut c_void) -> HRESULT;
        fn SetDefaultEndpoint(&self, id: PCWSTR, role: ERole) -> HRESULT;
        fn SetEndpointVisibility(&self, id: PCWSTR, visible: i32) -> HRESULT;
    }

    /// Make `id` the device for every role, as Windows' Sound settings do.
    pub fn use_device(id: &str) -> bool {
        let Ok(policy) = (unsafe { CoCreateInstance::<_, IPolicyConfig>(&POLICY_CONFIG, None, CLSCTX_ALL) }) else { return false };
        let id = HSTRING::from(id);
        [eConsole, eMultimedia, eCommunications].iter().all(|role| unsafe { policy.SetDefaultEndpoint(PCWSTR(id.as_ptr()), *role) }.is_ok())
    }
}
pub use policy::use_device;

const POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    /// The endpoint id, "{0.0.0.00000000}.{guid}".
    pub id: String,
    pub name: String,
    pub connected: bool,
}

impl Device {
    /// The endpoint GUID Equalizer APO matches devices by.
    fn guid(&self) -> &str {
        self.id.rsplit('.').next().unwrap_or(&self.id)
    }
}

fn enumerator() -> Option<IMMDeviceEnumerator> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok() }
}

fn id_of(device: &IMMDevice) -> Option<String> {
    unsafe {
        let p = device.GetId().ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const c_void));
        s
    }
}

fn name_of(device: &IMMDevice) -> Option<String> {
    unsafe {
        let store = device.OpenPropertyStore(STGM_READ).ok()?;
        let value = store.GetValue(&PKEY_Device_FriendlyName).ok()?;
        let p = PropVariantToStringAlloc(&value).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const c_void));
        s
    }
}

/// Outputs (`eRender`) or microphones (`eCapture`): the connected ones, and the unplugged ones Windows still
/// knows (their GUIDs let Equalizer APO's profile wait for them).
pub fn devices(flow: EDataFlow) -> Vec<Device> {
    let Some(e) = enumerator() else { return Vec::new() };
    let Ok(list) = (unsafe { e.EnumAudioEndpoints(flow, DEVICE_STATE(DEVICE_STATE_ACTIVE.0 | DEVICE_STATE_UNPLUGGED.0)) }) else { return Vec::new() };
    let n = unsafe { list.GetCount() }.unwrap_or(0);
    (0..n)
        .filter_map(|i| {
            let d = unsafe { list.Item(i) }.ok()?;
            let connected = unsafe { d.GetState() }.is_ok_and(|s| s == DEVICE_STATE_ACTIVE);
            Some(Device { id: id_of(&d)?, name: name_of(&d)?, connected })
        })
        .collect()
}

pub fn default_id(flow: EDataFlow) -> Option<String> {
    let e = enumerator()?;
    let d = unsafe { e.GetDefaultAudioEndpoint(flow, eConsole) }.ok()?;
    id_of(&d)
}

/// Device changes arrive on a Windows audio thread; the work happens on the main thread.
#[implement(IMMNotificationClient)]
struct Watcher;

/// Devices coming and going, and any switch of the default (by Windows, an app or a person).
impl IMMNotificationClient_Impl for Watcher_Impl {
    fn OnDeviceStateChanged(&self, _id: &PCWSTR, _state: DEVICE_STATE) -> windows::core::Result<()> {
        app::on_main(schedule);
        Ok(())
    }
    fn OnDeviceAdded(&self, _id: &PCWSTR) -> windows::core::Result<()> {
        app::on_main(schedule);
        Ok(())
    }
    fn OnDeviceRemoved(&self, _id: &PCWSTR) -> windows::core::Result<()> {
        app::on_main(schedule);
        Ok(())
    }
    fn OnDefaultDeviceChanged(&self, _flow: EDataFlow, _role: ERole, _id: &PCWSTR) -> windows::core::Result<()> {
        app::on_main(schedule);
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _id: &PCWSTR, _key: &PROPERTYKEY) -> windows::core::Result<()> {
        Ok(())
    }
}

thread_local! {
    static WATCH: RefCell<Option<(IMMDeviceEnumerator, IMMNotificationClient)>> = const { RefCell::new(None) };
    /// A refresh is on its way (Windows sends changes in bursts, one per role).
    static PENDING: Cell<bool> = const { Cell::new(false) };
    /// Livae's recent switches of the output and the microphone, and until when it stands down from fighting.
    static GUARD: RefCell<[(Vec<std::time::Instant>, Option<std::time::Instant>); 2]> = const { RefCell::new([(Vec::new(), None), (Vec::new(), None)]) };
    static STATUS: RefCell<String> = const { RefCell::new(String::new()) };
}

pub fn install() {
    if let Some(e) = enumerator() {
        let client: IMMNotificationClient = Watcher.into();
        if unsafe { e.RegisterEndpointNotificationCallback(&client) }.is_ok() {
            WATCH.with(|w| *w.borrow_mut() = Some((e, client)));
        }
    }
    refresh();
}

/// Refresh once for a burst of changes, a moment after the first.
pub fn schedule() {
    if !PENDING.with(|p| p.replace(true)) {
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(300));
            app::on_main(|| {
                PENDING.with(|p| p.set(false));
                refresh();
            });
        });
    }
}

pub fn settings_changed(before: &Settings, after: &Settings) {
    if before.sound != after.sound {
        schedule();
    }
}

/// Switch to the chosen device, unless something switched away from it six times in ten seconds: then leave it
/// for a minute rather than fight.
fn switch_to(flow: EDataFlow, id: &str) {
    let now = std::time::Instant::now();
    let allowed = GUARD.with(|g| {
        let mut g = g.borrow_mut();
        let (times, until) = &mut g[usize::from(flow == eCapture)];
        if until.is_some_and(|u| now < u) {
            return false;
        }
        times.retain(|t| now.duration_since(*t).as_secs() < 10);
        if times.len() >= 6 {
            *until = Some(now + std::time::Duration::from_secs(60));
            times.clear();
            return false;
        }
        times.push(now);
        true
    });
    if allowed {
        use_device(id);
    }
}

/// What the equaliser is doing, for the tray menu.
pub fn status() -> String {
    STATUS.with(|s| if s.borrow().is_empty() { "Off".into() } else { s.borrow().clone() })
}

/// Put the chosen devices in use, then write the equaliser for Equalizer APO.
pub fn refresh() {
    let sound = config::get().sound;
    let outputs = devices(eRender);
    for (wanted, list, flow) in [(&sound.outputs, outputs.clone(), eRender), (&sound.inputs, devices(eCapture), eCapture)] {
        let connected: Vec<String> = list.iter().filter(|d| d.connected).map(|d| d.name.clone()).collect();
        if let Some(name) = pick(wanted, &connected)
            && let Some(d) = list.iter().find(|d| d.connected && d.name == name)
            && default_id(flow).as_deref() != Some(d.id.as_str())
        {
            switch_to(flow, &d.id);
        }
    }
    let line = write_apo(&sound, &outputs);
    STATUS.with(|s| *s.borrow_mut() = line);
}

/// Equalizer APO's config folder, from its install (64-bit registry view), if it is installed. A test copy uses
/// a folder of its own instead, so a check never changes the real equaliser.
fn apo_folder() -> Option<PathBuf> {
    if config::test_mode() {
        let dir = config::data_dir().join("apo");
        let _ = std::fs::create_dir_all(&dir);
        return Some(dir);
    }
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let ok = unsafe {
        RegGetValueW(HKEY_LOCAL_MACHINE, &HSTRING::from("SOFTWARE\\EqualizerAPO"), &HSTRING::from("ConfigPath"), RRF_RT_REG_SZ | RRF_SUBKEY_WOW6464KEY, None, Some(buf.as_mut_ptr().cast()), Some(&mut size))
    };
    if ok.is_ok() {
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let path = PathBuf::from(String::from_utf16_lossy(&buf[..n]));
        if path.is_dir() {
            return Some(path);
        }
    }
    let fallback = PathBuf::from("C:\\Program Files\\EqualizerAPO\\config");
    fallback.is_dir().then_some(fallback)
}

/// `--dump --sound`: the outputs and microphones (connected, in use), the equaliser's status and what Livae
/// wrote for Equalizer APO.
pub fn dump() -> String {
    let mut s = String::new();
    for (flow, kind) in [(eRender, "output"), (eCapture, "input")] {
        let used = default_id(flow);
        for d in devices(flow) {
            let mark = if used.as_deref() == Some(d.id.as_str()) { "in use" } else if d.connected { "" } else { "unplugged" };
            s.push_str(&format!("{kind}\t{}\t{}\t{mark}\n", d.name, d.guid()));
        }
    }
    s.push_str(&format!("status\t{}\n", status()));
    if let Some(dir) = apo_folder() {
        for f in ["config.txt", "livae.txt"] {
            s.push_str(&format!("== {f}\n{}", std::fs::read_to_string(dir.join(f)).unwrap_or_default()));
        }
    }
    s
}

/// Write `livae.txt` beside Equalizer APO's `config.txt` and include it there once; the status line to show.
fn write_apo(sound: &livae_core::sound::Sound, outputs: &[Device]) -> String {
    let wanted = sound.eq && !sound.profiles.is_empty();
    let Some(folder) = apo_folder() else {
        return if wanted { "Needs Equalizer APO (free) to run".into() } else { "Off".into() };
    };
    let named: Vec<(String, String)> = outputs.iter().map(|d| (d.name.clone(), d.guid().to_string())).collect();
    let text = equalizer_apo(sound, &named);
    let mine = folder.join("livae.txt");
    if std::fs::read_to_string(&mine).ok().as_deref() != Some(text.as_str()) && livae_core::write_atomic(&mine, text.as_bytes()).is_err() {
        return "Cannot write to Equalizer APO's config folder".into();
    }
    let main = folder.join("config.txt");
    let current = std::fs::read_to_string(&main).unwrap_or_default();
    let fixed = livae_core::sound::include_livae(&current);
    if fixed != current && livae_core::write_atomic(&main, fixed.as_bytes()).is_err() {
        return "Cannot add Livae to Equalizer APO's config.txt".into();
    }
    if !sound.eq {
        return "Off".into();
    }
    let current = default_id(eRender).and_then(|id| outputs.iter().find(|d| d.id == id).cloned());
    match current.map(|d| (sound.design_for(&d.name), d.name)) {
        Some((Ok(Some(_)), name)) => format!("On for {name} (Equalizer APO)"),
        Some((Ok(None), name)) => format!("Nothing to change on {name}"),
        Some((Err(e), _)) => e,
        None => "No output".into(),
    }
}
