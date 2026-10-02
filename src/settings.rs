use std::io;

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_CREATE_KEY_DISPOSITION, REG_DWORD,
    REG_OPTION_NON_VOLATILE, REG_SZ, REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW, RegOpenKeyExW,
    RegQueryValueExW, RegSetValueExW,
};
use windows::core::PCWSTR;

const SETTINGS_KEY: &str = "Software\\KeySynth";
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "KeySynth";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u32)]
pub enum Language {
    English = 0,
    #[default]
    Russian = 1,
}

impl Language {
    pub fn from_u32(value: u32) -> Self {
        match value {
            0 => Self::English,
            1 => Self::Russian,
            _ => Self::Russian,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u32)]
pub enum Waveform {
    #[default]
    Sine = 0,
    Triangle = 1,
    Sawtooth = 2,
    Square = 3,
}

impl Waveform {
    pub fn from_u32(value: u32) -> Self {
        match value {
            1 => Self::Triangle,
            2 => Self::Sawtooth,
            3 => Self::Square,
            _ => Self::Sine,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u32)]
pub enum Mode {
    #[default]
    WickiHayden = 0,
    Chromatic = 1,
    Random = 2,
}

impl Mode {
    pub fn from_u32(value: u32) -> Self {
        match value {
            1 => Self::Chromatic,
            2 => Self::Random,
            _ => Self::WickiHayden,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Settings {
    pub enabled: bool,
    pub language: Language,
    pub volume: u32,
    pub transpose: i32,
    pub waveform: Waveform,
    pub mode: Mode,
    pub attack_ms: u32,
    pub release_ms: u32,
    /// Mirrors the current user's Run-key entry; change it with `set_autostart`.
    pub start_with_windows: bool,
    pub minimize_to_tray: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            language: Language::Russian,
            volume: 25,
            transpose: 0,
            waveform: Waveform::Sine,
            mode: Mode::WickiHayden,
            attack_ms: 10,
            release_ms: 50,
            start_with_windows: false,
            minimize_to_tray: true,
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        let mut settings = Self::default();
        let Ok(key) = open_key(SETTINGS_KEY, KEY_QUERY_VALUE) else {
            settings.start_with_windows = autostart_enabled();
            return settings;
        };

        settings.enabled = read_dword(key, "Enabled")
            .map(|v| v != 0)
            .unwrap_or(settings.enabled);
        settings.language = Language::from_u32(read_dword(key, "Language").unwrap_or(1));
        settings.volume = read_dword(key, "Volume")
            .filter(|&v| v <= 100)
            .unwrap_or(settings.volume);
        settings.transpose = read_dword(key, "Transpose")
            .map(|v| v as i32)
            .filter(|v| (-24..=24).contains(v))
            .unwrap_or(settings.transpose);
        settings.waveform = Waveform::from_u32(read_dword(key, "Waveform").unwrap_or(0));
        settings.mode = Mode::from_u32(read_dword(key, "Mode").unwrap_or(0));
        settings.attack_ms = read_dword(key, "AttackMs")
            .filter(|&v| v <= 200)
            .unwrap_or(settings.attack_ms);
        settings.release_ms = read_dword(key, "ReleaseMs")
            .filter(|&v| v <= 500)
            .unwrap_or(settings.release_ms);
        settings.minimize_to_tray =
            read_dword(key, "MinimizeToTray").unwrap_or(settings.minimize_to_tray as u32) != 0;
        settings.start_with_windows = autostart_enabled();
        unsafe {
            let _ = RegCloseKey(key);
        }
        settings
    }

    pub fn save(&self) -> io::Result<()> {
        // Startup is managed separately through the Run key so ordinary control
        // changes never rewrite the user's startup registration.
        let key = create_key(SETTINGS_KEY).map_err(registry_io_error)?;
        let values = [
            ("Enabled", self.enabled as u32),
            ("Language", self.language as u32),
            ("Volume", self.volume.min(100)),
            ("Transpose", self.transpose.clamp(-24, 24) as u32),
            ("Waveform", self.waveform as u32),
            ("Mode", self.mode as u32),
            ("AttackMs", self.attack_ms.min(200)),
            ("ReleaseMs", self.release_ms.min(500)),
            ("MinimizeToTray", self.minimize_to_tray as u32),
        ];
        let mut result = Ok(());
        for (name, value) in values {
            if let Err(error) = write_dword(key, name, value) {
                result = Err(registry_io_error(error));
                break;
            }
        }
        unsafe {
            let _ = RegCloseKey(key);
        }
        result?;
        Ok(())
    }
}

/// Adds or removes the current executable from the current user's Run key.
pub fn set_autostart(enabled: bool) -> Result<(), String> {
    let command = if enabled {
        let exe = std::env::current_exe().map_err(|e| format!("Could not find executable: {e}"))?;
        Some(format!("\"{}\" --tray", exe.display()))
    } else {
        None
    };
    let key = create_key(RUN_KEY).map_err(|e| format!("Could not open Run registry key: {e}"))?;
    let result = if let Some(command) = command {
        write_string(key, RUN_VALUE, &command)
            .map_err(|e| format!("Could not set startup entry: {e}"))
    } else {
        unsafe {
            let name = wide(RUN_VALUE);
            let status =
                windows::Win32::System::Registry::RegDeleteValueW(key, PCWSTR(name.as_ptr()));
            if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
                Ok(())
            } else {
                Err(format!("Could not remove startup entry: {}", status.0))
            }
        }
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    result
}

pub fn autostart_enabled() -> bool {
    let Ok(key) = open_key(RUN_KEY, KEY_QUERY_VALUE) else {
        return false;
    };
    let result = read_string(key, RUN_VALUE).is_some();
    unsafe {
        let _ = RegCloseKey(key);
    }
    result
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn open_key(
    path: &str,
    access: windows::Win32::System::Registry::REG_SAM_FLAGS,
) -> Result<HKEY, u32> {
    let mut key = HKEY::default();
    let path = wide(path);
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            Some(0),
            access,
            &mut key,
        )
    };
    if status == ERROR_SUCCESS {
        Ok(key)
    } else {
        Err(status.0)
    }
}

fn create_key(path: &str) -> Result<HKEY, u32> {
    let mut key = HKEY::default();
    let path = wide(path);
    let mut disposition = REG_CREATE_KEY_DISPOSITION(0);
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            Some(0),
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE | KEY_QUERY_VALUE,
            None,
            &mut key,
            Some(&mut disposition),
        )
    };
    if status == ERROR_SUCCESS {
        Ok(key)
    } else {
        Err(status.0)
    }
}

fn read_dword(key: HKEY, name: &str) -> Option<u32> {
    let name = wide(name);
    let mut value = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let mut kind = REG_VALUE_TYPE(0);
    let status = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut kind),
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    };
    (status == ERROR_SUCCESS && kind == REG_DWORD && size == 4).then_some(value)
}

