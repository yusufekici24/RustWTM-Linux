//! Update checks against the GitHub releases of RustWTM, and installing a
//! newer version: the MSI is downloaded, verified against its published
//! SHA-256 and, when this build is signed, against its publisher, and
//! handed to Windows Installer. What became of it is written down for the
//! RustWTM started afterwards to tell.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The latest release, as the GitHub API describes it.
pub const LATEST_URL: &str = crate::project::UPDATE_API;
/// The release page for people without an installer (zip, developer builds).
pub const RELEASES_PAGE: &str = crate::project::UPDATE_PAGE;

pub type Version = (u32, u32, u32);

/// `0.7.0` or `v0.7.0`; pre-release suffixes (`-rc1`) are ignored.
pub fn parse_version(s: &str) -> Option<Version> {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let core = s.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u32>().ok());
    let v = (
        parts.next()??,
        parts.next()??,
        parts.next().unwrap_or(Some(0))?,
    );
    parts.next().is_none().then_some(v)
}

/// The version of this build.
pub fn current() -> Version {
    parse_version(env!("CARGO_PKG_VERSION")).expect("the crate version is valid")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub tag: String,
    /// The release page.
    pub page: String,
    /// The installer and its `.sha256` file, when published.
    pub msi: Option<(String, String)>,
}

impl Release {
    pub fn is_newer_than(&self, version: Version) -> bool {
        self.version > version
    }

    pub fn version_text(&self) -> String {
        let (a, b, c) = self.version;
        format!("{a}.{b}.{c}")
    }
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
}

/// Reads the API's answer for one release.
pub fn parse_release(json: &[u8]) -> Result<Release, String> {
    let r: ApiRelease =
        serde_json::from_slice(json).map_err(|e| format!("unexpected answer: {e}"))?;
    if r.draft || r.prerelease {
        return Err(format!("{} is not a final release", r.tag_name));
    }
    let version =
        parse_version(&r.tag_name).ok_or_else(|| format!("unexpected tag {}", r.tag_name))?;
    let url = |name: &str| {
        r.assets
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(name))
            .map(|a| a.browser_download_url.clone())
    };
    let msi = r
        .assets
        .iter()
        .find(|a| a.name.to_ascii_lowercase().ends_with(INSTALLER_SUFFIX))
        .and_then(|a| {
            Some((
                a.browser_download_url.clone(),
                url(&format!("{}.sha256", a.name))?,
            ))
        });
    Ok(Release {
        version,
        tag: r.tag_name,
        page: r.html_url,
        msi,
    })
}

const USER_AGENT: &str = concat!("RustWTM/", env!("CARGO_PKG_VERSION"));

/// The release asset this platform installs: the MSI on Windows, the
/// Debian package on Linux.
#[cfg(windows)]
const INSTALLER_SUFFIX: &str = ".msi";
#[cfg(target_os = "linux")]
const INSTALLER_SUFFIX: &str = "_amd64.deb";

fn fetch(url: &str) -> Result<Vec<u8>, String> {
    match wtm_sys::http::get(url, USER_AGENT) {
        Ok((200, body)) => Ok(body),
        Ok((status, _)) => Err(format!("{url} answered HTTP {status}")),
        Err(e) => Err(format!("cannot reach {url}: {e}")),
    }
}

/// The latest published release.
pub fn latest() -> Result<Release, String> {
    latest_with(fetch_metadata)
}

fn fetch_metadata(url: &str) -> Result<Vec<u8>, String> {
    match wtm_sys::http::get_metadata(url, USER_AGENT) {
        Ok((200, body)) => Ok(body),
        Ok((status, _)) => Err(format!("{url} answered HTTP {status}")),
        Err(e) => Err(format!("cannot reach {url}: {e}")),
    }
}

fn latest_with(get: impl FnOnce(&str) -> Result<Vec<u8>, String>) -> Result<Release, String> {
    parse_release(&get(LATEST_URL)?)
}

#[cfg(test)]
fn latest_from(url: &str) -> Result<Release, String> {
    parse_release(&fetch_metadata(url)?)
}

/// The hash in a `sha256sum`-style line: `<64 hex digits>  <file name>`.
pub fn parse_sha256(text: &str) -> Option<[u8; 32]> {
    let hex = text.split_whitespace().next()?;
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// A downloaded installer and the SHA-256 it was checked against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    pub path: PathBuf,
    pub sha256: [u8; 32],
}

/// Downloads the release's installer into `dir` and checks it against the
/// published SHA-256. Nothing is written when the check fails.
pub fn download(release: &Release, dir: &Path) -> Result<Download, String> {
    let (msi_url, sha_url) = release
        .msi
        .as_ref()
        .ok_or("this release has no installer")?;
    let expected = parse_sha256(&String::from_utf8_lossy(&fetch(sha_url)?))
        .ok_or("the published checksum is unreadable")?;
    let msi = fetch(msi_url)?;
    let actual = wtm_sys::http::sha256(&msi).map_err(|e| e.to_string())?;
    if actual != expected {
        return Err(
            "the download does not match its published checksum; nothing was installed".into(),
        );
    }
    let name = msi_url
        .rsplit('/')
        .next()
        .filter(|n| !n.is_empty())
        .unwrap_or("RustWTM.msi");
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = dir.join(name);
    std::fs::write(&path, msi).map_err(|e| format!("cannot save {}: {e}", path.display()))?;
    Ok(Download {
        path,
        sha256: expected,
    })
}

