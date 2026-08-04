//! C ABI over the shared mobile runtime, linked into the iOS application.
//!
//! Every entry point returns a heap allocated UTF-8 C string that the caller
//! must release with [`tgws_free_string`]. `tgws_start` and `tgws_stop` return
//! `{"ok":bool,"error":string|null}`; `tgws_status` returns the status object.

use std::ffi::{CStr, CString, c_char};

fn into_c_string(value: String) -> *mut c_char {
    CString::new(value).map_or_else(
        |_| {
            CString::new(r#"{"ok":false,"error":"response contained a NUL byte"}"#)
                .expect("literal response is NUL-free")
                .into_raw()
        },
        CString::into_raw,
    )
}

/// Starts the proxy described by the JSON configuration string.
///
/// # Safety
/// `config_json` must be a valid NUL-terminated C string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tgws_start(config_json: *const c_char) -> *mut c_char {
    if config_json.is_null() {
        return into_c_string(r#"{"ok":false,"error":"configuration pointer is null"}"#.to_owned());
    }
    // SAFETY: the caller guarantees a valid NUL-terminated string.
    let raw = unsafe { CStr::from_ptr(config_json) };
    let Ok(config) = raw.to_str() else {
        return into_c_string(
            r#"{"ok":false,"error":"configuration is not valid UTF-8"}"#.to_owned(),
        );
    };
    into_c_string(tg_ws_proxy_mobile::start(config))
}

/// Requests a graceful shutdown of a running proxy.
#[unsafe(no_mangle)]
pub extern "C" fn tgws_stop() -> *mut c_char {
    into_c_string(tg_ws_proxy_mobile::stop())
}

/// Returns the current runtime state together with traffic counters.
#[unsafe(no_mangle)]
pub extern "C" fn tgws_status() -> *mut c_char {
    into_c_string(tg_ws_proxy_mobile::status())
}

/// Releases a string returned by this library.
///
/// # Safety
/// `pointer` must come from this library and must not be released twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tgws_free_string(pointer: *mut c_char) {
    if pointer.is_null() {
        return;
    }
    // SAFETY: the caller guarantees the pointer came from `CString::into_raw`.
    drop(unsafe { CString::from_raw(pointer) });
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, TcpListener, TcpStream};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;

    /// Calls an entry point and consumes the returned C string.
    fn take(pointer: *mut c_char) -> String {
        assert!(!pointer.is_null(), "bridge returned a null pointer");
        // SAFETY: the pointer came from one of the bridge entry points.
        let value = unsafe { CStr::from_ptr(pointer) }
            .to_str()
            .expect("bridge returns UTF-8")
            .to_owned();
        // SAFETY: the pointer is released exactly once.
        unsafe { tgws_free_string(pointer) };
        value
    }

    fn status_state() -> String {
        let status: serde_json::Value =
            serde_json::from_str(&take(tgws_status())).expect("status is JSON");
        status["state"]
            .as_str()
            .expect("state is a string")
            .to_owned()
    }

    fn wait_for_state(expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let state = status_state();
            if state == expected {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {expected}, last state: {state}"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn c_abi_starts_serves_and_stops() {
        let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let directory = tempfile::tempdir().unwrap();
        let config = serde_json::json!({
            "port": port,
            "secret": "00112233445566778899aabbccddeeff",
            "poolSize": 0,
            "fallbackCfproxy": false,
            "logPath": directory.path().join("proxy.log"),
        })
        .to_string();
        let config = CString::new(config).unwrap();

        // SAFETY: the configuration is a valid C string.
        let started = take(unsafe { tgws_start(config.as_ptr()) });
        assert_eq!(started, r#"{"ok":true,"error":null}"#);
        wait_for_state("running");
        TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();

        let stopped = take(tgws_stop());
        assert_eq!(stopped, r#"{"ok":true,"error":null}"#);
        wait_for_state("stopped");
        TcpListener::bind((Ipv4Addr::LOCALHOST, port))
            .expect("listener port must be released after the bridge stops");
    }

    #[test]
    fn c_abi_rejects_null_configuration() {
        // SAFETY: a null pointer is an accepted input.
        let response = take(unsafe { tgws_start(std::ptr::null()) });
        assert!(
            response.contains("configuration pointer is null"),
            "{response}"
        );
    }
}
