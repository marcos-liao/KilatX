"""Browser host end to end with the real KilatX.exe, started the way Chrome starts it
(`KilatX.exe chrome-extension://<id>/`, messages on stdin/stdout).

Usage (from app/, scenario_server.py running on 8766, debug build):
    python testkit/browser_host_test.py target/debug/KilatX.exe http://127.0.0.1:8766

Runs a portable copy in %TEMP% with its own single-instance name (KILATX_INSTANCE,
debug builds only), so a KilatX the user is running is never touched.
"""
import ctypes
import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import time
from ctypes import wintypes

EXE, BASE = sys.argv[1], sys.argv[2]
ORIGIN = "chrome-extension://mbnpaegbhgmmkfddjehcbmbdofgdlkok/"
work = os.path.join(tempfile.gettempdir(), "kilatx-browser-test")
shutil.rmtree(work, ignore_errors=True)
os.makedirs(os.path.join(work, "app", "data"))
os.makedirs(os.path.join(work, "dl"))
app = os.path.join(work, "app", "KilatX.exe")
shutil.copy(EXE, app)
events = os.path.join(work, "events.log")
env = dict(os.environ, KILATX_INSTANCE="-browser-test", KILATX_EVENT_LOG=events)
state = os.path.join(work, "app", "data", "state.json")
# Downloads go into the work folder, never the user's own Downloads folder.
with open(state, "w", encoding="utf-8") as f:
    json.dump({"settings": {"connections": 8, "download": {"base_folder": os.path.join(work, "dl")}}}, f)
results = []


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))


def procs():
    out = subprocess.run(["powershell", "-NoProfile", "-Command",
                          f"@(Get-Process KilatX -ErrorAction SilentlyContinue | Where-Object {{ $_.Path -eq '{app}' }}).Id -join ','"],
                         capture_output=True, text=True).stdout.strip()
    return [int(x) for x in out.split(",") if x]


def kill_all():
    for pid in procs():
        subprocess.run(["taskkill", "/F", "/PID", str(pid)], capture_output=True)
    time.sleep(0.5)


def wait_until(cond, sec=15):
    t = time.time()
    while time.time() - t < sec:
        if cond():
            return True
        time.sleep(0.2)
    return False


def event_lines(kind):
    try:
        with open(events, encoding="utf-8") as f:
            return [l.rstrip("\n").split("\t", 1)[1] for l in f if l.startswith(kind + "\t")]
    except FileNotFoundError:
        return []


def host(*messages, timeout=30):
    """Runs one host process like the browser does; returns (answers, seconds, exit code)."""
    data = b"".join(struct.pack("<I", len(b)) + b for b in (json.dumps(m).encode() for m in messages))
    t = time.time()
    p = subprocess.run([app, ORIGIN, "--parent-window=0"], input=data, capture_output=True, env=env, timeout=timeout)
    out, answers = p.stdout, []
    while len(out) >= 4:
        n = struct.unpack("<I", out[:4])[0]
        answers.append(json.loads(out[4:4 + n]))
        out = out[4 + n:]
    return answers, time.time() - t, p.returncode


def items():
    try:
        with open(state, encoding="utf-8") as f:
            return json.load(f).get("items", [])
    except (FileNotFoundError, json.JSONDecodeError):
        return []


def set_settings(**kw):
    try:
        with open(state, encoding="utf-8") as f:
            s = json.load(f)
    except FileNotFoundError:
        s = {"settings": {"connections": 8}}
    s["settings"].update(kw)
    with open(state, "w", encoding="utf-8") as f:
        json.dump(s, f)


user32 = ctypes.windll.user32


def window_visible(pid):
    found = []
    @ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
    def cb(h, _):
        p = wintypes.DWORD()
        user32.GetWindowThreadProcessId(h, ctypes.byref(p))
        if p.value == pid and user32.IsWindowVisible(h):
            buf = ctypes.create_unicode_buffer(256)
            user32.GetWindowTextW(h, buf, 256)
            if buf.value == "KilatX":
                found.append(h)
        return True
    user32.EnumWindows(cb, 0)
    return bool(found)


def offer(url, **extra):
    m = {"type": "offer", "url": url, "final_url": url, "referrer": "https://site.test/page",
         "filename": "", "cookies": "", "user_agent": "Mozilla/5.0 (Windows NT 10.0) Test"}
    m.update(extra)
    return m


# 1. hello, KilatX not running: answers, starts nothing, exits at once
a, secs, code = host({"type": "hello"})
check("hello: answers with version", a and a[0].get("ok") and a[0].get("version"), str(a))
check("hello: host exits by itself", code == 0 and secs < 5, f"{secs:.2f} s, exit {code}")
check("hello: does not start KilatX", not procs())

