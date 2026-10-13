# Test matrix

Every download model KilatX has to handle, with an automated test for each, plus a
real-server check. Last full run: **13 Oct 2026, v0.5.0: all suites passed**
(26 download + 22 options + 22 browser scenarios, engine and unit tests, 2 detection tests, 3 log/watchdog tests, 13 update tests: 105/105),
16/16 desktop end-to-end, 25/25 browser host (real process), 57/57 real browsers,
91/91 installer, MSI, company browser policy and updater (two UAC prompts), 5/5 repaint.

The release-build browser host test only works while no other KilatX runs (release builds
ignore `KILATX_INSTANCE`, so they hand over to a KilatX the user is running).

## How to run

```
cd app
python testkit/scenario_server.py 8766 8767
python testkit/range_server.py <folder with test.bin and Annual-Report-2026.pdf> 8765 2000000
set KILAT_SCEN=http://127.0.0.1:8766
set KILAT_SCEN_PROXY=http://127.0.0.1:8767
set KILAT_TEST_BASE=http://127.0.0.1:8765
set KILAT_TEST_SRC=<same folder>
cargo test -- --ignored --test-threads=1 --skip perf_connections
```

Desktop end-to-end (debug build; `KILATX_INSTANCE` gives the test its own single-instance
name so a KilatX the user is running is never touched. Release builds ignore it):

```
set KILATX_INSTANCE=-desktop-test
powershell -File testkit\desktop_test.ps1
```

Real servers (downloads ~600 MB, compares every file with curl and official checksums):

```
powershell -File testkit\realworld.ps1 -Runs 3
```

`scenario_server.py` generates files on the fly (byte *i* = *i* mod 251), so every test
checks that each byte landed at the right offset, not just the file size. `?r=` throttles
each connection, which forces Kilat to open several.

## Simulated scenarios

| # | Model | Real-world example | Expected behaviour | Result |
|---|---|---|---|---|
| 01 | Static file with Range | Mirrors, CDNs | Several connections when one can't fill the line, byte-exact | ✅ |
| 02 | No Range support | Old servers, scripts | One connection | ✅ |
| 03 | Speed limit per connection | Many mirrors / file hosts | All connections open: 3.1 s vs 20 s with one | ✅ |
| 04 | Unknown size (chunked) | Generated downloads | Size learned at the end | ✅ |
| 05 | Redirect chain | SourceForge, GitHub → S3 | Follows redirects | ✅ |
| 06 | Expiring signed link | S3 / CDN signed URLs | Clear "link expired"; **Change address** resumes | ✅ (fixed) |
| 07 | Name from Content-Disposition | `download.php?id=7` | UTF-8 name `Laporan Akhir – 2026.pdf` | ✅ |
| 08 | Server limits connections (503) | N connections per IP | Drops extra connections quietly | ✅ |
| 09 | Cookie / login required | Member areas, Google Drive | Clear 403; works with cookies (extension, stage 4) | ✅ engine |
| 10 | HTTP Basic auth | Internal servers | Clear 401; `user:pass@host` works | ✅ |
| 11 | Proxy | Office / campus | Goes through the proxy | ✅ |
| 12 | Connection dropped repeatedly | Flaky Wi-Fi | Retries while it makes progress | ✅ (fixed) |
| 13 | Empty file | Placeholders | Server's 416 handled | ✅ (fixed) |
| 14 | Server stops honouring ranges | Misconfigured servers | Restarts with one connection | ✅ (fixed) |
| 15 | HTTP 404 / 500 | Deleted files | Plain-language error | ✅ (fixed) |
| 16 | Tiny file | Icons, small installers | Works | ✅ |
| 17 | Same link twice | Double click | `name (1).ext` | ✅ |
| 18 | Folder can't be created | Bad path | Clear error | ✅ |
| 19 | Larger than 4 GB | ISOs, games | 4.6 GB byte-exact in 11 s locally | ✅ |
| 20 | Site serves browsers an HTML page | SourceForge | Gets the real file (honest `KilatDM/x` user agent) | ✅ (fixed) |
| 21 | Web page instead of the file | Login walls, interstitials | Error instead of saving HTML as `.exe` | ✅ (new) |
| 22 | One connection stalls (no data, no close) | Dead routes | Idle connections take it over: 3.6 s instead of ~30 s | ✅ (fixed) |
| 23 | 429 with Retry-After | Busy / rate-limited servers | Waits as asked and continues | ✅ (fixed) |
| 24 | Same name in another folder | Two categories | Keeps its name | ✅ (fixed) |
| 25 | One connection already fills the line | Nearby CDN on a 100 Mbit/s line | Smart connections stay at one | ✅ (new) |
| 26 | First request can't connect | Slow mirror, flaky Wi-Fi | Retried up to 3 times | ✅ (fixed) |

