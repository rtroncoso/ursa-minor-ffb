use std::{
    ffi::OsStr,
    fs,
    fs::{File, copy, create_dir_all, read_dir},
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use zip::ZipArchive;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    IDOK, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_OKCANCEL, MessageBoxW, SW_SHOWNORMAL,
};
use windows::core::{PCWSTR, w};

const LATEST_API: &str = "https://api.github.com/repos/chegevarko1982/aurora-vibra/releases/latest";
const UA: &str = "AuroraVibra-Updater (+https://github.com/chegevarko1982/aurora-vibra)";

/// Call this **at the very start of `main()`**. If the process was started in
/// helper mode it will perform the update and then relaunch the app.
/// Returns true if the helper ran and the process should exit immediately.
pub fn early_self_update_hook() -> bool {
    // Args:  --apply-update <app_dir> <extracted_dir> <new_exe_name>
    //        [--elevated] [--wait-pid <pid>]
    let mut args = std::env::args_os();
    if let Some(first) = args.nth(1)
        && first == "--apply-update"
    {
        let app_dir = args.next().expect("missing app_dir");
        let extract = args.next().expect("missing extracted_dir");
        let exe_name = args.next().expect("missing exe_name");
        // Хвост разбираем циклом, а не «следующий аргумент — это --elevated»:
        // флагов теперь два и порядок между ними не гарантирован.
        let mut elevated = false;
        let mut wait_pid: Option<u32> = None;
        while let Some(flag) = args.next() {
            if flag == "--elevated" {
                elevated = true;
            } else if flag == "--wait-pid" {
                wait_pid = args
                    .next()
                    .and_then(|v| v.to_string_lossy().parse::<u32>().ok());
            }
        }
        if let Some(pid) = wait_pid {
            // Ждём реального выхода приложения. Без этого helper начинал
            // копирование, пока основной процесс ещё жив (он висел на модальном
            // "Updating"-окне), и падал с "Отказано в доступе (os error 5)".
            wait_for_process_exit(pid, Duration::from_secs(30));
        }
        if let Err(e) = apply_update(
            Path::new(&app_dir),
            Path::new(&extract),
            &exe_name,
            elevated,
        ) {
            // Last-chance message box (no parent HWND here)
            msgbox_raw(
                crate::i18n::get().strings().upd_title_update_failed,
                &format!("{e:#}"),
                true,
            );
        }
        return true;
    }

    // Обычный запуск: подчищаем резервные копии, оставшиеся от прошлого
    // обновления (см. replace_file) — тогда они были заняты, сейчас нет.
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        cleanup_stale_backups(dir);
    }

    false
}

/// Суффикс, который получает занятый файл, если положить новый поверх него
/// не удалось (см. replace_file). Чистится при следующем обычном запуске.
const BACKUP_EXT: &str = "aurora-old";

/// Ждёт завершения процесса с указанным PID.
///
/// Не ошибка, если процесс уже мёртв: OpenProcess просто не даст хендл, и мы
/// сразу возвращаемся. Таймаут — страховка от зависшего приложения; по его
/// истечении обновление всё равно пробуется (replace_file умеет обходить
/// занятый файл переименованием).
fn wait_for_process_exit(pid: u32, timeout: Duration) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    // SAFETY: `pid` пришёл аргументом командной строки от основного
    // процесса; хендл `h` проверен на валидность до использования и
    // закрывается ровно один раз. WaitForSingleObject только ждёт —
    // состояние чужого процесса не меняет.
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else {
            return;
        };
        if h.is_invalid() {
            return;
        }
        let _ = WaitForSingleObject(h, timeout.as_millis() as u32);
        let _ = CloseHandle(h);
    }

    // Windows освобождает файловые блокировки образа не мгновенно после того,
    // как процесс сигналит о завершении.
    thread::sleep(Duration::from_millis(300));
}

