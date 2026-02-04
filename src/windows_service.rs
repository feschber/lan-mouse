use std::ffi::OsString;
use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceStatus, ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
};
use std::time::Duration;
use windows::Win32::System::Services::{
    OpenSCManagerW, OpenServiceW, ControlService,
    SC_MANAGER_ALL_ACCESS, SERVICE_ALL_ACCESS,
};
use windows::Win32::System::RemoteDesktop::{WTSGetActiveConsoleSessionId, ProcessIdToSessionId};
use windows::Win32::System::Threading::{
    CreateProcessAsUserW, PROCESS_INFORMATION, STARTUPINFOW, CREATE_UNICODE_ENVIRONMENT,
    CREATE_NO_WINDOW, TerminateProcess, WaitForSingleObject, OpenProcess,
    OpenProcessToken, PROCESS_ALL_ACCESS,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
    TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::Security::{
    DuplicateTokenEx, TOKEN_ALL_ACCESS,
    SecurityImpersonation, TokenPrimary, SetTokenInformation,
    TokenUIAccess, TOKEN_ASSIGN_PRIMARY,
};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::core::{HSTRING, PWSTR, w};
use std::ptr;

define_windows_service!(ffi_service_main, lan_mouse_service_main);

pub fn run_as_service() -> Result<(), windows_service::Error> {
    // Check if we were invoked with "watchdog" command
    // The SCM passes the service name as first arg, then our command-line args
    let args: Vec<String> = std::env::args().collect();
    let is_watchdog = args.iter().any(|arg| arg == "watchdog");
    
    if is_watchdog {
        service_dispatcher::start("lan-mouse", ffi_service_main)
    } else {
        // Not invoked as watchdog service
        Err(windows_service::Error::Winapi(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Not started by SCM",
        )))
    }
}

fn lan_mouse_service_main(_arguments: Vec<OsString>) {
    // Initialize file-based logging (services don't have console)
    let log_dir = std::path::Path::new("C:\\ProgramData\\lan-mouse");
    let _ = std::fs::create_dir_all(log_dir);
    let log_path = log_dir.join("watchdog.log");
    
    if let Ok(log_file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        use env_logger::Env;
        let env = Env::default().filter_or("LAN_MOUSE_LOG_LEVEL", "info");
        let _ = env_logger::Builder::from_env(env)
            .format_timestamp_secs()
            .target(env_logger::Target::Pipe(Box::new(log_file)))
            .try_init();
    }
    
    log::info!("lan-mouse watchdog service starting");
    
    if let Err(e) = run_watchdog_service() {
        log::error!("Watchdog service error: {:?}", e);
    }
}

fn run_watchdog_service() -> Result<(), windows_service::Error> {
    /* ==================================================================================
     * WATCHDOG SERVICE - Session Manager for lan-mouse
     * ==================================================================================
     * 
     * This service runs in Session 0 as SYSTEM and manages session daemon processes:
     * 
     * 1. Monitor active console session via WTSGetActiveConsoleSessionId()
     * 2. Spawn `lan-mouse daemon` in user session using CreateProcessAsUser()
     * 3. Acquire appropriate token (WTSQueryUserToken or winlogon token)
     * 4. Monitor daemon health, respawn on crash or session change
     * 5. Handle SendSAS for Ctrl+Alt+Del (future: when IPC is implemented)
     * 
     * Session daemons perform actual input capture/emulation since they run in the
     * user's session where SendInput works correctly.
     * ==================================================================================
     */

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                log::info!("Received stop/shutdown signal");
                tx.send(()).unwrap_or_default();
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle = service_control_handler::register("lan-mouse", event_handler)?;

    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: windows_service::service::ServiceState::StartPending,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::from_secs(5),
        process_id: None,
    })?;

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            log::error!("Failed to create tokio runtime: {:?}", e);
            return Err(windows_service::Error::Winapi(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Failed to create tokio runtime",
            )));
        }
    };

    let local = tokio::task::LocalSet::new();
    rt.block_on(local.run_until(async {
        status_handle
            .set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: windows_service::service::ServiceState::Running,
                controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
                exit_code: ServiceExitCode::Win32(0),
                checkpoint: 0,
                wait_hint: Duration::default(),
                process_id: None,
            })
            .unwrap();

        log::info!("Watchdog service running - monitoring sessions and managing daemons");

        if let Err(e) = watchdog_main_loop(rx).await {
            log::error!("Watchdog main loop error: {:?}", e);
        }

        status_handle
            .set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: windows_service::service::ServiceState::StopPending,
                controls_accepted: ServiceControlAccept::empty(),
                exit_code: ServiceExitCode::Win32(0),
                checkpoint: 0,
                wait_hint: Duration::from_secs(5),
                process_id: None,
            })
            .unwrap();
    }));

    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: windows_service::service::ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;

    log::info!("Watchdog service stopped");
    Ok(())
}

