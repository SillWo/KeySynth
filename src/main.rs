#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod events;
mod i18n;
mod keyboard;
mod mapping;
mod settings;
mod synth;
mod tray;
mod ui;

fn main() {
    if let Err(error) = ui::run() {
        use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
        use windows::core::w;

        let message: Vec<u16> = error.encode_utf16().chain(Some(0)).collect();
        unsafe {
            let _ = MessageBoxW(
                None,
                windows::core::PCWSTR(message.as_ptr()),
                w!("KeySynth"),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}