/// Удаляет `*.aurora-old`, оставшиеся от предыдущего обновления. Best-effort:
/// файл всё ещё может быть занят, тогда попробуем в следующий раз.
fn cleanup_stale_backups(dir: &Path) {
    let Ok(entries) = read_dir(dir) else {
        return;
    };
    for e in entries.filter_map(|e| e.ok()) {
        let p = e.path();
        // По имени, а не по extension(): у второй копии суффикс может быть
        // ".aurora-old.<pid>", и расширением тогда считается уже pid.
        if p.file_name()
            .and_then(OsStr::to_str)
            .map(|s| s.to_ascii_lowercase().contains(&format!(".{BACKUP_EXT}")))
            .unwrap_or(false)
        {
            let _ = fs::remove_file(&p);
        }
    }
}

/// Spawns a background thread that checks + prompts + downloads + launches helper.
pub fn spawn_check(hwnd_parent: HWND, current_version: &str) {
    let current = current_version.to_string();
    thread::spawn(move || {
        if let Err(e) = check_install_and_restart(hwnd_parent, &current) {
            let t = crate::i18n::get().strings();
            msgbox(
                hwnd_parent,
                t.upd_title_update_check_failed,
                &format!("{e:#}"),
                true,
            );
        }
    });
}

fn check_install_and_restart(hwnd: HWND, current_version: &str) -> Result<()> {
    let rel = fetch_latest_release()?;
    let LatestRelease {
        tag,
        name,
        asset_name,
        asset_url,
        sums_url,
    } = &rel;

    let new_ver = tag.trim_start_matches('v');
    let cur_ver = current_version.trim_start_matches('v');

    let lang = crate::i18n::get();
    let t = lang.strings();

    if !is_newer(new_ver, cur_ver) {
        msgbox(
            hwnd,
            t.upd_title_up_to_date,
            &crate::i18n::upd_body_up_to_date(lang, current_version),
            false,
        );
        return Ok(());
    }

    let text = crate::i18n::upd_body_update_available(lang, current_version, new_ver, name);
    if !confirm(hwnd, t.upd_title_update_available, &text) {
        return Ok(());
    }

    // Ожидаемый SHA-256 берём ДО скачивания: если в релизе нет SHA256SUMS.txt
    // или в нём нет строки под наш ассет — прерываемся, ничего не скачивая.
    let expected_sha = fetch_expected_sha256(sums_url.as_deref(), asset_name)?;

    // Download
    let zip_path = download_asset(asset_url, asset_name)?;
    // Проверяем целостность ДО распаковки и тем более до копирования поверх
    // приложения: дальше по этому пути helper при необходимости поднимает права
    // через UAC, так что непроверенный архив там недопустим.
    verify_sha256(&zip_path, &expected_sha)?;
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

/// Имя ассета с контрольными суммами, которое кладёт релизный workflow
/// (см. .github/workflows/rust.yml).
const SUMS_ASSET_NAME: &str = "SHA256SUMS.txt";

struct LatestRelease {
    tag: String,
    name: String,
    asset_name: String,
    asset_url: String,
    /// None, если релиз опубликован без SHA256SUMS.txt — обновление в этом
    /// случае не выполняется, см. fetch_expected_sha256.
    sums_url: Option<String>,
}

fn fetch_latest_release() -> Result<LatestRelease> {
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
    let mut sums_url: Option<String> = None;
    for a in assets {
        let aname = a.get("name").and_then(|x| x.as_str()).unwrap_or("");
        let url = a
            .get("browser_download_url")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if aname.eq_ignore_ascii_case(SUMS_ASSET_NAME) {
            sums_url = Some(url.to_string());
        }
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
    Ok(LatestRelease {
        tag,
        name,
        asset_name,
        asset_url,
        sums_url,
    })
}

/// Достаёт ожидаемый SHA-256 нужного ассета из опубликованного SHA256SUMS.txt.
///
/// Fail-closed: без файла сумм или без строки под конкретный ассет обновление
/// не выполняется вовсе. Дальше по этому пути архив распаковывается и
/// копируется поверх каталога приложения — при необходимости из процесса,
/// поднятого через UAC, — так что скачивать непроверяемое здесь нельзя.
/// Релизный workflow всегда публикует этот файл (`if-no-files-found: error`),
/// поэтому его отсутствие означает не «старый релиз», а что-то неожиданное.
fn fetch_expected_sha256(sums_url: Option<&str>, asset_name: &str) -> Result<String> {
    let url = sums_url.ok_or_else(|| {
        anyhow::anyhow!(
            "Release has no {SUMS_ASSET_NAME}; refusing to install an unverifiable update"
        )
    })?;

    let client = reqwest::blocking::Client::builder()
        .user_agent(UA)
        .build()?;
    let resp = client.get(url).send()?;
    if !resp.status().is_success() {
        bail!("Downloading {} failed: {}", SUMS_ASSET_NAME, resp.status());
    }
    let body = resp.text()?;

    parse_sha256sums(&body, asset_name).ok_or_else(|| {
        anyhow::anyhow!("{SUMS_ASSET_NAME} has no entry for '{asset_name}'; refusing to install")
    })
}

/// Разбирает формат `sha256sum`: `<hex>  <filename>` по строке на файл.
fn parse_sha256sums(text: &str, asset_name: &str) -> Option<String> {
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        // Имя может содержать пробелы — забираем остаток строки целиком.
        let Some(rest) = line.split_once(hash).map(|(_, r)| r.trim()) else {
            continue;
        };
        // Формат допускает префикс '*' у имени (binary mode у coreutils).
        let name = rest.trim_start_matches('*');
        if name.eq_ignore_ascii_case(asset_name) && hash.len() == 64 {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

/// Считает SHA-256 файла и сверяет с ожидаемым.
fn verify_sha256(path: &Path, expected_hex: &str) -> Result<()> {
    use sha2::{Digest, Sha256};

    let mut f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    io::copy(&mut f, &mut hasher).with_context(|| format!("hash {}", path.display()))?;
    let actual = hasher.finalize();
    let actual_hex = actual.iter().fold(String::with_capacity(64), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    });

    if !actual_hex.eq_ignore_ascii_case(expected_hex) {
        // Файл заведомо испорчен или подменён — не оставляем его в %TEMP%,
        // чтобы он не подвернулся под руку при следующей попытке.
        let _ = fs::remove_file(path);
        bail!(
            "Checksum mismatch for the downloaded update (expected {expected_hex}, got {actual_hex}); \
             the file was discarded and nothing was installed"
        );
    }
    Ok(())
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

/// Является ли версия пре-релизной (`4.1.0-rc1`, `2.0.0-beta`).
///
/// Такие релизы игнорируются: сравнение ниже работает только по MAJOR.MINOR.PATCH
/// и не может упорядочить `4.1.0-rc1` относительно `4.1.0`, а предлагать
/// пользователю release candidate как обычное обновление неправильно.
fn is_prerelease(v: &str) -> bool {
    v.contains('-') || v.contains('+')
}

fn is_newer(new_v: &str, cur_v: &str) -> bool {
    // Пре-релиз никогда не предлагается как обновление.
    if is_prerelease(new_v) {
        return false;
    }
    // Обратное направление важно не меньше: сидящий на 4.1.0-rc1 пользователь
    // должен получить вышедшую 4.1.0, а не услышать "у вас уже последняя".
    // parse() отбрасывает суффикс, поэтому 4.1.0-rc1 сравнивается как 4.1.0 —
    // при равенстве версий это дало бы "не новее". Считаем любой стабильный
    // релиз новее пре-релиза с тем же номером.
    let cur_is_pre = is_prerelease(cur_v);
    match parse_version(new_v).cmp(&parse_version(cur_v)) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Equal => cur_is_pre,
        std::cmp::Ordering::Less => false,
    }
}

/// MAJOR.MINOR.PATCH с отброшенным пре-релизным/метаданным хвостом.
///
/// Числовой префикс сегмента берётся честно: `"1-rc1"` это 1, а не 0, как было
/// бы при `parse::<i64>().unwrap_or(0)` на всей строке — из-за такого «тихого
/// обнуления» 4.2.0-rc1 читался как [4,0,0] и оказывался старше 4.1.0.
fn parse_version(v: &str) -> [i64; 3] {
    let mut out = [0i64; 3];
    for (i, part) in v.trim().split('.').take(3).enumerate() {
        let digits: String = part
            .trim()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        out[i] = digits.parse::<i64>().unwrap_or(0);
    }
    out
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
    out.push(format!("aurora-vibra-{}", asset_name));
    let mut f = File::create(&out)?;
    io::copy(&mut resp, &mut f)?;
    Ok(out)
}

fn extract_zip(zip_path: &Path) -> Result<PathBuf> {
    let file = File::open(zip_path)?;
    let mut zip = ZipArchive::new(file)?;
    let mut out_dir = std::env::temp_dir();
    out_dir.push(format!(
        "aurora-vibra-extract-{}",
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
                .contains("aurora")
        })
        .ok_or_else(|| anyhow::anyhow!("No .exe found in extracted package"))?;
    Ok(exe.file_name().unwrap().into())
}

/// Имя временной копии EXE, которая и выполняет обновление.
///
/// Слов "update"/"setup"/"install"/"patch" в этом имени НЕТ намеренно. Windows
/// опознаёт инсталляторы по имени файла (UAC installer detection и Program
/// Compatibility Assistant) и вешает на такой путь слой RUNASADMIN в
/// `HKCU\...\AppCompatFlags\Layers`. После этого CreateProcess по этому пути
/// падает с ERROR_ELEVATION_REQUIRED, и обновление обрывалось сообщением
/// "spawn helper: ... (os error 740)" ещё до того, как helper успевал сам
/// разобраться, нужны ли ему вообще права администратора.
///
/// Смена имени попутно уводит нас с уже помеченного пути: слой в реестре
/// привязан к конкретному пути, а не к содержимому файла.
const HELPER_EXE_NAME: &str = "aurora-vibra-helper.exe";

/// ERROR_ELEVATION_REQUIRED: CreateProcess отказывается запускать процесс, на
/// который Windows навесила требование прав администратора. Поднять права сам
/// CreateProcess не умеет — нужен ShellExecuteW("runas"), см. ниже.
const ERROR_ELEVATION_REQUIRED: i32 = 740;

fn copy_self_to_temp_helper(current_exe: &Path) -> Result<PathBuf> {
    let mut helper = std::env::temp_dir();
    helper.push(HELPER_EXE_NAME);
    fs::copy(current_exe, &helper).context("copy helper")?;
    Ok(helper)
}

fn launch_helper_and_exit(
    helper_path: &Path,
    app_dir: &Path,
    extracted_dir: &Path,
    exe_name: &str,
) -> Result<()> {
    // Сообщение показываем ДО запуска helper'а. MessageBoxW модальный: пока
    // пользователь не нажмёт OK, процесс не выходит — а helper тем временем
    // уже копировал бы файлы поверх работающего приложения и падал с
    // "Отказано в доступе (os error 5)".
    {
        let t = crate::i18n::get().strings();
        msgbox(HWND(0), t.upd_title_updating, t.upd_body_updating, false);
    }

    // Use a detached helper so it keeps running after we exit.
    let pid = std::process::id().to_string();
    let mut cmd = Command::new(helper_path);
    cmd.arg("--apply-update")
        .arg(app_dir)
        .arg(extracted_dir)
        .arg(exe_name)
        .arg("--wait-pid")
        .arg(&pid)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    match cmd.spawn() {
        Ok(_) => {}
        Err(e) if e.raw_os_error() == Some(ERROR_ELEVATION_REQUIRED) => {
            // Windows считает, что этот EXE обязан запускаться от админа, хотя
            // на ЭТОМ шаге права не нужны: helper сначала сам проверяет, пишем
            // ли каталог приложения, и просит UAC только если нет (см.
            // apply_update/relaunch_self_elevated). Пробуем два пути по
            // возрастанию неудобства для пользователя.
            //
            // 1. __COMPAT_LAYER=RunAsInvoker отменяет навязанное требование для
            //    дочернего процесса — обновление проходит вообще без запроса UAC.
            cmd.env("__COMPAT_LAYER", "RunAsInvoker");
            if cmd.spawn().is_err() {
                // 2. Не помогло — честно спрашиваем UAC. Права получаются выше
                //    необходимых, но это лучше, чем оборванное обновление.
                spawn_helper_via_uac(helper_path, app_dir, extracted_dir, exe_name)?;
            }
        }
        Err(e) => return Err(anyhow::Error::new(e).context("spawn helper")),
    }

    // Выходим немедленно: helper ждёт именно нашего PID и до тех пор ничего
    // не трогает.
    std::process::exit(0);
}

/// Запасной запуск helper'а — через ShellExecuteW("runas"), единственный путь,
/// когда CreateProcess упёрся в ERROR_ELEVATION_REQUIRED. Helper сразу получает
/// `--elevated`: права уже подняты, спрашивать UAC второй раз ему незачем.
fn spawn_helper_via_uac(
    helper_path: &Path,
    app_dir: &Path,
    extracted_dir: &Path,
    exe_name: &str,
) -> Result<()> {
    let params = format!(
        "--apply-update \"{}\" \"{}\" \"{}\" --elevated --wait-pid {}",
        app_dir.display(),
        extracted_dir.display(),
        exe_name,
        std::process::id()
    );
    let params_w = wide_str(&params);
    let helper_w = wide_os(helper_path.as_os_str());
    // SAFETY: `params_w` и `helper_w` — NUL-терминированные UTF-16 буферы,
    // живущие до конца вызова; строка с "runas" — статическая (макрос w!).
    // Код возврата ShellExecuteW — псевдо-хендл, сравниваем с 32 по
    // документированному контракту API.
    unsafe {
        let h = ShellExecuteW(
            None,
            w!("runas"),
            PCWSTR(helper_w.as_ptr()),
            PCWSTR(params_w.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
        if (h.0 as isize) <= 32 {
            let t = crate::i18n::get().strings();
            msgbox_raw(t.upd_title_admin_required, t.upd_body_admin_required, true);
            bail!(
                "spawn helper: elevation required, and ShellExecuteW(runas) failed or was denied"
            );
        }
    }
    Ok(())
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
    // SAFETY: `target_w` — NUL-терминированный UTF-16 буфер, живёт до конца
    // вызова; "open" — статическая строка (макрос w!). Псевдо-хендл результата
    // сравниваем с 32 по документированному контракту ShellExecuteW.
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
            let lang = crate::i18n::get();
            msgbox_raw(
                lang.strings().upd_title_launch_failed,
                &crate::i18n::upd_body_launch_failed(lang, &target.display().to_string()),
                true,
            );
        }
    }

    Ok(())
}

fn can_write_dir(dir: &Path) -> io::Result<bool> {
    let probe = dir.join(".__aurora_write_test.tmp");
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
    // SAFETY: `params_w` и `me_w` — NUL-терминированные UTF-16 буферы,
    // живущие до конца вызова; "runas" — статическая строка (макрос w!).
    // Псевдо-хендл результата сравниваем с 32 по контракту ShellExecuteW.
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
            let t = crate::i18n::get().strings();
            msgbox_raw(t.upd_title_admin_required, t.upd_body_admin_required, true);
            bail!("User denied elevation or ShellExecuteW(runas) failed");
        }
    }
    Ok(())
}

/// Дополнительная (слабая) проверка перед копированием.
///
/// Занятость запущенного EXE она НЕ ловит: Windows разрешает переименовывать
/// файл работающего образа, запрещая только удаление и перезапись. Настоящая
/// гарантия — ожидание PID приложения в early_self_update_hook, а страховка на
/// случай остаточных блокировок — replace_file.
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
            replace_file(&sp, &dp)?;
        }
    }
    Ok(())
}

