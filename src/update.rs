//! Updates aus den Releases von BosLook auf Gitea: neuere Version finden, das passende Programm
//! laden, gegen `SHA256SUMS` des Releases prüfen und das laufende Programm ersetzen.
//!
//! Jedes Release enthält je Plattform die Programme einzeln als `boslook-<target>` bzw.
//! `boslook-desktop-<target>` (unter Windows mit `.exe`), siehe `.gitea/workflows/release.yml`.

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

const RELEASES: &str = "https://git.boskrypt.ru/api/v1/repos/MrMurdog/BosLook/releases";

/// Adresse der Releases; mit `BOSLOOK_RELEASES_URL` z. B. für einen Fork oder zum Testen umlenkbar
fn releases() -> String {
    std::env::var("BOSLOOK_RELEASES_URL").unwrap_or_else(|_| RELEASES.to_string())
}
/// So oft wird nach Updates gefragt
pub const INTERVAL: Duration = Duration::from_secs(24 * 3600);
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Serialize)]
pub struct Release {
    pub version: String,
    /// Seite des Releases mit den Änderungen
    pub url: String,
    pub prerelease: bool,
    #[serde(skip)]
    assets: Vec<(String, String)>,
}

/// Plattform, wie sie in den Dateinamen der Releases steht
pub fn target() -> Option<&'static str> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("x86_64-unknown-linux-gnu")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("aarch64-unknown-linux-gnu")
    } else if cfg!(all(target_os = "linux", target_arch = "arm")) {
        Some("armv7-unknown-linux-gnueabihf")
    } else if cfg!(all(windows, target_arch = "x86_64")) {
        Some("x86_64-pc-windows-gnu")
    } else {
        None
    }
}

/// Versionsnummer zum Vergleichen: `0.3.0` > `0.3.0-beta.1` > `0.2.9`
fn key(version: &str) -> Option<(u64, u64, u64, bool, String)> {
    let v = version.trim().trim_start_matches('v');
    let (numbers, pre) = v.split_once('-').map_or((v, None), |(n, p)| (n, Some(p)));
    let mut parts = numbers.split('.').map(|p| p.parse::<u64>().ok());
    let (major, minor, patch) = (parts.next()??, parts.next()??, parts.next().unwrap_or(Some(0))?);
    Some((major, minor, patch, pre.is_none(), pre.unwrap_or_default().to_string()))
}

/// Ist `a` neuer als `b`?
pub fn newer(a: &str, b: &str) -> bool {
    matches!((key(a), key(b)), (Some(a), Some(b)) if a > b)
}

fn get(url: &str) -> Result<ureq::Body> {
    let response = ureq::get(url).header("User-Agent", concat!("BosLook/", env!("CARGO_PKG_VERSION"))).header("Accept", "application/json").call();
    match response {
        Ok(response) => Ok(response.into_body()),
        // Gitea antwortet bei privaten Repositories ohne Anmeldung mit 404
        Err(ureq::Error::StatusCode(404)) => bail!("{url} nicht gefunden (HTTP 404); Updates gehen nur, wenn das Repository öffentlich ist"),
        Err(e) => Err(anyhow::Error::new(e).context(format!("{url} laden"))),
    }
}

fn parse(release: &Value) -> Option<Release> {
    if release["draft"].as_bool().unwrap_or(false) {
        return None;
    }
    let assets = release["assets"].as_array()?.iter().filter_map(|a| Some((a["name"].as_str()?.to_string(), a["browser_download_url"].as_str()?.to_string()))).collect();
    Some(Release {
        version: release["tag_name"].as_str()?.trim_start_matches('v').to_string(),
        url: release["html_url"].as_str()?.to_string(),
        prerelease: release["prerelease"].as_bool().unwrap_or(false),
        assets,
    })
}

/// Neuestes Release aus der Liste der Gitea-API, das neuer als `current` ist
fn pick(list: &Value, current: &str, beta: bool) -> Option<Release> {
    list.as_array()?
        .iter()
        .filter_map(parse)
        .filter(|r| (beta || !r.prerelease) && newer(&r.version, current))
        .max_by(|a, b| key(&a.version).cmp(&key(&b.version)))
}

/// Neuere Version, falls es eine gibt; `beta` = auch Vorabversionen
pub fn latest(beta: bool) -> Result<Option<Release>> {
    let list: Value = serde_json::from_str(&get(&format!("{}?limit=30", releases()))?.read_to_string()?)?;
    Ok(pick(&list, CURRENT, beta))
}

/// Release einer bestimmten Version (für Clients, die der Server aktualisiert)
pub fn release(version: &str) -> Result<Release> {
    let value: Value = serde_json::from_str(&get(&format!("{}/tags/v{version}", releases()))?.read_to_string()?)?;
    parse(&value).with_context(|| format!("Release {version} nicht lesbar"))
}

/// Ergebnis der letzten Suche nach Updates
#[derive(Debug, Clone, Default, Serialize)]
pub struct State {
    pub available: Option<Release>,
    pub checked: Option<chrono::DateTime<chrono::Local>>,
    pub error: Option<String>,
}

/// Nach einer neueren Version suchen; Fehler (z. B. ohne Internet) landen in `error`
pub fn check(beta: bool) -> State {
    let (available, error) = match latest(beta) {
        Ok(release) => (release, None),
        Err(e) => (None, Some(format!("{e:#}"))),
    };
    State { available, checked: Some(chrono::Local::now()), error }
}