/// Whether `exe` runs from the folder the MSI installed RustWTM into (only
/// then can an installer update it).
#[cfg(target_os = "linux")]
pub fn installed_by_msi(exe: &Path) -> bool {
    // Installed by the rustwtm package, in /usr/bin.
    exe.parent() == Some(Path::new("/usr/bin"))
        && std::process::Command::new("dpkg-query")
            .args(["-W", "-f=${Status}", "rustwtm"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("install ok installed"))
}

#[cfg(windows)]
pub fn installed_by_msi(exe: &Path) -> bool {
    use winreg::RegKey;
    use winreg::enums::HKEY_LOCAL_MACHINE;
    let dir: Option<String> = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(r"Software\RustWTM")
        .and_then(|k| k.get_value("InstallDir"))
        .ok();
    dir.is_some_and(|d| same_dir(Path::new(&d), exe))
}

#[cfg_attr(not(windows), allow(dead_code))]
fn same_dir(install_dir: &Path, exe: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().trim_end_matches('\\').to_lowercase();
    exe.parent().is_some_and(|p| norm(p) == norm(install_dir))
}

/// Who signed a file, as far as the updater cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Publisher {
    /// An embedded Authenticode signature that holds. `subject` is the
    /// certificate's whole subject (name, organization, place, country):
    /// what the certificate authority validated, and what is compared;
    /// `name` is for messages.
    Signed {
        name: String,
        subject: String,
    },
    Unsigned,
    /// A signature that does not hold, or whose signer cannot be read.
    Unusable(String),
}

/// The publisher of the file at `path`, from its embedded signature only:
/// a catalog signature cannot travel with an installer.
pub fn publisher(path: &Path) -> Publisher {
    match wtm_sys::modules::embedded_signer(&path.to_string_lossy()) {
        Ok(Some(s)) => Publisher::Signed {
            name: s.name,
            subject: s.subject,
        },
        Ok(None) => Publisher::Unsigned,
        Err(e) => Publisher::Unusable(e),
    }
}

/// Whether an installer by `update` may replace a build by `current`, and
/// whether the elevated step must check the publisher again.
///
/// An unsigned build (every release before the first signed one, and
/// developer builds) takes an unsigned or signed installer: it has no
/// publisher to hold the update to. A signed build takes only an installer
/// with a valid embedded signature whose certificate has the same whole
/// subject: the same validated name, organization, place and country, so
/// another company of the same name cannot pass. Thumbprints are not
/// compared: signing certificates are renewed every few days. A build whose
/// own signature cannot be read takes nothing, rather than nothing checked.
pub fn publisher_policy(current: &Publisher, update: &Publisher) -> Result<bool, String> {
    match (current, update) {
        (_, Publisher::Unusable(e)) => Err(format!(
            "the installer's signature does not hold ({e}); nothing was installed"
        )),
        (Publisher::Unusable(e), _) => Err(format!(
            "this RustWTM's own signature cannot be checked ({e}), so the installer's \
             publisher cannot be either; nothing was installed. Download the release from \
             its page instead"
        )),
        (Publisher::Unsigned, _) => Ok(false),
        (
            Publisher::Signed { subject: ours, .. },
            Publisher::Signed {
                subject: theirs, ..
            },
        ) if ours == theirs => Ok(true),
        (Publisher::Signed { name: ours, .. }, Publisher::Signed { name: theirs, .. })
            if ours != theirs =>
        {
            Err(format!(
                "the installer is signed by {theirs}, not by {ours} like this RustWTM; nothing \
             was installed. Download the release from its page instead"
            ))
        }
        (Publisher::Signed { name, .. }, Publisher::Signed { subject, .. }) => Err(format!(
            "the installer is signed by a certificate of {name} with other details ({subject}) \
             than this RustWTM's; nothing was installed. Download the release from its page \
             instead"
        )),
        (Publisher::Signed { name: ours, .. }, Publisher::Unsigned) => Err(format!(
            "the installer is not signed, while this RustWTM is signed by {ours}; nothing was \
             installed. Download the release from its page instead"
        )),
    }
}

/// The elevated step's own exit codes, apart from PowerShell's 1 (an
/// unexpected error) and Windows Installer's.
const CHECKSUM_CHANGED: i32 = 9001;
const WRONG_PUBLISHER: i32 = 9002;
/// Where the launcher writes what became of the update, beside the download.
const OUTCOME_FILE: &str = "outcome.txt";
/// An outcome older than this is from another time; it is not told.
const OUTCOME_FRESH: std::time::Duration = std::time::Duration::from_secs(3600);

/// What became of an update, told by the RustWTM started after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Installed,
    /// Installed; Windows must restart to finish.
    RestartNeeded,
    /// Declined at the administrator prompt or canceled in the installer.
    Cancelled,
    /// The copy no longer matched the checked download: nothing installed.
    ChecksumChanged,
    /// The copy was not signed by this build's publisher: nothing installed.
    WrongPublisher,
    /// Windows Installer failed with this code.
    InstallerFailed(i32),
    /// The elevated step could not run; a Windows error code, or 0.
    NotStarted(i32),
}

impl Outcome {
    /// The elevated step's exit code.
    fn of_exit(code: i32) -> Self {
        match code {
            0 => Self::Installed,
            3010 | 1641 => Self::RestartNeeded,
            1602 => Self::Cancelled,
            CHECKSUM_CHANGED => Self::ChecksumChanged,
            WRONG_PUBLISHER => Self::WrongPublisher,
            code => Self::InstallerFailed(code),
        }
    }

    /// A line the launcher wrote: `<version> exit <code>`, `<version>
    /// cancelled` or `<version> failed <code>`. Anything else is `None`:
    /// the file is the user's to write, so only these are believed.
    pub fn parse(line: &str) -> Option<(Version, Self)> {
        let mut words = line.split_whitespace();
        let version = parse_version(words.next()?)?;
        let outcome = match (words.next()?, words.next()) {
            ("exit", Some(code)) => Self::of_exit(code.parse().ok()?),
            ("cancelled", None) => Self::Cancelled,
            ("failed", Some(code)) => match code.parse().ok()? {
                1223 => Self::Cancelled,
                code => Self::NotStarted(code),
            },
            _ => return None,
        };
        words.next().is_none().then_some((version, outcome))
    }
}

