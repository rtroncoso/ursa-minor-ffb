use std::{
    ffi::OsStr,
    fs,
    fs::{copy, create_dir_all, read_dir, File},
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use serde_json::Value;
use zip::ZipArchive;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDOK, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_OKCANCEL, SW_SHOWNORMAL,
};

const LATEST_API: &str = "https://api.github.com/repos/rtroncoso/ursa-minor-ffb/releases/latest";
const UA: &str = "UrsaMinorFFB-Updater (+https://github.com/rtroncoso/ursa-minor-ffb)";

/// Call this **at the very start of `main()`**. If the process was started in
/// helper mode it will perform the update and then relaunch the app.
/// Returns true if the helper ran and the process should exit immediately.
pub fn early_self_update_hook() -> bool {
    // Args:  --apply-update <app_dir> <extracted_dir> <new_exe_name> [--elevated]
    let mut args = std::env::args_os();
    if let Some(first) = args.nth(1) {
        if first == "--apply-update" {
            let app_dir = args.next().expect("missing app_dir");
            let extract = args.next().expect("missing extracted_dir");
            let exe_name = args.next().expect("missing exe_name");
            let mut elevated = false;
            if let Some(flag) = args.next() {
                if flag == "--elevated" {
                    elevated = true;
                }
            }
            if let Err(e) = apply_update(
                Path::new(&app_dir),
                Path::new(&extract),
                &exe_name,
                elevated,
            ) {
                // Last-chance message box (no parent HWND here)
                msgbox_raw("Update failed", &format!("{e:#}"), true);
            }
            return true;
        }
    }
    false
}

/// Spawns a background thread that checks + prompts + downloads + launches helper.
pub fn spawn_check(hwnd_parent: HWND, current_version: &str) {
    let current = current_version.to_string();
    thread::spawn(move || {
        if let Err(e) = check_install_and_restart(hwnd_parent, &current) {
            msgbox(hwnd_parent, "Update check failed", &format!("{e:#}"), true);
        }
    });
}

fn check_install_and_restart(hwnd: HWND, current_version: &str) -> Result<()> {
    let (tag, name, asset_name, asset_url) = fetch_latest_release()?;

    let new_ver = tag.trim_start_matches('v');
    let cur_ver = current_version.trim_start_matches('v');

    if !is_newer(new_ver, cur_ver) {
        msgbox(
            hwnd,
            "Up to date",
            &format!("You are running the latest version ({}).", current_version),
            false,
        );
        return Ok(());
    }

    let text = format!(
        "A new version is available.\n\nCurrent: {}\nLatest:  {}\n\nRelease: {}\n\nInstall now? The app will restart.",
        current_version, new_ver, name
    );
    if !confirm(hwnd, "Update available", &text) {
        return Ok(());
    }

    // Download
    let zip_path = download_asset(&asset_url, &asset_name)?;
    // Extract
    let extracted_dir = extract_zip(&zip_path)?;
    // Decide which exe to run after update
    let new_exe_name = find_new_exe_name(&extracted_dir)?
        .to_string_lossy()
        .into_owned();

    // Prepare helper copy of the current EXE (so we can replace the real one)
    let current_exe = std::env::current_exe().context("current_exe()")?;
    let app_dir = current_exe.parent().unwrap().to_path_buf();
    let helper_path = copy_self_to_temp_helper(&current_exe)?;

    // Launch helper: it will wait, copy files, then relaunch the new EXE.
    launch_helper_and_exit(&helper_path, &app_dir, &extracted_dir, &new_exe_name)?;

    Ok(())
}

fn fetch_latest_release() -> Result<(String, String, String, String)> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(UA)
        .build()?;

    let mut resp = client.get(LATEST_API).send()?;
    if !resp.status().is_success() {
        bail!("GitHub API returned {}", resp.status());
    }
    let mut body = String::new();
    resp.read_to_string(&mut body)?;

    let v: Value = serde_json::from_str(&body)?;
    let tag = v
        .get("tag_name")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let name = v
        .get("name")
        .and_then(|x| x.as_str())
        .unwrap_or(&tag)
        .to_string();

    let assets = v
        .get("assets")
        .and_then(|a| a.as_array())
        .ok_or_else(|| anyhow::anyhow!("No assets in latest release"))?;
    let mut chosen: Option<(String, String)> = None;
    for a in assets {
        let aname = a.get("name").and_then(|x| x.as_str()).unwrap_or("");
        let url = a
            .get("browser_download_url")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if aname.to_ascii_lowercase().ends_with(".zip") {
            let score = score_asset_name(aname);
            match chosen {
                None => chosen = Some((aname.to_string(), url.to_string())),
                Some((ref cur, _)) => {
                    if score > score_asset_name(cur) {
                        chosen = Some((aname.to_string(), url.to_string()));
                    }
                }
            }
        }
    }
    let (asset_name, asset_url) =
        chosen.ok_or_else(|| anyhow::anyhow!("No .zip asset found in latest release"))?;
    Ok((tag, name, asset_name, asset_url))
}

