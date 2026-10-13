const $ = (id) => document.getElementById(id);
const extension = typeof chrome !== "undefined" && chrome.runtime && chrome.runtime.id;

function show(ok, state, detail) {
  $("dot").className = ok ? "dot ok" : "dot bad";
  $("state").textContent = state;
  $("detail").textContent = detail || "";
}

// Never leaves the popup on "Checking…": no answer within 5 s counts as no KilatX.
function ask(message) {
  return new Promise((resolve) => {
    const timer = setTimeout(() => resolve({ ok: false, error: "KilatX did not answer" }), 5000);
    try {
      chrome.runtime.sendNativeMessage("app.kilatx.host", message, (reply) => {
        clearTimeout(timer);
        const err = chrome.runtime.lastError;
        resolve(err ? { ok: false, error: err.message } : reply || { ok: false, error: "empty answer" });
      });
    } catch (e) {
      clearTimeout(timer);
      resolve({ ok: false, error: String(e) });
    }
  });
}

(async () => {
  if (!extension) {
    // Opened as a plain web page (not from the browser toolbar).
    show(false, "Not running as an extension", "Add KilatX to the browser from KilatX › Settings › Browser, then use its toolbar button.");
    $("capture").disabled = true;
    $("open").disabled = true;
    return;
  }

  const { capture } = await chrome.storage.local.get({ capture: true }).catch(() => ({ capture: true }));
  $("capture").checked = capture;
  $("capture").onchange = () => chrome.storage.local.set({ capture: $("capture").checked });

  const { last } = await chrome.storage.session.get("last").catch(() => ({}));
  if (last && last.url) {
    $("lastbox").hidden = false;
    const what = last.action === "take" ? "Sent to KilatX" : "Left to the browser";
    $("last").textContent = `${what}: ${last.url}${last.reason ? ` (${last.reason})` : ""}`;
  }

  const hello = await ask({ type: "hello" });
  if (hello.ok) {
    show(true, `Connected to KilatX ${hello.version}`, hello.enabled ? "" : "Browser integration is turned off in KilatX › Settings › Browser.");
  } else {
    show(false, "KilatX not found", `Start KilatX once (Settings › Browser) so the browser can reach it. Downloads stay in the browser meanwhile. (${hello.error})`);
    $("open").disabled = true;
  }
  $("open").onclick = async () => {
    await ask({ type: "show" });
    window.close();
  };
})();