/// Кладёт `src` на место `dst`, переживая занятость `dst`.
///
/// Windows не даёт удалить или перезаписать файл запущенного образа (EXE/DLL),
/// но переименовать его — даёт. Поэтому если обычная замена упирается в
/// «Отказано в доступе», занятый файл уезжает в сторону под `.aurora-old`, а
/// новый встаёт на освободившееся имя; хвост подчищается при следующем старте
/// (cleanup_stale_backups). Раньше этой ветки не было, и обновление обрывалось
/// с "os error 5", если приложение ещё не успело закрыться.
fn replace_file(src: &Path, dst: &Path) -> Result<()> {
    // Сначала копия рядом с целью: rename в пределах тома атомарен, копирование
    // поверх живого файла — нет.
    let mut tmp = dst.to_path_buf();
    tmp.set_extension("updt");
    copy(src, &tmp).with_context(|| format!("copy to {}", tmp.display()))?;

    if fs::rename(&tmp, dst).is_ok() {
        return Ok(());
    }
    if fs::remove_file(dst).is_ok() && fs::rename(&tmp, dst).is_ok() {
        return Ok(());
    }

    // Цель занята — уводим её под уникальным именем и повторяем.
    let mut backup = dst.to_path_buf();
    backup.set_extension(BACKUP_EXT);
    let _ = fs::remove_file(&backup); // остаток прошлого обновления
    if backup.exists() {
        backup.set_extension(format!("{}.{}", BACKUP_EXT, std::process::id()));
    }
    fs::rename(dst, &backup).with_context(|| format!("move aside {}", dst.display()))?;
    match fs::rename(&tmp, dst) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Вернуть всё как было — иначе останется каталог без EXE.
            let _ = fs::rename(&backup, dst);
            let _ = fs::remove_file(&tmp);
            Err(anyhow::Error::new(e).context(format!("install {}", dst.display())))
        }
    }
}

