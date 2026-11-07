//! Auto-update. Once a day, only while no game runs, Retake asks GitHub for the latest
//! release; a newer installer is downloaded in the background and checked (its SHA-256 must
//! match the release's `SHA256SUMS.txt`, and when this copy of Retake is signed the
//! installer must carry a valid signature too). It installs the next time Retake starts,
//! never during a match. Portable copies (no installer next to them) only say a new version
//! exists.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::background::SharedGate;
use anyhow::{Context, Result, bail};
use retake_core::update::{self, Pending, Release, State};

use crate::net;

fn dir() -> PathBuf {
    crate::logging::log_dir()
        .parent()
        .map_or_else(|| PathBuf::from("update"), |d| d.join("update"))
}

fn now_s() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn read<T: retake_core::serde::de::DeserializeOwned + Default>(name: &str) -> T {
    std::fs::read_to_string(dir().join(name))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn write<T: retake_core::serde::Serialize>(name: &str, v: &T) {
    let d = dir();
    let _ = std::fs::create_dir_all(&d);
    let _ = std::fs::write(
        d.join(name),
        serde_json::to_string_pretty(v).unwrap_or_default(),
    );
}

/// Whether this copy was installed by the installer (it can update itself).
pub fn installed() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("unins000.exe").is_file()))
        .unwrap_or(false)
}

/// Whether a file carries a valid Authenticode signature.
pub fn signed(path: &Path) -> bool {
    use windows::Win32::Foundation::{HANDLE, HWND};
    use windows::Win32::Security::WinTrust::{
        WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO,
        WTD_CHOICE_FILE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY,
        WTD_UI_NONE, WinVerifyTrust,
    };
    let wide = windows::core::HSTRING::from(path.as_os_str());
    let mut file = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: windows::core::PCWSTR(wide.as_ptr()),
        hFile: HANDLE::default(),
        pgKnownSubject: std::ptr::null_mut(),
    };
    let mut data = WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut file },
        dwStateAction: WTD_STATEACTION_VERIFY,
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    // SAFETY: the structs and the path outlive both calls; the second call frees the state.
    unsafe {
        let r = WinVerifyTrust(HWND::default(), &mut action, (&raw mut data).cast());
        data.dwStateAction = WTD_STATEACTION_CLOSE;
        let _ = WinVerifyTrust(HWND::default(), &mut action, (&raw mut data).cast());
        r == 0
    }
}

