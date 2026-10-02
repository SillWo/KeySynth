use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::SetEvent;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED,
    LLKHF_LOWER_IL_INJECTED, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN,
    WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::events::{KeyEvent, Shared};

thread_local! {
    // Low-level hooks are called on the thread which installed the hook. Keeping
    // the shared pointer and pressed-key bitmap in TLS avoids locks/allocations
    // in the hook callback. KeyboardHook owns the Arc for this pointer's life.
    static HOOK_SHARED: Cell<*const Shared> = const { Cell::new(std::ptr::null()) };
    static PRESSED: Cell<[bool; 512]> = const { Cell::new([false; 512]) };
}

pub struct KeyboardHook {
    hook: HHOOK,
    shared: Arc<Shared>,
}

impl KeyboardHook {
    pub fn install(shared: Arc<Shared>) -> Result<Self, String> {
        HOOK_SHARED.with(|slot| slot.set(Arc::as_ptr(&shared)));
        PRESSED.with(|keys| keys.set([false; 512]));
        let module = unsafe { GetModuleHandleW(None) }.map_err(|error| {
            HOOK_SHARED.with(|slot| slot.set(std::ptr::null()));
            format!("Could not access the application module: {error}")
        })?;
        let hook = unsafe {
            SetWindowsHookExW(
                WH_KEYBOARD_LL,
                Some(keyboard_proc),
                Some(HINSTANCE(module.0)),
                0,
            )
        }
        .map_err(|error| {
            HOOK_SHARED.with(|slot| slot.set(std::ptr::null()));
            format!("Could not install the keyboard hook: {error}")
        })?;
        Ok(Self { hook, shared })
    }
}

impl Drop for KeyboardHook {
    fn drop(&mut self) {
        // Remove the callback before releasing its Arc-backed context.
        let _ = unsafe { UnhookWindowsHookEx(self.hook) };
        HOOK_SHARED.with(|slot| slot.set(std::ptr::null()));
        PRESSED.with(|keys| keys.set([false; 512]));
        let _ = &self.shared;
    }
}

/// Wakes the single audio worker after a producer adds input to its queue.
pub fn notify_audio(shared: &Shared) {
    let handle = shared.wake_handle.load(Ordering::Acquire);
    if handle != 0 {
        // SAFETY: AudioEngine publishes a live auto-reset event handle and clears
        // it before closing the handle. The caller retains Shared throughout.
        let _ = unsafe {
            SetEvent(windows::Win32::Foundation::HANDLE(
                handle as *mut std::ffi::c_void,
            ))
        };
    }
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: Windows supplies a KBDLLHOOKSTRUCT for HC_ACTION.
        let data = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        // Synthetic input is deliberately ignored so automation and virtual
        // keyboards cannot trigger notes. Physical events always flow onward.
        let injected = data.flags.0 & (LLKHF_INJECTED.0 | LLKHF_LOWER_IL_INJECTED.0) != 0;
        if !injected {
            let message = wparam.0 as u32;
            let down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
            let up = matches!(message, WM_KEYUP | WM_SYSKEYUP);
            if down || up {
                let mut key_id = (data.scanCode & 0xff) as usize;
                if data.flags.0 & LLKHF_EXTENDED.0 != 0 {
                    key_id |= 0x100;
                }
                // A few synthetic/firmware keys carry no scan code. Keep them
                // distinct and within the bounded 512-key namespace.
                if key_id == 0 {
                    key_id = 0x100 | (data.vkCode & 0xff) as usize;
                }
                let key = key_id.min(511);
                let prior = PRESSED.with(|keys| {
                    let mut pressed = keys.get();
                    let was_down = pressed[key];
                    pressed[key] = down;
                    keys.set(pressed);
                    was_down
                });
                // Suppress autorepeat keydowns and unmatched keyups.
                if (down && !prior) || (up && prior) {
                    HOOK_SHARED.with(|slot| {
                        let shared = slot.get();
                        if !shared.is_null() {
                            // SAFETY: the owning KeyboardHook retains the Arc
                            // until the hook has been removed.
                            let shared = unsafe { &*shared };
                            if !shared.queue.push(KeyEvent {
                                key_id: key as u16,
                                down,
                            }) {
                                shared.overflow.store(true, Ordering::Release);
                            }
                            notify_audio(shared);
                        }
                    });
                }
            }
        }
    }
    // Never consume keystrokes; this app observes physical keyboard input only.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}
