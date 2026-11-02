//! Sound on the Mac: keeps the chosen output and microphone in use, and plays the output through the equaliser.
//!
//! The equaliser taps what apps send to the output device (a Core Audio process tap) and plays it back through the
//! filters on that same device, inside a private aggregate device only Livae sees. The tap mutes the apps' own
//! sound only while Livae reads it, so if Livae stops, sound goes straight to the device again: never silence. No
//! driver and no stand-in device: the system's output stays the real one, and its volume keys keep working. The
//! device's own inputs stay off, so a headset's microphone never opens (AirPods stay in high quality).

use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_int, c_void};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchTime};
use livae_core::eq::{Chain, Design};
use livae_core::sound::pick;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::AnyThread;
use objc2_core_audio::*;
use objc2_core_audio_types::AudioBufferList;
use objc2_core_foundation::CFDictionary;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};

use crate::config;

/// Livae's own aggregate devices start with this UID; they are never offered as devices.
const OURS: &str = "com.mateus.livae.eq";
const SYSTEM: AudioObjectID = kAudioObjectSystemObject as AudioObjectID;

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub id: AudioObjectID,
    pub uid: String,
    pub name: String,
    pub output: bool,
    pub input: bool,
}

/// What the equaliser is doing, for Settings and the menu.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Off,
    /// On, but this output has no profile (and there is none for every device): its sound is left alone.
    Flat(String),
    Running(String),
    /// macOS has not allowed Livae to hear what apps play (System Settings, Privacy, Screen & System Audio Recording).
    NeedsPermission,
    /// This output also has a microphone (a headset, a USB DAC): macOS wants its permission before Livae plays
    /// through it, though Livae keeps that microphone off.
    NeedsMicrophone,
    Failed(String),
}

impl Status {
    pub fn text(&self) -> String {
        match self {
            Status::Off => "Off".into(),
            Status::Flat(d) => format!("Nothing to change on {d}"),
            Status::Running(d) => format!("On for {d}"),
            Status::NeedsPermission => "Needs permission to hear what apps play".into(),
            Status::NeedsMicrophone => "This output has a microphone: macOS asks first (Livae keeps it off)".into(),
            Status::Failed(why) => why.clone(),
        }
    }
}

/// Each step of the equaliser's start, on stderr, when `LIVAE_SOUND_LOG` is set (for diagnosing a device).
fn log(what: &str) {
    if std::env::var_os("LIVAE_SOUND_LOG").is_some() {
        eprintln!("sound: {what}");
    }
}

// ---- Core Audio properties --------------------------------------------------------------------------------------

fn address(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress { mSelector: selector, mScope: scope, mElement: kAudioObjectPropertyElementMain }
}

fn byte_size(object: AudioObjectID, a: &AudioObjectPropertyAddress) -> u32 {
    let mut size = 0u32;
    let ok = unsafe { AudioObjectGetPropertyDataSize(object, NonNull::from(a), 0, std::ptr::null(), NonNull::from(&mut size)) };
    if ok == 0 { size } else { 0 }
}

/// A fixed-size value; `qualifier` narrows some properties (a process id).
fn read<T: Copy>(object: AudioObjectID, selector: u32, scope: u32, qualifier: Option<&i32>) -> Option<T> {
    let a = address(selector, scope);
    let mut value = std::mem::MaybeUninit::<T>::uninit();
    let mut size = std::mem::size_of::<T>() as u32;
    let (qs, qp) = qualifier.map_or((0, std::ptr::null()), |q| (4, q as *const i32 as *const c_void));
    let ok = unsafe { AudioObjectGetPropertyData(object, NonNull::from(&a), qs, qp, NonNull::from(&mut size), NonNull::new_unchecked(value.as_mut_ptr() as *mut c_void)) };
    (ok == 0 && size as usize == std::mem::size_of::<T>()).then(|| unsafe { value.assume_init() })
}

