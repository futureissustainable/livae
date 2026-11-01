//! The Windows shell: known folders, opening and revealing files, the Startup-folder shortcut, the Start menu's
//! app list, and pictures (app and file icons, clipboard thumbnails) made on a background thread with WIC.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, OnceLock};

use windows::core::{w, Interface, GUID, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HWND, LPARAM, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{DeleteObject, HBITMAP, HPALETTE};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppBGRA, GUID_WICPixelFormat32bppPBGRA, IWICBitmapSource,
    IWICImagingFactory, WICBitmapDitherTypeNone, WICBitmapEncoderNoCache, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom,
    WICBitmapUsePremultipliedAlpha, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Shell::{
    ILCreateFromPathW, ILFree, IShellItem, IShellItemImageFactory, IShellLinkW, IEnumShellItems, SHCreateItemFromParsingName, SHGetKnownFolderItem,
    SHGetKnownFolderPath, SHOpenFolderAndSelectItems, ShellExecuteW, ShellLink, BHID_EnumItems, FOLDERID_AppsFolder, FOLDERID_Desktop,
    FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Startup, KF_FLAG_DEFAULT, SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING, SIIGBF_ICONONLY,
};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, PostMessageW, SW_SHOWNORMAL, WM_APP};

/// Posted to a window when a picture it asked for is ready.
pub const WM_PICTURE: u32 = WM_APP + 20;

pub fn known_folder(id: &GUID) -> Option<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as _));
        s.map(PathBuf::from)
    }
}

/// Desktop, Documents and Downloads, wherever OneDrive or the user moved them.
pub fn user_folders() -> Vec<PathBuf> {
    [FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads].iter().filter_map(known_folder).collect()
}

/// Open a file, folder, shortcut, `shell:` name or URL with its default handler.
pub fn open(target: &str) -> bool {
    unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(target), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL).0 as isize > 32 }
}

/// Open a text file; Notepad when `.toml` has no editor yet.
pub fn edit(path: &Path) {
    let p = path.to_string_lossy();
    if !open(&p) {
        unsafe {
            ShellExecuteW(None, w!("open"), w!("notepad.exe"), &HSTRING::from(format!("\"{p}\"")), PCWSTR::null(), SW_SHOWNORMAL);
        }
    }
}

/// Show the item selected in an Explorer window.
pub fn reveal(path: &Path) {
    unsafe {
        let pidl = ILCreateFromPathW(&HSTRING::from(path.as_os_str()));
        if pidl.is_null() {
            ShellExecuteW(None, w!("open"), w!("explorer.exe"), &HSTRING::from(format!("/select,\"{}\"", path.display())), PCWSTR::null(), SW_SHOWNORMAL);
            return;
        }
        let _ = SHOpenFolderAndSelectItems(pidl, None, 0);
        ILFree(Some(pidl));
    }
}

/// `--install-startup` / `--remove-startup`: a "Livae.lnk" in the user's Startup folder, one reversible file.
/// (No Run registry key: Defender flagged that as persistence on an unsigned build.)
pub fn startup_link(install: bool) -> Result<PathBuf, String> {
    let dir = known_folder(&FOLDERID_Startup).ok_or("no Startup folder")?;
    let link = dir.join("Livae.lnk");
    if !install {
        if link.exists() {
            std::fs::remove_file(&link).map_err(|e| e.to_string())?;
        }
        return Ok(link);
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    unsafe {
        let sl: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        sl.SetPath(&HSTRING::from(exe.as_os_str())).map_err(|e| e.to_string())?;
        if let Some(parent) = exe.parent() {
            let _ = sl.SetWorkingDirectory(&HSTRING::from(parent.as_os_str()));
        }
        let _ = sl.SetDescription(w!("Livae launcher and clipboard"));
        let pf: IPersistFile = sl.cast().map_err(|e| e.to_string())?;
        pf.Save(&HSTRING::from(link.as_os_str()), true).map_err(|e| e.to_string())?;
    }
    Ok(link)
}

/// Every app in Start's "All apps" (Store apps included) as (name, parsing name); open one with
/// `shell:AppsFolder\<parsing name>`. Slow-ish: call it off the UI thread (it sets up COM itself).
pub fn apps_folder() -> Vec<(String, String)> {
    let mut out = Vec::new();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let Ok(folder) = SHGetKnownFolderItem::<IShellItem>(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None) else { return out };
        let Ok(items) = folder.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) else { return out };
        loop {
            let mut batch: [Option<IShellItem>; 32] = Default::default();
            let mut got = 0u32;
            if items.Next(&mut batch, Some(&mut got)).is_err() && got == 0 {
                break;
            }
            if got == 0 {
                break;
            }
            for item in batch.iter().take(got as usize).flatten() {
                let name = take_name(item.GetDisplayName(SIGDN_NORMALDISPLAY));
                let id = take_name(item.GetDisplayName(SIGDN_PARENTRELATIVEPARSING));
                if let (Some(name), Some(id)) = (name, id) {
                    out.push((name, id));
                }
            }
        }
    }
    out
}

