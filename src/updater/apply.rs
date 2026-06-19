use std::{
    ffi::OsStr,
    fs::File,
    io::{self, Read},
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use super::{log_update_error, updates_dir, MAIN_EXE_NAME, UA};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const WAIT_TIMEOUT_MS: u32 = 60_000;
const PROCESS_POLL_TIMEOUT: Duration = Duration::from_secs(60);

pub struct ApplyArgs {
    pub pid: u32,
    pub app_dir: PathBuf,
    pub msi_url: String,
    pub msi_name: String,
    pub sha256: Option<String>,
}

pub fn run(args: ApplyArgs) -> Result<()> {
    if let Err(e) = apply_inner(&args) {
        log_update_error(&e);
        return Err(e);
    }
    Ok(())
}

fn apply_inner(args: &ApplyArgs) -> Result<()> {
    wait_for_process_exit(args.pid)?;
    wait_for_app_processes_gone()?;

    let msi_path = download_msi(&args.msi_url, &args.msi_name)?;
    if let Some(expected) = &args.sha256 {
        verify_sha256(&msi_path, expected)?;
    }

    run_msiexec(&msi_path)?;
    relaunch_app(&args.app_dir)?;
    Ok(())
}

fn wait_for_process_exit(pid: u32) -> Result<()> {
    if pid == 0 {
        return Ok(());
    }
    unsafe {
        let handle = OpenProcess(PROCESS_SYNCHRONIZE, false, pid)
            .context("OpenProcess for main app PID")?;
        if handle.is_invalid() {
            return Ok(());
        }
        let wait = WaitForSingleObject(handle, WAIT_TIMEOUT_MS);
        let _ = CloseHandle(handle);
        if wait != WAIT_OBJECT_0 {
            bail!("Timed out waiting for application process to exit");
        }
    }
    Ok(())
}

fn wait_for_app_processes_gone() -> Result<()> {
    let targets = [MAIN_EXE_NAME, "ursa-minor-ffb.exe"];
    let start = Instant::now();
    loop {
        if !any_target_running(&targets)? {
            return Ok(());
        }
        if start.elapsed() > PROCESS_POLL_TIMEOUT {
            bail!("Timed out waiting for Ursa Minor FFB processes to exit");
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn any_target_running(targets: &[&str]) -> Result<bool> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
            .context("CreateToolhelp32Snapshot")?;
        if snap == HANDLE::default() {
            bail!("CreateToolhelp32Snapshot returned invalid handle");
        }

        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };

        let mut found = false;
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                let name = wide_to_string(&entry.szExeFile);
                if targets.iter().any(|t| name.eq_ignore_ascii_case(t)) {
                    found = true;
                    break;
                }
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        Ok(found)
    }
}

fn wide_to_string(w: &[u16]) -> String {
    let len = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..len])
}

fn download_msi(url: &str, name: &str) -> Result<PathBuf> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(UA)
        .build()?;
    let mut resp = client.get(url).send()?;
    if !resp.status().is_success() {
        bail!("MSI download failed: HTTP {}", resp.status());
    }

    let dest = updates_dir().join(name);
    let mut file = File::create(&dest)?;
    io::copy(&mut resp, &mut file)?;
    Ok(dest)
}

fn verify_sha256(path: &Path, expected: &str) -> Result<()> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = format!("{:x}", hasher.finalize());
    if digest != expected.to_ascii_lowercase() {
        bail!(
            "SHA256 mismatch for {} (expected {}, got {})",
            path.display(),
            expected,
            digest
        );
    }
    Ok(())
}

fn run_msiexec(msi_path: &Path) -> Result<()> {
    let msi = msi_path
        .to_str()
        .context("MSI path is not valid UTF-8")?
        .to_string();
    let status = Command::new("msiexec")
        .arg("/i")
        .arg(&msi)
        .arg("/passive")
        .arg("/norestart")
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .context("msiexec")?;
    if !status.success() {
        bail!("msiexec exited with {}", status);
    }
    Ok(())
}

fn relaunch_app(app_dir: &Path) -> Result<()> {
    let target = app_dir.join(MAIN_EXE_NAME);
    if !target.is_file() {
        bail!("Main executable not found at {}", target.display());
    }
    let target_w = wide_os(target.as_os_str());
    unsafe {
        let hinst = ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(target_w.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
        if (hinst.0 as isize) <= 32 {
            bail!("Could not relaunch {}", target.display());
        }
    }
    Ok(())
}

fn wide_os(s: &OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.encode_wide().chain(Some(0)).collect()
}
