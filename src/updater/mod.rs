use std::{
    fs::create_dir_all,
    io::{self, Read},
    path::PathBuf,
};

#[cfg(feature = "app")]
use std::{
    path::Path,
    process::{Command, Stdio},
    thread,
};

use anyhow::{bail, Result};
#[cfg(feature = "app")]
use anyhow::Context;
use serde_json::Value;

#[cfg(feature = "updater")]
pub mod apply;

const LATEST_API: &str = "https://api.github.com/repos/rtroncoso/ursa-minor-ffb/releases/latest";
pub const UA: &str = "UrsaMinorFFB-Updater (+https://github.com/rtroncoso/ursa-minor-ffb)";

pub const MAIN_EXE_NAME: &str = "Ursa Minor FFB.exe";
pub const UPDATER_EXE_NAME: &str = "ursa-minor-updater.exe";

#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    pub tag: String,
    pub name: String,
    pub html_url: String,
    pub msi_name: String,
    pub msi_url: String,
    pub msi_sha256: Option<String>,
    pub zip_name: Option<String>,
    pub zip_url: Option<String>,
}

pub fn is_newer(new_v: &str, cur_v: &str) -> bool {
    fn parse(v: &str) -> [i64; 3] {
        let mut out = [0i64; 3];
        for (i, part) in v.split('.').take(3).enumerate() {
            out[i] = part.trim().parse::<i64>().unwrap_or(0);
        }
        out
    }
    parse(new_v) > parse(cur_v)
}

pub fn fetch_latest_release() -> Result<ReleaseInfo> {
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
    let html_url = v
        .get("html_url")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();

    let assets = v
        .get("assets")
        .and_then(|a| a.as_array())
        .ok_or_else(|| anyhow::anyhow!("No assets in latest release"))?;

    let mut best_msi: Option<(String, String)> = None;
    let mut best_zip: Option<(String, String)> = None;

    for a in assets {
        let aname = a.get("name").and_then(|x| x.as_str()).unwrap_or("");
        let url = a
            .get("browser_download_url")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let lower = aname.to_ascii_lowercase();
        if lower.ends_with(".msi") {
            let score = score_asset_name(aname) + 10;
            match &best_msi {
                None => best_msi = Some((aname.to_string(), url.to_string())),
                Some((cur, _)) if score > score_asset_name(cur) + 10 => {
                    best_msi = Some((aname.to_string(), url.to_string()));
                }
                _ => {}
            }
        } else if lower.ends_with(".zip") {
            let score = score_asset_name(aname);
            match &best_zip {
                None => best_zip = Some((aname.to_string(), url.to_string())),
                Some((cur, _)) if score > score_asset_name(cur) => {
                    best_zip = Some((aname.to_string(), url.to_string()));
                }
                _ => {}
            }
        }
    }

    let (msi_name, msi_url) = best_msi.ok_or_else(|| anyhow::anyhow!("No .msi asset found"))?;
    let msi_sha256 = fetch_checksum_for_asset(&client, &tag, &msi_name).ok();

    Ok(ReleaseInfo {
        tag,
        name,
        html_url,
        msi_name,
        msi_url,
        msi_sha256,
        zip_name: best_zip.as_ref().map(|(n, _)| n.clone()),
        zip_url: best_zip.map(|(_, u)| u),
    })
}

fn fetch_checksum_for_asset(
    client: &reqwest::blocking::Client,
    _tag: &str,
    asset_name: &str,
) -> Result<String> {
    let sums_url = format!(
        "https://github.com/rtroncoso/ursa-minor-ffb/releases/latest/download/SHA256SUMS.txt"
    );
    let mut resp = client.get(&sums_url).send()?;
    if !resp.status().is_success() {
        bail!("Could not fetch SHA256SUMS.txt");
    }
    let mut body = String::new();
    resp.read_to_string(&mut body)?;
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let hash = parts.next().unwrap_or("");
        let name = parts.next().unwrap_or("");
        if name == asset_name {
            return Ok(hash.to_ascii_lowercase());
        }
    }
    bail!("Checksum not found for {asset_name}")
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

#[cfg(feature = "app")]
pub fn spawn_startup_check(
    tx_ui: crossbeam_channel::Sender<crate::UiCmd>,
    current_version: &'static str,
) {
    thread::spawn(move || {
        match check_for_update(current_version) {
            Ok(Some(info)) => {
                let _ = tx_ui.send(crate::UiCmd::UpdateAvailable(info));
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("Update check failed: {e:#}");
            }
        }
    });
}

pub fn check_for_update(current_version: &str) -> Result<Option<ReleaseInfo>> {
    let info = fetch_latest_release()?;
    let new_ver = info.tag.trim_start_matches('v');
    let cur_ver = current_version.trim_start_matches('v');
    if is_newer(new_ver, cur_ver) {
        Ok(Some(info))
    } else {
        Ok(None)
    }
}

#[cfg(feature = "app")]
pub fn launch_updater(app_dir: &Path, pid: u32, release: &ReleaseInfo) -> Result<()> {
    let updater_path = app_dir.join(UPDATER_EXE_NAME);
    if !updater_path.is_file() {
        bail!(
            "Updater not found at {}. Reinstall from the MSI package.",
            updater_path.display()
        );
    }

    let mut cmd = Command::new(&updater_path);
    cmd.arg("--apply")
        .arg("--pid")
        .arg(pid.to_string())
        .arg("--app-dir")
        .arg(app_dir)
        .arg("--msi-url")
        .arg(&release.msi_url)
        .arg("--msi-name")
        .arg(&release.msi_name);
    if let Some(ref sha) = release.msi_sha256 {
        cmd.arg("--sha256").arg(sha);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    cmd.spawn()
        .with_context(|| format!("spawn {}", updater_path.display()))?;
    Ok(())
}

pub fn updates_dir() -> PathBuf {
    let mut p = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    p.push("UrsaMinorFFB");
    p.push("updates");
    let _ = create_dir_all(&p);
    p
}

pub fn update_error_log_path() -> PathBuf {
    let mut p = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    p.push("UrsaMinorFFB");
    let _ = create_dir_all(&p);
    p.push("update-error.log");
    p
}

pub fn log_update_error(err: &anyhow::Error) {
    let path = update_error_log_path();
    let msg = format!("{}\n{err:#}\n", chrono::Local::now().to_rfc3339());
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| io::Write::write_all(&mut f, msg.as_bytes()));
}