fn take_name(r: windows::core::Result<PWSTR>) -> Option<String> {
    let p = r.ok()?;
    let s = unsafe { p.to_string().ok() };
    unsafe { CoTaskMemFree(Some(p.0 as _)) };
    s.filter(|s| !s.trim().is_empty())
}

/// The program behind a window, by file name without ".exe" ("WindowsTerminal", "Code").
pub fn process_name(hwnd: HWND) -> String {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        if !ok {
            return String::new();
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        Path::new(&path).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
    }
}

/// A process with this file name ("Stretchly.exe", any case) is running in any session.
pub fn running(exe: &str) -> bool {
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return false };
        let mut e = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut found = false;
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok && !found {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            found = String::from_utf16_lossy(&e.szExeFile[..len]).eq_ignore_ascii_case(exe);
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
        found
    }
}

// Pictures. The UI thread asks; a worker thread with its own COM makes the pixels; the asking window gets
// WM_PICTURE and turns the pixels into a Direct2D bitmap at its next paint.

/// 32-bit BGRA pixels, top row first.
pub struct Pixels {
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

struct Job {
    key: String,
    source: String,
    px: (u32, u32),
    notify: isize,
}

#[derive(Default)]
struct Store {
    made: HashMap<String, Option<Arc<Pixels>>>,
    asked: HashSet<String>,
}

fn store() -> &'static Mutex<Store> {
    static S: OnceLock<Mutex<Store>> = OnceLock::new();
    S.get_or_init(Default::default)
}

fn worker() -> &'static Mutex<Sender<Job>> {
    static W: OnceLock<Mutex<Sender<Job>>> = OnceLock::new();
    W.get_or_init(|| {
        let (tx, rx) = channel::<Job>();
        std::thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            for job in rx {
                let pixels = make_picture(&job.source, job.px).map(Arc::new);
                if let Ok(mut s) = store().lock() {
                    if s.made.len() > 2000 {
                        s.made.clear();
                        s.asked.clear();
                    }
                    s.made.insert(job.key, pixels);
                }
                unsafe {
                    let _ = PostMessageW(Some(HWND(job.notify as _)), WM_PICTURE, WPARAM(0), LPARAM(0));
                }
            }
        });
        Mutex::new(tx)
    })
}

/// The picture for `key` when made (`Some(None)`: it could not be made), or None after queueing it.
/// `source` is a path or shell parsing name for an icon, or "thumb:<png path>" for a thumbnail.
pub fn picture(key: &str, source: &str, px: (u32, u32), notify: HWND) -> Option<Option<Arc<Pixels>>> {
    let mut s = store().lock().ok()?;
    if let Some(p) = s.made.get(key) {
        return Some(p.clone());
    }
    if s.asked.insert(key.to_string()) {
        if s.asked.len() > 4000 {
            s.asked.clear();
        }
        let job = Job { key: key.to_string(), source: source.to_string(), px, notify: notify.0 as isize };
        if let Ok(tx) = worker().lock() {
            let _ = tx.send(job);
        }
    }
    None
}

fn make_picture(source: &str, px: (u32, u32)) -> Option<Pixels> {
    if let Some(path) = source.strip_prefix("thumb:") {
        return decode_file(Path::new(path), Some(px), true);
    }
    unsafe {
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(source), None).ok()?;
        let hbm = factory.GetImage(SIZE { cx: px.0.max(1) as i32, cy: px.1.max(1) as i32 }, SIIGBF_ICONONLY).ok()?;
        let p = from_hbitmap(hbm);
        let _ = DeleteObject(hbm.into());
        p
    }
}

// WIC: decoding and encoding images.

