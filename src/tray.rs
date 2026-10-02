use crate::i18n::{Text, text};
use crate::settings::Language;
use std::mem::size_of;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, HICON, MF_SEPARATOR, MF_STRING,
    SetForegroundWindow, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TrackPopupMenu,
};
use windows::core::PCWSTR;

pub const TRAY_CALLBACK: u32 = 0x8001;
pub const CMD_OPEN: u16 = 41001;
pub const CMD_TOGGLE: u16 = 41002;
pub const CMD_EXIT: u16 = 41003;

pub struct TrayIcon {
    data: NOTIFYICONDATAW,
    installed: bool,
}

impl TrayIcon {
    pub fn install(hwnd: HWND, icon: HICON) -> Result<Self, String> {
        let tip: Vec<u16> = "KeySynth".encode_utf16().chain(Some(0)).collect();
        let mut tip_buffer = [0u16; 128];
        let count = tip.len().min(tip_buffer.len());
        tip_buffer[..count].copy_from_slice(&tip[..count]);
        let data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: TRAY_CALLBACK,
            hIcon: icon,
            szTip: tip_buffer,
            ..Default::default()
        };
        if unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
            Ok(Self {
                data,
                installed: true,
            })
        } else {
            Err("Could not add the KeySynth icon to the system tray.".to_owned())
        }
    }

    pub fn remove(&mut self) {
        if self.installed {
            let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &self.data) };
            self.installed = false;
        }
    }

    pub fn is_installed(&self) -> bool {
        self.installed
    }

    pub fn show_menu(hwnd: HWND, enabled: bool, language: Language) {
        unsafe {
            let Ok(menu) = CreatePopupMenu() else {
                return;
            };
            let toggle_text = text(
                language,
                if enabled {
                    Text::DisableSounds
                } else {
                    Text::EnableSounds
                },
            );
            let open_text: Vec<u16> = text(language, Text::Open)
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let toggle_wide: Vec<u16> = toggle_text.encode_utf16().chain(Some(0)).collect();
            let exit_text: Vec<u16> = text(language, Text::Exit)
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                CMD_OPEN as usize,
                PCWSTR(open_text.as_ptr()),
            );
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                CMD_TOGGLE as usize,
                PCWSTR(toggle_wide.as_ptr()),
            );
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                CMD_EXIT as usize,
                PCWSTR(exit_text.as_ptr()),
            );

            let mut point = windows::Win32::Foundation::POINT::default();
            let _ = GetCursorPos(&mut point);
            let _ = SetForegroundWindow(hwnd);
            let _ = TrackPopupMenu(
                menu,
                TPM_LEFTALIGN | TPM_BOTTOMALIGN,
                point.x,
                point.y,
                None,
                hwnd,
                None,
            );
            let _ = DestroyMenu(menu);
        }
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        self.remove();
    }
}

pub fn callback_event(lparam: LPARAM) -> u16 {
    lparam.0 as u16
}