fn score_asset_name(n: &str) -> i32 {
    let s = n.to_ascii_lowercase();
    let mut score = 0;
    if s.contains("win") || s.contains("windows") {
        score += 2;
    }
    if s.contains("x64") || s.contains("x86_64") {
        score += 1;
    }
    score
}

fn is_newer(new_v: &str, cur_v: &str) -> bool {
    fn parse(v: &str) -> [i64; 3] {
        let mut out = [0i64; 3];
        for (i, part) in v.split('.').take(3).enumerate() {
            out[i] = part.trim().parse::<i64>().unwrap_or(0);
        }
        out
    }
    parse(new_v) > parse(cur_v)
}

fn download_asset(url: &str, asset_name: &str) -> Result<PathBuf> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(UA)
        .build()?;

    let mut resp = client.get(url).send()?;
    if !resp.status().is_success() {
        bail!("Download failed: {}", resp.status());
    }

    let mut out = std::env::temp_dir();
    out.push(format!("ursa-minor-ffb-{}", asset_name));
    let mut f = File::create(&out)?;
    io::copy(&mut resp, &mut f)?;
    Ok(out)
}

fn extract_zip(zip_path: &Path) -> Result<PathBuf> {
    let file = File::open(zip_path)?;
    let mut zip = ZipArchive::new(file)?;
    let mut out_dir = std::env::temp_dir();
    out_dir.push(format!(
        "ursa-minor-ffb-extract-{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ));
    create_dir_all(&out_dir)?;
    zip.extract(&out_dir)?;
    Ok(out_dir)
}

fn find_new_exe_name(extracted_dir: &Path) -> Result<PathBuf> {
    let mut candidates: Vec<PathBuf> = vec![];
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
        for e in read_dir(dir)? {
            let e = e?;
            let p = e.path();
            if p.is_dir() {
                walk(&p, out)?;
            } else if p
                .extension()
                .and_then(OsStr::to_str)
                .map(|s| s.eq_ignore_ascii_case("exe"))
                .unwrap_or(false)
            {
                out.push(p);
            }
        }
        Ok(())
    }
    walk(extracted_dir, &mut candidates)?;
    let exe = candidates
        .into_iter()
        .find(|p| {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_ascii_lowercase()
                .contains("ursa")
        })
        .ok_or_else(|| anyhow::anyhow!("No .exe found in extracted package"))?;
    Ok(exe.file_name().unwrap().into())
}

fn copy_self_to_temp_helper(current_exe: &Path) -> Result<PathBuf> {
    let mut helper = std::env::temp_dir();
    helper.push("ursa-minor-updater-helper.exe");
    fs::copy(current_exe, &helper).context("copy helper")?;
    Ok(helper)
}

fn launch_helper_and_exit(
    helper_path: &Path,
    app_dir: &Path,
    extracted_dir: &Path,
    exe_name: &str,
) -> Result<()> {
    // Use a detached helper so it keeps running after we exit.
    let mut cmd = Command::new(helper_path);
    cmd.arg("--apply-update")
        .arg(app_dir)
        .arg(extracted_dir)
        .arg(exe_name)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let _ = cmd.spawn().context("spawn helper")?;

    msgbox(
        HWND(0),
        "Updating",
        "The application will now close to apply the update. It will relaunch automatically.",
        false,
    );

    thread::sleep(Duration::from_millis(200));
    std::process::exit(0);
}

/// Runs in the helper copy. If the app directory is protected, we auto-elevate
/// (UAC) and continue the update from the elevated helper.
fn apply_update(
    app_dir: &Path,
    extracted_dir: &Path,
    new_exe_name: &OsStr,
    elevated: bool,
) -> Result<()> {
    // If not already elevated, check writability and request elevation if needed.
    if !elevated {
        match can_write_dir(app_dir) {
            Ok(true) => { /* fine */ }
            Ok(false) => {
                // Ask for elevation and re-run this helper with --elevated
                relaunch_self_elevated(app_dir, extracted_dir, new_exe_name)?;
                return Ok(()); // this (non-elevated) helper exits; elevated one takes over
            }
            Err(_) => {
                // Unknown error probing — keep going and let the normal logic handle it.
            }
        }
    }

    wait_for_writable(app_dir, Duration::from_secs(30))?;

    // Copy all files from extracted_dir into app_dir
    recursive_copy_overwrite(extracted_dir, app_dir)?;

    // Launch the new version
    let mut target = app_dir.to_path_buf();
    target.push(new_exe_name);

    // Build a stable wide string that stays alive for the call.
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
            msgbox_raw(
                "Launch failed",
                &format!("Could not start:\n{}", target.display()),
                true,
            );
        }
    }

    Ok(())
}

