//! Windows service ("LanMouse") that keeps the Lan Mouse daemon running in
//! the active console session, the way Barrier's barrierd does.
//!
//! The daemon is started with the SYSTEM token of that session's
//! winlogon.exe, so it can follow input onto the secure desktop (UAC
//! prompts, sign-in and lock screen) and reach elevated windows. It still
//! uses the configured user's settings: `--user-profile` (set by the
//! installer) points USERPROFILE / LOCALAPPDATA at that user's folders.
//!
//!   lan-mouse.exe service --user-profile C:\Users\me

use std::{
    ffi::{OsStr, OsString},
    fs::OpenOptions,
    io::Write,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr,
    sync::mpsc,
    time::{Duration, Instant},
};

use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0},
    Security::{
        DuplicateTokenEx, SECURITY_ATTRIBUTES, SecurityImpersonation, SetTokenInformation,
        TOKEN_ALL_ACCESS, TokenPrimary, TokenUIAccess,
    },
    Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_END, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_ALWAYS, SetFilePointerEx,
    },
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        },
        RemoteDesktop::{ProcessIdToSessionId, WTSGetActiveConsoleSessionId},
        Threading::{
            CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, OpenProcess,
            OpenProcessToken, PROCESS_INFORMATION, PROCESS_QUERY_INFORMATION, STARTF_USESTDHANDLES,
            STARTUPINFOW, TerminateProcess, WaitForSingleObject,
        },
    },
};

const SERVICE_NAME: &str = "LanMouse";
const NO_SESSION: u32 = 0xFFFF_FFFF;
/// the daemon log is rotated when it grows beyond this
const MAX_LOG: u64 = 20 * 1024 * 1024;

define_windows_service!(ffi_service_main, service_main);

/// Entry point for `lan-mouse.exe service ...` (called by the service
/// control manager).
pub fn main() {
    if let Err(e) = service_dispatcher::start(SERVICE_NAME, ffi_service_main) {
        eprintln!("lan-mouse service: must be started by the service manager ({e})");
        std::process::exit(1);
    }
}

fn user_profile() -> Option<PathBuf> {
    let mut args = std::env::args_os().skip_while(|a| a != "--user-profile");
    args.next()?;
    args.next().map(PathBuf::from)
}

/// "2026-11-12T16:55:18Z", like the daemon's log
fn utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rem) = (secs / 86400, secs % 86400);
    // civil date from days since 1970-01-01 (Howard Hinnant's algorithm)
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

struct Log(Option<PathBuf>);

impl Log {
    fn line(&self, msg: impl AsRef<str>) {
        if let Some(path) = &self.0 {
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(f, "[{} service] {}", utc_now(), msg.as_ref());
            }
        }
    }
}

fn service_main(_args: Vec<OsString>) {
    let profile = user_profile();
    let data_dir = profile
        .as_ref()
        .map(|p| p.join("AppData").join("Local").join("lan-mouse"));
    if let Some(d) = &data_dir {
        let _ = std::fs::create_dir_all(d);
    }
    let log = Log(data_dir.as_ref().map(|d| d.join("service.log")));
    if let Err(e) = run(&log, profile, data_dir) {
        log.line(format!("stopped with error: {e}"));
    }
}

fn run(
    log: &Log,
    profile: Option<PathBuf>,
    data_dir: Option<PathBuf>,
) -> Result<(), windows_service::Error> {
    let (stop_tx, stop_rx) = mpsc::channel();
    let handle = service_control_handler::register(SERVICE_NAME, move |event| match event {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = stop_tx.send(());
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })?;
    let status = |state, accept| ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: accept,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::from_secs(5),
        process_id: None,
    };
    handle.set_service_status(status(
        ServiceState::Running,
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
    ))?;
    log.line(format!("started (user profile: {profile:?})"));

    let exe = std::env::current_exe().expect("own path");
    let mut child: Option<(HANDLE, u32)> = None;
    let mut next_try = Instant::now();
    loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }
        let session = unsafe { WTSGetActiveConsoleSessionId() };
        // the daemon exited, or the console moved to another session
        if let Some((proc_, sess)) = child {
            let exited = unsafe { WaitForSingleObject(proc_, 0) } == WAIT_OBJECT_0;
            if exited || sess != session {
                if !exited {
                    log.line(format!(
                        "console moved to session {session}; restarting Lan Mouse there"
                    ));
                    unsafe { TerminateProcess(proc_, 0) };
                } else {
                    log.line("Lan Mouse exited; restarting it");
                }
                unsafe { CloseHandle(proc_) };
                child = None;
                next_try = Instant::now() + Duration::from_secs(2);
            }
        }
        if child.is_none() && session != NO_SESSION && Instant::now() >= next_try {
            match spawn(&exe, session, profile.as_deref(), data_dir.as_deref()) {
                Ok(p) => {
                    log.line(format!("started Lan Mouse in session {session}"));
                    child = Some((p, session));
                }
                Err(e) => {
                    log.line(format!(
                        "could not start Lan Mouse in session {session}: {e}"
                    ));
                    next_try = Instant::now() + Duration::from_secs(5);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    if let Some((p, _)) = child {
        unsafe {
            TerminateProcess(p, 0);
            CloseHandle(p);
        }
    }
    log.line("stopped");
    handle.set_service_status(status(ServiceState::Stopped, ServiceControlAccept::empty()))?;
    Ok(())
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

fn last_error() -> String {
    std::io::Error::last_os_error().to_string()
}

/// winlogon.exe of a session.
fn winlogon_pid(session: u32) -> Option<u32> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = None;
        let mut ok = Process32FirstW(snap, &mut entry) != 0;
        while ok {
            let end = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..end]);
            if name.eq_ignore_ascii_case("winlogon.exe") {
                let mut s = 0u32;
                if ProcessIdToSessionId(entry.th32ProcessID, &mut s) != 0 && s == session {
                    found = Some(entry.th32ProcessID);
                    break;
                }
            }
            ok = Process32NextW(snap, &mut entry) != 0;
        }
        CloseHandle(snap);
        found
    }
}

