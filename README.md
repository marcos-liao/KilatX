# KilatX

**A free and open-source download manager for Windows.** Native Rust with a Slint user
interface (no web view, ~8 MB), for Windows 10 and 11 on x64 and ARM64.

*Kilat* is Indonesian for lightning.

- Free for everyone, no ads, no accounts, no tracking ([privacy policy](app/docs/PRIVACY.txt))
- License: [GNU GPL v3](LICENSE)

## Download

Get the latest version from [Releases](https://github.com/marcos-liao/KilatX/releases/latest):

| File | For |
|---|---|
| `KilatX-Setup-<version>-x64.exe` | Most PCs (Intel/AMD). Installs to Program Files |
| `KilatX-Setup-<version>-arm64.exe` | Windows on ARM (Snapdragon) |
| `KilatX-<version>-portable-win64.zip` / `-arm64.zip` | Portable: unzip and run, settings stay in the folder |
| `KilatX-browser-extension-<version>.zip` | Browser extension, for manual install (see below) |
| `KilatX-<version>-x64.msi` / `-arm64.msi` | For IT departments: installs for all users (see [ENTERPRISE.md](ENTERPRISE.md)) |

### "Windows protected your PC"?

KilatX isn't code-signed yet, so Windows SmartScreen may warn about files downloaded from the
internet, and setup shows "Unknown publisher". KilatX is safe to run: its full source code is
in this repository, and every release lists the SHA-256 checksum of each file. Any one of these
lets it run:

- **On the warning:** click **More info**, then **Run anyway**.
- **Before starting it:** right-click the downloaded file › **Properties** › tick **Unblock** › **OK**.
- **PowerShell:** `Unblock-File .\KilatX-Setup-0.5.0-x64.exe` (use the name of the file you downloaded).

For the portable version, unblock the **zip** before extracting it; otherwise every extracted file
keeps the warning.

To check that a file is exactly the published one, compare its checksum with the one on the release page:

```
Get-FileHash .\KilatX-Setup-0.5.0-x64.exe
```

## Features

**Downloading**
- Multi-connection downloads with dynamic segmentation; **smart connections**: one connection when it
  already fills your line, more when it doesn't (far servers, per-connection limits)
- Pause and resume, change the address of an expired link, retries that keep going while progress is made
- Stalled connections taken over by idle ones; `429`/`Retry-After` respected
- Automatic categories (`Downloads\<category>`), search, filters, detail panel with a segment map
- Queues, scheduler (start/stop times, days, shut down when done), speed limits (global and per download)
- Portable mode (`portable.txt` or a `data` folder next to `KilatX.exe`)
- Command line: `KilatX.exe /d URL [/d URL2] [/p folder] [/a] [/tray]`

**Desktop**
- Keeps running in the notification area; tray menu; Windows notifications when a download finishes or fails
- One KilatX per Windows user: launching it again brings the window back and passes the links over
- Clipboard watcher: copying a link to a file opens the Add dialog
- Optional start with Windows (straight to the tray)

**Options**
- File types and per-category folders, download and temporary folders (also on another drive)
- If the file exists: rename / overwrite / skip / ask; duplicate links: ask / add again / skip
- User agent presets, proxy (Windows / none / HTTP / SOCKS5, with login and bypass list),
  site logins (Basic and Digest, passwords encrypted with Windows DPAPI)

**Browser integration** (Chrome, Edge, Brave, Vivaldi, Chromium)
- Downloads go to KilatX with the browser's cookies, referrer and user agent, so logins and
  hotlink-protected files work
- Safety net: KilatX checks the link first; if it can't fetch it (form POST, one-time link, login wall),
  the browser keeps the download
- "Download with KilatX" in the right-click menu; on/off switch in the toolbar popup
- Until the extension is in the Chrome Web Store: Settings › Browser › Install, or unzip the extension
  zip, open `chrome://extensions`, turn on Developer mode, "Load unpacked"

**Installer and updates**
- Installs to Program Files (one administrator prompt), so only administrators can change KilatX.exe;
  KilatX itself always runs as the signed-in user
- Updates close KilatX properly and keep settings and downloads; no restart needed
- Update check once a day (can be turned off): KilatX asks "Update now / Later / Skip this version".
  The update information is Ed25519-signed and the setup's SHA-256 is checked before it runs
- Silent install: `KilatX-Setup.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART [/DIR="folder"] [/TASKS=desktopicon]`;
  silent uninstall: `unins000.exe /VERYSILENT [/removedata]`
- Hardened: DLLs load only from System32; C runtime built in (no Visual C++ Redistributable needed)

## Build

Requirements: Rust (rustup, plus the `aarch64-pc-windows-msvc` target), Visual Studio C++ Build Tools
(with the ARM64 component), Python 3 (tests), Inno Setup 6 (`winget install JRSoftware.InnoSetup`).

```
cd app
cargo build --release                       # target\release\KilatX.exe
cd ..
powershell -File setup\build.ps1            # setups, portable zips, extension zip -> dist\<version>\
```

| Folder | Contents |
|---|---|
| `app/src/` | KilatX (Rust): `engine.rs` downloads, `main.rs` window, `browser.rs` extension host, `update.rs` updates |
| `app/ui/` | User interface (Slint) |
| `app/extension/` | Browser extension (Chromium, Manifest V3) |
| `app/testkit/` | Test servers and end-to-end test scripts |
| `setup/` | Installer (Inno Setup) and release build script |

## Tests

See [TESTING.md](TESTING.md): simulated download scenarios (ranges, redirects, cookies, logins, proxies,
broken servers, files over 4 GB), real browsers with the extension, installer and updater end to end,
and desktop behaviour.

## Releasing (maintainer)

1. Raise `version` in `app/Cargo.toml` (and `app/extension/manifest.json`), run `setup\build.ps1`.
2. Create a GitHub release `v<version>` with the files from `dist\<version>\`, and paste
   `dist\<version>\SHA256SUMS.txt` into the release notes.
3. Make the signed `update.json` with `sign-update` (see `app/update-signing/README.txt`; the signing key is
   kept offline, never in this repository) and attach it to the same release. KilatX reads
   `releases/latest/download/update.json`.

## License

Copyright © 2026 marcos-liao and KilatX contributors.

KilatX is free software: you can redistribute it and/or modify it under the terms of the
GNU General Public License as published by the Free Software Foundation, version 3.
It is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; see [LICENSE](LICENSE).

Third-party components and their licenses: [app/docs/THIRD-PARTY-NOTICES.txt](app/docs/THIRD-PARTY-NOTICES.txt).
The user interface uses [Slint](https://slint.dev) under its GPL-3.0 option.
