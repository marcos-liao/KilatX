// KilatX browser integration (Chromium, Manifest V3).
//
// When the browser starts a download, KilatX is asked first (native messaging).
// KilatX checks the link itself, with the browser's cookies, referrer and user
// agent. Only when it can really fetch the file does the browser's download get
// cancelled; otherwise the browser simply carries on, so a download is never lost.

const HOST = "app.kilatx.host";
const HTTP = /^https?:\/\//i;

function native(message) {
  return new Promise((resolve) => {
    try {
      chrome.runtime.sendNativeMessage(HOST, message, (reply) => {
        const err = chrome.runtime.lastError;
        if (err) resolve({ ok: false, error: err.message, missing: /not found|forbidden/i.test(err.message) });
        else resolve(reply || { ok: false, error: "KilatX did not answer" });
      });
    } catch (e) {
      resolve({ ok: false, error: String(e) });
    }
  });
}

async function captureOn() {
  const { capture } = await chrome.storage.local.get({ capture: true });
  return capture;
}

// "name=value; name2=value2" for the Cookie header, from the right cookie store.
async function cookieHeader(url, incognito) {
  try {
    const query = { url };
    if (incognito) query.storeId = "1";
    const list = await chrome.cookies.getAll(query);
    return list.map((c) => `${c.name}=${c.value}`).join("; ");
  } catch {
    return "";
  }
}

function baseName(path) {
  return (path || "").split(/[\\/]/).pop();
}

async function remember(result) {
  await chrome.storage.session.set({ last: { ...result, at: Date.now() } }).catch(() => {});
  await chrome.action.setBadgeText({ text: result.missing ? "!" : "" }).catch(() => {});
}

async function offer(item) {
  const url = item.url;
  const finalUrl = item.finalUrl || item.url;
  const reply = await native({
    type: "offer",
    url,
    final_url: finalUrl,
    referrer: item.referrer || "",
    filename: baseName(item.filename),
    mime: item.mime || "",
    size: item.totalBytes > 0 ? item.totalBytes : item.fileSize > 0 ? item.fileSize : null,
    cookies: await cookieHeader(url, item.incognito),
    final_cookies: finalUrl !== url ? await cookieHeader(finalUrl, item.incognito) : "",
    user_agent: navigator.userAgent,
  });
  await remember({ url: finalUrl, action: reply.ok ? reply.action : "leave", reason: reply.reason || reply.error || "", missing: !!reply.missing });
  return reply.ok && reply.action === "take";
}

chrome.downloads.onDeterminingFilename.addListener((item, suggest) => {
  const url = item.finalUrl || item.url;
  // Downloads this extension started itself, and data/blob links, stay in the browser.
  if (item.byExtensionId === chrome.runtime.id || !HTTP.test(url) || item.state !== "in_progress") return;
  (async () => {
    let taken = false;
    try {
      taken = (await captureOn()) && (await offer(item));
    } catch {
      taken = false;
    }
    if (taken) {
      await chrome.downloads.cancel(item.id).catch(() => {});
      await chrome.downloads.erase({ id: item.id }).catch(() => {});
    }
    try {
      suggest();
    } catch {}
  })();
  return true; // suggest() is called asynchronously
});

function createMenus() {
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({ id: "kilatx-link", title: "Download with KilatX", contexts: ["link"] });
    chrome.contextMenus.create({ id: "kilatx-media", title: "Download with KilatX", contexts: ["video", "audio", "image"] });
  });
}
chrome.runtime.onInstalled.addListener(createMenus);
chrome.runtime.onStartup.addListener(createMenus);

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  const url = info.linkUrl || info.srcUrl || "";
  if (!HTTP.test(url)) return;
  const reply = await native({
    type: "add",
    url,
    final_url: url,
    referrer: info.pageUrl || "",
    filename: "",
    mime: "",
    size: null,
    cookies: await cookieHeader(url, tab && tab.incognito),
    final_cookies: "",
    user_agent: navigator.userAgent,
  });
  await remember({ url, action: reply.ok ? "take" : "leave", reason: reply.reason || reply.error || "", missing: !!reply.missing });
  // KilatX isn't available: let the browser download it instead.
  if (!reply.ok) chrome.downloads.download({ url }).catch(() => {});
});

async function updateIcon() {
  const on = await captureOn();
  const path = on
    ? { 16: "icons/icon16.png", 32: "icons/icon32.png" }
    : { 16: "icons/off16.png", 32: "icons/off32.png" };
  await chrome.action.setIcon({ path }).catch(() => {});
  await chrome.action.setTitle({ title: on ? "KilatX: capturing downloads" : "KilatX: capture is off" }).catch(() => {});
}
chrome.storage.onChanged.addListener((changes) => {
  if (changes.capture) updateIcon();
});
updateIcon();