/// What became of the last update, if the launcher wrote it within the
/// last hour; the record is removed either way.
pub fn take_outcome(dir: &Path) -> Option<(Version, Outcome)> {
    let path = dir.join(OUTCOME_FILE);
    let fresh = std::fs::metadata(&path)
        .ok()?
        .modified()
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < OUTCOME_FRESH);
    let text = std::fs::read(&path).ok();
    let _ = std::fs::remove_file(&path);
    let text = text.filter(|t| t.len() <= 256 && fresh)?;
    Outcome::parse(std::str::from_utf8(&text).ok()?.trim())
}

/// Runs the installer (Windows asks for administrator rights) and starts
/// `relaunch` once it is done. The caller should exit right away so its
/// files can be replaced.
///
/// When `relaunch`, this build, is signed, the installer must be signed by
/// the same publisher. The download sits in the user's temp folder, where
/// anything running as the user could swap it after it was checked. So the
/// elevated step copies it to a folder only administrators can reach,
/// checks the copy against the hash, and the publisher, again and installs
/// that. What became of it is written beside the download for the RustWTM
/// started afterwards ([`take_outcome`]).
#[cfg(target_os = "linux")]
pub fn install(deb: &Download, relaunch: &Path, version: Version) -> Result<(), String> {
    let dir = deb.path.parent().unwrap_or(Path::new("."));
    let outcome = dir.join(OUTCOME_FILE);
    let _ = std::fs::remove_file(&outcome);
    let hash: String = deb.sha256.iter().map(|b| format!("{b:02x}")).collect();
    let (a, b, c) = version;
    // The download sits where the user can write: the elevated step copies
    // it to a root-only place, checks the copy against the hash and installs
    // that copy. polkit asks for the password; 126 means it was dismissed.
    let elevated = "t=$(mktemp -d /root/.rustwtm-update.XXXXXX) || exit 92; cp -- \"$1\" \"$t/rustwtm.deb\" || exit 92; \
         echo \"$2  $t/rustwtm.deb\" | sha256sum -c --status || { rm -rf \"$t\"; exit 91; }; \
         apt-get install -y --allow-downgrades \"$t/rustwtm.deb\"; rc=$?; rm -rf \"$t\"; exit $rc";
    let script = format!(
        "pkexec /bin/sh -c '{elevated}' sh \"$1\" \"$2\"; rc=$?; \
         case $rc in 0) r='exit 0';; 126|127) r='cancelled';; 91) r='exit {CHECKSUM_CHANGED}';; *) r=\"exit $rc\";; esac; \
         echo \"{a}.{b}.{c} $r\" > \"$3\" 2>/dev/null; exec \"$4\""
    );
    std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .arg("sh")
        .arg(&deb.path)
        .arg(hash)
        .arg(&outcome)
        .arg(relaunch)
        .stdin(std::process::Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| e.to_string())
}

#[cfg(windows)]
pub fn install(msi: &Download, relaunch: &Path, version: Version) -> Result<(), String> {
    let pin = publisher_policy(&publisher(relaunch), &publisher(&msi.path))?;
    let dir = msi.path.parent().unwrap_or(Path::new("."));
    let outcome = dir.join(OUTCOME_FILE);
    let _ = std::fs::remove_file(&outcome);
    let elevate = elevate(&elevated_install(msi, pin.then_some(relaunch)));
    let start = format!(
        "Start-Process -FilePath {}",
        ps_quote(&relaunch.to_string_lossy())
    );
    let args = format!(
        "-NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand {}",
        encode_command(&launcher(&elevate, &outcome, version, &start))
    );
    wtm_sys::shell::launch_hidden("powershell.exe", &args).map_err(|e| e.to_string())
}

/// A command that runs `script` as administrator (Windows asks) and gives
/// its process once it has ended. Process.Start, not Start-Process: only it
/// keeps the Windows error (1223 when the prompt is declined) inside the
/// exception it throws.
#[cfg_attr(not(windows), allow(dead_code))]
fn elevate(script: &str) -> String {
    format!(
        "& {{ $i = New-Object Diagnostics.ProcessStartInfo 'powershell.exe'; \
         $i.UseShellExecute = $true; $i.Verb = 'runas'; $i.WindowStyle = 'Hidden'; \
         $i.Arguments = '-NoProfile -NonInteractive -EncodedCommand {}'; \
         $q = [Diagnostics.Process]::Start($i); $q.WaitForExit(); $q }}",
        encode_command(script)
    )
}

/// Runs `elevate`, a command that runs the elevated step and gives its
/// process, then writes what became of it to `outcome` and runs `relaunch`,
/// also when elevation is declined or fails, and also when the outcome
/// cannot be written.
#[cfg_attr(not(windows), allow(dead_code))]
fn launcher(elevate: &str, outcome: &Path, version: Version, relaunch: &str) -> String {
    let (a, b, c) = version;
    format!(
        "$r = 'failed 0'\n\
         try {{\n\
         $p = {elevate}\n\
         $r = 'exit ' + $p.ExitCode\n\
         }} catch {{\n\
         $c = 0; $e = $_.Exception\n\
         while ($e) {{ if ($e -is [ComponentModel.Win32Exception]) {{ $c = $e.NativeErrorCode }}; $e = $e.InnerException }}\n\
         if ($c -eq 1223) {{ $r = 'cancelled' }} else {{ $r = 'failed ' + $c }}\n\
         }} finally {{\n\
         try {{ Set-Content -LiteralPath {} -Value ('{a}.{b}.{c} ' + $r) -Encoding ASCII }} catch {{}}\n\
         {relaunch}\n\
         }}\n",
        ps_quote(&outcome.to_string_lossy()),
    )
}

