//! Windows settings pages, Control Panel applets and system tools the launcher can open, with the words people type
//! for them ("wifi", "dark mode", "uninstall"). Windows only lists the Settings and Control Panel apps themselves in
//! Start's "All apps", so without this table "display" or "bluetooth" found nothing.

/// One place in Windows: its own name, a short kind shown beside it, extra search words, and what to open.
pub struct SystemItem {
    pub name: &'static str,
    /// "Settings", "Control Panel", "Tool" or "Folder".
    pub detail: &'static str,
    /// Lowercase words that also find it; never shown.
    pub keywords: &'static str,
    /// An `ms-settings:` page, a `shell:` folder, or a program, `.cpl` or `.msc` in System32.
    pub target: &'static str,
    /// Arguments for `target`, when it is a program that needs them.
    pub args: Option<&'static str>,
}

const fn s(name: &'static str, keywords: &'static str, target: &'static str) -> SystemItem {
    SystemItem { name, detail: "Settings", keywords, target, args: None }
}
const fn cp(name: &'static str, keywords: &'static str, target: &'static str, args: Option<&'static str>) -> SystemItem {
    SystemItem { name, detail: "Control Panel", keywords, target, args }
}
const fn tool(name: &'static str, keywords: &'static str, target: &'static str) -> SystemItem {
    SystemItem { name, detail: "Tool", keywords, target, args: None }
}
const fn folder(name: &'static str, keywords: &'static str, target: &'static str) -> SystemItem {
    SystemItem { name, detail: "Folder", keywords, target, args: None }
}

