use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{
    COLORREF, CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HINSTANCE, HWND, LPARAM,
    LRESULT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, COLOR_WINDOW, CreateFontW,
    CreateSolidBrush, DEFAULT_CHARSET, DeleteObject, EndPaint, FillRect, GetStockObject,
    GetSysColorBrush, HBRUSH, HDC, HFONT, HGDIOBJ, InvalidateRect, NULL_PEN, OPAQUE,
    OUT_DEFAULT_PRECIS, PAINTSTRUCT, RoundRect, SelectObject, SetBkColor, SetBkMode, SetTextColor,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemServices::{SS_LEFT, SS_RIGHT};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Controls::{
    BST_CHECKED, BST_UNCHECKED, ICC_BAR_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx,
    TBM_SETPOS, TBM_SETRANGE, TBS_NOTICKS,
};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey, VK_M,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::audio::AudioEngine;
use crate::events::Shared;
use crate::i18n::{Text, text};
use crate::keyboard::KeyboardHook;
use crate::settings::{self, Language, Mode, Settings, Waveform};
use crate::tray::{self, TrayIcon};

const CLASS_NAME: PCWSTR = w!("KeySynthMainWindow");
const APP_HOTKEY_ID: i32 = 1;
const WM_TRAY: u32 = tray::TRAY_CALLBACK;
const WM_AUDIO_ERROR: u32 = WM_APP + 2;
const WM_AUDIO_READY: u32 = WM_APP + 3;
const WM_APP_EXIT: u32 = WM_APP + 4;
const TBM_GETPOS: u32 = WM_USER;

const ID_ENABLED: i32 = 100;
const ID_VOLUME: i32 = 101;
const ID_VOLUME_VALUE: i32 = 102;
const ID_TRANSPOSE: i32 = 103;
const ID_TRANSPOSE_VALUE: i32 = 104;
const ID_WAVEFORM: i32 = 105;
const ID_MODE: i32 = 106;
const ID_ATTACK: i32 = 107;
const ID_ATTACK_VALUE: i32 = 108;
const ID_RELEASE: i32 = 109;
const ID_RELEASE_VALUE: i32 = 110;
const ID_AUTOSTART: i32 = 111;
const ID_MINIMIZE: i32 = 112;
const ID_STATUS: i32 = 113;
const ID_LANGUAGE: i32 = 114;

struct Controls {
    title: HWND,
    enabled: HWND,
    subtitle: HWND,
    language_label: HWND,
    language: HWND,
    volume_label: HWND,
    volume: HWND,
    volume_value: HWND,
    transpose: HWND,
    transpose_label: HWND,
    transpose_value: HWND,
    waveform: HWND,
    waveform_label: HWND,
    mode: HWND,
    mode_label: HWND,
    advanced_label: HWND,
    attack: HWND,
    attack_label: HWND,
    attack_value: HWND,
    release: HWND,
    release_label: HWND,
    release_value: HWND,
    autostart: HWND,
    minimize: HWND,
    status: HWND,
}

struct WindowState {
    settings: Settings,
    shared: Arc<Shared>,
    controls: Option<Controls>,
    tray: Option<TrayIcon>,
    audio: Option<AudioEngine>,
    keyboard: Option<KeyboardHook>,
    exiting: bool,
    hotkey_registered: bool,
    ownership_live: Arc<AtomicBool>,
    taskbar_created: u32,
    audio_available: bool,
    keyboard_available: bool,
    icon: windows::Win32::UI::WindowsAndMessaging::HICON,
    background_brush: HBRUSH,
    card_brush: HBRUSH,
    font: HFONT,
    title_font: HFONT,
    dpi: u32,
}

struct InstanceMutex(HANDLE);

impl Drop for InstanceMutex {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub fn run() -> Result<(), String> {
    unsafe {
        let instance_mutex = CreateMutexW(None, false, w!("Local\\KeySynth-Application")).ok();
        let _instance_mutex = instance_mutex.map(InstanceMutex);
        if _instance_mutex.is_some() && GetLastError() == ERROR_ALREADY_EXISTS {
            if let Ok(existing) = FindWindowW(CLASS_NAME, w!("KeySynth")) {
                let _ = ShowWindow(existing, SW_RESTORE);
                let _ = ShowWindow(existing, SW_SHOW);
                let _ = SetForegroundWindow(existing);
            }
            return Ok(());
        }
        let settings = Settings::load();
        let error_language = settings.language;
        let common_controls = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_BAR_CLASSES,
        };
        if !InitCommonControlsEx(&common_controls).as_bool() {
            return Err(text(settings.language, Text::ControlsError).to_owned());
        }
        let instance = GetModuleHandleW(None)
            .map_err(|_| text(settings.language, Text::ModuleError).to_owned())?;
        let instance = HINSTANCE(instance.0);
        let icon = load_app_icon(instance);
        let class = WNDCLASSW {
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hIcon: icon,
            hInstance: instance,
            lpszClassName: CLASS_NAME,
            lpfnWndProc: Some(window_proc),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err(text(settings.language, Text::WindowClassError).to_owned());
        }

        let shared = Arc::new(Shared::new(&settings));
        let ownership_live = Arc::new(AtomicBool::new(true));
        let taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated"));
        let state = Box::new(WindowState {
            settings,
            shared,
            controls: None,
            tray: None,
            audio: None,
            keyboard: None,
            exiting: false,
            hotkey_registered: false,
            ownership_live: ownership_live.clone(),
            taskbar_created,
            audio_available: false,
            keyboard_available: false,
            icon,
            background_brush: CreateSolidBrush(COLORREF(0x00F6F7F8)),
            card_brush: CreateSolidBrush(COLORREF(0x00FFFFFF)),
            font: create_font(16),
            title_font: create_font(30),
            dpi: 96,
        });
        let state_ptr = Box::into_raw(state);
        let hwnd_result = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            CLASS_NAME,
            w!("KeySynth"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            adjusted_window_size(
                520,
                632,
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
                WINDOW_EX_STYLE::default(),
            )
            .0,
            adjusted_window_size(
                520,
                632,
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
                WINDOW_EX_STYLE::default(),
            )
            .1,
            None,
            None,
            Some(instance),
            Some(state_ptr.cast()),
        );
        let hwnd = match hwnd_result {
            Ok(hwnd) => hwnd,
            Err(_) => {
                if ownership_live.load(Ordering::Acquire) {
                    drop(Box::from_raw(state_ptr));
                }
                return Err(text(error_language, Text::WindowCreateError).to_owned());
            }
        };
        if hwnd.0.is_null() {
            return Err(text(error_language, Text::WindowCreateError).to_owned());
        }
        let state_ref = state_from(hwnd);
        if let Some(state) = state_ref {
            let dpi = GetDpiForWindow(hwnd).max(72);
            let (width, height) = adjusted_window_size_for_dpi(
                scale_for_dpi(520, dpi),
                scale_for_dpi(632, dpi),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
                WINDOW_EX_STYLE::default(),
                dpi,
            );
            let mut current = RECT::default();
            let _ = GetWindowRect(hwnd, &mut current);
            let _ = SetWindowPos(
                hwnd,
                None,
                current.left,
                current.top,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            resize_controls(hwnd, state, dpi);
            start_services(hwnd, state);
            // --tray is consumed by main before entering; startup visibility is selected here.
            let tray_start = std::env::args().any(|arg| arg == "--tray");
            if tray_start && state.tray.as_ref().is_some_and(TrayIcon::is_installed) {
                let _ = ShowWindow(hwnd, SW_HIDE);
            } else {
                let _ = ShowWindow(hwnd, SW_SHOW);
            }
        }

        let mut message = MSG::default();
        loop {
            let result = GetMessageW(&mut message, None, 0, 0).0;
            if result == 0 {
                break;
            }
            if result == -1 {
                let _ = DestroyWindow(hwnd);
                break;
            }
            if IsDialogMessageW(hwnd, &message).as_bool() {
                continue;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        let _ = UnregisterClassW(CLASS_NAME, Some(instance));
    }
    Ok(())
}

unsafe fn start_services(hwnd: HWND, state: &mut WindowState) {
    unsafe {
        state
            .shared
            .window_handle
            .store(hwnd.0 as isize, Ordering::Release);
        if let Ok(tray_icon) = TrayIcon::install(hwnd, state.icon) {
            state.tray = Some(tray_icon);
        }
        state.hotkey_registered = RegisterHotKey(
            Some(hwnd),
            APP_HOTKEY_ID,
            MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
            VK_M.0 as u32,
        )
        .is_ok();
        match AudioEngine::start(state.shared.clone()) {
            Ok(engine) => {
                state.audio = Some(engine);
                state.audio_available = true;
            }
            Err(_) => state.audio_available = false,
        }
        match KeyboardHook::install(state.shared.clone()) {
            Ok(hook) => {
                state.keyboard = Some(hook);
                state.keyboard_available = true;
            }
            Err(_) => state.keyboard_available = false,
        }
        state.shared.apply(&state.settings);
        update_service_status(state);
    }
}

unsafe fn cleanup_services(hwnd: HWND, state: &mut WindowState) {
    unsafe {
        state
            .shared
            .shutdown
            .store(true, std::sync::atomic::Ordering::Release);
        state
            .shared
            .window_handle
            .store(0, std::sync::atomic::Ordering::Release);
        if state.hotkey_registered {
            let _ = UnregisterHotKey(Some(hwnd), APP_HOTKEY_ID);
            state.hotkey_registered = false;
        }
        state.keyboard.take();
        state.audio.take();
        if let Some(mut tray) = state.tray.take() {
            tray.remove();
        }
    }
}

unsafe fn create_controls(hwnd: HWND, state: &mut WindowState) {
    unsafe {
        let font = state.font;
        let controls = Controls {
            title: create_control_text(
                hwnd,
                "STATIC",
                "KeySynth",
                WINDOW_STYLE(SS_LEFT.0),
                22,
                16,
                250,
                36,
                0,
            ),
            enabled: create_control_text(
                hwnd,
                "BUTTON",
                &format!(
                    "{} {}",
                    text(state.settings.language, Text::KeySounds),
                    text(
                        state.settings.language,
                        if state.settings.enabled {
                            Text::On
                        } else {
                            Text::Off
                        }
                    )
                ),
                WINDOW_STYLE(BS_AUTOCHECKBOX as u32) | WS_TABSTOP,
                36,
                120,
                230,
                28,
                ID_ENABLED,
            ),
            subtitle: create_control_text(
                hwnd,
                "STATIC",
                text(state.settings.language, Text::Subtitle),
                WINDOW_STYLE(SS_LEFT.0),
                24,
                57,
                300,
                22,
                0,
            ),
            language_label: create_control_text(
                hwnd,
                "STATIC",
                text(state.settings.language, Text::Language),
                WINDOW_STYLE(SS_LEFT.0),
                318,
                24,
                76,
                24,
                0,
            ),
            language: create_combo(hwnd, ID_LANGUAGE, 396, 20, 105),
            volume_label: create_control_text(
                hwnd,
                "STATIC",
                text(state.settings.language, Text::Volume),
                WINDOW_STYLE(SS_LEFT.0),
                36,
                170,
                108,
                24,
                0,
            ),
            volume: create_slider(
                hwnd,
                ID_VOLUME,
                155,
                165,
                0,
                100,
                state.settings.volume as i32,
            ),
            volume_value: create_control(
                hwnd,
                "STATIC",
                w!(""),
                WINDOW_STYLE(SS_RIGHT.0),
                435,
                170,
                48,
                22,
                ID_VOLUME_VALUE,
            ),
            transpose_label: create_control_text(
                hwnd,
                "STATIC",
                text(state.settings.language, Text::Transpose),
                WINDOW_STYLE(SS_LEFT.0),
                36,
                220,
                108,
                24,
                0,
            ),
            transpose: create_slider(
                hwnd,
                ID_TRANSPOSE,
                155,
                215,
                -24,
                24,
                state.settings.transpose,
            ),
            transpose_value: create_control(
                hwnd,
                "STATIC",
                w!(""),
                WINDOW_STYLE(SS_RIGHT.0),
                435,
                220,
                48,
                22,
                ID_TRANSPOSE_VALUE,
            ),
            waveform_label: create_control_text(
                hwnd,
                "STATIC",
                text(state.settings.language, Text::Waveform),
                WINDOW_STYLE(SS_LEFT.0),
                36,
                270,
                108,
                24,
                0,
            ),
            waveform: create_combo(hwnd, ID_WAVEFORM, 155, 266, 346),
            mode_label: create_control_text(
                hwnd,
                "STATIC",
                text(state.settings.language, Text::Mode),
                WINDOW_STYLE(SS_LEFT.0),
                36,
                320,
                108,
                24,
                0,
            ),
            mode: create_combo(hwnd, ID_MODE, 155, 316, 346),
            advanced_label: create_control_text(
                hwnd,
                "STATIC",
                text(state.settings.language, Text::Advanced),
                WINDOW_STYLE(SS_LEFT.0),
                36,
                445,
                390,
                22,
                0,
            ),
            attack_label: create_control_text(
                hwnd,
                "STATIC",
                text(state.settings.language, Text::Attack),
                WINDOW_STYLE(SS_LEFT.0),
                36,
                478,
                108,
                24,
                0,
            ),
            attack: create_slider(
                hwnd,
                ID_ATTACK,
                155,
                473,
                0,
                200,
                state.settings.attack_ms as i32,
            ),
            attack_value: create_control(
                hwnd,
                "STATIC",
                w!(""),
                WINDOW_STYLE(SS_RIGHT.0),
                452,
                478,
                48,
                22,
                ID_ATTACK_VALUE,
            ),
            release_label: create_control_text(
                hwnd,
                "STATIC",
                text(state.settings.language, Text::Release),
                WINDOW_STYLE(SS_LEFT.0),
                36,
                528,
                108,
                24,
                0,
            ),
            release: create_slider(
                hwnd,
                ID_RELEASE,
                155,
                523,
                0,
                500,
                state.settings.release_ms as i32,
            ),
            release_value: create_control(
                hwnd,
                "STATIC",
                w!(""),
                WINDOW_STYLE(SS_RIGHT.0),
                452,
                528,
                48,
                22,
                ID_RELEASE_VALUE,
            ),
            autostart: create_control(
                hwnd,
                "BUTTON",
                PCWSTR(std::ptr::null()),
                WINDOW_STYLE(BS_AUTOCHECKBOX as u32) | WS_TABSTOP,
                36,
                575,
                250,
                24,
                ID_AUTOSTART,
            ),
            minimize: create_control(
                hwnd,
                "BUTTON",
                PCWSTR(std::ptr::null()),
                WINDOW_STYLE(BS_AUTOCHECKBOX as u32) | WS_TABSTOP,
                292,
                575,
                210,
                24,
                ID_MINIMIZE,
            ),
            status: create_control(
                hwnd,
                "STATIC",
                w!(""),
                WINDOW_STYLE(SS_LEFT.0),
                24,
                610,
                475,
                22,
                ID_STATUS,
            ),
        };
        for control in [
            controls.title,
            controls.enabled,
            controls.subtitle,
            controls.language_label,
            controls.language,
            controls.volume_label,
            controls.volume,
            controls.volume_value,
            controls.transpose,
            controls.transpose_label,
            controls.transpose_value,
            controls.waveform,
            controls.waveform_label,
            controls.mode,
            controls.mode_label,
            controls.advanced_label,
            controls.attack,
            controls.attack_label,
            controls.attack_value,
            controls.release,
            controls.release_label,
            controls.release_value,
            controls.autostart,
            controls.minimize,
            controls.status,
        ] {
            if !control.0.is_null() {
                let _ = SendMessageW(
                    control,
                    WM_SETFONT,
                    Some(WPARAM(font.0 as usize)),
                    Some(LPARAM(1)),
                );
            }
        }
        let _ = SendMessageW(
            controls.title,
            WM_SETFONT,
            Some(WPARAM(state.title_font.0 as usize)),
            Some(LPARAM(1)),
        );
        set_check(controls.enabled, state.settings.enabled);
        set_check(controls.autostart, state.settings.start_with_windows);
        set_check(controls.minimize, state.settings.minimize_to_tray);
        add_combo_items(controls.language, &["English", "Русский"]);
        add_combo_items(
            controls.waveform,
            &[
                text(state.settings.language, Text::Sine),
                text(state.settings.language, Text::Triangle),
                text(state.settings.language, Text::Sawtooth),
                text(state.settings.language, Text::Square),
            ],
        );
        add_combo_items(
            controls.mode,
            &[
                text(state.settings.language, Text::WickiHayden),
                text(state.settings.language, Text::Chromatic),
                text(state.settings.language, Text::Random),
            ],
        );
        let _ = SendMessageW(
            controls.language,
            CB_SETCURSEL,
            Some(WPARAM(state.settings.language as usize)),
            Some(LPARAM(0)),
        );
        let _ = SendMessageW(
            controls.waveform,
            CB_SETCURSEL,
            Some(WPARAM(state.settings.waveform as usize)),
            Some(LPARAM(0)),
        );
        let _ = SendMessageW(
            controls.mode,
            CB_SETCURSEL,
            Some(WPARAM(state.settings.mode as usize)),
            Some(LPARAM(0)),
        );
        state.controls = Some(controls);
        localize_controls(state);
        update_all_values(state);
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn create_control(
    hwnd: HWND,
    class: &str,
    text: PCWSTR,
    style: WINDOW_STYLE,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    id: i32,
) -> HWND {
    unsafe {
        let class_wide: Vec<u16> = class.encode_utf16().chain(Some(0)).collect();
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(class_wide.as_ptr()),
            text,
            WS_CHILD | WS_VISIBLE | style,
            x,
            y,
            width,
            height,
            Some(hwnd),
            Some(HMENU(id as usize as *mut _)),
            None,
            None,
        )
        .unwrap_or_default()
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn create_control_text(
    hwnd: HWND,
    class: &str,
    text: &str,
    style: WINDOW_STYLE,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    id: i32,
) -> HWND {
    unsafe {
        let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        create_control(
            hwnd,
            class,
            PCWSTR(wide.as_ptr()),
            style,
            x,
            y,
            width,
            height,
            id,
        )
    }
}

unsafe fn create_slider(
    hwnd: HWND,
    id: i32,
    x: i32,
    y: i32,
    min: i32,
    max: i32,
    value: i32,
) -> HWND {
    unsafe {
        let slider = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("msctls_trackbar32"),
            w!(""),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(TBS_NOTICKS),
            x,
            y,
            265,
            32,
            Some(hwnd),
            Some(HMENU(id as usize as *mut _)),
            None,
            None,
        )
        .unwrap_or_default();
        let range = ((max as u32) << 16) | (min as u16 as u32);
        let _ = SendMessageW(
            slider,
            TBM_SETRANGE,
            Some(WPARAM(1)),
            Some(LPARAM(range as isize)),
        );
        let _ = SendMessageW(
            slider,
            TBM_SETPOS,
            Some(WPARAM(1)),
            Some(LPARAM(value as isize)),
        );
        slider
    }
}

unsafe fn create_combo(hwnd: HWND, id: i32, x: i32, y: i32, width: i32) -> HWND {
    unsafe {
        create_control(
            hwnd,
            "COMBOBOX",
            w!(""),
            WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
            x,
            y,
            width,
            180,
            id,
        )
    }
}

unsafe fn add_combo_items(combo: HWND, items: &[&str]) {
    unsafe {
        for item in items {
            let wide: Vec<u16> = item.encode_utf16().chain(Some(0)).collect();
            let _ = SendMessageW(
                combo,
                CB_ADDSTRING,
                Some(WPARAM(0)),
                Some(LPARAM(wide.as_ptr() as isize)),
            );
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            return DefWindowProcW(hwnd, message, wparam, lparam);
        }
        let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowState;
        if state_ptr.is_null() {
            return DefWindowProcW(hwnd, message, wparam, lparam);
        }
        if message == (*state_ptr).taskbar_created {
            if let Some(mut tray) = (*state_ptr).tray.take() {
                tray.remove();
            }
            if let Ok(tray) = TrayIcon::install(hwnd, (*state_ptr).icon) {
                (*state_ptr).tray = Some(tray);
            }
            return LRESULT(0);
        }
        match message {
            WM_ERASEBKGND => {
                let hdc = HDC(wparam.0 as *mut _);
                let mut rect = RECT::default();
                let _ = GetClientRect(hwnd, &mut rect);
                let _ = FillRect(hdc, &rect, (*state_ptr).background_brush);
                LRESULT(1)
            }
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut paint);
                let scale = |value: i32| ((value as i64 * (*state_ptr).dpi as i64) / 96) as i32;
                let old_brush = SelectObject(hdc, HGDIOBJ((*state_ptr).card_brush.0));
                let old_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
                let _ = RoundRect(
                    hdc,
                    scale(18),
                    scale(98),
                    scale(502),
                    scale(410),
                    scale(18),
                    scale(18),
                );
                let _ = RoundRect(
                    hdc,
                    scale(18),
                    scale(428),
                    scale(502),
                    scale(560),
                    scale(18),
                    scale(18),
                );
                let _ = SelectObject(hdc, old_pen);
                let _ = SelectObject(hdc, old_brush);
                let _ = EndPaint(hwnd, &paint);
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let dpi = (wparam.0 >> 16) as u32;
                let suggested = *(lparam.0 as *const RECT);
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    suggested.left,
                    suggested.top,
                    suggested.right - suggested.left,
                    suggested.bottom - suggested.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                resize_controls(hwnd, &mut *state_ptr, dpi.max(72));
                LRESULT(0)
            }
            WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
                let hdc = HDC(wparam.0 as *mut _);
                let _ = SetTextColor(hdc, COLORREF(0x002A2F37));
                let child = HWND(lparam.0 as *mut _);
                let brush = if is_card_control(&*state_ptr, child) {
                    (*state_ptr).card_brush
                } else {
                    (*state_ptr).background_brush
                };
                let color = if is_card_control(&*state_ptr, child) {
                    COLORREF(0x00FFFFFF)
                } else {
                    COLORREF(0x00F6F7F8)
                };
                let _ = SetBkMode(hdc, OPAQUE);
                let _ = SetBkColor(hdc, color);
                LRESULT(brush.0 as isize)
            }
            WM_CREATE => {
                create_controls(hwnd, &mut *state_ptr);
                LRESULT(0)
            }
            WM_COMMAND => {
                handle_command(hwnd, &mut *state_ptr, wparam, lparam);
                LRESULT(0)
            }
            WM_HSCROLL => {
                handle_slider(&mut *state_ptr, lparam);
                LRESULT(0)
            }
            WM_HOTKEY if wparam.0 as i32 == APP_HOTKEY_ID => {
                toggle_enabled(&mut *state_ptr);
                LRESULT(0)
            }
            WM_TRAY => {
                match tray::callback_event(lparam) {
                    x if x == WM_LBUTTONDBLCLK as u16 => show_window(hwnd),
                    x if x == WM_RBUTTONUP as u16 || x == WM_CONTEXTMENU as u16 => {
                        TrayIcon::show_menu(
                            hwnd,
                            (*state_ptr).settings.enabled,
                            (*state_ptr).settings.language,
                        )
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_AUDIO_ERROR => {
                (*state_ptr).audio_available = false;
                update_service_status(&*state_ptr);
                LRESULT(0)
            }
            WM_AUDIO_READY => {
                (*state_ptr).audio_available = true;
                update_service_status(&*state_ptr);
                LRESULT(0)
            }
            WM_SIZE
                if wparam.0 == SIZE_MINIMIZED as usize
                    && (*state_ptr).settings.minimize_to_tray
                    && (*state_ptr)
                        .tray
                        .as_ref()
                        .is_some_and(TrayIcon::is_installed) =>
            {
                let _ = ShowWindow(hwnd, SW_HIDE);
                LRESULT(0)
            }
            WM_CLOSE => {
                if (*state_ptr).settings.minimize_to_tray
                    && (*state_ptr)
                        .tray
                        .as_ref()
                        .is_some_and(TrayIcon::is_installed)
                    && !(*state_ptr).exiting
                {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                } else {
                    let _ = PostMessageW(Some(hwnd), WM_APP_EXIT, WPARAM(0), LPARAM(0));
                }
                LRESULT(0)
            }
            WM_APP_EXIT => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                cleanup_services(hwnd, &mut *state_ptr);
                PostQuitMessage(0);
                LRESULT(0)
            }
            WM_NCDESTROY => {
                let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                (*state_ptr).ownership_live.store(false, Ordering::Release);
                let _ = DeleteObject(HGDIOBJ((*state_ptr).background_brush.0));
                let _ = DeleteObject(HGDIOBJ((*state_ptr).card_brush.0));
                let _ = DeleteObject(HGDIOBJ((*state_ptr).font.0));
                let _ = DeleteObject(HGDIOBJ((*state_ptr).title_font.0));
                let _ = Box::from_raw(state_ptr);
                DefWindowProcW(hwnd, message, wparam, lparam)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }
}

unsafe fn handle_command(hwnd: HWND, state: &mut WindowState, wparam: WPARAM, lparam: LPARAM) {
    unsafe {
        let id = (wparam.0 & 0xffff) as i32;
        let notification = ((wparam.0 >> 16) & 0xffff) as u16;
        if lparam.0 == 0 {
            match id as u16 {
                tray::CMD_OPEN => show_window(hwnd),
                tray::CMD_TOGGLE => toggle_enabled(state),
                tray::CMD_EXIT => {
                    state.exiting = true;
                    let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                _ => {}
            }
            return;
        }
        if notification == BN_CLICKED as u16 {
            match id {
                ID_ENABLED => {
                    if let Some(controls) = &state.controls {
                        state.settings.enabled = is_checked(controls.enabled);
                        localize_controls(state);
                        apply_settings(state);
                        persist(state);
                    }
                }
                ID_AUTOSTART => {
                    if let Some(controls) = &state.controls {
                        let enabled = is_checked(controls.autostart);
                        match settings::set_autostart(enabled) {
                            Ok(()) => state.settings.start_with_windows = enabled,
                            Err(_) => {
                                set_check(controls.autostart, state.settings.start_with_windows);
                                set_status(
                                    state,
                                    text(state.settings.language, Text::AutostartError),
                                );
                            }
                        }
                        persist(state);
                    }
                }
                ID_MINIMIZE => {
                    if let Some(controls) = &state.controls {
                        state.settings.minimize_to_tray = is_checked(controls.minimize);
                        persist(state);
                    }
                }
                _ => {}
            }
        } else if notification == CBN_SELCHANGE as u16
            && let Some(controls) = &state.controls
        {
            if id == ID_LANGUAGE {
                let selected = SendMessageW(
                    controls.language,
                    CB_GETCURSEL,
                    Some(WPARAM(0)),
                    Some(LPARAM(0)),
                )
                .0 as u32;
                state.settings.language = Language::from_u32(selected);
                localize_controls(state);
                update_all_values(state);
                persist(state);
                return;
            } else if id == ID_WAVEFORM {
                let selected = SendMessageW(
                    controls.waveform,
                    CB_GETCURSEL,
                    Some(WPARAM(0)),
                    Some(LPARAM(0)),
                )
                .0 as u32;
                state.settings.waveform = match selected {
                    1 => Waveform::Triangle,
                    2 => Waveform::Sawtooth,
                    3 => Waveform::Square,
                    _ => Waveform::Sine,
                };
            } else if id == ID_MODE {
                let selected = SendMessageW(
                    controls.mode,
                    CB_GETCURSEL,
                    Some(WPARAM(0)),
                    Some(LPARAM(0)),
                )
                .0 as u32;
                state.settings.mode = match selected {
                    1 => Mode::Chromatic,
                    2 => Mode::Random,
                    _ => Mode::WickiHayden,
                };
            }
            apply_settings(state);
            persist(state);
        }
    }
}

unsafe fn handle_slider(state: &mut WindowState, lparam: LPARAM) {
    unsafe {
        let Some(controls) = &state.controls else {
            return;
        };
        let control = HWND(lparam.0 as *mut _);
        let value = SendMessageW(control, TBM_GETPOS, Some(WPARAM(0)), Some(LPARAM(0))).0 as i32;
        if control == controls.volume {
            state.settings.volume = value.clamp(0, 100) as u32;
        } else if control == controls.transpose {
            state.settings.transpose = value.clamp(-24, 24);
        } else if control == controls.attack {
            state.settings.attack_ms = value.clamp(0, 200) as u32;
        } else if control == controls.release {
            state.settings.release_ms = value.clamp(0, 500) as u32;
        }
        update_all_values(state);
        apply_settings(state);
        persist(state);
    }
}

unsafe fn toggle_enabled(state: &mut WindowState) {
    unsafe {
        state.settings.enabled = !state.settings.enabled;
        if let Some(controls) = &state.controls {
            set_check(controls.enabled, state.settings.enabled);
        }
        localize_controls(state);
        apply_settings(state);
        persist(state);
    }
}

fn apply_settings(state: &mut WindowState) {
    state.shared.apply(&state.settings);
    crate::keyboard::notify_audio(&state.shared);
}

fn persist(state: &WindowState) {
    if state.settings.save().is_err() {
        unsafe {
            set_status_raw(
                state
                    .controls
                    .as_ref()
                    .map(|c| c.status)
                    .unwrap_or_default(),
                text(state.settings.language, Text::SaveError),
            );
        }
    }
}

unsafe fn set_status(state: &WindowState, text: &str) {
    unsafe {
        if let Some(controls) = &state.controls {
            set_status_raw(controls.status, text);
        }
    }
}

unsafe fn update_service_status(state: &WindowState) {
    unsafe {
        if state.audio_available && state.keyboard_available && state.hotkey_registered {
            let status = format!(
                "{} · Ctrl+Alt+M",
                text(state.settings.language, Text::AudioReady)
            );
            set_status(state, &status);
            return;
        }
        let status = if !state.audio_available {
            text(state.settings.language, Text::AudioUnavailable)
        } else if !state.keyboard_available {
            text(state.settings.language, Text::KeyboardUnavailable)
        } else if !state.hotkey_registered {
            text(state.settings.language, Text::HotkeyUnavailable)
        } else {
            text(state.settings.language, Text::AudioReady)
        };
        set_status(state, status);
    }
}

unsafe fn localize_controls(state: &WindowState) {
    unsafe {
        let Some(c) = &state.controls else {
            return;
        };
        let language = state.settings.language;
        let key_sound_label = format!(
            "{} {}",
            text(language, Text::KeySounds),
            text(
                language,
                if state.settings.enabled {
                    Text::On
                } else {
                    Text::Off
                }
            )
        );
        set_window_text(c.enabled, &key_sound_label);
        set_window_text(c.subtitle, text(language, Text::Subtitle));
        set_window_text(c.language_label, text(language, Text::Language));
        set_window_text(c.volume_label, text(language, Text::Volume));
        set_window_text(c.transpose_label, text(language, Text::Transpose));
        set_window_text(c.waveform_label, text(language, Text::Waveform));
        set_window_text(c.mode_label, text(language, Text::Mode));
        set_window_text(c.advanced_label, text(language, Text::Advanced));
        set_window_text(c.attack_label, text(language, Text::Attack));
        set_window_text(c.release_label, text(language, Text::Release));
        set_window_text(c.autostart, text(language, Text::StartWithWindows));
        set_window_text(c.minimize, text(language, Text::MinimizeToTray));
        update_service_status(state);

        let _ = SendMessageW(
            c.language,
            CB_RESETCONTENT,
            Some(WPARAM(0)),
            Some(LPARAM(0)),
        );
        add_combo_items(c.language, &["English", "Русский"]);
        let _ = SendMessageW(
            c.language,
            CB_SETCURSEL,
            Some(WPARAM(language as usize)),
            Some(LPARAM(0)),
        );
        let _ = SendMessageW(
            c.waveform,
            CB_RESETCONTENT,
            Some(WPARAM(0)),
            Some(LPARAM(0)),
        );
        add_combo_items(
            c.waveform,
            &[
                text(language, Text::Sine),
                text(language, Text::Triangle),
                text(language, Text::Sawtooth),
                text(language, Text::Square),
            ],
        );
        let _ = SendMessageW(
            c.waveform,
            CB_SETCURSEL,
            Some(WPARAM(state.settings.waveform as usize)),
            Some(LPARAM(0)),
        );
        let _ = SendMessageW(c.mode, CB_RESETCONTENT, Some(WPARAM(0)), Some(LPARAM(0)));
        add_combo_items(
            c.mode,
            &[
                text(language, Text::WickiHayden),
                text(language, Text::Chromatic),
                text(language, Text::Random),
            ],
        );
        let _ = SendMessageW(
            c.mode,
            CB_SETCURSEL,
            Some(WPARAM(state.settings.mode as usize)),
            Some(LPARAM(0)),
        );
    }
}

unsafe fn set_window_text(hwnd: HWND, text: &str) {
    unsafe {
        let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let _ = SetWindowTextW(hwnd, PCWSTR(wide.as_ptr()));
    }
}

unsafe fn set_status_raw(hwnd: HWND, text: &str) {
    unsafe {
        if !hwnd.0.is_null() {
            let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
            let _ = SetWindowTextW(hwnd, PCWSTR(wide.as_ptr()));
        }
    }
}

unsafe fn set_check(hwnd: HWND, checked: bool) {
    unsafe {
        let _ = SendMessageW(
            hwnd,
            BM_SETCHECK,
            Some(WPARAM(if checked {
                BST_CHECKED.0 as usize
            } else {
                BST_UNCHECKED.0 as usize
            })),
            Some(LPARAM(0)),
        );
    }
}

unsafe fn is_checked(hwnd: HWND) -> bool {
    unsafe {
        SendMessageW(hwnd, BM_GETCHECK, Some(WPARAM(0)), Some(LPARAM(0))).0
            == BST_CHECKED.0 as isize
    }
}

unsafe fn update_all_values(state: &WindowState) {
    unsafe {
        if let Some(c) = &state.controls {
            set_status_raw(c.volume_value, &format!("{}%", state.settings.volume));
            set_status_raw(
                c.transpose_value,
                &format!("{:+}", state.settings.transpose),
            );
            set_status_raw(
                c.attack_value,
                &format!(
                    "{} {}",
                    state.settings.attack_ms,
                    text(state.settings.language, Text::Ms)
                ),
            );
            set_status_raw(
                c.release_value,
                &format!(
                    "{} {}",
                    state.settings.release_ms,
                    text(state.settings.language, Text::Ms)
                ),
            );
        }
    }
}

unsafe fn show_window(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

fn adjusted_window_size(
    client_width: i32,
    client_height: i32,
    style: WINDOW_STYLE,
    ex_style: WINDOW_EX_STYLE,
) -> (i32, i32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: client_width,
        bottom: client_height,
    };
    unsafe {
        let _ = AdjustWindowRectEx(&mut rect, style, false, ex_style);
    }
    (rect.right - rect.left, rect.bottom - rect.top)
}

fn adjusted_window_size_for_dpi(
    client_width: i32,
    client_height: i32,
    style: WINDOW_STYLE,
    ex_style: WINDOW_EX_STYLE,
    dpi: u32,
) -> (i32, i32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: client_width,
        bottom: client_height,
    };
    unsafe {
        let _ = AdjustWindowRectExForDpi(&mut rect, style, false, ex_style, dpi);
    }
    (rect.right - rect.left, rect.bottom - rect.top)
}

fn scale_for_dpi(value: i32, dpi: u32) -> i32 {
    ((value as i64 * dpi as i64) / 96) as i32
}

fn is_card_control(state: &WindowState, child: HWND) -> bool {
    let Some(c) = &state.controls else {
        return false;
    };
    [
        c.enabled,
        c.volume_label,
        c.volume,
        c.volume_value,
        c.transpose_label,
        c.transpose,
        c.transpose_value,
        c.waveform_label,
        c.waveform,
        c.mode_label,
        c.mode,
        c.advanced_label,
        c.attack_label,
        c.attack,
        c.attack_value,
        c.release_label,
        c.release,
        c.release_value,
    ]
    .contains(&child)
}

fn load_app_icon(instance: HINSTANCE) -> windows::Win32::UI::WindowsAndMessaging::HICON {
    unsafe {
        LoadIconW(Some(instance), PCWSTR(101usize as *const u16))
            .unwrap_or_else(|_| LoadIconW(None, IDI_APPLICATION).unwrap_or_default())
    }
}

fn create_font(height: i32) -> HFONT {
    create_font_at_dpi(height, 96)
}

fn create_font_at_dpi(height: i32, dpi: u32) -> HFONT {
    unsafe {
        CreateFontW(
            -((height as u32 * dpi / 96) as i32),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            w!("Segoe UI"),
        )
    }
}

unsafe fn resize_controls(parent: HWND, state: &mut WindowState, dpi: u32) {
    unsafe {
        let Some(c) = &state.controls else {
            return;
        };
        let scale = |value: i32| ((value as i64 * dpi as i64) / 96) as i32;
        let controls = [
            (c.title, 22, 16, 250, 36),
            (c.subtitle, 24, 57, 300, 22),
            (c.language_label, 318, 24, 76, 24),
            (c.language, 396, 20, 105, 180),
            (c.enabled, 36, 120, 230, 28),
            (c.volume_label, 36, 170, 108, 24),
            (c.volume, 155, 165, 265, 32),
            (c.volume_value, 435, 170, 48, 22),
            (c.transpose_label, 36, 220, 108, 24),
            (c.transpose, 155, 215, 265, 32),
            (c.transpose_value, 435, 220, 48, 22),
            (c.waveform_label, 36, 270, 108, 24),
            (c.waveform, 155, 266, 346, 180),
            (c.mode_label, 36, 320, 108, 24),
            (c.mode, 155, 316, 346, 180),
            (c.advanced_label, 36, 445, 390, 22),
            (c.attack_label, 36, 478, 108, 24),
            (c.attack, 155, 473, 265, 32),
            (c.attack_value, 452, 478, 48, 22),
            (c.release_label, 36, 528, 108, 24),
            (c.release, 155, 523, 265, 32),
            (c.release_value, 452, 528, 48, 22),
            (c.autostart, 36, 575, 250, 24),
            (c.minimize, 292, 575, 210, 24),
            (c.status, 24, 610, 475, 22),
        ];
        for (hwnd, x, y, width, height) in controls {
            let _ = MoveWindow(hwnd, scale(x), scale(y), scale(width), scale(height), true);
        }
        let old_font = state.font;
        let old_title_font = state.title_font;
        state.font = create_font_at_dpi(16, dpi);
        state.title_font = create_font_at_dpi(30, dpi);
        state.dpi = dpi;
        for (hwnd, _, _, _, _) in controls {
            let font = if hwnd == c.title {
                state.title_font
            } else {
                state.font
            };
            let _ = SendMessageW(
                hwnd,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
        }
        let _ = DeleteObject(HGDIOBJ(old_font.0));
        let _ = DeleteObject(HGDIOBJ(old_title_font.0));
        let _ = InvalidateRect(Some(parent), None, true);
    }
}

unsafe fn state_from(hwnd: HWND) -> Option<&'static mut WindowState> {
    unsafe {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowState;
        if ptr.is_null() { None } else { Some(&mut *ptr) }
    }
}