## Real servers (curl = one connection, like a browser)

All files matched curl byte for byte; Ubuntu and Node.js also matched the official SHA-256.
Line: ~100 Mbit/s, Indonesia.

| Server | Kind | Kilat | curl | |
|---|---|---|---|---|
| Citrahost 🇮🇩 (371 MB) | Local mirror | 29.3 s | 29.3 s | equal (line full) |
| Python (Fastly CDN), line speed known | Nearby CDN | 2.15–2.39 s | 2.33–3.18 s | equal or faster |
| Python (Fastly CDN), first download | Nearby CDN | 4.0 s | 2.3–3.2 s | slower (opens 8 connections before it knows the line) |
| Tele2 🇸🇪 (HTTP) | Far server | 3.7 s | 15.7 s | **4.2× faster** |
| SourceForge (7-Zip) | Mirror redirect | 1.0 s | 3.2 s | 3.2× faster |
| GitHub Releases | Signed redirect | 0.5–0.6 s | 0.5–0.9 s | equal |
| GitHub archive | No size | 0.6–1.1 s | 0.6 s | equal |
| Node.js / VC++ | CDN / redirect | correct | correct | timing depends on whether the line speed is known |

Hetzner speed-test servers throttle an IP after repeated 100 MB downloads (even curl slowed to 15–31 s
and once failed), so they are no longer used for timing.

## Smart connections (why not always 8?)

On a nearby CDN that one connection already saturates, 8 connections compete and were ~12% slower
(median 2.73 s vs 2.42 s over 5 rounds). On far or per-connection-limited servers, 8 connections are
4–5× faster. Kilat measures one connection between 0.3 s and 0.7 s; if it reaches 60% of the line speed
remembered from earlier downloads, it stays with it, otherwise it opens all connections at once.

## Desktop end-to-end (`testkit\desktop_test.ps1`, v0.4.0: 16/16)