fn wic() -> Option<IWICImagingFactory> {
    thread_local! {
        static WIC: Option<IWICImagingFactory> = unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok() };
    }
    WIC.with(|w| w.clone())
}

/// Some apps put 32-bit images on the clipboard with every alpha byte zero: treat those as opaque.
fn fix_alpha(data: &mut [u8]) {
    if data.chunks_exact(4).all(|p| p[3] == 0) {
        data.chunks_exact_mut(4).for_each(|p| p[3] = 255);
    }
}

/// Pixels of `source`, scaled to exactly `fit` when given; premultiplied for drawing, straight for saving.
fn read(source: &IWICBitmapSource, fit: Option<(u32, u32)>, premultiplied: bool) -> Option<Pixels> {
    unsafe {
        let f = wic()?;
        let (mut w, mut h) = (0u32, 0u32);
        source.GetSize(&mut w, &mut h).ok()?;
        if w == 0 || h == 0 || (w as u64) * (h as u64) > 120_000_000 {
            return None;
        }
        let src: IWICBitmapSource = match fit {
            Some((fw, fh)) if fw > 0 && fh > 0 && (fw, fh) != (w, h) => {
                let s = f.CreateBitmapScaler().ok()?;
                s.Initialize(source, fw, fh, WICBitmapInterpolationModeFant).ok()?;
                w = fw;
                h = fh;
                s.cast().ok()?
            }
            _ => source.clone(),
        };
        let conv = f.CreateFormatConverter().ok()?;
        let fmt = if premultiplied { GUID_WICPixelFormat32bppPBGRA } else { GUID_WICPixelFormat32bppBGRA };
        conv.Initialize(&src, &fmt, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
        let mut data = vec![0u8; (w * h * 4) as usize];
        conv.CopyPixels(std::ptr::null(), w * 4, &mut data).ok()?;
        fix_alpha(&mut data);
        Some(Pixels { w, h, data })
    }
}

/// Decode PNG, BMP (a clipboard DIB with a file header) or any format WIC knows, as straight BGRA.
pub fn decode(bytes: &[u8]) -> Option<Pixels> {
    unsafe {
        let f = wic()?;
        let stream = f.CreateStream().ok()?;
        stream.InitializeFromMemory(bytes).ok()?;
        let decoder = f.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        read(&frame.cast().ok()?, None, false)
    }
}

pub fn decode_file(path: &Path, fit: Option<(u32, u32)>, premultiplied: bool) -> Option<Pixels> {
    unsafe {
        let f = wic()?;
        let decoder = f.CreateDecoderFromFilename(&HSTRING::from(path.as_os_str()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        read(&frame.cast().ok()?, fit, premultiplied)
    }
}

fn from_hbitmap(hbm: HBITMAP) -> Option<Pixels> {
    unsafe {
        let f = wic()?;
        let bmp = f.CreateBitmapFromHBITMAP(hbm, HPALETTE::default(), WICBitmapUsePremultipliedAlpha).ok()?;
        read(&bmp.cast().ok()?, None, true)
    }
}

/// Save straight BGRA pixels as a PNG file.
pub fn encode_png(p: &Pixels, path: &Path) -> bool {
    let run = || -> Option<()> {
        unsafe {
            let f = wic()?;
            let stream = f.CreateStream().ok()?;
            stream.InitializeFromFilename(&HSTRING::from(path.as_os_str()), GENERIC_WRITE.0).ok()?;
            let enc = f.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null()).ok()?;
            enc.Initialize(&stream, WICBitmapEncoderNoCache).ok()?;
            let mut frame = None;
            let mut props = None;
            enc.CreateNewFrame(&mut frame, &mut props).ok()?;
            let frame = frame?;
            frame.Initialize(props.as_ref()).ok()?;
            frame.SetSize(p.w, p.h).ok()?;
            let mut fmt = GUID_WICPixelFormat32bppBGRA;
            frame.SetPixelFormat(&mut fmt).ok()?;
            if fmt != GUID_WICPixelFormat32bppBGRA {
                return None;
            }
            frame.WritePixels(p.h, p.w * 4, &p.data).ok()?;
            frame.Commit().ok()?;
            enc.Commit().ok()?;
            Some(())
        }
    };
    let ok = run().is_some();
    if !ok {
        let _ = std::fs::remove_file(path);
    }
    ok
}
