// Real browser end to end: loads the KilatX extension into Chrome / Edge / Brave
// (temporary profile, CDP over a pipe, Extensions.loadUnpacked) and checks what
// happens to real downloads.
//
//   node testkit/browser_e2e.mjs <browser.exe> <extension dir> <scenario base> <kilatx data dir> <events.log>
//
// Run it through browser_e2e.ps1, which prepares a portable KilatX copy and registers it.

import { spawn, execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const [exe, extDir, BASE, dataDir, eventsLog] = process.argv.slice(2);
const EXT_ID = "mbnpaegbhgmmkfddjehcbmbdofgdlkok";
const name = path.basename(exe);
const profile = fs.mkdtempSync(path.join(os.tmpdir(), "kilatx-e2e-profile-"));
const dlDir = fs.mkdtempSync(path.join(os.tmpdir(), "kilatx-e2e-dl-"));
fs.mkdirSync(path.join(profile, "Default"), { recursive: true });
fs.writeFileSync(
  path.join(profile, "Default", "Preferences"),
  JSON.stringify({ download: { default_directory: dlDir, prompt_for_download: false, directory_upgrade: true } }),
);

const results = [];
const check = (n, ok, detail = "") => results.push({ n, ok: !!ok, detail: ok ? "" : String(detail) });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(cond, ms = 15000) {
  const t = Date.now();
  while (Date.now() - t < ms) {
    try {
      if (await cond()) return true;
    } catch {}
    await sleep(250);
  }
  return false;
}

// ---- CDP over the pipe (messages separated by \0)
const child = spawn(
  exe,
  [
    `--user-data-dir=${profile}`,
    "--remote-debugging-pipe",
    "--enable-unsafe-extension-debugging",
    "--no-first-run",
    "--no-default-browser-check",
    // The test server's HTTPS certificate is a throwaway one (test profile only).
    "--ignore-certificate-errors",
    "about:blank",
  ],
  { stdio: ["ignore", "ignore", "ignore", "pipe", "pipe"], env: process.env },
);
const out = child.stdio[3];
const inp = child.stdio[4];
let seq = 0;
const pending = new Map();
let buffer = "";
inp.on("data", (d) => {
  buffer += d.toString("utf8");
  let i;
  while ((i = buffer.indexOf("\0")) >= 0) {
    const msg = JSON.parse(buffer.slice(0, i));
    buffer = buffer.slice(i + 1);
    if (msg.id && pending.has(msg.id)) {
      const { res, rej } = pending.get(msg.id);
      pending.delete(msg.id);
      msg.error ? rej(new Error(msg.error.message)) : res(msg.result);
    } else if (msg.method) {
      for (const h of listeners) h(msg);
    }
  }
});
const listeners = [];

// A tab that answers the site's login prompt (Basic auth) like a user typing it in.
async function openTabWithLogin(url, username, password) {
  const { targetId } = await send("Target.createTarget", { url: "about:blank" });
  const { sessionId } = await send("Target.attachToTarget", { targetId, flatten: true });
  listeners.push((m) => {
    if (m.sessionId !== sessionId) return;
    if (m.method === "Fetch.authRequired")
      send("Fetch.continueWithAuth", { requestId: m.params.requestId, authChallengeResponse: { response: "ProvideCredentials", username, password } }, sessionId).catch(() => {});
    if (m.method === "Fetch.requestPaused") send("Fetch.continueRequest", { requestId: m.params.requestId }, sessionId).catch(() => {});
  });
  await send("Fetch.enable", { handleAuthRequests: true, patterns: [{ urlPattern: "*" }] }, sessionId);
  await send("Page.navigate", { url }, sessionId).catch(() => {});
  return sessionId;
}
function send(method, params = {}, sessionId) {
  return new Promise((res, rej) => {
    const m = { id: ++seq, method, params };
    if (sessionId) m.sessionId = sessionId;
    pending.set(m.id, { res, rej });
    out.write(JSON.stringify(m) + "\0");
    setTimeout(() => pending.has(m.id) && (pending.delete(m.id), rej(new Error(`${method} timed out`))), 30000);
  });
}

async function openTab(url) {
  const { targetId } = await send("Target.createTarget", { url });
  const { sessionId } = await send("Target.attachToTarget", { targetId, flatten: true });
  return { targetId, sessionId };
}
async function evaluate(sessionId, expression) {
  const r = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true }, sessionId);
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.text + " " + JSON.stringify(r.exceptionDetails.exception?.description || ""));
  return r.result.value;
}