fn write<T>(object: AudioObjectID, selector: u32, scope: u32, value: &T) -> bool {
    let a = address(selector, scope);
    unsafe { AudioObjectSetPropertyData(object, NonNull::from(&a), 0, std::ptr::null(), std::mem::size_of::<T>() as u32, NonNull::from(value).cast()) == 0 }
}

fn ids(object: AudioObjectID, selector: u32, scope: u32) -> Vec<AudioObjectID> {
    let a = address(selector, scope);
    let size = byte_size(object, &a);
    let mut v = vec![0 as AudioObjectID; size as usize / 4];
    if v.is_empty() {
        return v;
    }
    let mut size = size;
    let ok = unsafe { AudioObjectGetPropertyData(object, NonNull::from(&a), 0, std::ptr::null(), NonNull::from(&mut size), NonNull::new_unchecked(v.as_mut_ptr() as *mut c_void)) };
    if ok != 0 {
        return Vec::new();
    }
    v.truncate(size as usize / 4);
    v
}

fn string(object: AudioObjectID, selector: u32) -> Option<String> {
    let s: *mut NSString = read(object, selector, kAudioObjectPropertyScopeGlobal, None)?;
    // The property hands over a retained CFString.
    let s = unsafe { Retained::from_raw(s)? };
    Some(s.to_string())
}

/// Every device but Livae's own.
pub fn devices() -> Vec<Device> {
    ids(SYSTEM, kAudioHardwarePropertyDevices, kAudioObjectPropertyScopeGlobal)
        .into_iter()
        .filter_map(|id| {
            let uid = string(id, kAudioDevicePropertyDeviceUID)?;
            if uid.starts_with(OURS) {
                return None;
            }
            Some(Device {
                id,
                name: string(id, kAudioObjectPropertyName)?,
                uid,
                output: !ids(id, kAudioDevicePropertyStreams, kAudioObjectPropertyScopeOutput).is_empty(),
                input: !ids(id, kAudioDevicePropertyStreams, kAudioObjectPropertyScopeInput).is_empty(),
            })
        })
        .collect()
}

fn default_selector(output: bool) -> u32 {
    if output { kAudioHardwarePropertyDefaultOutputDevice } else { kAudioHardwarePropertyDefaultInputDevice }
}

pub fn default_device(output: bool) -> Option<AudioObjectID> {
    read::<AudioObjectID>(SYSTEM, default_selector(output), kAudioObjectPropertyScopeGlobal, None).filter(|id| *id != 0)
}

/// Make `id` the system's output (or microphone).
pub fn use_device(output: bool, id: AudioObjectID) -> bool {
    write(SYSTEM, default_selector(output), kAudioObjectPropertyScopeGlobal, &id)
}

// ---- Permission to hear what apps play (no public API: TCC, as Apple's own tools and AudioCap use it) ------------

mod tcc {
    use super::*;

    unsafe extern "C" {
        fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }

    #[derive(Clone, Copy, PartialEq)]
    pub enum Access {
        Granted,
        Denied,
        Unknown,
    }

    fn symbol(name: &std::ffi::CStr) -> *mut c_void {
        thread_local!(static LIB: Cell<usize> = const { Cell::new(0) });
        let lib = LIB.with(|l| {
            if l.get() == 0 {
                l.set(unsafe { dlopen(c"/System/Library/PrivateFrameworks/TCC.framework/Versions/A/TCC".as_ptr(), 2) } as usize);
            }
            l.get()
        });
        if lib == 0 { std::ptr::null_mut() } else { unsafe { dlsym(lib as *mut c_void, name.as_ptr()) } }
    }

    /// What Livae needs: hearing what apps play, and for an output that has a microphone, the microphone.
    #[derive(Clone, Copy, PartialEq)]
    pub enum Service {
        AudioCapture,
        Microphone,
    }