async fn watchdog_main_loop(mut shutdown_rx: tokio::sync::mpsc::UnboundedReceiver<()>) -> Result<(), std::io::Error> {
    log::info!("Watchdog main loop started - monitoring console sessions");
    
    let mut current_session_id: Option<u32> = None;
    let mut session_daemon: Option<SessionDaemonHandle> = None;
    let mut crash_count = 0u32;
    let mut last_crash_time = std::time::Instant::now();
    
    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                log::info!("Watchdog received shutdown signal");
                
                // Terminate session daemon if running
                if let Some(daemon) = session_daemon.take() {
                    log::info!("Terminating session daemon (PID={})", daemon.process_id);
                    daemon.terminate();
                }
                
                break;
            }
            _ = tokio::time::sleep(Duration::from_millis(500)) => {
                // Check active console session
                let active_session = get_active_console_session();
                
                // 0xFFFFFFFF means no active session (e.g., no user logged in)
                if active_session == 0xFFFFFFFF {
                    if current_session_id.is_some() {
                        log::info!("No active console session (user logged out or switching sessions)");
                        
                        // Terminate session daemon
                        if let Some(daemon) = session_daemon.take() {
                            log::info!("Terminating session daemon (PID={})", daemon.process_id);
                            daemon.terminate();
                        }
                        
                        current_session_id = None;
                    }
                    continue;
                }
                
                // Check if daemon crashed (if we think we have one but it's not running)
                if let Some(ref daemon) = session_daemon {
                    if !daemon.is_running() {
                        let now = std::time::Instant::now();
                        let time_since_last_crash = now.duration_since(last_crash_time);
                        
                        // Reset crash count if it's been more than 60 seconds since last crash
                        if time_since_last_crash.as_secs() > 60 {
                            crash_count = 0;
                        }
                        
                        crash_count += 1;
                        last_crash_time = now;
                        
                        log::error!("Session daemon crashed (PID={}, crash #{}) - will respawn after backoff", 
                                   daemon.process_id, crash_count);
                        
                        // Exponential backoff: 1s, 2s, 4s, 8s, max 30s
                        let backoff_secs = std::cmp::min(1u64 << (crash_count - 1), 30);
                        log::info!("Waiting {}s before respawn attempt...", backoff_secs);
                        tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                        
                        session_daemon = None;
                        // Will respawn below
                    } else {
                        // Daemon is running - reset crash count
                        if crash_count > 0 {
                            crash_count = 0;
                        }
                    }
                }
                
                // Check if session changed
                let session_changed = current_session_id != Some(active_session);
                let need_spawn = session_changed || (session_daemon.is_none() && current_session_id.is_some());
                
                if session_changed {
                    log::info!("Console session changed: {:?} -> {}", current_session_id, active_session);
                    
                    // Terminate old session daemon
                    if let Some(daemon) = session_daemon.take() {
                        log::info!("Terminating session daemon in old session (PID={})", daemon.process_id);
                        daemon.terminate();
                    }
                    
                    current_session_id = Some(active_session);
                }
                
                // Spawn daemon in active session if needed
                if need_spawn && session_daemon.is_none() {
                    match get_session_token(active_session) {
                        Ok(token) => {
                            match spawn_session_daemon(active_session, token) {
                                Ok(daemon) => {
                                    log::info!("Successfully spawned session daemon in session {} (PID={})", 
                                               active_session, daemon.process_id);
                                    session_daemon = Some(daemon);
                                }
                                Err(e) => {
                                    log::error!("Failed to spawn session daemon: {}", e);
                                }
                            }
                            
                            // Clean up token
                            unsafe { let _ = CloseHandle(token); }
                        }
                        Err(e) => {
                            log::warn!("Failed to get session token for session {}: {} (will retry)", active_session, e);
                        }
                    }
                }
            }
        }
    }
    
    log::info!("Watchdog main loop shutting down");
    Ok(())
}

fn get_active_console_session() -> u32 {
    unsafe { WTSGetActiveConsoleSessionId() }
}

