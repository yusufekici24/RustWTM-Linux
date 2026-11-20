# Install and verify RustWTM

## MSI

Download the versioned `RustWTM-VERSION-x64.msi` and its `.sha256` file from
[GitHub Releases](https://github.com/RustWTM/RustWTM/releases/latest). The MSI
installs the GUI, CLI and governor service in Program Files, adds the command
to PATH and creates a Start Menu shortcut. Windows asks for elevation.

From the download directory, set `$version` to the chosen release number.
Verify before running the installer:

```powershell
$version = Read-Host 'Release version (without v)'
$msi = "RustWTM-$version-x64.msi"
$expected = ((Get-Content -LiteralPath "$msi.sha256" -Raw).Trim() -split '\s+')[0]
$actual = (Get-FileHash -LiteralPath $msi -Algorithm SHA256).Hash
if ($actual -ne $expected) { throw 'MSI checksum mismatch' }

gh attestation verify $msi --repo RustWTM/RustWTM `
  --signer-workflow RustWTM/RustWTM/.github/workflows/release.yml `
  --source-ref "refs/tags/v$version" --deny-self-hosted-runners
if ($LASTEXITCODE -ne 0) { throw 'MSI provenance verification failed' }
```

The published SHA-256 checks that the downloaded bytes match the release
checksum. The attestation additionally checks that those bytes came from the
expected workflow and tag. This step uses an authenticated GitHub CLI.
[GitHub CLI verification options](https://cli.github.com/manual/gh_attestation_verify) ·
[Historical 0.28.0 verification](release-provenance.md)

After successful verification, open the MSI or run `msiexec /i $msi`. To
upgrade the GUI, CLI and service together, close the GUI and run the new MSI.
Uninstall from **Settings → Apps**. The installer preserves configuration and
history in `%ProgramData%\RustWTM` across upgrades and removal.

## Portable ZIP and install script

The release also includes `rustwtm-x86_64-pc-windows-msvc.zip`, its checksum and
an attestation. Substitute the ZIP name for `$msi` in the verification commands
above. Extract it and run `rustwtm.exe` for portable use.

To choose install options, use the local `install.ps1` from the verified ZIP
or a reviewed checkout. The script downloads the chosen tag's ZIP and checks
it against the published SHA-256. To install the exact MSI bytes already
verified above, launch that MSI directly.

```powershell
.\install.ps1 -Version "v$version"
.\install.ps1 -DownloadOnly -Version "v$version" -InstallDir D:\Tools\RustWTM
.\install.ps1 -FromSource .
```

The first and third commands require an elevated PowerShell. For a
script-installed copy, run `uninstall.ps1` from its installed directory.
Add `-RemoveData` only to remove saved configuration and history as well.

`-DownloadOnly` defaults to `%LOCALAPPDATA%\RustWTM\portable`. Close programs
running from that directory before updating the portable files. Use a separate
directory for each portable copy.

A script upgrade must target the directory of the registered RustWTM service.
`-NoService` is for a machine without a registered RustWTM service. Use
`-DownloadOnly` for a separate copy alongside an existing installation.

## Build from source

```powershell
git clone https://github.com/RustWTM/RustWTM
cd RustWTM
cargo build --locked --release --workspace
./scripts/build-hardware.ps1 -OutputDir target/release/hardware
```

The hardware helper is optional. See [hardware setup](hardware.md) for its
.NET build requirement and [Telegram](telegram.md) for custom build profiles.