    fn service(s: Service) -> Retained<NSString> {
        NSString::from_str(match s {
            Service::AudioCapture => "kTCCServiceAudioCapture",
            Service::Microphone => "kTCCServiceMicrophone",
        })
    }

    pub fn preflight(s: Service) -> Access {
        let f = symbol(c"TCCAccessPreflight");
        if f.is_null() {
            return Access::Unknown;
        }
        let f: extern "C" fn(*const c_void, *const c_void) -> c_int = unsafe { std::mem::transmute(f) };
        match f(Retained::as_ptr(&service(s)) as *const c_void, std::ptr::null()) {
            0 => Access::Granted,
            1 => Access::Denied,
            _ => Access::Unknown,
        }
    }

    /// Ask (macOS shows its prompt once); `done` runs on the main thread with the answer.
    pub fn request(s: Service, done: fn(bool)) {
        let f = symbol(c"TCCAccessRequest");
        if f.is_null() {
            return;
        }
        let f: extern "C" fn(*const c_void, *const c_void, *const c_void) = unsafe { std::mem::transmute(f) };
        let block = RcBlock::new(move |granted: u8| {
            DispatchQueue::main().exec_async(move || done(granted != 0));
        });
        f(Retained::as_ptr(&service(s)) as *const c_void, std::ptr::null(), &*block as *const _ as *const c_void);
    }
}

// ---- The equaliser ----------------------------------------------------------------------------------------------

/// Chains handed between the main thread and the audio thread without the audio thread ever waiting or freeing.
#[derive(Default)]
struct Handover {
    next: Option<Box<Chain>>,
    retired: Option<Box<Chain>>,
}

/// What the audio thread owns.
struct Rt {
    chain: Chain,
    handover: Arc<Mutex<Handover>>,
    calls: Arc<AtomicU64>,
}

/// Called by Core Audio on its real-time thread for every buffer: the tap's sound (the last input stream) goes
/// through the chain into the device's first output stream. It never allocates, locks or frees.
unsafe extern "C-unwind" fn io(
    _device: AudioObjectID,
    _now: NonNull<objc2_core_audio_types::AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    _input_time: NonNull<objc2_core_audio_types::AudioTimeStamp>,
    output: NonNull<AudioBufferList>,
    _output_time: NonNull<objc2_core_audio_types::AudioTimeStamp>,
    data: *mut c_void,
) -> i32 {
    let rt = unsafe { &mut *(data as *mut Rt) };
    rt.calls.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut h) = rt.handover.try_lock()
        && let Some(mut next) = h.next.take()
    {
        next.continue_from(&rt.chain);
        std::mem::swap(&mut rt.chain, &mut next);
        h.retired = Some(next);
    }
    let (input, output) = unsafe { (input.as_ref(), &mut *output.as_ptr()) };
    let ins = unsafe { std::slice::from_raw_parts(input.mBuffers.as_ptr(), input.mNumberBuffers as usize) };
    let outs = unsafe { std::slice::from_raw_parts_mut(output.mBuffers.as_mut_ptr(), output.mNumberBuffers as usize) };
    for (i, out) in outs.iter_mut().enumerate() {
        let n = out.mNumberChannels as usize;
        if out.mData.is_null() || n == 0 {
            continue;
        }
        let dst = unsafe { std::slice::from_raw_parts_mut(out.mData as *mut f32, out.mDataByteSize as usize / 4) };
        let src = ins.last().filter(|b| i == 0 && !b.mData.is_null() && b.mNumberChannels > 0);
        let Some(src) = src else {
            dst.fill(0.0);
            continue;
        };
        let s = unsafe { std::slice::from_raw_parts(src.mData as *const f32, src.mDataByteSize as usize / 4) };
        let frames = livae_core::eq::route(s, src.mNumberChannels as usize, dst, n);
        rt.chain.process(&mut dst[..frames * n], n);
    }
    0
}

type Listener = RcBlock<dyn Fn(u32, NonNull<AudioObjectPropertyAddress>)>;