/// Holds process information for a spawned session daemon
struct SessionDaemonHandle {
    process_handle: HANDLE,
    thread_handle: HANDLE,
    process_id: u32,
}

impl SessionDaemonHandle {
    fn is_running(&self) -> bool {
        unsafe {
            // WAIT_TIMEOUT = 0x00000102, means process still running
            WaitForSingleObject(self.process_handle, 0) != WAIT_OBJECT_0
        }
    }

    fn terminate(&self) {
        unsafe {
            let _ = TerminateProcess(self.process_handle, 1);
            // Wait up to 5 seconds for process to exit
            let _ = WaitForSingleObject(self.process_handle, 5000);
        }
    }
}

impl Drop for SessionDaemonHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.process_handle);
            let _ = CloseHandle(self.thread_handle);
        }
    }
}

/// Find a process by name in a specific session
fn find_process_in_session(process_name: &str, session_id: u32) -> Result<u32, std::io::Error> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
            .map_err(|e| std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("CreateToolhelp32Snapshot failed: {}", e)
            ))?;

        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };

        if Process32FirstW(snapshot, &mut entry).is_err() {
            let _ = CloseHandle(snapshot);
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Process32FirstW failed"
            ));
        }

        let target_name_lower = process_name.to_lowercase();

        loop {
            // Convert process name from wide string
            let name_len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            let process_name_str = String::from_utf16_lossy(&entry.szExeFile[..name_len]);

            if process_name_str.to_lowercase() == target_name_lower {
                // Check if process is in the target session
                let mut proc_session_id = 0u32;
                if ProcessIdToSessionId(entry.th32ProcessID, &mut proc_session_id).is_ok() {
                    if proc_session_id == session_id {
                        let pid = entry.th32ProcessID;
                        let _ = CloseHandle(snapshot);
                        return Ok(pid);
                    }
                }
            }

            if Process32NextW(snapshot, &mut entry).is_err() {
                break;
            }
        }

        let _ = CloseHandle(snapshot);
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Process '{}' not found in session {}", process_name, session_id)
        ))
    }
}

/// Check if a process exists in a session
fn process_exists_in_session(process_name: &str, session_id: u32) -> bool {
    find_process_in_session(process_name, session_id).is_ok()
}

/// Get winlogon token for login screen access (elevated token with UIAccess)
fn get_winlogon_token(session_id: u32) -> Result<HANDLE, std::io::Error> {
    unsafe {
        // Find winlogon.exe in the target session
        let winlogon_pid = find_process_in_session("winlogon.exe", session_id)?;

        // Open the winlogon process
        let process_handle = OpenProcess(PROCESS_ALL_ACCESS, false, winlogon_pid)
            .map_err(|e| std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("OpenProcess failed for winlogon PID {}: {}", winlogon_pid, e)
            ))?;

        // Get the winlogon process token
        let mut source_token = HANDLE::default();
        let result = OpenProcessToken(
            process_handle,
            TOKEN_ASSIGN_PRIMARY | TOKEN_ALL_ACCESS,
            &mut source_token
        );
        let _ = CloseHandle(process_handle);
        
        result.map_err(|e| std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("OpenProcessToken failed for winlogon: {}", e)
        ))?;

        // Duplicate the token as a primary token
        let mut new_token = HANDLE::default();
        DuplicateTokenEx(
            source_token,
            TOKEN_ASSIGN_PRIMARY | TOKEN_ALL_ACCESS,
            None,
            SecurityImpersonation,
            TokenPrimary,
            &mut new_token,
        ).map_err(|e| {
            let _ = CloseHandle(source_token);
            std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("DuplicateTokenEx failed: {}", e)
            )
        })?;

        let _ = CloseHandle(source_token);

        // Enable UIAccess on the token for secure desktop access
        let ui_access: u32 = 1;
        SetTokenInformation(
            new_token,
            TokenUIAccess,
            &ui_access as *const u32 as *const _,
            std::mem::size_of::<u32>() as u32,
        ).map_err(|e| {
            log::warn!("SetTokenInformation(TokenUIAccess) failed: {} (may need code signing)", e);
            // Don't fail here - token is still usable, just without UIAccess
        }).ok();

        log::info!("Acquired winlogon token for session {} (PID={})", session_id, winlogon_pid);
        Ok(new_token)
    }
}

