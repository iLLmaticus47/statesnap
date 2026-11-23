//! Thin wrappers over the handful of raw Win32 calls the app needs.

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::ptr;
use winapi::shared::windef::HWND;
use winapi::um::winuser;

pub fn wide(text: &str) -> Vec<u16> {
    std::ffi::OsStr::new(text).encode_wide().chain(Some(0)).collect()
}

/// What is in front of the user right now.
pub struct Foreground {
    pub hwnd: isize,
    pub pid: u32,
    pub exe: PathBuf,
}

#[link(name = "ntdll")]
extern "system" {
    fn NtSuspendProcess(process: winapi::um::winnt::HANDLE) -> i32;
    fn NtResumeProcess(process: winapi::um::winnt::HANDLE) -> i32;
}

/// Process currently frozen by us (0 = none). Global so that the panic hook
/// and the exit path can always thaw it: a game must never stay frozen.
static SUSPENDED_PID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn with_process(pid: u32, action: unsafe extern "system" fn(winapi::um::winnt::HANDLE) -> i32) -> bool {
    use winapi::um::handleapi::CloseHandle;
    use winapi::um::processthreadsapi::OpenProcess;
    const PROCESS_SUSPEND_RESUME: u32 = 0x0800;
    unsafe {
        let process = OpenProcess(PROCESS_SUSPEND_RESUME, 0, pid);
        if process.is_null() {
            return false; // e.g. the game runs as administrator and we don't
        }
        let status = action(process);
        CloseHandle(process);
        status >= 0
    }
}

/// Freezes a whole process (every thread), like a universal pause button.
pub fn suspend_process(pid: u32) -> bool {
    use std::sync::atomic::Ordering;
    resume_suspended();
    if pid != 0 && with_process(pid, NtSuspendProcess) {
        SUSPENDED_PID.store(pid, Ordering::SeqCst);
        return true;
    }
    false
}

/// Thaws the process frozen by `suspend_process`, if any. Safe to call anytime.
pub fn resume_suspended() {
    let pid = SUSPENDED_PID.swap(0, std::sync::atomic::Ordering::SeqCst);
    if pid != 0 {
        with_process(pid, NtResumeProcess);
    }
}

pub fn set_timer(hwnd: HWND, id: usize, millis: u32) {
    unsafe {
        winuser::SetTimer(hwnd, id, millis, None);
    }
}

pub fn kill_timer(hwnd: HWND, id: usize) {
    unsafe {
        winuser::KillTimer(hwnd, id);
    }
}

/// An error message that shows on top of everything, including a game.
pub fn error_box_on_top(title: &str, text: &str) {
    unsafe {
        let flags = winuser::MB_OK | winuser::MB_ICONERROR | winuser::MB_TOPMOST | winuser::MB_SETFOREGROUND;
        winuser::MessageBoxW(ptr::null_mut(), wide(text).as_ptr(), wide(title).as_ptr(), flags);
    }
}

/// The foreground window and the exe that owns it. None when the foreground
/// window is ours, or its process cannot be inspected.
pub fn foreground_program() -> Option<Foreground> {
    use winapi::um::handleapi::CloseHandle;
    use winapi::um::processthreadsapi::{GetCurrentProcessId, OpenProcess};
    use winapi::um::winbase::QueryFullProcessImageNameW;
    use winapi::um::winnt::PROCESS_QUERY_LIMITED_INFORMATION;
    unsafe {
        let hwnd = winuser::GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }
        let mut pid = 0u32;
        winuser::GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == 0 || pid == GetCurrentProcessId() {
            return None;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut buffer = vec![0u16; 32768];
        let mut len = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut len);
        CloseHandle(process);
        if ok == 0 {
            return None;
        }
        Some(Foreground { hwnd: hwnd as isize, pid, exe: PathBuf::from(OsString::from_wide(&buffer[..len as usize])) })
    }
}

/// Current foreground window, unless it belongs to this process.
pub fn foreground_window() -> isize {
    use winapi::um::processthreadsapi::GetCurrentProcessId;
    unsafe {
        let hwnd = winuser::GetForegroundWindow();
        let mut pid = 0u32;
        if !hwnd.is_null() {
            winuser::GetWindowThreadProcessId(hwnd, &mut pid);
        }
        if pid == GetCurrentProcessId() {
            0
        } else {
            hwnd as isize
        }
    }
}

pub fn activate(hwnd: isize) {
    if hwnd != 0 {
        unsafe {
            if winuser::IsWindow(hwnd as HWND) != 0 {
                winuser::SetForegroundWindow(hwnd as HWND);
            }
        }
    }
}