fn write_dword(key: HKEY, name: &str, value: u32) -> Result<(), u32> {
    let name = wide(name);
    let bytes = unsafe { std::slice::from_raw_parts((&value as *const u32).cast::<u8>(), 4) };
    let status =
        unsafe { RegSetValueExW(key, PCWSTR(name.as_ptr()), Some(0), REG_DWORD, Some(bytes)) };
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(status.0)
    }
}

fn write_string(key: HKEY, name: &str, value: &str) -> Result<(), u32> {
    let name = wide(name);
    let value = wide(value);
    let bytes = (value.len() * std::mem::size_of::<u16>()) as u32;
    let bytes = unsafe { std::slice::from_raw_parts(value.as_ptr().cast::<u8>(), bytes as usize) };
    let status =
        unsafe { RegSetValueExW(key, PCWSTR(name.as_ptr()), Some(0), REG_SZ, Some(bytes)) };
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(status.0)
    }
}

fn read_string(key: HKEY, name: &str) -> Option<String> {
    let name = wide(name);
    let mut kind = REG_VALUE_TYPE(0);
    let mut size = 0;
    let status = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut kind),
            None,
            Some(&mut size),
        )
    };
    if status != ERROR_SUCCESS || kind != REG_SZ || size < 2 {
        return None;
    }
    let mut value = vec![0u16; (size as usize).div_ceil(2)];
    let status = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut kind),
            Some(value.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let end = value
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(value.len());
    String::from_utf16(&value[..end]).ok()
}

fn registry_io_error(code: u32) -> io::Error {
    io::Error::from_raw_os_error(code as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_enum_values_are_stable() {
        let settings = Settings::default();
        assert!(settings.enabled);
        assert_eq!(settings.language, Language::Russian);
        assert_eq!(settings.volume, 25);
        assert_eq!(settings.transpose, 0);
        assert_eq!(settings.waveform, Waveform::Sine);
        assert_eq!(settings.mode, Mode::WickiHayden);
        assert_eq!((settings.attack_ms, settings.release_ms), (10, 50));
        assert!(!settings.start_with_windows);
        assert!(settings.minimize_to_tray);
        assert_eq!(
            [
                Waveform::Sine as u32,
                Waveform::Triangle as u32,
                Waveform::Sawtooth as u32,
                Waveform::Square as u32
            ],
            [0, 1, 2, 3]
        );
        assert_eq!(Language::English as u32, 0);
        assert_eq!(Language::Russian as u32, 1);
        assert_eq!(Language::from_u32(0), Language::English);
        assert_eq!(Language::from_u32(1), Language::Russian);
        assert_eq!(Language::from_u32(u32::MAX), Language::Russian);
        assert_eq!(
            [
                Mode::WickiHayden as u32,
                Mode::Chromatic as u32,
                Mode::Random as u32
            ],
            [0, 1, 2]
        );
    }
}
