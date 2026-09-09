//! Restore the native window independently of egui's suspended repaint loop.
use anyhow::{Result, bail};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    IsIconic, SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow,
};

#[derive(Clone, Copy)]
pub(super) struct WindowRestore {
    hwnd: std::num::NonZeroIsize,
    thread: std::thread::ThreadId,
}

impl WindowRestore {
    pub(super) fn new(context: &eframe::CreationContext<'_>) -> Result<Self> {
        let RawWindowHandle::Win32(handle) = context.window_handle()?.as_raw() else {
            bail!("expected a Windows control window");
        };
        Ok(Self {
            hwnd: handle.hwnd,
            thread: std::thread::current().id(),
        })
    }

    #[allow(unsafe_code)]
    pub(super) fn restore(self) {
        if self.thread != std::thread::current().id() {
            tracing::warn!("tray callback arrived off the UI thread; skipping window restore");
            return;
        }
        let hwnd = self.hwnd.get() as *mut std::ffi::c_void;
        // SAFETY: the handle comes from eframe's live root window. Tray callbacks
        // run on its UI thread and call this only after successfully sending to
        // the receiver owned by ControlWindow, which drops before the window.
        // These APIs neither own nor dereference application memory. Preserve maximization when the window is hidden.
        unsafe {
            ShowWindow(
                hwnd,
                if IsIconic(hwnd) != 0 {
                    SW_RESTORE
                } else {
                    SW_SHOW
                },
            );
            SetForegroundWindow(hwnd);
        }
    }
}

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, IsWindowVisible, IsZoomed, SW_HIDE, SW_MAXIMIZE,
        SW_MINIMIZE, WS_OVERLAPPEDWINDOW,
    };

    #[test]
    fn restores_hidden_and_minimized_window_without_rendering() {
        // SAFETY: STATIC is a system window class; all optional pointers are
        // null. The test owns this window and destroys it on the creating thread.
        unsafe {
            let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                320,
                240,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            assert!(!hwnd.is_null());
            let window = WindowRestore {
                hwnd: std::num::NonZeroIsize::new(hwnd as isize).unwrap(),
                thread: std::thread::current().id(),
            };
            // No egui frame or message-pump redraw is needed for restoration.
            for _ in 0..3 {
                ShowWindow(hwnd, SW_HIDE);
                assert_eq!(IsWindowVisible(hwnd), 0);
                window.restore();
                assert_ne!(IsWindowVisible(hwnd), 0);
                assert_eq!(IsIconic(hwnd), 0);

                ShowWindow(hwnd, SW_MINIMIZE);
                assert_ne!(IsIconic(hwnd), 0);
                window.restore();
                assert_ne!(IsWindowVisible(hwnd), 0);
                assert_eq!(IsIconic(hwnd), 0);

                ShowWindow(hwnd, SW_MINIMIZE);
                ShowWindow(hwnd, SW_HIDE);
                window.restore();
                assert_ne!(IsWindowVisible(hwnd), 0);
                assert_eq!(IsIconic(hwnd), 0);
            }
            ShowWindow(hwnd, SW_MAXIMIZE);
            ShowWindow(hwnd, SW_HIDE);
            window.restore();
            assert_ne!(IsWindowVisible(hwnd), 0);
            assert_ne!(IsZoomed(hwnd), 0);
            assert_ne!(DestroyWindow(hwnd), 0);
        }
    }
}