/// Windows 11 (names as the Settings app shows them in English).
pub const WINDOWS: &[SystemItem] = &[
    // System
    s("Display", "screen resolution brightness scale scaling monitor refresh rate hdr orientation multiple displays", "ms-settings:display"),
    s("Night light", "blue light warm color temperature evening", "ms-settings:nightlight"),
    s("Graphics", "gpu preference high performance hardware accelerated scheduling", "ms-settings:display-advancedgraphics"),
    s("Sound", "volume audio speakers headphones output input microphone levels", "ms-settings:sound"),
    s("Volume mixer", "app volume sound per app", "ms-settings:apps-volume"),
    s("Notifications", "do not disturb alerts banners", "ms-settings:notifications"),
    s("Power & battery", "sleep screen timeout power mode battery saver energy", "ms-settings:powersleep"),
    s("Storage", "disk space storage sense cleanup free up drive", "ms-settings:storagesense"),
    s("Multitasking", "snap windows alt tab virtual desktops title bar shake", "ms-settings:multitasking"),
    s("Remote Desktop", "rdp remote access", "ms-settings:remotedesktop"),
    s("About", "pc name rename this pc device specifications system info windows version specs", "ms-settings:about"),
    s("Activation", "product key license windows edition", "ms-settings:activation"),
    s("Troubleshoot", "fix problems troubleshooters", "ms-settings:troubleshoot"),
    s("Recovery", "reset this pc advanced startup go back reinstall", "ms-settings:recovery"),
    s("Optional features", "add a feature capabilities", "ms-settings:optionalfeatures"),
    s("For developers", "developer mode sudo dev drive", "ms-settings:developers"),
    // Bluetooth & devices
    s("Bluetooth & devices", "bluetooth pair add device headphones controller", "ms-settings:bluetooth"),
    s("Printers & scanners", "printer scanner print", "ms-settings:printers"),
    s("Mouse", "pointer speed scroll cursor buttons", "ms-settings:mousetouchpad"),
    s("Touchpad", "trackpad gestures tap", "ms-settings:devices-touchpad"),
    s("Typing", "autocorrect spelling text suggestions keyboard", "ms-settings:typing"),
    s("AutoPlay", "usb drive media inserted", "ms-settings:autoplay"),
    // Network & internet
    s("Network & internet", "internet connection network status", "ms-settings:network-status"),
    s("Wi-Fi", "wifi wireless wlan hotspot networks", "ms-settings:network-wifi"),
    s("Ethernet", "lan cable wired ip dns", "ms-settings:network-ethernet"),
    s("VPN", "virtual private network", "ms-settings:network-vpn"),
    s("Mobile hotspot", "tethering share internet", "ms-settings:network-mobilehotspot"),
    s("Airplane mode", "flight mode", "ms-settings:network-airplanemode"),
    s("Proxy", "proxy server", "ms-settings:network-proxy"),
    s("Advanced network settings", "network adapters reset network data usage", "ms-settings:network-advancedsettings"),
    // Personalization
    s("Personalization", "appearance look customize", "ms-settings:personalization"),
    s("Background", "wallpaper desktop picture slideshow", "ms-settings:personalization-background"),
    s("Colors", "dark mode light mode accent color theme transparency", "ms-settings:colors"),
    s("Themes", "desktop icons theme", "ms-settings:themes"),
    s("Lock screen", "lock screen picture spotlight", "ms-settings:lockscreen"),
    s("Start", "start menu layout pins recommended", "ms-settings:personalization-start"),
    s("Taskbar", "tray icons system tray hide taskbar alignment", "ms-settings:taskbar"),
    s("Fonts", "typefaces install font", "ms-settings:fonts"),
    // Apps
    s("Installed apps", "uninstall remove programs apps and features", "ms-settings:appsfeatures"),
    s("Default apps", "default browser file associations open with", "ms-settings:defaultapps"),
    s("Startup apps", "startup autostart launch at login boot", "ms-settings:startupapps"),
    // Accounts
    s("Your info", "account microsoft account profile picture", "ms-settings:yourinfo"),
    s("Sign-in options", "password pin windows hello fingerprint face login", "ms-settings:signinoptions"),
    s("Email & accounts", "mail accounts work school", "ms-settings:emailandaccounts"),
    s("Other users", "add user family accounts", "ms-settings:otherusers"),
    s("Windows Backup", "onedrive sync remember preferences", "ms-settings:backup"),
    // Time & language
    s("Date & time", "clock time zone sync", "ms-settings:dateandtime"),
    s("Language & region", "display language region format country", "ms-settings:regionlanguage"),
    s("Keyboard", "keyboard layout input method languages", "ms-settings:keyboard"),
    s("Speech", "voice recognition", "ms-settings:speech"),
    // Gaming
    s("Game Bar", "xbox game bar gaming overlay", "ms-settings:gaming-gamebar"),
    s("Game Mode", "gaming performance", "ms-settings:gaming-gamemode"),
    s("Captures", "screen recording clips game dvr", "ms-settings:gaming-gamedvr"),
    // Accessibility
    s("Accessibility", "ease of access", "ms-settings:easeofaccess"),
    s("Text size", "make text bigger font size", "ms-settings:easeofaccess-display"),
    s("Visual effects", "animation effects transparency scrollbars", "ms-settings:easeofaccess-visualeffects"),
    s("Mouse pointer and touch", "cursor size color", "ms-settings:easeofaccess-mousepointer"),
    s("Magnifier", "zoom screen", "ms-settings:easeofaccess-magnifier"),
    s("Narrator", "screen reader", "ms-settings:easeofaccess-narrator"),
    s("Color filters", "colorblind grayscale invert", "ms-settings:easeofaccess-colorfilter"),
    s("Contrast themes", "high contrast", "ms-settings:easeofaccess-highcontrast"),
    s("Keyboard accessibility", "sticky keys filter keys on-screen keyboard", "ms-settings:easeofaccess-keyboard"),
    // Privacy & security
    s("Privacy & security", "permissions privacy", "ms-settings:privacy"),
    s("Location", "gps location services", "ms-settings:privacy-location"),
    s("Camera", "webcam camera permissions", "ms-settings:privacy-webcam"),
    s("Microphone", "mic permissions", "ms-settings:privacy-microphone"),
    s("Find my device", "lost device", "ms-settings:findmydevice"),
    s("Windows Security", "antivirus defender virus firewall protection", "windowsdefender:"),
    // Windows Update
    s("Windows Update", "updates check for updates patch upgrade", "ms-settings:windowsupdate"),
    s("Windows Update history", "installed updates", "ms-settings:windowsupdate-history"),
    // Control Panel applets and tools (programs and snap-ins in System32)
    tool("Device Manager", "drivers hardware devices", "devmgmt.msc"),
    tool("Disk Management", "partitions drives format volume disks", "diskmgmt.msc"),
    tool("Computer Management", "admin management", "compmgmt.msc"),
    tool("Group Policy Editor", "gpedit policies", "gpedit.msc"),
    tool("Local Users and Groups", "users groups lusrmgr", "lusrmgr.msc"),
    tool("Certificates", "certmgr", "certmgr.msc"),
    tool("System Configuration", "msconfig boot safe mode", "msconfig.exe"),
    tool("DirectX Diagnostic Tool", "dxdiag graphics card gpu info", "dxdiag.exe"),
    tool("Character Map", "symbols special characters charmap", "charmap.exe"),
    tool("On-Screen Keyboard", "osk virtual keyboard", "osk.exe"),
    tool("Color Management", "icc color profile monitor calibration", "colorcpl.exe"),
    tool("Display Color Calibration", "dccw calibrate gamma", "dccw.exe"),
    tool("User Accounts", "netplwiz automatic sign in passwords", "netplwiz.exe"),
    cp("System Properties", "computer name advanced system settings performance options virtual memory paging file", "sysdm.cpl", None),
    cp("Environment Variables", "path env variables system variables", "rundll32.exe", Some("sysdm.cpl,EditEnvironmentVariables")),
    cp("Network Connections", "network adapters ip address dns ncpa", "ncpa.cpl", None),
    cp("Programs and Features", "uninstall a program appwiz", "appwiz.cpl", None),
    cp("Sound devices", "playback recording devices mmsys audio", "mmsys.cpl", None),
    cp("Power Options", "power plan high performance balanced powercfg", "powercfg.cpl", None),
    cp("Mouse Properties", "double click speed pointer main.cpl", "main.cpl", None),
    cp("Internet Options", "inetcpl proxy internet properties", "inetcpl.cpl", None),
    cp("Windows Defender Firewall", "firewall allow app port", "firewall.cpl", None),
    cp("Credential Manager", "saved passwords credentials windows credentials", "control.exe", Some("/name Microsoft.CredentialManager")),
    cp("Network and Sharing Center", "sharing adapter settings", "control.exe", Some("/name Microsoft.NetworkAndSharingCenter")),
    cp("Devices and Printers", "printers devices", "control.exe", Some("printers")),
    folder("Recycle Bin", "trash deleted files", "shell:RecycleBinFolder"),
    folder("Startup folder", "startup programs autostart shell startup", "shell:startup"),
];

