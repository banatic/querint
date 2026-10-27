use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::System::Registry::{RegDeleteKeyValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("Querint");

#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
pub struct Settings {
    /// Printers shown in the window; their queue is held unless listed in `bypass`.
    pub managed: Vec<String>,
    /// Printers we have paused; kept so unmanaged ones are resumed once drained.
    pub held_by_us: Vec<String>,
    /// Managed printers whose new jobs currently pass straight through.
    pub bypass: Vec<String>,
    pub autostart: bool,
    pub notify: bool,
}

fn path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("Querint").join("settings.json")
}

impl Settings {
    pub fn load() -> Self {
        fs::read_to_string(path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let p = path();
        if let Some(dir) = p.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = fs::write(p, json);
        }
    }
}

/// Per-user Run key; the app needs no elevation.
pub fn set_autostart(on: bool) -> bool {
    unsafe {
        if !on {
            let err = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE);
            return err.is_ok() || err.0 == 2; // ERROR_FILE_NOT_FOUND: already off
        }
        let Ok(exe) = std::env::current_exe() else { return false };
        let value = HSTRING::from(format!("\"{}\"", exe.display()));
        let bytes = (value.len() + 1) * 2;
        RegSetKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, REG_SZ.0, Some(value.as_ptr() as *const _), bytes as u32)
            .is_ok()
    }
}