/// Shows a (possibly hidden or minimized) window and brings it to the front.
pub fn show_and_activate(hwnd: HWND) {
    unsafe {
        if winuser::IsIconic(hwnd) != 0 {
            winuser::ShowWindow(hwnd, winuser::SW_RESTORE);
        } else {
            winuser::ShowWindow(hwnd, winuser::SW_SHOW);
        }
        winuser::SetForegroundWindow(hwnd);
    }
}

pub fn beep(ok: bool) {
    unsafe {
        winuser::MessageBeep(if ok { winuser::MB_OK } else { winuser::MB_ICONHAND });
    }
}

const MOD_NOREPEAT: u32 = 0x4000;

pub fn register_hotkey(hwnd: HWND, id: i32, mods: u32, vk: u32) -> bool {
    unsafe { winuser::RegisterHotKey(hwnd, id, mods | MOD_NOREPEAT, vk) != 0 }
}

pub fn unregister_hotkey(hwnd: HWND, id: i32) {
    unsafe {
        winuser::UnregisterHotKey(hwnd, id);
    }
}

/// Returns false when another instance already holds the app mutex.
pub fn claim_single_instance() -> bool {
    use winapi::shared::winerror::ERROR_ALREADY_EXISTS;
    use winapi::um::errhandlingapi::GetLastError;
    use winapi::um::synchapi::CreateMutexW;
    unsafe {
        // The handle is deliberately leaked: it must live as long as the process.
        let handle = CreateMutexW(ptr::null_mut(), 0, wide("Local\\StateSnap.SingleInstance").as_ptr());
        !(handle.is_null() || GetLastError() == ERROR_ALREADY_EXISTS)
    }
}

/// Asks an already running instance to show its window.
pub fn poke_running_instance(window_title: &str, message: u32) -> bool {
    unsafe {
        let hwnd = winuser::FindWindowW(ptr::null(), wide(window_title).as_ptr());
        !hwnd.is_null() && winuser::PostMessageW(hwnd, message, 0, 0) != 0
    }
}

pub fn system_language_is_serbian() -> bool {
    // Primary language 0x1a covers Serbian, Croatian and Bosnian.
    unsafe { winapi::um::winnls::GetUserDefaultUILanguage() & 0x3ff == 0x1a }
}

/// Builds an icon from PNG bytes at the given pixel size.
pub fn icon_from_png(png: &[u8], size: i32) -> *mut winapi::ctypes::c_void {
    unsafe {
        winuser::CreateIconFromResourceEx(png.as_ptr() as *mut u8, png.len() as u32, 1, 0x0003_0000, size, size, winuser::LR_DEFAULTCOLOR) as _
    }
}

pub fn small_icon_size() -> i32 {
    unsafe { winuser::GetSystemMetrics(winuser::SM_CXSMICON).max(16) }
}

/// Opens a folder in File Explorer, in front of other windows.
pub fn open_in_explorer(path: &Path) {
    use winapi::um::shellapi::ShellExecuteW;
    unsafe {
        // Explorer is a different process; let it take the foreground,
        // otherwise its window can open behind ours.
        winuser::AllowSetForegroundWindow(winuser::ASFW_ANY);
        let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        ShellExecuteW(ptr::null_mut(), wide("open").as_ptr(), path.as_ptr(), ptr::null(), ptr::null(), winuser::SW_SHOWNORMAL);
    }
}

/// Ctrl/Alt/Shift/Win currently held, as `config::MOD_*` bits.
pub fn current_modifiers() -> u32 {
    use crate::config::{MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN};
    let down = |vk: i32| unsafe { winuser::GetKeyState(vk) < 0 };
    let mut mods = 0;
    if down(winuser::VK_CONTROL) {
        mods |= MOD_CONTROL;
    }
    if down(winuser::VK_MENU) {
        mods |= MOD_ALT;
    }
    if down(winuser::VK_SHIFT) {
        mods |= MOD_SHIFT;
    }
    if down(winuser::VK_LWIN) || down(winuser::VK_RWIN) {
        mods |= MOD_WIN;
    }
    mods
}

pub fn is_modifier_key(vk: u32) -> bool {
    matches!(vk, 0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0..=0xA5)
}

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "StateSnap";

pub fn autostart_enabled() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    winreg::RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(RUN_KEY)
        .and_then(|key| key.get_value::<String, _>(RUN_VALUE))
        .is_ok()
}

pub fn set_autostart(enabled: bool) -> std::io::Result<()> {
    use winreg::enums::HKEY_CURRENT_USER;
    let (key, _) = winreg::RegKey::predef(HKEY_CURRENT_USER).create_subkey(RUN_KEY)?;
    if enabled {
        let exe = std::env::current_exe()?;
        key.set_value(RUN_VALUE, &format!("\"{}\"", exe.display()))
    } else {
        match key.delete_value(RUN_VALUE) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}