/// The tap, the aggregate device and its IO, built for one output device and torn down in reverse on drop.
struct Engine {
    device: AudioObjectID,
    name: String,
    rate: f64,
    tap: AudioObjectID,
    aggregate: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    started: bool,
    rt: *mut Rt,
    handover: Arc<Mutex<Handover>>,
    calls: Arc<AtomicU64>,
    rate_listener: Option<Listener>,
}

fn process_object(pid: i32) -> Option<AudioObjectID> {
    read::<AudioObjectID>(SYSTEM, kAudioHardwarePropertyTranslatePIDToProcessObject, kAudioObjectPropertyScopeGlobal, Some(&pid)).filter(|id| *id != 0)
}

fn check(status: i32, what: &str) -> Result<(), String> {
    if status == 0 { Ok(()) } else { Err(format!("Could not {what} ({status})")) }
}

impl Engine {
    fn start(device: &Device, design: &Design) -> Result<Self, String> {
        let mut e = Engine {
            device: device.id,
            name: device.name.clone(),
            rate: 0.0,
            tap: 0,
            aggregate: 0,
            proc_id: None,
            started: false,
            rt: std::ptr::null_mut(),
            handover: Arc::default(),
            calls: Arc::default(),
            rate_listener: None,
        };
        // Everything apps send to the device's first output stream, but Livae's own sounds, muted there only while
        // Livae reads it.
        // Without Livae itself left out, the equaliser would hear its own output and feed back.
        let me = process_object(std::process::id() as i32).ok_or("Could not find Livae among the audio clients")?;
        let me = [NSNumber::new_u32(me)];
        let description = unsafe {
            CATapDescription::initExcludingProcesses_andDeviceUID_withStream(
                CATapDescription::alloc(),
                &NSArray::from_retained_slice(&me),
                &NSString::from_str(&device.uid),
                0,
            )
        };
        unsafe {
            description.setName(&NSString::from_str("Livae EQ"));
            description.setPrivate(true);
            description.setMuteBehavior(CATapMuteBehavior::MutedWhenTapped);
        }
        check(unsafe { AudioHardwareCreateProcessTap(Some(&description), &mut e.tap) }, "listen to the output")?;
        let tap_uid = unsafe { description.UUID() }.UUIDString();

        // A private aggregate: the device is its clock and output, the tap its input.
        let key = |k: &std::ffi::CStr| NSString::from_str(k.to_str().unwrap_or_default());
        let dict = |pairs: &[(&std::ffi::CStr, &AnyObject)]| {
            let keys: Vec<Retained<NSString>> = pairs.iter().map(|(k, _)| key(k)).collect();
            let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
            let values: Vec<&AnyObject> = pairs.iter().map(|(_, v)| *v).collect();
            NSDictionary::from_slices(&key_refs, &values)
        };
        let device_uid = NSString::from_str(&device.uid);
        let sub = dict(&[(kAudioSubDeviceUIDKey, device_uid.as_ref())]);
        let tap = dict(&[(kAudioSubTapUIDKey, tap_uid.as_ref()), (kAudioSubTapDriftCompensationKey, NSNumber::new_bool(true).as_ref())]);
        let uid = NSString::from_str(&format!("{OURS}.{}", tap_uid));
        let (yes, no) = (NSNumber::new_bool(true), NSNumber::new_bool(false));
        let subs = NSArray::from_retained_slice(&[sub]);
        let taps = NSArray::from_retained_slice(&[tap]);
        let name = NSString::from_str("Livae EQ");
        let description = dict(&[
            (kAudioAggregateDeviceUIDKey, uid.as_ref()),
            (kAudioAggregateDeviceNameKey, name.as_ref()),
            (kAudioAggregateDeviceIsPrivateKey, yes.as_ref()),
            (kAudioAggregateDeviceIsStackedKey, no.as_ref()),
            (kAudioAggregateDeviceMainSubDeviceKey, device_uid.as_ref()),
            (kAudioAggregateDeviceSubDeviceListKey, subs.as_ref()),
            (kAudioAggregateDeviceTapListKey, taps.as_ref()),
            (kAudioAggregateDeviceTapAutoStartKey, yes.as_ref()),
        ]);
        let cf = unsafe { &*(Retained::as_ptr(&description) as *const CFDictionary) };
        check(unsafe { AudioHardwareCreateAggregateDevice(cf, NonNull::from(&mut e.aggregate)) }, "make the equaliser's device")?;
        log(&format!("device {} for {}: inputs {:?} outputs {:?}", e.aggregate, device.name, ids(e.aggregate, kAudioDevicePropertyStreams, kAudioObjectPropertyScopeInput), ids(e.aggregate, kAudioDevicePropertyStreams, kAudioObjectPropertyScopeOutput)));

        e.rate = read::<f64>(e.aggregate, kAudioDevicePropertyNominalSampleRate, kAudioObjectPropertyScopeGlobal, None).filter(|r| *r > 0.0).ok_or("The output reports no sample rate")?;
        let rt = Box::new(Rt { chain: Chain::new(design, e.rate), handover: e.handover.clone(), calls: e.calls.clone() });
        e.rt = Box::into_raw(rt);
        check(unsafe { AudioDeviceCreateIOProcID(e.aggregate, Some(io), e.rt as *mut c_void, NonNull::from(&mut e.proc_id)) }, "start the equaliser")?;
        if std::env::var_os("LIVAE_SOUND_KEEP_INPUTS").is_none() {
            e.inputs_off();
        }
        check(unsafe { AudioDeviceStart(e.aggregate, e.proc_id) }, "start the equaliser")?;
        log(&format!("started at {} Hz", e.rate));
        e.started = true;

        // A new sample rate (another app, or Bluetooth switching profiles) retunes the filters.
        let listener: Listener = RcBlock::new(|_n: u32, _a: NonNull<AudioObjectPropertyAddress>| schedule());
        let a = address(kAudioDevicePropertyNominalSampleRate, kAudioObjectPropertyScopeGlobal);
        if unsafe { AudioObjectAddPropertyListenerBlock(e.aggregate, NonNull::from(&a), Some(DispatchQueue::main()), &*listener as *const _ as *mut _) } == 0 {
            e.rate_listener = Some(listener);
        }
        Ok(e)
    }

