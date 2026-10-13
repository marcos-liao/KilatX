# KilatX in companies

Notes for IT departments that deploy KilatX to managed PCs.

## Installing

Use the MSI from the release (`KilatX-<version>-x64.msi`, `KilatX-<version>-arm64.msi`):

```
msiexec /i KilatX-0.5.0-x64.msi /qn
msiexec /x KilatX-0.5.0-x64.msi /qn
```

- Installs for all users to `C:\Program Files\KilatX` (one entry in Settings › Apps).
- Works from Group Policy Software Installation, Intune (line-of-business or Win32 app),
  Configuration Manager and similar tools, which install as the SYSTEM account.
- A newer MSI upgrades an older one in place. Running KilatX windows and browser connections
  are closed first, so no restart is needed. Users' settings and download lists stay.
- The MSI and the regular `KilatX-Setup-<version>.exe` don't mix on one PC: each refuses to
  install while the other's installation is there.
- `KilatX-Setup-<version>.exe` can also be deployed silently:
  `KilatX-Setup-0.5.0-x64.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART`

Everything per user (settings, download list, start with Windows, notifications) is kept in
`%APPDATA%\KilatX` and set up by KilatX itself the first time each user starts it.

## Application control (AppLocker, WDAC)

KilatX runs from `C:\Program Files\KilatX\KilatX.exe` only; it starts no other programs from
user-writable folders. The portable version (run from Downloads or a USB drive) is usually blocked
by default AppLocker rules; use the MSI on managed PCs.

## Browsers

The installer registers KilatX for the browsers of all users (machine-wide native messaging host
`app.kilatx.host` under `HKLM\SOFTWARE\<browser>\NativeMessagingHosts`, both registry views), so it
also works where per-user hosts are turned off by policy (`NativeMessagingUserLevelHosts = 0`).

The extension can be installed for all users by policy once it is in the stores:

| Browser | Policy | Value |
|---|---|---|
| Chrome | `ExtensionInstallForcelist` | `<Chrome Web Store ID>;https://clients2.google.com/service/update2/crx` |
| Edge | `ExtensionInstallForcelist` | `<Edge Add-ons ID>;https://edge.microsoft.com/extensionwebstorebase/v1/crx` |

(The store IDs will be listed in the README when the extension is published.)

## Network

KilatX connects only to the servers of the files users download, the configured proxy (Windows
proxy settings by default), and GitHub once a day to look for a new version. The update check
can be turned off per user in Settings › About; updates are only installed when a user agrees,
and need administrator rights. On managed PCs, update with a newer MSI instead.