/// Determine which token to use based on session context
/// 
/// For UAC/secure desktop access, we ALWAYS use the winlogon token.
/// The winlogon process runs with SYSTEM privileges and has access to the
/// Secure Desktop (UAC prompts). When we spawn the daemon with a winlogon-derived
/// token, it inherits those access rights, allowing OpenInputDesktop to succeed.
/// 
/// Using WTSQueryUserToken only gets a limited user token which cannot access
/// the Secure Desktop even with TokenUIAccess set.
fn get_session_token(session_id: u32) -> Result<HANDLE, std::io::Error> {
    // Check if we're at the login screen (logonui.exe present, no explorer.exe)
    let is_login_screen = process_exists_in_session("logonui.exe", session_id)
        && !process_exists_in_session("explorer.exe", session_id);

    if is_login_screen {
        log::info!("Detected login screen in session {} - using winlogon token", session_id);
    } else {
        log::info!("Detected normal user session {} - using winlogon token for secure desktop access", session_id);
    }
    
    // Always use winlogon token - it has the privileges needed to access
    // the Secure Desktop (UAC prompts) via OpenInputDesktop
    get_winlogon_token(session_id)
}

/// Spawn session daemon in the specified session with the given token
fn spawn_session_daemon(session_id: u32, token: HANDLE) -> Result<SessionDaemonHandle, std::io::Error> {
    unsafe {
        let exe_path = std::env::current_exe()?;
        
        // Build command line with explicit config path pointing to ProgramData
        // This ensures the session daemon uses the machine-wide config regardless of
        // which user token (or winlogon token) is used to spawn it
        let config_path = r"C:\ProgramData\lan-mouse\config.toml";
        let command = format!(r#""{}" --config "{}" daemon"#, exe_path.display(), config_path);
        
        log::info!("Spawning session daemon in session {} with command: {}", session_id, command);
        
        let mut command_wide: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
        
        let startup_info = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(w!("winsta0\\Default").as_ptr() as *mut u16),
            ..Default::default()
        };
        
        // Create environment block for the user session
        let mut env_block = ptr::null_mut();
        CreateEnvironmentBlock(&mut env_block, Some(token), false)
            .map_err(|e| std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("CreateEnvironmentBlock failed: {}", e)
            ))?;
        
        let mut proc_info = PROCESS_INFORMATION::default();
        
        let result = CreateProcessAsUserW(
            Some(token),
            None,
            Some(PWSTR(command_wide.as_mut_ptr())),
            None, // Process security
            None, // Thread security
            true, // Inherit handles
            CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
            Some(env_block as *const _),
            None, // Current directory
            &startup_info,
            &mut proc_info,
        );
        
        DestroyEnvironmentBlock(env_block)
            .map_err(|e| log::warn!("DestroyEnvironmentBlock failed: {}", e))
            .ok();
        
        result.map_err(|e| std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("CreateProcessAsUserW failed: {}", e)
        ))?;
        
        log::info!("Session daemon spawned successfully: PID={}", proc_info.dwProcessId);
        
        Ok(SessionDaemonHandle {
            process_handle: proc_info.hProcess,
            thread_handle: proc_info.hThread,
            process_id: proc_info.dwProcessId,
        })
    }
}

pub fn service_status() -> Result<(), String> {
    unsafe {
        let scm = OpenSCManagerW(None, None, SC_MANAGER_ALL_ACCESS)
            .map_err(|e| format!("Failed to open SCM: {}", e))?;

        let service_name = HSTRING::from("lan-mouse");
        let service = match OpenServiceW(scm, &service_name, SERVICE_ALL_ACCESS) {
            Ok(s) => s,
            Err(e) => {
                // Check if the service doesn't exist (error code 1060)
                let hresult = e.code();
                if hresult.0 == -2147024908i32 {  // 1060 in HRESULT format (ERROR_SERVICE_DOES_NOT_EXIST)
                    println!("Service not installed");
                    return Ok(());
                }
                return Err(format!("Failed to open service: {}", e));
            }
        };

        // Query service status
        let mut status = windows::Win32::System::Services::SERVICE_STATUS::default();
        ControlService(service, windows::Win32::System::Services::SERVICE_CONTROL_INTERROGATE, &mut status)
            .ok();

        let status_str = match status.dwCurrentState.0 {
            1 => "Stopped",
            2 => "Start Pending",
            3 => "Stop Pending",
            4 => "Running",
            5 => "Continue Pending",
            6 => "Pause Pending",
            7 => "Paused",
            _ => "Unknown",
        };

        println!("Service Status: {}", status_str);
        Ok(())
    }
}