fn confirm(hwnd: HWND, title: &str, text: &str) -> bool {
    let title_w = wide_str(title);
    let text_w = wide_str(text);
    // SAFETY: `title_w`/`text_w` — NUL-терминированные UTF-16 буферы, живущие
    // до конца вызова; hwnd — либо валидное окно, либо HWND(0) (без родителя),
    // оба варианта допустимы для MessageBoxW.
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
    // SAFETY: см. confirm() выше — те же инварианты буферов и hwnd.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_exe_name_avoids_installer_detection_keywords() {
        // Регрессия: helper назывался aurora-vibra-updater-helper.exe, Windows
        // опознавала его как инсталлятор по подстроке "update" и требовала
        // прав администратора — CreateProcess падал с os error 740.
        let name = HELPER_EXE_NAME.to_ascii_lowercase();
        for kw in ["update", "setup", "install", "patch"] {
            assert!(!name.contains(kw), "{HELPER_EXE_NAME} contains {kw:?}");
        }
        assert!(name.ends_with(".exe"));
    }

    #[test]
    fn replace_file_overwrites_and_backups_are_cleaned_up() {
        let dir = std::env::temp_dir().join(format!(
            "aurora-vibra-replace-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = create_dir_all(&dir);

        let src = dir.join("new.bin");
        let dst = dir.join("app.exe");
        fs::write(&src, b"new").unwrap();
        fs::write(&dst, b"old").unwrap();

        replace_file(&src, &dst).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"new");
        assert!(!dir.join("app.updt").exists(), "временный файл не убран");

        // Хвост от прошлого обновления подчищается по имени, включая вариант
        // с ".aurora-old.<pid>".
        let b1 = dir.join(format!("app.{BACKUP_EXT}"));
        let b2 = dir.join(format!("app.{BACKUP_EXT}.4242"));
        fs::write(&b1, b"x").unwrap();
        fs::write(&b2, b"x").unwrap();
        cleanup_stale_backups(&dir);
        assert!(!b1.exists() && !b2.exists());
        assert!(dst.exists(), "чистка не должна трогать обычные файлы");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn newer_patch_minor_and_major_are_offered() {
        assert!(is_newer("4.0.2", "4.0.1"));
        assert!(is_newer("4.1.0", "4.0.9"));
        assert!(is_newer("5.0.0", "4.9.9"));
    }

    #[test]
    fn same_or_older_version_is_not_offered() {
        assert!(!is_newer("4.0.1", "4.0.1"));
        assert!(!is_newer("4.0.0", "4.0.1"));
        assert!(!is_newer("3.9.9", "4.0.0"));
    }

    #[test]
    fn prerelease_is_never_offered_as_an_update() {
        // Регрессия: parse::<i64>().unwrap_or(0) по всей строке превращал
        // "0-rc1" в 0, из-за чего 4.1.0-rc1 читался как 4.1.0 и предлагался
        // как обычный релиз.
        assert!(!is_newer("4.1.0-rc1", "4.0.1"));
        assert!(!is_newer("5.0.0-beta.2", "4.0.1"));
        assert!(!is_newer("4.0.2+build7", "4.0.1"));
    }

    #[test]
    fn stable_release_supersedes_prerelease_of_same_number() {
        assert!(is_newer("4.1.0", "4.1.0-rc1"));
        assert!(!is_newer("4.0.9", "4.1.0-rc1"));
    }

    #[test]
    fn version_segments_keep_their_numeric_prefix() {
        // Вторая регрессия того же бага: 4.2.0-rc1 давал [4,0,0] и оказывался
        // СТАРШЕ 4.1.0.
        assert_eq!(parse_version("4.2.0-rc1"), [4, 2, 0]);
        assert_eq!(parse_version("v10.20.30"), [0, 20, 30]); // 'v' снимается вызывающей стороной
        assert_eq!(parse_version("1.2"), [1, 2, 0]);
        assert!(parse_version("4.2.0-rc1") > parse_version("4.1.0"));
    }

    #[test]
    fn windows_x64_asset_outscores_generic_one() {
        assert!(
            score_asset_name("aurora-vibra-v4.0.1-windows-x64.zip")
                > score_asset_name("aurora-vibra-v4.0.1.zip")
        );
        assert!(score_asset_name("app-win-x64.zip") > score_asset_name("app-win.zip"));
    }

    #[test]
    fn sha256sums_entry_is_found_for_the_matching_asset() {
        let text = "\
d4be3aa2f2d7a69a5cc6943c20064b05dc44c6c2ed76ab84bdd04acc500571a4  aurora-vibra-v4.0.1-windows-x64.zip
2e0d98ef994fa3b2681f6da3815d28970dc4f14a76e21e7a170820d98b72e8e0  other.zip";
        assert_eq!(
            parse_sha256sums(text, "aurora-vibra-v4.0.1-windows-x64.zip").as_deref(),
            Some("d4be3aa2f2d7a69a5cc6943c20064b05dc44c6c2ed76ab84bdd04acc500571a4")
        );
        assert_eq!(parse_sha256sums(text, "missing.zip"), None);
    }

    #[test]
    fn sha256sums_tolerates_binary_mode_star_and_spaces_in_name() {
        let text = "d4be3aa2f2d7a69a5cc6943c20064b05dc44c6c2ed76ab84bdd04acc500571a4 *Aurora Vibra v4.0.1.zip";
        assert_eq!(
            parse_sha256sums(text, "Aurora Vibra v4.0.1.zip").as_deref(),
            Some("d4be3aa2f2d7a69a5cc6943c20064b05dc44c6c2ed76ab84bdd04acc500571a4")
        );
    }

    #[test]
    fn missing_sums_asset_refuses_instead_of_installing_unverified() {
        assert!(fetch_expected_sha256(None, "whatever.zip").is_err());
    }

    #[test]
    fn verify_sha256_accepts_matching_and_deletes_mismatching_file() {
        let dir =
            std::env::temp_dir().join(format!("aurora-vibra-sha-test-{}", std::process::id()));
        let _ = create_dir_all(&dir);
        let f = dir.join("payload.bin");
        fs::write(&f, b"aurora").unwrap();
        // sha256("aurora")
        let good = "c8f0ba1e17c4ffa4b1b0e5e2b5a4c0e1d4f2d6de5f38ea0e9b4e5f4b4de2b1a4";

        // Считаем настоящий хеш, чтобы тест не зависел от вручную вписанной
        // константы, и проверяем обе ветки.
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"aurora");
        let real = h.finalize();
        let real_hex: String = real.iter().map(|b| format!("{b:02x}")).collect();
        assert_ne!(real_hex, good, "заглушка не должна совпасть случайно");

        assert!(verify_sha256(&f, &real_hex).is_ok());
        assert!(f.exists());

        assert!(verify_sha256(&f, good).is_err());
        assert!(!f.exists(), "испорченный архив должен быть удалён");

        let _ = fs::remove_dir_all(&dir);
    }
}