    /// Only the tap's stream is read: the device's own inputs (a headset's microphone) stay closed.
    fn inputs_off(&self) {
        let a = address(kAudioDevicePropertyIOProcStreamUsage, kAudioObjectPropertyScopeInput);
        let size = byte_size(self.aggregate, &a) as usize;
        let ptr = std::mem::size_of::<usize>();
        if size <= ptr + 4 {
            return;
        }
        // The IOProc pointer, the stream count, then one on/off flag per stream.
        let mut buf = vec![0u8; size];
        let proc_ptr = self.proc_id.map_or(0usize, |p| p as usize);
        buf[..ptr].copy_from_slice(&proc_ptr.to_ne_bytes());
        let mut got = size as u32;
        let ok = unsafe { AudioObjectGetPropertyData(self.aggregate, NonNull::from(&a), 0, std::ptr::null(), NonNull::from(&mut got), NonNull::new_unchecked(buf.as_mut_ptr() as *mut c_void)) };
        if ok != 0 {
            return;
        }
        let count = (u32::from_ne_bytes(buf[ptr..ptr + 4].try_into().unwrap()) as usize).min((size - ptr - 4) / 4);
        // Only the last stream, the tap's, stays on.
        for i in 0..count {
            let at = ptr + 4 + 4 * i;
            buf[at..at + 4].copy_from_slice(&u32::from(i + 1 == count).to_ne_bytes());
        }
        let set = unsafe { AudioObjectSetPropertyData(self.aggregate, NonNull::from(&a), 0, std::ptr::null(), got, NonNull::new_unchecked(buf.as_mut_ptr() as *mut c_void)) };
        log(&format!("{count} input streams, only the last on (status {set})"));
    }