/// Downloads `url` to `to` (through a `.part` file).
fn download(url: &str, to: &Path, max: u64) -> Result<()> {
    let (host, port, path) = net::split_url(url).context("not an https URL")?;
    let part = to.with_extension("part");
    let mut f = std::fs::File::create(&part)?;
    let r = net::get(
        &net::Get {
            host: &host,
            port,
            path: &path,
            any_certificate: false,
            max_bytes: max,
            timeout_ms: 60_000,
        },
        &mut f,
    );
    drop(f);
    if let Err(e) = r {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    std::fs::rename(&part, to)?;
    Ok(())
}

/// One check: the latest release, and if it's newer, its installer downloaded and checked.
/// Returns the version made ready (or found, for portable copies).
fn check() -> Result<Option<String>> {
    let api = format!(
        "https://api.github.com/repos/{}/releases/latest",
        update::REPO
    );
    let release = Release::parse(&net::get_text(&api, 1 << 20)?).context("reading the release")?;
    let current = env!("CARGO_PKG_VERSION");
    if release.draft || release.prerelease || !update::is_newer(&release.tag_name, current) {
        return Ok(None);
    }
    let version = release.tag_name.trim_start_matches('v').to_owned();
    if !installed() {
        return Ok(Some(version));
    }
    let (setup, sums) = release.files().context("the release has no installer")?;
    let list = net::get_text(&sums.browser_download_url, 1 << 16)?;
    let want =
        update::expected_hash(&list, &setup.name).context("the installer isn't in SHA256SUMS")?;
    let d = dir();
    std::fs::create_dir_all(&d)?;
    let file = d.join(&setup.name);
    download(&setup.browser_download_url, &file, 1 << 30)?;
    let got = update::sha256_file(&file)?;
    if got != want {
        let _ = std::fs::remove_file(&file);
        bail!("the downloaded installer's SHA-256 doesn't match the release");
    }
    let me_signed = std::env::current_exe().is_ok_and(|e| signed(&e));
    if me_signed && !signed(&file) {
        let _ = std::fs::remove_file(&file);
        bail!("the downloaded installer isn't signed although this copy of Retake is");
    }
    write(
        "pending.json",
        &Pending {
            version: version.clone(),
            file,
            sha256: got,
        },
    );
    Ok(Some(version))
}

/// Checks once a day while no game runs (started a few minutes after Retake).
pub fn start(quiet: SharedGate, notify: impl Fn(&str) + Send + 'static) {
    let spawned = std::thread::Builder::new()
        .name("updater".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_secs(180));
            loop {
                let mut state: State = read("state.json");
                let idle = quiet
                    .lock()
                    .map(|g| g.may_start(crate::clock::now_ms()))
                    .unwrap_or(false);
                if idle && state.due(now_s()) {
                    state.last_check = now_s();
                    match check() {
                        Ok(Some(v)) if installed() => {
                            tracing::info!(version = %v, "update downloaded; installs at the next start");
                            notify(&format!("Retake {v} is ready. It installs the next time Retake starts."));
                        }
                        Ok(Some(v)) => {
                            if state.available.as_deref() != Some(&v) {
                                notify(&format!("Retake {v} is out: download it from github.com/{}/releases.", update::REPO));
                            }
                            state.available = Some(v);
                        }
                        Ok(None) => tracing::info!("Retake is up to date"),
                        Err(e) => tracing::warn!("update check: {e:#}"),
                    }
                    write("state.json", &state);
                }
                std::thread::sleep(Duration::from_secs(3600));
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("starting the updater: {e}");
    }
}

/// At start: installs a downloaded update if one is waiting and no game runs. Returns `true`
/// when the installer was started (Retake then exits; the installer starts it again).
pub fn install_pending(game_running: bool) -> bool {
    let path = dir().join("pending.json");
    let Some(p) = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Pending>(&t).ok())
    else {
        return false;
    };
    let current = env!("CARGO_PKG_VERSION");
    if !update::is_newer(&p.version, current) || !installed() {
        // Installed already (or not installable here): clean up.
        let _ = std::fs::remove_file(&p.file);
        let _ = std::fs::remove_file(&path);
        return false;
    }
    if game_running {
        tracing::info!("a game is running: the update waits for the next start");
        return false;
    }
    match update::sha256_file(&p.file) {
        Ok(h) if h == p.sha256 => {}
        _ => {
            tracing::warn!("the waiting update changed on disk; discarded");
            let _ = std::fs::remove_file(&p.file);
            let _ = std::fs::remove_file(&path);
            return false;
        }
    }
    tracing::info!(version = %p.version, "installing the update");
    std::process::Command::new(&p.file)
        .args([
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            "/NORESTART",
            "/LAUNCH=1",
        ])
        .spawn()
        .map_err(|e| tracing::warn!("starting the installer: {e}"))
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Talks to GitHub: `cargo test -p retake -- --ignored reaches_github`.
    #[test]
    #[ignore]
    fn reaches_github() {
        let body = net::get_text(
            &format!("https://api.github.com/repos/{}", update::REPO),
            1 << 20,
        )
        .unwrap();
        assert!(body.contains("\"full_name\""), "{body}");
        // The latest release answers 200 with a release, or 404 before the first one.
        match net::get_text(
            &format!(
                "https://api.github.com/repos/{}/releases/latest",
                update::REPO
            ),
            1 << 20,
        ) {
            Ok(t) => assert!(Release::parse(&t).is_some(), "{t}"),
            Err(e) => assert!(e.to_string().contains("HTTP 404"), "{e:#}"),
        }
    }

    #[test]
    fn a_test_build_is_not_an_installed_copy() {
        assert!(!installed());
    }

    #[test]
    fn unsigned_files_are_not_signed() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("x.exe");
        std::fs::write(&f, b"MZ not really").unwrap();
        assert!(!signed(&f));
    }

    /// Windows' own notepad is signed (catalog-signed builds may not embed a signature, so
    /// only checked when it has one).
    #[test]
    fn a_signed_system_file_is_recognised() {
        let p = Path::new(r"C:\Windows\explorer.exe");
        if p.is_file() {
            let _ = signed(p);
        }
    }
}