/// Exits with [`WRONG_PUBLISHER`] unless the file `$msi` has a valid
/// embedded signature whose certificate has the same whole subject as the
/// valid signature of `own`, the running RustWTM. Both are read the same
/// way, so their text can be compared as it is.
#[cfg_attr(not(windows), allow(dead_code))]
fn publisher_check(own: &Path) -> String {
    // Neither certificate is read unless both signatures hold: under
    // 'Stop' a missing one would throw and exit 1 for the wrong reason.
    // Windows PowerShell's own modules only: a module path inherited from
    // PowerShell 7 (RustWTM started from its terminal) makes the
    // Microsoft.PowerShell.Security module, and this cmdlet, fail to load.
    format!(
        "$env:PSModulePath = Join-Path $PSHOME 'Modules'\n\
         $own = Get-AuthenticodeSignature -LiteralPath {}\n\
         $s = Get-AuthenticodeSignature -LiteralPath $msi\n\
         if ($own.Status -ne 'Valid' -or $null -eq $own.SignerCertificate -or $s.Status -ne 'Valid' -or $s.SignatureType -ne 'Authenticode' -or $null -eq $s.SignerCertificate) {{ exit {WRONG_PUBLISHER} }}\n\
         if ($s.SignerCertificate.Subject -cne $own.SignerCertificate.Subject) {{ exit {WRONG_PUBLISHER} }}\n",
        ps_quote(&own.to_string_lossy())
    )
}

/// Copies the installer to a fresh folder under the system's temp folder
/// that only SYSTEM and administrators can reach, checks it (and, with
/// `own`, that the copy is signed by the publisher of that file, the
/// running RustWTM) and installs it.
#[cfg_attr(not(windows), allow(dead_code))]
fn elevated_install(msi: &Download, own: Option<&Path>) -> String {
    let hash: String = msi.sha256.iter().map(|b| format!("{b:02X}")).collect();
    let signed = own.map_or_else(String::new, publisher_check);
    format!(
        "$ErrorActionPreference = 'Stop'\n\
         $env:PSModulePath = Join-Path $PSHOME 'Modules'\n\
         $dir = Join-Path $env:SystemRoot ('Temp\\rustwtm-update-' + [Guid]::NewGuid().ToString('N'))\n\
         New-Item -ItemType Directory -Path $dir | Out-Null\n\
         try {{\n\
         $acl = New-Object Security.AccessControl.DirectorySecurity\n\
         $acl.SetSecurityDescriptorSddlForm('O:BAD:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)')\n\
         Set-Acl -LiteralPath $dir -AclObject $acl\n\
         $msi = Join-Path $dir 'RustWTM.msi'\n\
         Copy-Item -LiteralPath {} -Destination $msi\n\
         if ((Get-FileHash -Algorithm SHA256 -LiteralPath $msi).Hash -ne '{hash}') {{ exit {CHECKSUM_CHANGED} }}\n\
         {signed}\
         $p = Start-Process msiexec.exe -ArgumentList '/i', ('\"' + $msi + '\"'), '/passive', '/norestart' -Wait -PassThru\n\
         exit $p.ExitCode\n\
         }} finally {{ cmd.exe /d /c rd /s /q \"$dir\" }}\n",
        ps_quote(&msi.path.to_string_lossy())
    )
}

