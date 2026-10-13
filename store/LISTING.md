# KilatX extension: store listing

Everything to fill in on the Chrome Web Store developer dashboard and Microsoft Partner Center
(Edge Add-ons). Package: `dist\<version>\extension\KilatX-extension-<version>-store.zip`
(made by `setup\build.ps1`; it has no `key`, the stores give the extension its own ID).
Pictures: `store\images\` (made by `store\make-images.ps1`).

## Product details

**Name:** KilatX

**Summary** (132 characters max):
> Sends your browser's downloads to the free KilatX app for Windows: many connections, pause and resume, queues.

**Category:** Productivity (Chrome: "Tools" also fits)

**Language:** English

**Description:**

> KilatX is a free and open-source download program for Windows. This extension connects your
> browser to it: when the browser starts a download, KilatX takes it over and downloads it with
> several connections at once, with pause and resume, queues and a scheduler.
>
> - Works with sites that need a login: the download uses the browser's cookies for that site.
> - Safety net: if KilatX can't fetch a file (a form, a one-time link), the browser keeps the
>   download, so nothing is lost.
> - "Download with KilatX" in the right-click menu for links.
> - Turn it on or off from the toolbar button.
>
> Needs KilatX for Windows, free from https://github.com/marcos-liao/KilatX/releases
> Without it, the browser simply downloads as usual.
>
> No accounts, no ads, nothing collected: the extension only talks to KilatX on your own computer.
> Source code (GNU GPL v3): https://github.com/marcos-liao/KilatX

**Website / support:** https://github.com/marcos-liao/KilatX — issues: https://github.com/marcos-liao/KilatX/issues

**Pictures**

| Field | File |
|---|---|
| Store icon (128×128) | `images/icon-128.png` |
| Screenshots (1280×800) | `images/screenshot-1-downloads.png`, `images/screenshot-2-settings.png` |
| Small promo tile (440×280) | `images/promo-small.png` |

## Privacy practices

**Single purpose:**
> Hands the downloads that the browser starts over to the KilatX download program installed on
> the same computer.

**Permission justifications:**

| Permission | Why |
|---|---|
| `downloads` | To see a download the browser starts, offer it to KilatX, and cancel the browser's copy once KilatX has taken it over. |
| `nativeMessaging` | To talk to the KilatX program on the same computer (the only place the extension sends anything). |
| `cookies` | When a download starts, the cookies for that download's address are passed to KilatX so that downloads from sites the user is logged in to work. Only for the download's own address, only at that moment. |
| `contextMenus` | Adds "Download with KilatX" to the right-click menu of links. |
| `storage` | Remembers the on/off switch and the last download shown in the toolbar popup. |
| Host permission `<all_urls>` | Downloads can come from any site; reading the cookies of a download's site needs permission for that site. Pages themselves are never read or changed. |

**Remote code:** No, the extension uses no remote code.

**Data usage** (declare these, as the extension handles them on the user's computer):
- Authentication information: cookies for the address of a download.
- Web history: the address of a download and the page it came from (referrer).

Certify that the data is not sold, not transferred to third parties, not used for purposes
unrelated to the single purpose, and not used for creditworthiness or lending. It is never sent
to the developer or any server: only to the KilatX program on the same computer.

**Privacy policy URL:** https://github.com/marcos-liao/KilatX/blob/main/app/docs/PRIVACY.txt

## After publishing

The stores give the extension IDs of their own. Add both to `EXTENSION_IDS` in
`app/src/browser.rs` (so KilatX accepts those extensions) and release a new KilatX version.
Until users have that version, the store extension shows "KilatX not found".