    /// New filters (an edit, or a new sample rate), taken up by the audio thread on its next buffer.
    fn retune(&mut self, design: &Design) {
        if let Some(rate) = read::<f64>(self.aggregate, kAudioDevicePropertyNominalSampleRate, kAudioObjectPropertyScopeGlobal, None).filter(|r| *r > 0.0) {
            self.rate = rate;
        }
        let chain = Box::new(Chain::new(design, self.rate));
        let mut h = self.handover.lock().unwrap_or_else(|p| p.into_inner());
        h.retired = None;
        h.next = Some(chain);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        unsafe {
            if let Some(l) = self.rate_listener.take() {
                let a = address(kAudioDevicePropertyNominalSampleRate, kAudioObjectPropertyScopeGlobal);
                AudioObjectRemovePropertyListenerBlock(self.aggregate, NonNull::from(&a), Some(DispatchQueue::main()), &*l as *const _ as *mut _);
            }
            if self.started {
                AudioDeviceStop(self.aggregate, self.proc_id);
            }
            if self.proc_id.is_some() {
                AudioDeviceDestroyIOProcID(self.aggregate, self.proc_id);
            }
            if self.aggregate != 0 {
                AudioHardwareDestroyAggregateDevice(self.aggregate);
            }
            if self.tap != 0 {
                AudioHardwareDestroyProcessTap(self.tap);
            }
            // Core Audio no longer calls `io`: the audio thread's state can go.
            if !self.rt.is_null() {
                drop(Box::from_raw(self.rt));
            }
        }
    }
}

// ---- Keeping it all in step ---------------------------------------------------------------------------------------

struct State {
    engine: Option<Engine>,
    status: Status,
    /// The device whose start failed, not tried again until something changes.
    failed: Option<AudioObjectID>,
    /// Asked this run: hearing apps, the microphone.
    asked: [bool; 2],
    listeners: Vec<Listener>,
    woke: Option<Retained<objc2::runtime::ProtocolObject<dyn objc2::runtime::NSObjectProtocol>>>,
}

thread_local! {
    static STATE: RefCell<State> = const { RefCell::new(State { engine: None, status: Status::Off, failed: None, asked: [false; 2], listeners: Vec::new(), woke: None }) };
    /// A refresh is already on its way (Core Audio sends changes in bursts).
    static PENDING: Cell<bool> = const { Cell::new(false) };
    /// Livae's recent switches of the output and the microphone, and until when it stands down from fighting.
    static GUARD: RefCell<[(Vec<std::time::Instant>, Option<std::time::Instant>); 2]> = const { RefCell::new([(Vec::new(), None), (Vec::new(), None)]) };
}

pub fn status() -> Status {
    STATE.with(|s| s.borrow().status.clone())
}

/// Refresh soon, once for a burst of changes.
pub fn schedule() {
    if !PENDING.with(|p| p.replace(true)) {
        let _ = DispatchQueue::main().after(DispatchTime::NOW.time(300_000_000), || {
            PENDING.with(|p| p.set(false));
            refresh();
        });
    }
}