/// Environment for the daemon: ours (SYSTEM's), with the user's folders.
fn environment(profile: Option<&Path>) -> Vec<u16> {
    let mut vars: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    let mut set = |k: &str, v: OsString| {
        vars.retain(|(key, _)| !key.to_string_lossy().eq_ignore_ascii_case(k));
        vars.push((k.into(), v));
    };
    if let Some(p) = profile {
        let local = p.join("AppData").join("Local");
        set("USERPROFILE", p.as_os_str().into());
        set("LOCALAPPDATA", local.as_os_str().into());
        set(
            "APPDATA",
            p.join("AppData").join("Roaming").as_os_str().into(),
        );
        set("TEMP", local.join("Temp").as_os_str().into());
        set("TMP", local.join("Temp").as_os_str().into());
    }
    if std::env::var_os("LAN_MOUSE_LOG_LEVEL").is_none() {
        set("LAN_MOUSE_LOG_LEVEL", "info".into());
    }
    vars.sort_by_key(|(k, _)| k.to_string_lossy().to_uppercase());
    let mut block = Vec::new();
    for (k, v) in vars {
        block.extend(k.encode_wide());
        block.push('=' as u16);
        block.extend(v.encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

/// Append-mode, inheritable handle to the daemon log (rotated when large).
fn log_handle(data_dir: Option<&Path>) -> HANDLE {
    let Some(dir) = data_dir else {
        return ptr::null_mut();
    };
    let path = dir.join("daemon.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG) {
        let _ = std::fs::rename(&path, dir.join("daemon.log.1"));
    }
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: ptr::null_mut(),
        bInheritHandle: 1,
    };
    let name = wide(path.as_os_str());
    unsafe {
        let h = CreateFileW(
            name.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &sa,
            OPEN_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            ptr::null_mut(),
        );
        if h == INVALID_HANDLE_VALUE {
            return ptr::null_mut();
        }
        SetFilePointerEx(h, 0, ptr::null_mut(), FILE_END);
        h
    }
}

/// Start `lan-mouse.exe daemon` in `session` as SYSTEM.
fn spawn(
    exe: &Path,
    session: u32,
    profile: Option<&Path>,
    data_dir: Option<&Path>,
) -> Result<HANDLE, String> {
    let pid = winlogon_pid(session).ok_or("winlogon.exe not found in the session")?;
    unsafe {
        let wl = OpenProcess(PROCESS_QUERY_INFORMATION, 0, pid);
        if wl.is_null() {
            return Err(format!("OpenProcess(winlogon): {}", last_error()));
        }
        let mut token: HANDLE = ptr::null_mut();
        let ok = OpenProcessToken(wl, TOKEN_ALL_ACCESS, &mut token);
        CloseHandle(wl);
        if ok == 0 {
            return Err(format!("OpenProcessToken: {}", last_error()));
        }
        let mut primary: HANDLE = ptr::null_mut();
        let ok = DuplicateTokenEx(
            token,
            TOKEN_ALL_ACCESS,
            ptr::null(),
            SecurityImpersonation,
            TokenPrimary,
            &mut primary,
        );
        CloseHandle(token);
        if ok == 0 {
            return Err(format!("DuplicateTokenEx: {}", last_error()));
        }
        // allowed to drive windows of any integrity level (as Barrier does)
        let ui_access: u32 = 1;
        SetTokenInformation(primary, TokenUIAccess, (&ui_access as *const u32).cast(), 4);

        let log = log_handle(data_dir);
        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut desktop = wide(OsStr::new("winsta0\\default"));
        si.lpDesktop = desktop.as_mut_ptr();
        if !log.is_null() {
            si.dwFlags = STARTF_USESTDHANDLES;
            si.hStdOutput = log;
            si.hStdError = log;
        }
        let mut cmd = wide(OsStr::new(&format!("\"{}\" daemon", exe.display())));
        let app = wide(exe.as_os_str());
        let env = environment(profile);
        let cwd = profile.map(|p| wide(p.as_os_str()));
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let ok = CreateProcessAsUserW(
            primary,
            app.as_ptr(),
            cmd.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            1, // inherit the log handle
            CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
            env.as_ptr().cast(),
            cwd.as_ref().map_or(ptr::null(), |c| c.as_ptr()),
            &si,
            &mut pi,
        );
        let err = last_error();
        CloseHandle(primary);
        if !log.is_null() {
            CloseHandle(log);
        }
        if ok == 0 {
            return Err(format!("CreateProcessAsUser: {err}"));
        }
        CloseHandle(pi.hThread);
        Ok(pi.hProcess)
    }
}