/// Prüfsumme der Datei `name` aus dem Inhalt von `SHA256SUMS` (Format von `sha256sum`)
fn checksum<'a>(sums: &'a str, name: &str) -> Option<&'a str> {
    sums.lines().find_map(|l| {
        let (hash, file) = l.split_once(char::is_whitespace)?;
        (file.trim().trim_start_matches('*') == name).then_some(hash)
    })
}

/// Lädt das Programm `program` (`boslook` oder `boslook-desktop`) aus dem Release, prüft es und
/// ersetzt damit das laufende Programm. Wirksam nach einem Neustart.
pub fn install(release: &Release, program: &str) -> Result<()> {
    use sha2::Digest;
    let target = target().context("Für diese Plattform gibt es keine Updates")?;
    let name = format!("{program}-{target}{}", std::env::consts::EXE_SUFFIX);
    let asset = |file: &str| release.assets.iter().find(|(n, _)| n == file).map(|(_, url)| url.as_str());
    let url = asset(&name).with_context(|| format!("{name} fehlt im Release {}", release.version))?;
    let sums = get(asset("SHA256SUMS").context("SHA256SUMS fehlt im Release")?)?.read_to_string()?;
    let expected = checksum(&sums, &name).with_context(|| format!("Keine Prüfsumme für {name}"))?.to_lowercase();
    let bytes = get(url)?.with_config().limit(512 << 20).read_to_vec()?;
    let actual: String = sha2::Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    if actual != expected {
        bail!("Prüfsumme von {name} stimmt nicht, Update abgebrochen");
    }
    replace_exe(&bytes).context("Programm ersetzen")?;
    eprintln!("Update auf {} eingespielt, wirksam nach dem Neustart", release.version);
    Ok(())
}

/// Laufendes Programm durch `bytes` ersetzen
fn replace_exe(bytes: &[u8]) -> Result<()> {
    let exe = exe()?;
    let new = exe.with_extension("new");
    std::fs::write(&new, bytes).with_context(|| format!("{} schreiben", new.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755))?;
    }
    // Windows: Ein laufendes Programm lässt sich nicht überschreiben, aber umbenennen
    #[cfg(windows)]
    std::fs::rename(&exe, exe.with_extension("old"))?;
    std::fs::rename(&new, &exe)?;
    Ok(())
}

/// Pfad des laufenden Programms; unter Linux auch dann noch richtig, wenn die Datei schon durch
/// ein Update ersetzt wurde (dann hängt " (deleted)" am Pfad)
pub fn exe() -> Result<std::path::PathBuf> {
    let exe = std::env::current_exe().context("Programmpfad")?;
    Ok(match exe.to_str().and_then(|p| p.strip_suffix(" (deleted)")) {
        Some(path) => path.into(),
        None => exe,
    })
}

/// Reste eines früheren Updates entfernen (Windows: das umbenannte alte Programm)
pub fn cleanup() {
    if let Ok(exe) = exe() {
        let _ = std::fs::remove_file(exe.with_extension("old"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn versions() {
        assert!(newer("0.3.0", "0.2.3"));
        assert!(newer("v0.10.0", "0.9.9"));
        assert!(newer("0.3.0", "0.3.0-beta.1"));
        assert!(newer("0.3.0-beta.2", "0.3.0-beta.1"));
        assert!(!newer("0.3.0", "0.3.0"));
        assert!(!newer("0.2.9", "0.3.0"));
        assert!(!newer("kaputt", "0.3.0"));
    }

    #[test]
    fn picks_newest_matching_release() {
        let asset = |n: &str| json!({"name": n, "browser_download_url": format!("https://x/{n}")});
        let list = json!([
            {"tag_name": "v0.4.0-beta.1", "html_url": "https://r/4b", "prerelease": true, "draft": false, "assets": [asset("SHA256SUMS")]},
            {"tag_name": "v0.3.1", "html_url": "https://r/31", "prerelease": false, "draft": false, "assets": [asset("SHA256SUMS")]},
            {"tag_name": "v0.5.0", "html_url": "https://r/5", "prerelease": false, "draft": true, "assets": []},
            {"tag_name": "v0.3.0", "html_url": "https://r/30", "prerelease": false, "draft": false, "assets": []},
        ]);
        assert_eq!(pick(&list, "0.3.0", false).unwrap().version, "0.3.1");
        let beta = pick(&list, "0.3.0", true).unwrap();
        assert_eq!((beta.version.as_str(), beta.prerelease), ("0.4.0-beta.1", true));
        assert_eq!(beta.assets[0].1, "https://x/SHA256SUMS");
        assert!(pick(&list, "0.3.1", false).is_none());
    }

    #[test]
    fn checksums() {
        let sums = "abc123  boslook-x86_64-unknown-linux-gnu\ndef456 *boslook-x86_64-pc-windows-gnu.exe\n";
        assert_eq!(checksum(sums, "boslook-x86_64-unknown-linux-gnu"), Some("abc123"));
        assert_eq!(checksum(sums, "boslook-x86_64-pc-windows-gnu.exe"), Some("def456"));
        assert_eq!(checksum(sums, "boslook"), None);
    }
}
