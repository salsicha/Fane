//! Synthesize the paste shortcut in whatever window currently has focus.
//!
//! Wayland gives ordinary clients no way to type into other apps, so we fall back
//! through: a virtual keyboard on /dev/uinput (needs write access), ydotool,
//! and xdotool (XWayland windows only).

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing::warn;

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const SYN_REPORT: u16 = 0;
const KEY_LEFTCTRL: u16 = 29;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_V: u16 = 47;

const UI_SET_EVBIT: libc::Ioctl = 0x4004_5564;
const UI_SET_KEYBIT: libc::Ioctl = 0x4004_5565;
const UI_DEV_CREATE: libc::Ioctl = 0x5501;
const UI_DEV_DESTROY: libc::Ioctl = 0x5502;

/// Tiny virtual keyboard using the legacy uinput write() setup API.
struct UInputKeyboard {
    fd: OwnedFd,
}

impl UInputKeyboard {
    fn new() -> io::Result<Self> {
        // SAFETY: plain open(2) of a NUL-terminated path; ownership of the fd moves into OwnedFd.
        let raw = unsafe { libc::open(c"/dev/uinput".as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a freshly opened, valid descriptor we own.
        let kb = UInputKeyboard { fd: unsafe { OwnedFd::from_raw_fd(raw) } };
        kb.ioctl(UI_SET_EVBIT, libc::c_int::from(EV_KEY))?;
        for key in [KEY_LEFTCTRL, KEY_LEFTSHIFT, KEY_V] {
            kb.ioctl(UI_SET_KEYBIT, libc::c_int::from(key))?;
        }
        // SAFETY: uinput_user_dev is a plain C struct; all-zero is a valid value.
        let mut dev: libc::uinput_user_dev = unsafe { std::mem::zeroed() };
        for (dst, src) in dev.name.iter_mut().zip(b"fane-virtual-keyboard") {
            *dst = *src as libc::c_char;
        }
        dev.id = libc::input_id { bustype: 0x06, vendor: 0x1234, product: 0x5678, version: 1 };
        kb.write_struct(&dev)?;
        kb.ioctl(UI_DEV_CREATE, 0)?;
        std::thread::sleep(Duration::from_millis(300)); // give the compositor time to pick the device up
        Ok(kb)
    }

    fn ioctl(&self, request: libc::Ioctl, arg: libc::c_int) -> io::Result<()> {
        // SAFETY: the uinput ioctls used here take an int argument (or none).
        if unsafe { libc::ioctl(self.fd.as_raw_fd(), request, arg) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn write_struct<T>(&self, value: &T) -> io::Result<()> {
        let size = std::mem::size_of::<T>();
        // SAFETY: `value` points to `size` initialized bytes of a repr(C) struct.
        let n = unsafe { libc::write(self.fd.as_raw_fd(), (value as *const T).cast(), size) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        if n as usize != size {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "short write to /dev/uinput"));
        }
        Ok(())
    }

    fn emit(&self, type_: u16, code: u16, value: i32) -> io::Result<()> {
        // The kernel stamps the event time itself, so it can stay zero.
        // SAFETY: input_event is a plain C struct; all-zero is a valid value.
        let mut ev: libc::input_event = unsafe { std::mem::zeroed() };
        ev.type_ = type_;
        ev.code = code;
        ev.value = value;
        self.write_struct(&ev)
    }

    fn chord(&self, keys: &[u16]) -> io::Result<()> {
        for &k in keys {
            self.emit(EV_KEY, k, 1)?;
            self.emit(EV_SYN, SYN_REPORT, 0)?;
            std::thread::sleep(Duration::from_millis(8));
        }
        for &k in keys.iter().rev() {
            self.emit(EV_KEY, k, 0)?;
            self.emit(EV_SYN, SYN_REPORT, 0)?;
            std::thread::sleep(Duration::from_millis(8));
        }
        Ok(())
    }
}

impl Drop for UInputKeyboard {
    fn drop(&mut self) {
        let _ = self.ioctl(UI_DEV_DESTROY, 0);
        // OwnedFd closes the descriptor.
    }
}

/// One virtual keyboard for the whole process (creating it costs a 0.3 s settle).
static KEYBOARD: Mutex<Option<UInputKeyboard>> = Mutex::new(None);

fn uinput_chord(keys: &[u16]) -> io::Result<()> {
    let mut guard = KEYBOARD.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        *guard = Some(UInputKeyboard::new()?);
    }
    let result = guard.as_ref().map_or(Ok(()), |kb| kb.chord(keys));
    if result.is_err() {
        *guard = None; // recreate it next time
    }
    result
}

fn uinput_writable() -> bool {
    // SAFETY: access(2) on a NUL-terminated path.
    unsafe { libc::access(c"/dev/uinput".as_ptr(), libc::W_OK) == 0 }
}

/// Ways this machine can synthesize the paste shortcut, best first.
pub fn paste_methods() -> Vec<&'static str> {
    let mut methods = vec![];
    if uinput_writable() {
        methods.push("uinput");
    }
    if which::which("ydotool").is_ok() {
        methods.push("ydotool");
    }
    if which::which("xdotool").is_ok() && std::env::var_os("DISPLAY").is_some() {
        methods.push("xdotool");
    }
    methods
}

/// Press the paste shortcut in the focused window (Ctrl+V; Ctrl+Shift+V in Linux terminals;
/// Cmd+V on macOS). Blocking. Returns the method used, or None if nothing worked.
pub fn send_paste(terminal: bool) -> Option<&'static str> {
    let keys: &[u16] = if terminal { &[KEY_LEFTCTRL, KEY_LEFTSHIFT, KEY_V] } else { &[KEY_LEFTCTRL, KEY_V] };
    for method in paste_methods() {
        let result = match method {
            "uinput" => uinput_chord(keys),
            "ydotool" => {
                let mut args = vec!["key".to_string()];
                args.extend(keys.iter().map(|k| format!("{k}:1")));
                args.extend(keys.iter().rev().map(|k| format!("{k}:0")));
                run_checked("ydotool", &args)
            }
            "xdotool" => {
                let combo = if terminal { "ctrl+shift+v" } else { "ctrl+v" };
                run_checked("xdotool", &["key", "--clearmodifiers", combo])
            }
            _ => continue,
        };
        match result {
            Ok(()) => return Some(method),
            Err(e) => warn!("paste via {method} failed: {e}"),
        }
    }
    None
}