function events(kind) {
  try {
    return fs.readFileSync(eventsLog, "utf8").split(/\r?\n/).filter((l) => l.startsWith(kind + "\t")).map((l) => l.split("\t")[1]);
  } catch {
    return [];
  }
}
function items() {
  try {
    return JSON.parse(fs.readFileSync(path.join(dataDir, "state.json"), "utf8")).items || [];
  } catch {
    return [];
  }
}
const item = (url) => items().find((i) => i.url === url);

let ext; // session on an extension page (full chrome.* access)
async function downloads(urlPart) {
  return evaluate(ext, `chrome.downloads.search({}).then(l => l.filter(d => (d.url + d.finalUrl).includes(${JSON.stringify(urlPart)})).map(d => ({state: d.state, error: d.error, danger: d.danger, exists: d.exists, filename: d.filename})))`);
}

try {
  try {
    await send("Browser.getVersion");
  } catch {
    // Some builds (e.g. Brave's launcher) don't pass the automation pipe through.
    console.log(JSON.stringify([{ n: `${name}: automation pipe not available, browser skipped`, ok: true, detail: "" }]));
    child.kill();
    process.exit(0);
  }
  const { id } = await send("Extensions.loadUnpacked", { path: extDir });
  check(`${name}: extension loads with the fixed ID`, id === EXT_ID, id);

  // The popup talks to KilatX through native messaging, like a user opening it.
  ext = (await openTab(`chrome-extension://${EXT_ID}/popup.html`)).sessionId;
  const connected = await until(async () => (await evaluate(ext, "document.getElementById('state').textContent")).startsWith("Connected to KilatX"), 15000);
  check(`${name}: popup says "Connected to KilatX"`, connected, await evaluate(ext, "document.getElementById('state').textContent").catch((e) => e));

  // A. a normal file: KilatX takes it, the browser's own download disappears
  const a = `${BASE}/plain/3000000/e2e-take.zip?r=1500000`;
  await openTab(a);
  check(`${name}: zip handed to KilatX`, await until(() => events("browser-start").includes(a), 20000), events("browser-start").join(" | "));
  check(`${name}: browser's own download cancelled and removed`, await until(async () => (await downloads("e2e-take.zip")).length === 0), JSON.stringify(await downloads("e2e-take.zip")));
  check(`${name}: KilatX finishes it`, await until(() => item(a)?.status === "Complete", 30000), JSON.stringify(item(a)?.error));

  // B. a file type KilatX doesn't take: the browser downloads it itself
  const b = `${BASE}/plain/200000/e2e-leave.bin`;
  await openTab(b);
  const leftOk = await until(async () => (await downloads("e2e-leave.bin"))[0]?.state === "complete", 20000);
  check(`${name}: .bin stays in the browser and completes`, leftOk, JSON.stringify(await downloads("e2e-leave.bin")));
  check(`${name}: .bin never reached KilatX`, !item(b));

  // C. a members-only file: the browser's cookie goes along
  await send("Storage.setCookies", { cookies: [{ name: "session", value: "ok", domain: "127.0.0.1", path: "/" }] });
  const c = `${BASE}/cookie/1500000/e2e-members.zip?r=1000000`;
  await openTab(c);
  check(`${name}: members-only zip finished by KilatX with the browser's cookie`, await until(() => item(c)?.status === "Complete", 30000), JSON.stringify(item(c)?.error ?? "not received"));

  // D. a redirect chain (mirror / signed link): KilatX keeps the original link
  const d = `${BASE}/redirect/2/1000000/e2e-hop.zip`;
  await openTab(d);
  check(`${name}: redirected zip taken with its original link`, await until(() => item(d)?.status === "Complete", 30000), JSON.stringify(items().map((i) => i.url)));

  // E. name from Content-Disposition, as the browser determined it
  const e = `${BASE}/cd/500000/download.php?id=9`;
  await openTab(e);
  check(`${name}: Content-Disposition name kept`, await until(() => item(e)?.status === "Complete", 30000) && item(e).file_name === "Laporan Akhir – 2026.pdf", JSON.stringify(item(e)?.file_name));

  // F. a blob download made by the page: stays in the browser
  const page = (await openTab(`${BASE}/__count`)).sessionId;
  await sleep(500);
  await evaluate(page, `(() => { const a = document.createElement('a'); a.href = URL.createObjectURL(new Blob(['hello'], {type: 'application/zip'})); a.download = 'e2e-blob.zip'; document.body.appendChild(a); a.click(); return true; })()`);
  check(`${name}: blob download stays in the browser`, await until(async () => (await downloads("blob:")).some((x) => x.state === "complete"), 15000), JSON.stringify(await downloads("blob:")));

  // G. capture switched off in the popup: the browser keeps everything
  await evaluate(ext, "chrome.storage.local.set({capture: false}).then(() => true)");
  const g = `${BASE}/plain/100000/e2e-off.zip`;
  await openTab(g);
  check(`${name}: capture off, browser downloads the zip`, await until(async () => (await downloads("e2e-off.zip"))[0]?.state === "complete", 15000), JSON.stringify(await downloads("e2e-off.zip")));
  check(`${name}: capture off, KilatX not involved`, !item(g));
  await evaluate(ext, "chrome.storage.local.set({capture: true}).then(() => true)");

  // I. a site behind a login the browser answered: KilatX logs in with its saved Site login
  //    (saved for 127.0.0.1/auth/1500000/ only, see browser_e2e.ps1)
  const i1 = `${BASE}/auth/1500000/e2e-login.zip?r=1000000`;
  await openTabWithLogin(i1, "user", "pass");
  check(`${name}: login site with a saved Site login: KilatX logs in and finishes`, await until(() => item(i1)?.status === "Complete", 30000), JSON.stringify(item(i1)?.error ?? "not received"));
  check(`${name}: login site with a saved Site login: browser's download removed`, await until(async () => (await downloads("e2e-login.zip")).length === 0));

  // J. same site, folder without a saved login: KilatX can't log in, the browser keeps it
  const j = `${BASE}/auth/1000000/e2e-nologin.zip`;
  await openTabWithLogin(j, "user", "pass");
  check(`${name}: login site without a saved login: the browser downloads it`, await until(async () => (await downloads("e2e-nologin.zip"))[0]?.state === "complete", 20000), JSON.stringify(await downloads("e2e-nologin.zip")));
  check(`${name}: login site without a saved login: KilatX not involved`, !item(j));

  // H. KilatX not registered (e.g. traces removed): downloads keep working in the browser
  // All five keys KilatX writes (as "Remove traces" does): some browsers read more than their own.
  for (const key of ["Google\\Chrome", "Microsoft\\Edge", "BraveSoftware\\Brave-Browser", "Vivaldi", "Chromium"]) {
    try {
      execFileSync("reg", ["delete", `HKCU\\Software\\${key}\\NativeMessagingHosts\\app.kilatx.host`, "/f"], { stdio: "ignore" });
    } catch {}
    let still = true;
    try {
      execFileSync("reg", ["query", `HKCU\\Software\\${key}\\NativeMessagingHosts\\app.kilatx.host`], { stdio: "ignore" });
    } catch {
      still = false;
    }
    if (still) check(`${name}: registry key removed for the test (${key})`, false, "still there");
  }
  const reply = await evaluate(ext, `new Promise(r => chrome.runtime.sendNativeMessage("app.kilatx.host", {type: "hello"}, x => r(chrome.runtime.lastError ? "error: " + chrome.runtime.lastError.message : JSON.stringify(x))))`);
  check(`${name}: host unreachable once unregistered`, reply.startsWith("error"), reply);
  const h = `${BASE}/plain/100000/e2e-nohost.zip`;
  await openTab(h);
  check(`${name}: without KilatX the browser downloads normally`, await until(async () => (await downloads("e2e-nohost.zip"))[0]?.state === "complete", 15000), JSON.stringify(await downloads("e2e-nohost.zip")));
} catch (err) {
  check(`${name}: run`, false, err.stack || err);
} finally {
  await send("Browser.close").catch(() => {});
  await sleep(1000);
  try {
    child.kill();
  } catch {}
  for (const dir of [profile, dlDir]) {
    try {
      fs.rmSync(dir, { recursive: true, force: true });
    } catch {}
  }
}
console.log(JSON.stringify(results));
process.exit(0);