#[cfg(test)]
mod tests {
    use super::WINDOWS;
    use crate::search::rank_words;

    fn top(q: &str) -> &'static str {
        let items: Vec<(&str, &str)> = WINDOWS.iter().map(|i| (i.name, i.keywords)).collect();
        WINDOWS[*rank_words(q, &items, |_| 0).first().expect("a match")].name
    }

    #[test]
    fn everyday_words_find_the_page() {
        for (q, want) in [
            ("display", "Display"),
            ("bluetooth", "Bluetooth & devices"),
            ("wifi", "Wi-Fi"),
            ("dark mode", "Colors"),
            ("device manager", "Device Manager"),
            ("uninstall", "Installed apps"),
            ("environment", "Environment Variables"),
            ("resolution", "Display"),
            ("update", "Windows Update"),
            ("sound", "Sound"),
            ("wallpaper", "Background"),
            ("startup", "Startup apps"),
        ] {
            assert_eq!(top(q), want, "query {q:?}");
        }
    }

    #[test]
    fn hidden_words_need_the_whole_word() {
        let items: Vec<(&str, &str)> = WINDOWS.iter().map(|i| (i.name, i.keywords)).collect();
        assert!(rank_words("chrome", &items, |_| 0).is_empty());
    }

    #[test]
    fn names_are_unique_and_targets_set() {
        let mut seen = std::collections::HashSet::new();
        for i in WINDOWS {
            assert!(seen.insert(i.name.to_lowercase()), "duplicate {}", i.name);
            assert!(!i.target.is_empty());
            assert_eq!(i.keywords, i.keywords.to_lowercase(), "{} keywords lowercase", i.name);
        }
    }
}