fn can_write_dir(dir: &Path) -> io::Result<bool> {
    let probe = dir.join(".__ursa_write_test.tmp");
    match File::create(&probe) {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            Ok(true)
        }
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => Ok(false),
        Err(e) => Err(e),
    }
}

fn relaunch_self_elevated(
    app_dir: &Path,
    extracted_dir: &Path,
    new_exe_name: &OsStr,
) -> Result<()> {
    let me = std::env::current_exe().context("current_exe()")?;
    let params = format!(
        "--apply-update \"{}\" \"{}\" \"{}\" --elevated",
        app_dir.display(),
        extracted_dir.display(),
        PathBuf::from(new_exe_name).display()
    );
    let params_w = wide_str(&params);
    let me_w = wide_os(me.as_os_str());
    unsafe {
        let h = ShellExecuteW(
            None,
            w!("runas"),
            PCWSTR(me_w.as_ptr()),
            PCWSTR(params_w.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
        if (h.0 as isize) <= 32 {
            msgbox_raw(
                "Administrator permission required",
                "The app is installed in a protected folder (e.g., Program Files).\n\
                    To update, click Yes on the elevation prompt, or move the app to a writable folder (e.g., Documents) and try again.",
                true,
            );
            bail!("User denied elevation or ShellExecuteW(runas) failed");
        }
    }
    Ok(())
}

fn wait_for_writable(app_dir: &Path, timeout: Duration) -> Result<()> {
    let start = Instant::now();

    let try_exes: Vec<PathBuf> = read_dir(app_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(OsStr::to_str)
                .map(|s| s.eq_ignore_ascii_case("exe"))
                .unwrap_or(false)
        })
        .collect();

    loop {
        let mut all_ok = true;
        for exe in &try_exes {
            let probe = exe.with_extension("probe");
            match fs::rename(exe, &probe) {
                Ok(_) => {
                    let _ = fs::rename(&probe, exe); // put it back
                }
                Err(_) => {
                    all_ok = false;
                    break;
                }
            }
        }
        if all_ok {
            return Ok(());
        }
        if start.elapsed() > timeout {
            bail!("Timeout waiting for application files to be writable");
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn recursive_copy_overwrite(src: &Path, dst: &Path) -> Result<()> {
    if !dst.exists() {
        create_dir_all(dst)?;
    }
    for entry in read_dir(src)? {
        let entry = entry?;
        let sp = entry.path();
        let mut dp = dst.to_path_buf();
        dp.push(entry.file_name());
        if entry.file_type()?.is_dir() {
            recursive_copy_overwrite(&sp, &dp)?;
        } else {
            if let Some(parent) = dp.parent() {
                create_dir_all(parent)?;
            }
            // Try atomic replace: copy to temp in the destination dir, then rename into place.
            let mut tmp = dp.clone();
            tmp.set_extension("updt");
            copy(&sp, &tmp)?;
            let _ = fs::rename(&tmp, &dp).or_else(|_| {
                let _ = fs::remove_file(&dp);
                fs::rename(&tmp, &dp)
            })?;
        }
    }
    Ok(())
}

fn confirm(hwnd: HWND, title: &str, text: &str) -> bool {
    let title_w = wide_str(title);
    let text_w = wide_str(text);
    unsafe {
        MessageBoxW(
            hwnd,
            PCWSTR(text_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            MB_OKCANCEL | MB_ICONINFORMATION,
        ) == IDOK
    }
}

fn msgbox(hwnd: HWND, title: &str, text: &str, warn: bool) {
    let flags = if warn {
        MB_OK | MB_ICONWARNING
    } else {
        MB_OK | MB_ICONINFORMATION
    };
    let title_w = wide_str(title);
    let text_w = wide_str(text);
    unsafe {
        let _ = MessageBoxW(
            hwnd,
            PCWSTR(text_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            flags,
        );
    }
}

fn msgbox_raw(title: &str, text: &str, warn: bool) {
    msgbox(HWND(0), title, text, warn);
}

// --- Local wide-string helpers that keep storage alive for the Win32 calls ---

fn wide_str(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

fn wide_os(s: &OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.encode_wide().chain(Some(0)).collect()
}