/// Run a command (5 s timeout) and fail unless it exits successfully.
fn run_checked<S: AsRef<std::ffi::OsStr>>(program: &str, args: &[S]) -> io::Result<()> {
    let mut child =
        Command::new(program).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(io::Error::other(format!("{program} exited with {status}")))
            };
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(io::ErrorKind::TimedOut, format!("{program} timed out")));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_layouts_match_the_kernel() {
        // What the Python packed by hand: 80-byte name, HHHHi, 4 * 64 * i32.
        assert_eq!(std::mem::size_of::<libc::uinput_user_dev>(), 80 + 12 + 4 * 64 * 4);
        #[cfg(target_pointer_width = "64")]
        assert_eq!(std::mem::size_of::<libc::input_event>(), 24);
    }

    /// Creates (and destroys) the virtual keyboard without pressing anything:
    /// `cargo test uinput_device -- --ignored`.
    #[test]
    #[ignore]
    fn uinput_device_lifecycle() {
        if !uinput_writable() {
            eprintln!("/dev/uinput is not writable here; skipping");
            return;
        }
        let kb = UInputKeyboard::new().expect("create uinput keyboard");
        let name = std::fs::read_to_string("/proc/bus/input/devices").unwrap_or_default();
        assert!(name.contains("fane-virtual-keyboard"), "device not registered with the input core");
        drop(kb);
    }

    #[test]
    fn methods_are_known() {
        for m in paste_methods() {
            assert!(["uinput", "ydotool", "xdotool"].contains(&m));
        }
    }
}