/// Switch to the chosen device, unless something switched away from it six times in ten seconds: then leave it
/// for a minute rather than fight.
fn switch_to(output: bool, id: AudioObjectID) {
    let now = std::time::Instant::now();
    let allowed = GUARD.with(|g| {
        let mut g = g.borrow_mut();
        let (times, until) = &mut g[usize::from(!output)];
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
        use_device(output, id);
    }
}

/// Start listening to the system's devices and apply the settings once.
pub fn install() {
    let mut listeners = Vec::new();
    // Devices coming and going, and any switch of the output or the microphone (by macOS, an app or a person).
    for selector in [kAudioHardwarePropertyDevices, kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyDefaultInputDevice] {
        let block: Listener = RcBlock::new(|_n: u32, _a: NonNull<AudioObjectPropertyAddress>| schedule());
        let a = address(selector, kAudioObjectPropertyScopeGlobal);
        if unsafe { AudioObjectAddPropertyListenerBlock(SYSTEM, NonNull::from(&a), Some(DispatchQueue::main()), &*block as *const _ as *mut _) } == 0 {
            listeners.push(block);
        }
    }
    let center = objc2_app_kit::NSWorkspace::sharedWorkspace().notificationCenter();
    let wake = RcBlock::new(|_n: NonNull<objc2_foundation::NSNotification>| woke());
    let observer = unsafe { center.addObserverForName_object_queue_usingBlock(Some(objc2_app_kit::NSWorkspaceDidWakeNotification), None, None, &wake) };
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.listeners = listeners;
        s.woke = Some(observer);
    });
    refresh();
}

/// After sleep the hardware may have been reset: build everything again.
pub fn woke() {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.engine = None;
        s.failed = None;
    });
    schedule();
}

/// Put the chosen devices in use, then run the output through its equaliser, or not.
pub fn refresh() {
    let settings = config::get().sound;
    let list = devices();
    for (wanted, output) in [(&settings.outputs, true), (&settings.inputs, false)] {
        let names: Vec<String> = list.iter().filter(|d| if output { d.output } else { d.input }).map(|d| d.name.clone()).collect();
        if let Some(name) = pick(wanted, &names)
            && let Some(d) = list.iter().find(|d| d.name == name && if output { d.output } else { d.input })
            && default_device(output) != Some(d.id)
        {
            switch_to(output, d.id);
        }
    }
    // Ask for the permission as soon as the equaliser has something to do on any output, not only this one.
    if settings.eq && !settings.profiles.is_empty() && tcc::preflight(tcc::Service::AudioCapture) == tcc::Access::Unknown && std::env::var_os("LIVAE_TEST_EQ_DEVICE").is_none() {
        ask(tcc::Service::AudioCapture);
    }
    // A check can run the equaliser on a named device (BlackHole) while the real output plays on untouched.
    let test_device = std::env::var("LIVAE_TEST_EQ_DEVICE").ok().filter(|_| crate::app::test_mode());
    let out = match &test_device {
        Some(name) => list.iter().find(|d| d.name == *name).cloned(),
        None => default_device(true).and_then(|id| list.iter().find(|d| d.id == id).cloned()),
    };
    STATE.with(|state| {
        let mut st = state.borrow_mut();
        let Some(device) = out else {
            st.engine = None;
            st.status = Status::Off;
            return;
        };
        let design = match settings.design_for(&device.name) {
            Ok(Some(d)) => d,
            Ok(None) => {
                st.engine = None;
                st.status = if settings.eq { Status::Flat(device.name) } else { Status::Off };
                return;
            }
            Err(e) => {
                st.engine = None;
                st.status = Status::Failed(e);
                return;
            }
        };
        if tcc::preflight(tcc::Service::AudioCapture) != tcc::Access::Granted {
            st.engine = None;
            st.status = Status::NeedsPermission;
            return;
        }
        // An output with a microphone joins Livae's private device whole, and macOS checks the microphone's
        // permission for it (Livae switches that microphone off for itself; nothing is recorded).
        if device.input {
            match tcc::preflight(tcc::Service::Microphone) {
                tcc::Access::Granted => {}
                access => {
                    st.engine = None;
                    st.status = Status::NeedsMicrophone;
                    if access == tcc::Access::Unknown && test_device.is_none() {
                        drop(st);
                        ask(tcc::Service::Microphone);
                    }
                    return;
                }
            }
        }
        if let Some(e) = st.engine.as_mut().filter(|e| e.device == device.id) {
            e.retune(&design);
            st.status = Status::Running(device.name);
            return;
        }
        st.engine = None;
        if st.failed == Some(device.id) {
            return;
        }
        match Engine::start(&device, &design) {
            Ok(engine) => {
                let calls = engine.calls.clone();
                st.engine = Some(engine);
                st.status = Status::Running(device.name.clone());
                st.failed = None;
                watch(device.id, calls);
            }
            Err(why) => {
                st.status = Status::Failed(why);
                st.failed = Some(device.id);
            }
        }
    });
    crate::settings::sound_changed();
}