/// `text` as a single-quoted PowerShell string. PowerShell ends one at any
/// of ' ‘ ’ ‚ ‛ (as in C:\Users\O’Brien), so each is doubled.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn ps_quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for c in text.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// A script for `powershell -EncodedCommand`: base64 of its UTF-16LE.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn encode_command(script: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_picks_the_debian_package_and_its_checksum() {
        let json = serde_json::json!({
            "tag_name": "v0.40.0", "html_url": "https://x/r", "draft": false, "prerelease": false,
            "assets": [
                {"name": "RustWTM-0.40.0-x64.msi", "browser_download_url": "https://dl/a.msi"},
                {"name": "RustWTM-0.40.0-x64.msi.sha256", "browser_download_url": "https://dl/a.msi.sha256"},
                {"name": "rustwtm_0.40.0_amd64.deb", "browser_download_url": "https://dl/rustwtm_0.40.0_amd64.deb"},
                {"name": "rustwtm_0.40.0_amd64.deb.sha256", "browser_download_url": "https://dl/rustwtm_0.40.0_amd64.deb.sha256"}
            ]
        });
        let r = parse_release(json.to_string().as_bytes()).unwrap();
        assert_eq!(r.version, (0, 40, 0));
        assert_eq!(
            r.msi,
            Some((
                "https://dl/rustwtm_0.40.0_amd64.deb".into(),
                "https://dl/rustwtm_0.40.0_amd64.deb.sha256".into()
            ))
        );
        assert!(!installed_by_msi(Path::new("/tmp/rustwtm")));
    }
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[cfg(windows)]
    #[test]
    fn repository_transfer_keeps_discovery_and_asset_pairs_on_the_same_identity() {
        for repository in ["ersinkoc/RustWTM", "rustwtm/rustwtm"] {
            let base = format!("https://github.com/{repository}/releases");
            let installer = format!("{base}/download/v0.28.0/RustWTM-0.28.0-x64.msi");
            let json = serde_json::json!({
                "tag_name": "v0.28.0", "html_url": format!("{base}/tag/v0.28.0"),
                "assets": [
                    {"name":"RustWTM-0.28.0-x64.msi","browser_download_url":installer},
                    {"name":"RustWTM-0.28.0-x64.msi.sha256","browser_download_url":format!("{installer}.sha256")}
                ]
            });
            let release = latest_with(|url| {
                assert_eq!(
                    url, "https://api.github.com/repositories/1425760656/releases/latest",
                    "discovery must survive an owner/name change without trusting a reused old name"
                );
                Ok(serde_json::to_vec(&json).unwrap())
            })
            .unwrap();
            assert_eq!(release.version, (0, 28, 0));
            assert_eq!(release.page, format!("{base}/tag/v0.28.0"));
            assert_eq!(
                release.msi,
                Some((installer.clone(), format!("{installer}.sha256")))
            );
        }
        assert_eq!(
            latest_with(|_| Err("repository unavailable".into())).unwrap_err(),
            "repository unavailable"
        );
        assert!(latest_with(|_| Ok(b"not JSON".to_vec())).is_err());
    }

    #[test]
    #[ignore = "read-only unauthenticated GitHub metadata probe; no download or install"]
    fn identity_release_metadata_smoke() {
        let release = latest().expect("identity-bound public release API");
        assert!(
            release.msi.is_some(),
            "published installer and checksum pair"
        );
        println!(
            "Repository {}: {} ({})",
            crate::project::REPOSITORY_ID,
            release.tag,
            release.page
        );
    }

    const JSON: &str = r#"{
        "tag_name": "v0.8.1", "html_url": "https://github.com/x/releases/tag/v0.8.1",
        "draft": false, "prerelease": false,
        "assets": [
            {"name": "RustWTM-0.8.1-x64.msi", "browser_download_url": "https://dl/RustWTM-0.8.1-x64.msi"},
            {"name": "RustWTM-0.8.1-x64.msi.sha256", "browser_download_url": "https://dl/RustWTM-0.8.1-x64.msi.sha256"},
            {"name": "rustwtm.zip", "browser_download_url": "https://dl/rustwtm.zip"}
        ]
    }"#;

    #[test]
    fn versions_parse_and_compare() {
        assert_eq!(parse_version("v0.7.0"), Some((0, 7, 0)));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("V2.10.3-rc1"), Some((2, 10, 3)));
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("x1"), None);
        assert_eq!(parse_version(""), None);
        assert!((0, 10, 0) > (0, 9, 9));
        assert_eq!(current(), parse_version(env!("CARGO_PKG_VERSION")).unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn reads_a_release_and_its_installer() {
        let r = parse_release(JSON.as_bytes()).unwrap();
        assert_eq!(r.version, (0, 8, 1));
        assert_eq!(r.version_text(), "0.8.1");
        assert_eq!(
            r.msi,
            Some((
                "https://dl/RustWTM-0.8.1-x64.msi".into(),
                "https://dl/RustWTM-0.8.1-x64.msi.sha256".into()
            ))
        );
        assert!(r.is_newer_than((0, 7, 0)) && !r.is_newer_than((0, 8, 1)));
        // An installer without a checksum is not offered.
        let unsigned = JSON.replace("RustWTM-0.8.1-x64.msi.sha256", "other.sha256");
        assert_eq!(parse_release(unsigned.as_bytes()).unwrap().msi, None);
        let pre = JSON.replace(r#""prerelease": false"#, r#""prerelease": true"#);
        assert!(
            parse_release(pre.as_bytes())
                .unwrap_err()
                .contains("not a final release")
        );
        let odd = JSON.replace("v0.8.1\",", "nightly\",");
        assert!(
            parse_release(odd.as_bytes())
                .unwrap_err()
                .starts_with("unexpected tag")
        );
        assert!(
            parse_release(b"<html>")
                .unwrap_err()
                .starts_with("unexpected answer")
        );
    }

    /// The inverse of [`encode_command`], for the tests.
    fn decode_command(text: &str) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let bits: Vec<u8> = text
            .bytes()
            .filter(|b| *b != b'=')
            .map(|b| ALPHABET.iter().position(|a| *a == b).unwrap() as u8)
            .collect();
        let mut bytes = Vec::new();
        for chunk in bits.chunks(4) {
            let n = chunk
                .iter()
                .enumerate()
                .fold(0u32, |n, (i, b)| n | u32::from(*b) << (18 - 6 * i));
            for i in 0..chunk.len() - 1 {
                bytes.push((n >> (16 - 8 * i)) as u8);
            }
        }
        let units: Vec<u16> = bytes
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16(&units).unwrap()
    }

    #[test]
    fn commands_are_encoded_as_powershell_expects() {
        // `powershell -EncodedCommand` takes base64 of UTF-16LE.
        assert_eq!(encode_command("a"), "YQA=");
        assert_eq!(encode_command("ab"), "YQBiAA==");
        assert_eq!(encode_command("abc"), "YQBiAGMA");
        for text in ["", "x", "Get-Date; 'ç'", "{}[]\"'"] {
            assert_eq!(decode_command(&encode_command(text)), text);
        }
    }

    #[cfg(windows)]
    #[test]
    fn the_installer_is_checked_again_where_only_administrators_reach() {
        let msi = Download {
            path: PathBuf::from(r"C:\Users\o'neil\AppData\Local\Temp\RustWTM-update\a.msi"),
            sha256: [0xab; 32],
        };
        let unpinned = elevated_install(&msi, None);
        assert!(
            unpinned.contains(r"Copy-Item -LiteralPath 'C:\Users\o''neil\AppData"),
            "{unpinned}"
        );
        assert!(unpinned.contains(&format!("-ne '{}') {{ exit 9001 }}", "AB".repeat(32))));
        assert!(unpinned.contains("O:BAD:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"));
        assert!(
            !unpinned.contains("Get-AuthenticodeSignature"),
            "no publisher to hold to"
        );
        // The copy is installed, never the download.
        assert!(unpinned.contains("'/i', ('\"' + $msi + '\"')"));
        let pinned = elevated_install(
            &msi,
            Some(Path::new(r"C:\Program Files\RustWTM\rustwtm.exe")),
        );
        let check = pinned
            .find("Get-AuthenticodeSignature -LiteralPath $msi")
            .unwrap();
        assert!(
            check > pinned.find("Get-FileHash").unwrap(),
            "after the copy's hash"
        );
        assert!(
            check < pinned.find("msiexec.exe").unwrap(),
            "before installing"
        );
        assert!(pinned.contains("$s.Status -ne 'Valid' -or $s.SignatureType -ne 'Authenticode'"));
        assert!(pinned.contains(
            r"$own = Get-AuthenticodeSignature -LiteralPath 'C:\Program Files\RustWTM\rustwtm.exe'"
        ));
        assert!(pinned.contains(
            "$s.SignerCertificate.Subject -cne $own.SignerCertificate.Subject) { exit 9002 }"
        ));

        let launcher = launcher(
            "Start-Elevated",
            Path::new(r"C:\Users\o'neil\AppData\Local\Temp\RustWTM-update\outcome.txt"),
            (0, 36, 0),
            r"Start-Process -FilePath 'C:\Program Files\RustWTM\rustwtm.exe'",
        );
        let args = format!(
            "-EncodedCommand {}",
            encode_command(&launcher.replace("Start-Elevated", &elevate(&pinned)))
        );
        assert!(args.len() < 32_000, "fits a command line: {}", args.len());

        // PowerShell itself reads the scripts, passed the way they will be.
        for script in [&launcher, &unpinned, &pinned] {
            let check = format!(
                "$e = $null; [void][Management.Automation.Language.Parser]::ParseInput({}, \
                 [ref]$null, [ref]$e); exit $e.Count",
                ps_quote(script)
            );
            let status = std::process::Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-EncodedCommand"])
                .arg(encode_command(&check))
                .status()
                .unwrap();
            assert!(status.success(), "{script}");
        }
    }

    #[test]
    fn only_a_signed_build_holds_an_update_to_its_publisher() {
        use Publisher::*;
        let signed = |name: &str, place: &str| Signed {
            name: name.into(),
            subject: format!("CN={name}, O={name}, L={place}, C=EE"),
        };
        let ours = signed("OXOGNET OÜ", "Tallinn");
        let other = signed("Someone Else", "Tallinn");
        // Another company of the same name, validated elsewhere.
        let namesake = signed("OXOGNET OÜ", "Tartu");
        let broken = Unusable("tampered".into());
        // Unsigned builds take what the checksum allows, as before.
        assert_eq!(publisher_policy(&Unsigned, &Unsigned), Ok(false));
        assert_eq!(publisher_policy(&Unsigned, &ours), Ok(false));
        // A signed build takes only its own publisher, and says so.
        assert_eq!(publisher_policy(&ours, &ours.clone()), Ok(true));
        let refused = publisher_policy(&ours, &namesake).unwrap_err();
        assert!(refused.contains("L=Tartu"), "{refused}");
        let refused = publisher_policy(&ours, &other).unwrap_err();
        assert!(
            refused.contains("signed by Someone Else, not by OXOGNET OÜ"),
            "{refused}"
        );
        assert!(refused.contains("release from its page"), "{refused}");
        assert!(
            publisher_policy(&ours, &Unsigned)
                .unwrap_err()
                .contains("not signed")
        );
        // A signature that does not hold, on either side, stops it.
        for current in [&Unsigned, &ours, &broken] {
            assert!(publisher_policy(current, &broken).is_err());
        }
        for update in [&Unsigned, &ours] {
            let refused = publisher_policy(&broken, update).unwrap_err();
            assert!(
                refused.contains("own signature cannot be checked"),
                "{refused}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn this_test_binary_and_a_plain_file_have_no_publisher() {
        // Unsigned, as developer and every release build so far.
        let exe = std::env::current_exe().unwrap();
        assert_eq!(publisher(&exe), Publisher::Unsigned);
        let dir = std::env::temp_dir().join(format!("wtm-publisher-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.msi");
        std::fs::write(&file, b"not an installer").unwrap();
        assert!(matches!(
            publisher(&file),
            Publisher::Unsigned | Publisher::Unusable(_)
        ));
        // A real, empty installer database, as every release so far ships
        // unsigned: it must read as unsigned, or no update would install.
        // Only a file is written; nothing is installed.
        let msi = dir.join("empty.msi");
        let make = format!(
            "$i = New-Object -ComObject WindowsInstaller.Installer; \
             $db = $i.OpenDatabase({}, 3); $db.Commit()",
            ps_quote(&msi.to_string_lossy())
        );
        let status = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-EncodedCommand"])
            .arg(encode_command(&make))
            .status()
            .unwrap();
        assert!(status.success() && msi.exists());
        assert_eq!(publisher(&msi), Publisher::Unsigned);
        assert_eq!(
            publisher_policy(&Publisher::Unsigned, &publisher(&msi)),
            Ok(false)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn the_elevated_publisher_check_runs_and_refuses_cleanly() {
        // The check alone, run in PowerShell on files that exist: nothing
        // is elevated or installed. A refusal must be its own exit code,
        // never an error reading a certificate that is not there.
        let dir = std::env::temp_dir().join(format!("wtm-pubcheck-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let unsigned = dir.join("plain.exe");
        std::fs::write(&unsigned, b"MZ").unwrap();
        let run = |own: &Path, msi: &Path| -> i32 {
            let script = format!(
                "$ErrorActionPreference = 'Stop'\n$msi = {}\n{}exit 0\n",
                ps_quote(&msi.to_string_lossy()),
                publisher_check(own)
            );
            std::process::Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-EncodedCommand"])
                .arg(encode_command(&script))
                .status()
                .unwrap()
                .code()
                .unwrap()
        };
        assert_eq!(run(&unsigned, &unsigned), 9002);
        // Microsoft Edge carries an embedded signature, where installed.
        let edge = PathBuf::from(
            std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".into()),
        )
        .join(r"Microsoft\Edge\Application\msedge.exe");
        if edge.exists() {
            let Publisher::Signed { name, subject } = publisher(&edge) else {
                panic!("Edge is signed inside the file: {:?}", publisher(&edge));
            };
            assert!(subject.contains(&format!("CN={name}")), "{subject}");
            assert_eq!(run(&edge, &edge), 0, "the same publisher passes");
            assert_eq!(run(&edge, &unsigned), 9002);
            assert_eq!(
                run(&unsigned, &edge),
                9002,
                "this build unsigned: no pin to hold"
            );
            let same = publisher(&edge);
            assert_eq!(publisher_policy(&same, &publisher(&edge)), Ok(true));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn outcomes_are_read_strictly() {
        let v = (0, 36, 0);
        for (line, outcome) in [
            ("0.36.0 exit 0", Outcome::Installed),
            ("0.36.0 exit 3010", Outcome::RestartNeeded),
            ("0.36.0 exit 1641", Outcome::RestartNeeded),
            ("0.36.0 exit 1602", Outcome::Cancelled),
            ("0.36.0 exit 1603", Outcome::InstallerFailed(1603)),
            ("0.36.0 exit 1618", Outcome::InstallerFailed(1618)),
            ("0.36.0 exit 1", Outcome::InstallerFailed(1)),
            ("0.36.0 exit 9001", Outcome::ChecksumChanged),
            ("0.36.0 exit 9002", Outcome::WrongPublisher),
            ("0.36.0 cancelled", Outcome::Cancelled),
            ("0.36.0 failed 1223", Outcome::Cancelled),
            ("0.36.0 failed 2", Outcome::NotStarted(2)),
        ] {
            assert_eq!(Outcome::parse(line), Some((v, outcome)), "{line}");
        }
        for line in [
            "",
            "0.36.0",
            "0.36.0 exit",
            "0.36.0 exit x",
            "0.36.0 exit 0 more",
            "0.36.0 installed",
            "<b>0.36.0</b> exit 0",
            "0.36.0 cancelled please",
        ] {
            assert_eq!(Outcome::parse(line), None, "{line:?}");
        }
    }

    #[test]
    fn an_outcome_is_told_once_and_only_while_fresh() {
        let dir = std::env::temp_dir().join(format!("wtm-outcome-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(OUTCOME_FILE);
        std::fs::write(&path, "0.36.0 exit 0\r\n").unwrap();
        assert_eq!(take_outcome(&dir), Some(((0, 36, 0), Outcome::Installed)));
        assert_eq!(take_outcome(&dir), None, "removed once read");
        // Left from a run long ago: removed, not told.
        std::fs::write(&path, "0.36.0 exit 0").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 3600);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert_eq!(take_outcome(&dir), None);
        assert!(!path.exists());
        // Too long to be the launcher's.
        std::fs::write(&path, format!("0.36.0 exit 0{}", " ".repeat(300))).unwrap();
        assert_eq!(take_outcome(&dir), None);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn the_launcher_tells_what_happened_and_always_starts_rustwtm_again() {
        // The steps are stand-ins: nothing is elevated, installed or shown.
        let dir = std::env::temp_dir().join(format!("wtm-launcher-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let outcome = dir.join(OUTCOME_FILE);
        let marker = dir.join("relaunched.txt");
        let relaunch = format!(
            "Set-Content -LiteralPath {} -Value 'relaunched'",
            ps_quote(&marker.to_string_lossy())
        );
        let run = |elevate: &str, outcome: &Path| {
            let _ = std::fs::remove_file(outcome);
            let _ = std::fs::remove_file(&marker);
            let script = launcher(elevate, outcome, (0, 36, 0), &relaunch);
            let status = std::process::Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-EncodedCommand"])
                .arg(encode_command(&script))
                .status()
                .unwrap();
            assert!(status.success(), "{script}");
            assert!(marker.exists(), "started again after: {elevate}");
            std::fs::read_to_string(outcome).ok()
        };
        let exits = |code: i32| format!("[pscustomobject]@{{ ExitCode = {code} }}");
        for (elevate, expected) in [
            (exits(0), Outcome::Installed),
            (exits(1603), Outcome::InstallerFailed(1603)),
            (exits(9002), Outcome::WrongPublisher),
            // Declined at the prompt: Process.Start throws this, the
            // Windows error inside PowerShell's MethodInvocationException.
            (
                "throw [Management.Automation.MethodInvocationException]::new(\
                 'Exception calling \"Start\"', [ComponentModel.Win32Exception]::new(1223))"
                    .to_owned(),
                Outcome::Cancelled,
            ),
            // A real process started the same way (hidden, no prompt): its
            // exit code is read once it has ended.
            (
                "& { $i = New-Object Diagnostics.ProcessStartInfo 'cmd.exe'; \
                 $i.UseShellExecute = $true; $i.WindowStyle = 'Hidden'; \
                 $i.Arguments = '/d /c exit 1603'; \
                 $q = [Diagnostics.Process]::Start($i); $q.WaitForExit(); $q }"
                    .to_owned(),
                Outcome::InstallerFailed(1603),
            ),
            // A real Process.Start failure (no prompt: the file is missing),
            // unwrapped the same way.
            (
                "& { $i = New-Object Diagnostics.ProcessStartInfo \
                 'C:\\no-such-folder-rustwtm\\none.exe'; $i.UseShellExecute = $true; \
                 $q = [Diagnostics.Process]::Start($i); $q.WaitForExit(); $q }"
                    .to_owned(),
                Outcome::NotStarted(2),
            ),
        ] {
            let text = run(&elevate, &outcome).expect("the outcome is written");
            assert_eq!(
                Outcome::parse(text.trim()),
                Some(((0, 36, 0), expected)),
                "{elevate}: {text}"
            );
        }
        // An outcome that cannot be written still starts RustWTM again.
        assert_eq!(
            run(&exits(0), &dir.join("missing").join(OUTCOME_FILE)),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn checksum_lines() {
        let line = format!("{}  RustWTM.msi\n", "ab".repeat(32));
        assert_eq!(parse_sha256(&line), Some([0xab; 32]));
        assert_eq!(parse_sha256("abc  x"), None);
        assert_eq!(parse_sha256(&"zz".repeat(32)), None);
        assert_eq!(parse_sha256(""), None);
    }

    /// Serves `bodies` by path on a local port, then stops.
    fn serve(routes: Vec<(&'static str, u16, Vec<u8>)>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            for _ in 0..routes.len() {
                let Ok((mut s, _)) = listener.accept() else {
                    return;
                };
                let mut buf = [0u8; 2048];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                let path = req.split_whitespace().nth(1).unwrap_or("").to_owned();
                let (status, body) = routes
                    .iter()
                    .find(|r| r.0 == path)
                    .map_or((404, Vec::new()), |r| (r.1, r.2.clone()));
                let _ = write!(
                    s,
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(&body);
            }
        });
        base
    }

    fn release(base: &str) -> Release {
        Release {
            version: (9, 9, 9),
            tag: "v9.9.9".into(),
            page: base.into(),
            msi: Some((format!("{base}/a.msi"), format!("{base}/a.msi.sha256"))),
        }
    }

    #[test]
    fn downloads_only_a_verified_installer() {
        let msi = b"fake installer".to_vec();
        let hash: String = wtm_sys::http::sha256(&msi)
            .unwrap()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let dir = std::env::temp_dir().join(format!("wtm-update-{}", std::process::id()));
        let base = serve(vec![
            (
                "/a.msi.sha256",
                200,
                format!("{hash}  a.msi\n").into_bytes(),
            ),
            ("/a.msi", 200, msi.clone()),
        ]);
        let got = download(&release(&base), &dir).unwrap();
        assert_eq!(got.path, dir.join("a.msi"));
        assert_eq!(got.sha256, wtm_sys::http::sha256(&msi).unwrap());
        assert_eq!(std::fs::read(&got.path).unwrap(), msi);
        std::fs::remove_file(&got.path).unwrap();

        // A tampered download is refused and not saved.
        let base = serve(vec![
            (
                "/a.msi.sha256",
                200,
                format!("{}  a.msi\n", "00".repeat(32)).into_bytes(),
            ),
            ("/a.msi", 200, msi.clone()),
        ]);
        let err = download(&release(&base), &dir).unwrap_err();
        assert!(err.starts_with("the download does not match"), "{err}");
        assert!(!dir.join("a.msi").exists());

        let base = serve(vec![("/a.msi.sha256", 200, b"garbage".to_vec())]);
        assert_eq!(
            download(&release(&base), &dir).unwrap_err(),
            "the published checksum is unreadable"
        );
        let base = serve(vec![("/a.msi.sha256", 404, Vec::new())]);
        assert!(
            download(&release(&base), &dir)
                .unwrap_err()
                .ends_with("answered HTTP 404")
        );
        let none = Release {
            msi: None,
            ..release(&base)
        };
        assert_eq!(
            download(&none, &dir).unwrap_err(),
            "this release has no installer"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Talks to GitHub; run by hand with `--ignored`.
    #[test]
    #[ignore]
    fn the_real_latest_release_has_a_verified_installer() {
        let r = latest().unwrap();
        assert!(r.version >= (0, 7, 0), "{r:?}");
        let dir = std::env::temp_dir().join(format!("wtm-update-real-{}", std::process::id()));
        let path = download(&r, &dir).unwrap().path;
        assert!(std::fs::metadata(&path).unwrap().len() > 1_000_000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn checks_the_latest_release() {
        let base = serve(vec![("/latest", 200, JSON.as_bytes().to_vec())]);
        assert_eq!(
            latest_from(&format!("{base}/latest")).unwrap().version,
            (0, 8, 1)
        );
        assert!(
            latest_from("http://127.0.0.1:1/")
                .unwrap_err()
                .starts_with("cannot reach")
        );
    }

    #[cfg(windows)]
    #[test]
    fn knows_whether_it_runs_from_the_installed_folder() {
        let dir = Path::new(r"C:\Program Files\RustWTM\");
        assert!(same_dir(
            dir,
            Path::new(r"c:\program files\rustwtm\rustwtm.exe")
        ));
        assert!(!same_dir(
            dir,
            Path::new(r"D:\src\target\debug\rustwtm.exe")
        ));
        // A test binary is never the installed one.
        assert!(!installed_by_msi(&std::env::current_exe().unwrap()));
    }

    #[test]
    fn every_kind_of_single_quote_is_doubled() {
        assert_eq!(ps_quote("O'Brien"), "'O''Brien'");
        assert_eq!(
            ps_quote("C:\\Users\\O\u{2019}Brien \u{2018}\u{201A}\u{201B}"),
            "'C:\\Users\\O\u{2019}\u{2019}Brien \u{2018}\u{2018}\u{201A}\u{201A}\u{201B}\u{201B}'"
        );
        assert_eq!(ps_quote("\u{201C}x\u{201D}"), "'\u{201C}x\u{201D}'");
    }
}
