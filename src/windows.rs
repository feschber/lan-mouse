//! Windows-specific platform support: install/uninstall commands.
//!
//! This module handles registering lan-mouse as a Windows service
//! and related setup tasks like config/cert migration and firewall rules.

use lan_mouse_ipc::DEFAULT_PORT;
use log::info;
use std::os::windows::ffi::OsStrExt;
use std::process::Command;
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegSetValueExW,
};
use windows::Win32::System::Services::{
    ChangeServiceConfig2W, ControlService, CreateServiceW, DeleteService, OpenSCManagerW,
    OpenServiceW, SC_MANAGER_ALL_ACCESS, SERVICE_ALL_ACCESS, SERVICE_AUTO_START,
    SERVICE_CONFIG_DESCRIPTION, SERVICE_CONTROL_STOP, SERVICE_DESCRIPTIONW, SERVICE_ERROR_NORMAL,
    SERVICE_WIN32_OWN_PROCESS, StartServiceW,
};
use windows::core::{HSTRING, PWSTR};

/// Install lan-mouse as a Windows service.
pub fn install() -> Result<(), String> {
    unsafe {
        let scm = OpenSCManagerW(None, None, SC_MANAGER_ALL_ACCESS)
            .map_err(|e| format!("Failed to open SCM: {}", e))?;

        let exe_path = std::env::current_exe()
            .map_err(|e| format!("Failed to get current exe path: {}", e))?;

        // Add "win-svc" argument to the service command line
        let exe_path_str = exe_path.to_str().ok_or("Invalid exe path")?;
        let cmd_line = format!("\"{}\" win-svc", exe_path_str);
        let cmd_line_h = HSTRING::from(cmd_line);

        let service_name = HSTRING::from("lan-mouse");
        let display_name = HSTRING::from("Lan Mouse");

        let service = CreateServiceW(
            scm,
            &service_name,
            &display_name,
            SERVICE_ALL_ACCESS,
            SERVICE_WIN32_OWN_PROCESS,
            SERVICE_AUTO_START,
            SERVICE_ERROR_NORMAL,
            &cmd_line_h,
            None,
            None,
            None,
            None,
            None,
        )
        .map_err(|e| format!("Failed to create service: {}", e))?;

        info!("Service installed successfully");

        // Copy config to ProgramData
        let program_data = std::path::Path::new("C:\\ProgramData\\lan-mouse");
        let _ = std::fs::create_dir_all(program_data);
        let dst_config = program_data.join("config.toml");

        if !dst_config.exists() {
            if let Ok(app_data) = std::env::var("LOCALAPPDATA") {
                let src_config = std::path::Path::new(&app_data)
                    .join("lan-mouse")
                    .join("config.toml");
                if src_config.exists() {
                    let _ = std::fs::copy(src_config, dst_config);
                }
            }
        }

        // Copy certificate (lan-mouse.pem) from user's LOCALAPPDATA if present
        let dst_cert = program_data.join("lan-mouse.pem");
        if !dst_cert.exists() {
            if let Ok(app_data) = std::env::var("LOCALAPPDATA") {
                let src_cert = std::path::Path::new(&app_data)
                    .join("lan-mouse")
                    .join("lan-mouse.pem");
                if src_cert.exists() {
                    match std::fs::copy(&src_cert, &dst_cert) {
                        Ok(_) => info!("Copied user certificate to ProgramData: {:?}", dst_cert),
                        Err(e) => log::warn!("Failed to copy certificate to ProgramData: {}", e),
                    }
                }
            }
        }

        // Create Windows Firewall rule to allow incoming connections on DEFAULT_PORT
        // Use netsh advfirewall to add a rule for domain and private profiles (not Public)
        let port = DEFAULT_PORT.to_string();
        let rule_name = format!("Lan Mouse ({})", port);
        let netsh_args: Vec<String> = vec![
            "advfirewall".to_string(),
            "firewall".to_string(),
            "add".to_string(),
            "rule".to_string(),
            format!("name={}", rule_name),
            "dir=in".to_string(),
            "action=allow".to_string(),
            "protocol=TCP".to_string(),
            format!("localport={}", port),
            "profile=domain,private".to_string(),
            "enable=yes".to_string(),
        ];

        // Run netsh; don't fail install if firewall command fails, just log
        match Command::new("netsh").args(&netsh_args).output() {
            Ok(output) => {
                if output.status.success() {
                    info!("Firewall rule added: {}", rule_name);
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    log::warn!(
                        "Failed to add firewall rule (netsh returned non-zero): {}",
                        stderr
                    );
                }
            }
            Err(e) => {
                log::warn!("Failed to execute netsh to add firewall rule: {}", e);
            }
        }

        // Register event source
        let sub_key =
            HSTRING::from("SYSTEM\\CurrentControlSet\\Services\\EventLog\\Application\\lan-mouse");
        let mut h_key = HKEY::default();
        if RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            &sub_key,
            Some(0),
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut h_key,
            None,
        )
        .is_ok()
        {
            let path_wide: Vec<u16> = exe_path
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let _ = RegSetValueExW(
                h_key,
                &HSTRING::from("EventMessageFile"),
                Some(0),
                REG_SZ,
                Some(std::slice::from_raw_parts(
                    path_wide.as_ptr() as *const u8,
                    path_wide.len() * 2,
                )),
            );
            let types_supported = 7u32;
            let _ = RegSetValueExW(
                h_key,
                &HSTRING::from("TypesSupported"),
                Some(0),
                REG_DWORD,
                Some(std::slice::from_raw_parts(
                    &types_supported as *const u32 as *const u8,
                    4,
                )),
            );
            let _ = RegCloseKey(h_key);
        }

        // Try to set service description using ChangeServiceConfig2W (preferred)
        let description = "Lan Mouse - share mouse and keyboard across local networks";
        let mut desc_wide: Vec<u16> = description
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut svc_desc = SERVICE_DESCRIPTIONW {
            lpDescription: PWSTR(desc_wide.as_mut_ptr()),
        };

        let desc_ptr = &mut svc_desc as *mut _ as *const std::ffi::c_void;
        // Some Windows versions or environments may not support ChangeServiceConfig2W
        // Treat failure to set the description as non-fatal: log and continue.
        match ChangeServiceConfig2W(service, SERVICE_CONFIG_DESCRIPTION, Some(desc_ptr)) {
            Ok(_) => info!("Service description set via ChangeServiceConfig2W"),
            Err(e) => log::warn!("ChangeServiceConfig2W failed (continuing): {}", e),
        }

        if let Err(e) = StartServiceW(service, None) {
            log::warn!("Failed to start service after installation: {}", e);
        } else {
            info!("Service started");
        }

        Ok(())
    }
}