Runs a portable copy in `%TEMP%` (never touches the user's data) against `scenario_server.py`.
KilatX writes on-screen events to `KILATX_EVENT_LOG` when it is set, so dialogs and notifications
can be checked without looking at the screen.

| Check | Result |
|---|---|
| Second launch exits, its link is downloaded by the running KilatX | ✅ |
| Closing the window keeps KilatX in the tray, one-time hint notification | ✅ |
| Second launch without arguments brings the hidden window back | ✅ |
| Notification when a download finishes / fails (also in Action Center) | ✅ (fixed: downloads finishing within 0.5 s were missed) |
| Copied file link opens the Add dialog; plain text ignored; nothing downloads by itself | ✅ |
| `/tray` starts without a window | ✅ |
| GPU rendering on but the last start never showed its window: back to software, remembered | ✅ |
| Ten launches at once leave exactly one KilatX; idle in the tray uses no CPU (33 MB) | ✅ (new) |
| Close to tray → second launch, 5 rounds: window drawn every time (not blank) | ✅ (fixed: white window with GPU rendering) |

Single instance is strict: one KilatX per Windows user, whichever copy or folder it runs from
(named mutex, released by Windows even after a crash; a pipe hands links to the running copy).
If the running copy doesn't answer within 3 s, the new launch says so and quits instead of
starting a second instance. Software rendering is the default; GPU rendering is an option.

Unit tests (`cargo test desktop:: -- --ignored`): start with Windows registered and removed again ·
second claim refused · silent and garbage pipe clients don't block later launches · 20 hand-overs in a row.

ARM64 build: compiles and links (PE machine type verified as ARM64); **not run yet**, no ARM device available.

## Repainting (`testkit\repaint_test.ps1`: 5/5)

Windows sometimes throws away what a window shows (screen off or sleep, lock, Remote Desktop,
display changes) and asks for all of it again. The software renderer copies only what changed,
so KilatX 0.4 stayed partly blank or showed the windows behind it (reported 11 Oct 2026).
The test paints the window magenta, invalidates it the way Windows does, and counts what is left.

| Check | 0.4 / before the fix | 0.5 |
|---|---|---|
| Window drawn at start | ✅ | ✅ |
| Windows asks for a repaint: everything drawn again | ❌ 100% left | ✅ 0% |
| Shown again from the tray: fully drawn | ✅ | ✅ |
| Second repaint request also handled | ❌ 100% left | ✅ 0% |
| No repaint loop (idle CPU with the window open) | ✅ | ✅ |

Clicks still reach the buttons under the full-window repaint layer (checked: "Add URL" opens the dialog).

## Log (v0.5.1)

| Check | Result |
|---|---|
| Addresses lose query strings and `user:password@`; no token in the log of a real download with `?token=…` | ✅ |
| Log rotation keeps kilatx.log + 3 older parts of 1 MB | ✅ |
| Watchdog: a UI thread without heartbeat for more than 5 s is written down with what it was busy with, and again when it answers | ✅ |
| Detailed level: first answer, connections opened, segment splits, file saved; Normal level: start, exit, every download state change with size | ✅ |

## Engine tests (`engine::tests`, local range server)

Multi-connection byte-exact · pause/resume byte-exact · no-range fallback · global speed limit
(4 MB at 1 MB/s → 3.87 s) · per-download limit (2 MB/s → 1.96 s) · queue order & one-at-a-time ·
scheduler start/stop times · client never negotiates HTTP/2.

## Options scenarios 27–48 (`options_tests`)

File types and per-category folders · base and temporary folder (also across drives) ·
existing file: rename / overwrite / skip / ask · duplicate link: ask / add again / skip ·
user agent presets and custom · proxy: system / none / HTTP / HTTP with login / SOCKS5 /
SOCKS5 with login / bypass list / HTTPS through an HTTP proxy (real GitHub) / unreachable proxy ·
site logins: Basic and Digest (RFC 2617), most specific pattern wins, never sent to other sites ·
passwords stored with DPAPI.

## Browser integration (stage 4)

How it works: the KilatX extension (Chromium, Manifest V3) asks KilatX through native
messaging before the browser saves a download. KilatX checks the link itself with the
browser's cookies, referrer and user agent; only if it can really fetch the file is the
browser's download cancelled. Otherwise the browser keeps it, so nothing is lost.

Scenarios 49–72 (`browser_tests`, `cargo test browser -- --include-ignored`; 71–72: another download
manager connected to the browsers is recognised by its own description, without any product names in KilatX):

| # | Scenario | Result |
|---|---|---|
| 49 | `hello`: version, running, integration on/off | ✅ |
| 50 | Broken messages (garbage, unknown type, 64 MB length, cut off, closed pipe): answered or ended, never a crash or hang | ✅ |
| 51 | Header values with line breaks (header injection) are dropped | ✅ |
| 52 | 20 KB of cookies and a UTF-8 name survive the hand-over; `file://` links are dropped | ✅ |
| 53 | Host launch detected only for `chrome-extension://` | ✅ |
| 54 | Integration off, `.html`, excepted site, `data:` link: left to the browser without any request | ✅ |
| 55 | Normal file taken with cookies, referrer, user agent and the browser's name | ✅ |
| 56 | 404, 500, HTML instead of the file, POST-only (405), login wall, hotlink protection: left to the browser | ✅ |
| 57 | Browser cookies / referrer open login walls and hotlink protection | ✅ |
| 58 | Original link fails but the redirect target works: target used; working original preferred | ✅ |
| 59 | KilatX can't start: left to the browser | ✅ |
| 60 | Context menu "Download with KilatX": sent without filters or checks | ✅ |
| 61 | Unreachable server: the check gives up in time | ✅ |
| 62 | Download with browser cookies over 8 connections, browser's name kept, cookies dropped when done | ✅ |
| 63 | Cookies saved encrypted (DPAPI), resume after a restart still sends them | ✅ |
| 64 | Server's Content-Disposition doesn't replace the browser's name; forbidden characters cleaned | ✅ |
| 65 | Registration in HKCU for 5 browsers, manifest points at this exe, moved copy detected and fixed, removed again | ✅ |
| 66 | Extension ID matches the key in manifest.json; extension version follows KilatX | ✅ |
| 67 | Login site (Basic), the browser logged in itself: KilatX takes it with the saved Site login and downloads over 8 connections | ✅ |
| 68 | Login site without a saved login: left to the browser, reason points to Site logins | ✅ |
| 69 | Login site (Digest) with a saved login: taken | ✅ |
| 70 | Wrong saved password: left to the browser; a login saved for another site is never sent | ✅ |

Real process (`testkit\browser_host_test.py`, KilatX.exe started exactly like a browser starts it): 25/25.
KilatX starts in the tray when needed and keeps running after the host exits · Add dialog and window ·
left downloads never reach KilatX · no-dialog mode downloads byte-exact with the browser's Referer and name ·
5 simultaneous hand-overs → still one KilatX · `show` · a host that never gets a message exits by itself.
(Found and fixed: KilatX started by the host inherited the browser's pipe, so the browser would have waited forever.)

Real browsers (`testkit\browser_e2e.ps1`, extension loaded with CDP `Extensions.loadUnpacked` into a
temporary profile): **Chrome 155 19/19, Edge 155 19/19, Brave 154 19/19**.

| Check (each browser) | Result |
|---|---|
| Extension loads with the fixed ID; popup says "Connected to KilatX" (real native messaging) | ✅ |
| Zip: handed to KilatX (started in the tray by the browser), browser's own download cancelled and removed, KilatX finishes it | ✅ |
| `.bin` (not a captured type): stays in the browser and completes; never reaches KilatX | ✅ |
| Members-only zip: finished by KilatX with the browser's cookie | ✅ |
| Redirect chain: taken with the original link | ✅ |
| Content-Disposition name (UTF-8 "Laporan Akhir – 2026.pdf") kept | ✅ |
| Blob download made by the page: stays in the browser | ✅ |
| Capture switched off in the popup: browser downloads, KilatX not involved | ✅ |
| Login site the browser logged in to, with a saved Site login: KilatX logs in and finishes | ✅ |
| Same site, folder without a saved login: the browser downloads it | ✅ |
| KilatX unregistered (all 5 keys, as "Remove traces"): host unreachable, the browser downloads normally | ✅ |

Found and fixed along the way: `state.json` with a byte-order mark (Notepad, PowerShell) was ignored
and reset to defaults; Edge and Brave also read another browser's registry key, so unregistering has to
remove all five (KilatX already did; the test now does too).

Note: some other download managers' browser integration, when installed, intercepts plain-HTTP
downloads inside the browser process before any extension sees them (the server sees an answer
like "204 Intercepted"). The browser test therefore uses HTTPS (`scenario_server.py <port> <proxy> <https port>`).
KilatX warns in Settings › Browser when such an integration is present.

## Installer, MSI and updates (v0.5, `testkit\installer_test.ps1`: 91/91)

Inno Setup test build of the debug exe in test mode: product "KilatX-Test" (own AppId, Program Files
folder, shortcuts, Installed-apps entry), own single-instance name, `KILATX_DATA_DIR` for data, a dead
update address, so a real install and the user's data are never touched. Part 1 runs as the signed-in
user (UAC prompt), part 2 (`installer_test_admin.ps1`) as administrator (second UAC prompt).
`installer_test_reset.ps1` (as administrator) removes what an interrupted run left.

| Check | Result |
|---|---|
| Install from a normal account: UAC, Program Files, KilatX started again **without** administrator rights | ✅ |
| Per-user setup (browser connection, notification name) runs as the signed-in user | ✅ |
| Uninstall while KilatX runs: closed first; folder, Installed-apps entry, Start menu shortcut, browser connection removed; settings kept | ✅ |
| Migration from 0.4 (per-user, running, start with Windows on): closed, replaced, 0.4 entry and files removed, downloads list kept, start with Windows and browser connection now point at Program Files | ✅ |
| Machine-wide Installed-apps entry, Start menu shortcut for all users with AppUserModelID, no desktop shortcut unless chosen | ✅ |
| Program Files folder and exe: only administrators/Windows may change them | ✅ |
| Update while KilatX runs (`/launch`): asked to quit, exe replaced, version 0.5.1, started again, downloads kept | ✅ |
| Update while a browser's connection process holds KilatX.exe: ended, replaced at once, nothing pending for restart | ✅ |
| Updater: forged `update.json` (changed after signing) refused; nothing installed | ✅ |
| Updater: download that doesn't match the signed SHA-256 refused and deleted; nothing installed | ✅ |
| Updater: offers 0.5.1, downloads, verifies, runs setup, 0.5.1 installed, KilatX running again | ✅ |
| Refused: portable folder; folder with other files (files and permissions untouched) | ✅ |
| Custom folder (spaces, non-ASCII) outside Program Files: protected like Program Files, users can still run it; desktop shortcut on request | ✅ |
| Planted DLLs (16 fake system DLLs) next to the exe: KilatX 0.4 fails to start (control), 0.5 starts and runs normally | ✅ |
| Uninstall `/removedata`: data folder, install folder, desktop shortcut, start with Windows removed; nothing left in Installed apps | ✅ |
| Machine-wide browser registration (all 5 browsers, both registry views, manifest next to KilatX.exe); removed on uninstall | ✅ |
| Company policy `NativeMessagingUserLevelHosts = 0`, real Chrome and Edge run by the signed-in user: control without machine-wide registration can't reach KilatX; with it, Chrome 17/17 and Edge 17/17 browser checks (handover, cookies, logins, redirects, names, capture off, files left to the browser). Policies put back as they were | ✅ |
| MSI installed by the SYSTEM account (as Intune/SCCM/GPO do): files, Start menu shortcut with AppUserModelID, one Installed-apps entry, machine-wide browser registration, protected folder | ✅ |
| MSI: a user starts KilatX, which connects that user's browsers | ✅ |
| MSI upgrade 0.5.0 → 0.5.1 while KilatX and a browser connection run: exit 0 (no restart), exe replaced, one entry, downloads kept | ✅ |
| MSI uninstall (SYSTEM): folder, shortcut, entry, machine-wide registration removed; users' settings kept | ✅ |
| KilatX Setup refuses to install over the MSI, and the MSI over KilatX Setup's installation | ✅ |

Unit tests `update::tests` (13): version compare, signature accepted / changed field / added file / other key /
missing signature / invalid version refused, skip-version and once-a-day rules, https only, checksum check
on download (bad checksum, 404, odd version string), server errors (500, too large, unreachable), setup not
started when the file changed after download.

Found along the way: icacls `/T` left the files in a custom folder with no permissions at all (fixed:
the folder's protection is inherited instead); KilatX 0.4 needed the Visual C++ Redistributable
(fixed: C runtime built in).

The browsers are driven by the signed-in user's part of the test while the administrator part waits
(browsers can't be driven as administrator). Found along the way: the first version of this check ran
the browsers as administrator, where they could not be automated, and a driver message reported them
as "skipped" with ok status; such a result now counts as failed.

`testkit\dll_check.ps1`: KilatX loads 46 DLLs, all from Windows (`C:\Windows`), none from any other program.

Not automated: the setup wizard pages and clicking "No" in the UAC prompt (one manual check).

## Not covered

| Model | Status |
|---|---|
| FTP | Skipped by decision (10 Oct 2026) |
| Video streaming (HLS/DASH, `.m3u8`) | Skipped for now by decision (10 Oct 2026) |
| Google Drive / sites behind a login | Needs the browser extension (stage 4) |