/// `--ask-microphone`: show macOS's microphone prompt for Livae now (outputs that have a microphone need it).
pub fn ask_microphone() {
    ask(tcc::Service::Microphone);
}

/// Ask macOS once per run for a permission; refresh with the answer.
fn ask(service: tcc::Service) {
    let first = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let i = usize::from(service == tcc::Service::Microphone);
        !std::mem::replace(&mut s.asked[i], true)
    });
    if first {
        tcc::request(service, |_granted| schedule());
    }
}

/// Every 3 s while the equaliser runs: when the output is playing (some app is using it) but the equaliser got no
/// audio since the last look, twice in a row, stand down (the device plays the apps directly again) rather than
/// leave it silent. An idle output is not a failure: Core Audio runs the equaliser only while something plays.
fn watch(device: AudioObjectID, calls: Arc<AtomicU64>) {
    fn look(device: AudioObjectID, calls: Arc<AtomicU64>, seen: u64, strikes: u32) {
        let _ = DispatchQueue::main().after(DispatchTime::NOW.time(3_000_000_000), move || {
            let ours = STATE.with(|s| s.borrow().engine.as_ref().is_some_and(|e| e.device == device && Arc::ptr_eq(&e.calls, &calls)));
            if !ours {
                return;
            }
            let now = calls.load(Ordering::Relaxed);
            let playing = read::<u32>(device, kAudioDevicePropertyDeviceIsRunningSomewhere, kAudioObjectPropertyScopeGlobal, None).unwrap_or(0) != 0;
            let strikes = if playing && now == seen { strikes + 1 } else { 0 };
            log(&format!("watch: {now} callbacks, output playing {playing}, strikes {strikes}"));
            if strikes < 2 {
                return look(device, calls, now, strikes);
            }
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                s.engine = None;
                s.failed = Some(device);
                s.status = Status::Failed("macOS did not start the equaliser on this output".into());
            });
            crate::settings::sound_changed();
        });
    }
    look(device, calls, 0, 0);
}

/// Hear an edit at once (the Equaliser window), before it is saved: the running equaliser takes the new filters.
pub fn preview(sound: &livae_core::sound::Sound) {
    STATE.with(|s| {
        if let Some(e) = s.borrow_mut().engine.as_mut()
            && let Ok(Some(design)) = sound.design_for(&e.name)
        {
            e.retune(&design);
        }
    });
}

/// `--sound-dump`: every device, the two in use, and the equaliser (its device, rate and audio callbacks so far).
pub fn dump() -> String {
    let (out, inp) = (default_device(true), default_device(false));
    let mut s = String::new();
    for d in devices() {
        let used: Vec<&str> = [(out == Some(d.id), "output"), (inp == Some(d.id), "input")].iter().filter(|(on, _)| *on).map(|(_, w)| *w).collect();
        s.push_str(&format!("device\t{}\t{}{}\t{}\n", d.name, if d.output { "out" } else { "" }, if d.input { "in" } else { "" }, used.join(" ")));
    }
    STATE.with(|st| {
        let st = st.borrow();
        s.push_str(&format!("status\t{}\n", st.status.text()));
        if let Some(e) = &st.engine {
            s.push_str(&format!("engine\t{}\t{} Hz\t{} callbacks\n", e.name, e.rate, e.calls.load(Ordering::Relaxed)));
        }
    });
    s
}