# 2. offer while KilatX is not running: KilatX starts in the tray, Add dialog with the link
url = f"{BASE}/cookie/1500000/members-area.zip?r=500000"
a, secs, _ = host(offer(url, cookies="session=ok"))
check("offer (not running): taken", a and a[0].get("action") == "take", str(a))
check("offer (not running): answered within 15 s", secs < 15, f"{secs:.2f} s")
check("KilatX started", wait_until(lambda: len(procs()) == 1), str(procs()))
check("Add dialog shows the link", wait_until(lambda: any(url in l for l in event_lines("add-dialog"))), str(event_lines("add-dialog")))
pid = procs()[0] if procs() else 0
check("Window comes up for the dialog", wait_until(lambda: window_visible(pid)))
time.sleep(1)
check("KilatX keeps running after the host exits", procs() == [pid], str(procs()))

# 3. offers the host must leave to the browser
for name, u, why in [("web page", f"{BASE}/plain/1000/index.html", "file type"),
                     ("POST-only download", f"{BASE}/postonly/1000/report.pdf", "405"),
                     ("login wall without cookies", f"{BASE}/cookie/1000/members.zip", "Access denied"),
                     ("error page instead of file", f"{BASE}/htmlpage/1000/setup.exe", "web page")]:
    a, secs, _ = host(offer(u))
    ok = a and a[0].get("action") == "leave" and why in a[0].get("reason", "")
    check(f"leave: {name}", ok, f"{a} ({secs:.2f} s)")
check("left downloads never reach KilatX", not any("members.zip" in l or "report.pdf" in l for l in event_lines("add-dialog")))

# 4. no dialog: starts at once and downloads with the browser's cookies, referrer and name
kill_all()
set_settings(browser_dialog=False)
before = len(event_lines("browser-start"))
url2 = f"{BASE}/needref/2000000/hotlinked.zip?ref=site.test&r=500000"
a, _, _ = host(offer(url2, filename="Gallery Pack.zip"))
check("no dialog: taken", a and a[0].get("action") == "take", str(a))
check("no dialog: started at once", wait_until(lambda: len(event_lines("browser-start")) > before))
def done():
    its = [i for i in items() if i["url"] == url2]
    return its and its[0]["status"] in ("Complete", "Error")
wait_until(done, 40)
its = [i for i in items() if i["url"] == url2]
it = its[0] if its else {}
check("download completes with the browser's Referer", it.get("status") == "Complete", str(it.get("error")))
check("saved under the browser's name", it.get("file_name") == "Gallery Pack.zip", it.get("file_name", ""))
path = os.path.join(it.get("save_dir", ""), it.get("file_name", ""))
size_ok = os.path.isfile(path) and os.path.getsize(path) == 2000000
if size_ok:
    with open(path, "rb") as f:
        data = f.read()
    size_ok = all(data[i] == i % 251 for i in range(0, len(data), 997))
check("file is byte-exact", size_ok, path)
check("cookies/headers dropped once complete", not it.get("headers"), str(it.get("headers")))

# 5. while running: a burst of 5 offers -> still one KilatX, all delivered
urls = [f"{BASE}/plain/200000/burst-{i}.zip" for i in range(5)]
before = len(event_lines("browser-start"))
ps = [subprocess.Popen([app, ORIGIN], stdin=subprocess.PIPE, stdout=subprocess.PIPE, env=env) for _ in urls]
for p, u in zip(ps, urls):
    b = json.dumps(offer(u)).encode()
    p.stdin.write(struct.pack("<I", len(b)) + b)
    p.stdin.close()
for p in ps:
    p.wait(30)
check("burst of 5: all hosts exited", all(p.returncode == 0 for p in ps))
check("burst of 5: all delivered", wait_until(lambda: len(event_lines("browser-start")) - before >= 5), str(len(event_lines("browser-start")) - before))
check("burst of 5: still exactly one KilatX", len(procs()) == 1, str(procs()))

# 6. "show" brings the window up
a, _, _ = host({"type": "show"})
pid = procs()[0] if procs() else 0
check("show: window visible", a and a[0].get("ok") and wait_until(lambda: window_visible(pid)), str(a))

# 7. a host that never gets a message (browser hung) exits on its own
t = time.time()
p = subprocess.Popen([app, ORIGIN], stdin=subprocess.PIPE, stdout=subprocess.PIPE, env=env)
try:
    p.wait(75)
except subprocess.TimeoutExpired:
    p.kill()
check("silent host exits within its 60 s safety limit", p.returncode is not None and time.time() - t < 70, f"{time.time() - t:.0f} s")
p.stdin.close()

kill_all()
# The test copy registered itself as the browsers' host: remove that if it still points here.
import winreg
for key in (r"Software\Google\Chrome", r"Software\Microsoft\Edge", r"Software\BraveSoftware\Brave-Browser", r"Software\Vivaldi", r"Software\Chromium"):
    path = key + r"\NativeMessagingHosts\app.kilatx.host"
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, path) as k:
            target = winreg.QueryValue(k, None)
        if target.lower().startswith(work.lower()):
            winreg.DeleteKey(winreg.HKEY_CURRENT_USER, path)
    except OSError:
        pass
width = max(len(r[0]) for r in results)
for name, ok, detail in results:
    print(f"{name:<{width}}  {'PASS' if ok else 'FAIL'}  {detail if not ok else ''}")
passed = sum(1 for r in results if r[1])
print(f"\n{passed}/{len(results)} passed")
sys.exit(0 if passed == len(results) else 1)