/// Uninstall the lan-mouse Windows service.
///
/// Stops the service if running, removes service registration, and cleans up
/// registry entries.
pub fn uninstall() -> Result<(), String> {
    unsafe {
        let scm = OpenSCManagerW(None, None, SC_MANAGER_ALL_ACCESS)
            .map_err(|e| format!("Failed to open SCM: {}", e))?;

        let service_name = HSTRING::from("lan-mouse");
        let service = match OpenServiceW(scm, &service_name, SERVICE_ALL_ACCESS) {
            Ok(s) => s,
            Err(e) => {
                // Check if the service doesn't exist (error code 1060)
                let hresult = e.code();
                if hresult.0 == -2147024908i32 {
                    // 1060 in HRESULT format (ERROR_SERVICE_DOES_NOT_EXIST)
                    return Ok(());
                }
                return Err(format!("Failed to open service: {}", e));
            }
        };

        let mut status = windows::Win32::System::Services::SERVICE_STATUS::default();
        let _ = ControlService(service, SERVICE_CONTROL_STOP, &mut status);

        DeleteService(service).map_err(|e| format!("Failed to delete service: {}", e))?;

        // Cleanup event source registry
        let sub_key =
            HSTRING::from("SYSTEM\\CurrentControlSet\\Services\\EventLog\\Application\\lan-mouse");
        let _ = windows::Win32::System::Registry::RegDeleteKeyW(HKEY_LOCAL_MACHINE, &sub_key);

        info!("Service uninstalled successfully");
        Ok(())
    }
}
