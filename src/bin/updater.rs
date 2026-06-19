#![cfg(windows)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{env, path::PathBuf, process};

use anyhow::{bail, Context, Result};
use ursa_minor_ffb::updater::apply::{ApplyArgs, run};

fn main() {
    if let Err(e) = real_main() {
        ursa_minor_ffb::updater::log_update_error(&e);
        process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let mut args = env::args().skip(1);
    let mut apply = false;
    let mut pid: u32 = 0;
    let mut app_dir: Option<PathBuf> = None;
    let mut msi_url: Option<String> = None;
    let mut msi_name: Option<String> = None;
    let mut sha256: Option<String> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--apply" => apply = true,
            "--pid" => pid = args.next().context("missing --pid value")?.parse()?,
            "--app-dir" => app_dir = Some(PathBuf::from(args.next().context("missing --app-dir")?)),
            "--msi-url" => msi_url = Some(args.next().context("missing --msi-url")?),
            "--msi-name" => msi_name = Some(args.next().context("missing --msi-name")?),
            "--sha256" => sha256 = Some(args.next().context("missing --sha256")?),
            other => bail!("Unknown argument: {other}"),
        }
    }

    if !apply {
        bail!("Usage: ursa-minor-updater --apply --pid <pid> --app-dir <dir> --msi-url <url> --msi-name <name> [--sha256 <hash>]");
    }

    run(ApplyArgs {
        pid,
        app_dir: app_dir.context("missing --app-dir")?,
        msi_url: msi_url.context("missing --msi-url")?,
        msi_name: msi_name.context("missing --msi-name")?,
        sha256,
    })
}
