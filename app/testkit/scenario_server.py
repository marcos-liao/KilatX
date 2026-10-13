"""Simulated download servers, one per real-world "download model".

Usage:  python scenario_server.py <port> [proxy_port] [https_port]

With an https port, the same scenarios are also served over HTTPS with a throwaway
self-signed certificate (for real-browser tests: some download managers intercept
plain-HTTP downloads inside the browser process; HTTPS they can't).

With a proxy port P, these also start:
  P     HTTP proxy (GET and CONNECT), no login
  P+1   HTTP proxy that needs login pu:pp
  P+2   SOCKS5 proxy, no login
  P+3   SOCKS5 proxy that needs login su:sp
/__count, /__socks_count on the main port report how many requests went through them.

Every file is generated on the fly: byte i of a file is (i % 251), so any size
works without disk space and a test can verify every byte's position.

URL scheme: /<scenario>/<size>/<name>[?params]   (?r=<bytes/s> throttles each connection)

  plain       Range supported (206), like most mirrors and CDNs
  norange     Ignores Range, always 200 (old servers, scripts)
  throttle    Range supported, each connection limited to ?r= bytes/s
  chunked     No Content-Length, chunked encoding, no Range
  redirect    /redirect/<hops>/<size>/<name>: 302 chain ending at /plain/...
  expiring    /expiring/<unix_expiry>/<size>/<name>: 403 once expired (signed links)
  cd          Name comes from Content-Disposition (download.php?id=...)
  connlimit   At most ?n= concurrent connections per client, others get 503
  cookie      403 unless the request carries "Cookie: session=ok"
  auth        401 unless Basic auth user:pass
  drop        Connection cut after ?k= bytes on the first attempt of every range
  empty       0-byte file (Range request answered 416, like nginx/Apache)
  liar        First answer is 206, later range requests get the whole file (200)
  stall       The first range request that doesn't start at 0 sends 100 KB, then hangs
  busy        The first 2 requests for a path get 429 with Retry-After: 1
  flaky       The first request for a path is cut before any answer
  uasniff     Browsers ("Mozilla" agents) get an HTML page, download tools get the file
  htmlpage    Always answers with an HTML page, even for /x.exe
  needua      403 unless the User-Agent contains ?ua=
  needref     403 unless the Referer contains ?ref= (hotlink protection)
  postonly    405 for GET: the file only comes from a form POST (KilatX must leave it to the browser)
  digest      Digest login (user:pass, MD5, qop=auth)
  whoauth     Serves the file and remembers the Authorization header (/__auth?path=...)
  notfound    404
  servererror 500
"""
import base64
import hashlib
import json
import re
import select
import socket
import struct
import sys
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1])
PROXY_PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 0
HTTPS_PORT = int(sys.argv[3]) if len(sys.argv) > 3 else 0
PATTERN = bytes(i % 251 for i in range(251 * 261))  # 65511 bytes, a whole number of periods

lock = threading.Lock()
active = {}           # scenario key -> open connections (for connlimit)
dropped_starts = set()  # (path, start) already cut once (for drop)
liar_seen = set()     # paths that already got their honest first answer
stalled_paths = set()  # paths whose stalling connection was already used
busy_count = {}        # path -> requests answered 429 so far
flaky_seen = set()     # paths whose first request was already cut
auth_seen = {}         # path -> Authorization header of the last request (whoauth)
counters = {"proxy": 0, "socks": 0}
DIGEST_NONCE = "kx" + hashlib.md5(str(time.time()).encode()).hexdigest()


def body_bytes(start, end):
    """Bytes start..end inclusive of the generated file, in chunks."""
    pos = start
    while pos <= end:
        off = pos % 251
        n = min(len(PATTERN) - off, end - pos + 1)
        yield PATTERN[off:off + n]
        pos += n


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def send_plain(self, code, text, extra=None):
        data = text.encode()
        self.send_response(code)
        for k, v in (extra or {}).items():
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def digest_ok(self):
        h = self.headers.get("Authorization") or ""
        if not h.startswith("Digest "):
            return False
        f = dict(re.findall(r'(\w+)="?([^",]*)"?', h[7:]))
        md5 = lambda x: hashlib.md5(x.encode()).hexdigest()
        if f.get("username") != "user" or f.get("nonce") != DIGEST_NONCE:
            return False
        ha1 = md5(f"user:{f.get('realm')}:pass")
        ha2 = md5(f"GET:{f.get('uri')}")
        want = md5(f"{ha1}:{DIGEST_NONCE}:{f.get('nc')}:{f.get('cnonce')}:auth:{ha2}")
        return f.get("response") == want and f.get("uri") == self.path

    def do_GET(self):
        url = urllib.parse.urlsplit(self.path)
        q = dict(urllib.parse.parse_qsl(url.query))
        parts = url.path.strip("/").split("/")
        scen = parts[0] if parts else ""
        try:
            self.route(scen, parts, q, url)
        except (ConnectionResetError, BrokenPipeError, ConnectionAbortedError):
            pass

    def route(self, scen, parts, q, url):
        if scen == "htmlpage" or (scen == "uasniff" and "Mozilla" in (self.headers.get("User-Agent") or "")):
            page = b"<html><body>Your download will start shortly...</body></html>"
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(page)))
            self.end_headers()
            self.wfile.write(page)
            return
        if url.path == "/__count":
            return self.send_plain(200, str(counters["proxy"]))
        if url.path == "/__socks_count":
            return self.send_plain(200, str(counters["socks"]))
        if url.path == "/__auth":
            return self.send_plain(200, json.dumps(auth_seen.get(q.get("path", ""))))
        if scen == "whoauth":
            with lock:
                auth_seen[url.path] = self.headers.get("Authorization")
        if scen == "needua" and q.get("ua", "") not in (self.headers.get("User-Agent") or ""):
            return self.send_plain(403, "wrong user agent")
        if scen == "needref" and q.get("ref", "-") not in (self.headers.get("Referer") or ""):
            return self.send_plain(403, "hotlinking not allowed")
        if scen == "postonly":
            return self.send_plain(405, "use the form", {"Allow": "POST"})
        if scen == "digest" and not self.digest_ok():
            return self.send_plain(401, "digest login required", {
                "WWW-Authenticate": f'Digest realm="kilat", qop="auth", nonce="{DIGEST_NONCE}", opaque="op42", algorithm=MD5'})
        if scen == "flaky":
            with lock:
                first = url.path not in flaky_seen
                flaky_seen.add(url.path)
            if first:
                self.close_connection = True
                self.connection.shutdown(2)
                return
        if scen == "busy":
            with lock:
                n = busy_count.get(url.path, 0)
                busy_count[url.path] = n + 1
            if n < 2:
                return self.send_plain(429, "slow down", {"Retry-After": "1"})
        if scen == "notfound":
            return self.send_plain(404, "not found")
        if scen == "servererror":
            return self.send_plain(500, "boom")
        if scen == "redirect":
            hops, rest = int(parts[1]), "/".join(parts[2:])
            target = f"/redirect/{hops - 1}/{rest}" if hops > 1 else f"/plain/{rest}"
            return self.send_plain(302, "moved", {"Location": target})
        if scen == "expiring":
            if time.time() > int(parts[1]):
                return self.send_plain(403, "link expired")
            parts = parts[1:]
        if scen == "cookie" and "session=ok" not in (self.headers.get("Cookie") or ""):
            return self.send_plain(403, "login required")
        if scen == "auth":
            want = "Basic " + base64.b64encode(b"user:pass").decode()
            if self.headers.get("Authorization") != want:
                return self.send_plain(401, "auth required", {"WWW-Authenticate": 'Basic realm="t"'})
        size = int(parts[1])
        if scen == "connlimit":
            key = (scen, self.client_address[0])
            with lock:
                if active.get(key, 0) >= int(q.get("n", 4)):
                    return self.send_plain(503, "too many connections", {"Retry-After": "1"})
                active[key] = active.get(key, 0) + 1
            try:
                return self.serve(scen, size, q, url)
            finally:
                with lock:
                    active[key] -= 1
        return self.serve(scen, size, q, url)

    def serve(self, scen, size, q, url):
        rng = re.match(r"bytes=(\d+)-(\d*)", self.headers.get("Range", ""))
        ranges_ok = scen not in ("norange", "chunked")
        if scen == "liar" and rng:
            with lock:
                honest = url.path not in liar_seen
                liar_seen.add(url.path)
            ranges_ok = honest
        if rng and ranges_ok:
            start = int(rng.group(1))
            end = int(rng.group(2)) if rng.group(2) else size - 1
            if start >= size:
                self.send_response(416)
                self.send_header("Content-Range", f"bytes */{size}")
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
            end = min(end, size - 1)
            self.send_response(206)
            self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
        else:
            start, end = 0, size - 1
            self.send_response(200)
        if scen == "cd":
            self.send_header(
                "Content-Disposition",
                "attachment; filename=\"Laporan Akhir 2026.pdf\"; "
                "filename*=UTF-8''Laporan%20Akhir%20%E2%80%93%202026.pdf",
            )
        if ranges_ok:
            self.send_header("Accept-Ranges", "bytes")
        if scen == "chunked":
            self.send_header("Transfer-Encoding", "chunked")
        else:
            self.send_header("Content-Length", str(max(0, end - start + 1)))
        self.end_headers()

        # ?r= limits each connection's speed in any scenario, which pushes the
        # downloader to open several connections.
        rate = int(q.get("r", 0))
        cut_after = None
        if scen == "drop":
            with lock:
                if (url.path, start) not in dropped_starts:
                    dropped_starts.add((url.path, start))
                    cut_after = int(q.get("k", 200000))
        stall_after = None
        if scen == "stall" and start > 0:
            with lock:
                if url.path not in stalled_paths:
                    stalled_paths.add(url.path)
                    stall_after = 100_000
        sent = 0
        for chunk in body_bytes(start, end):
            if stall_after is not None and sent >= stall_after:
                time.sleep(120)  # hang without closing, like a dead route
                return
            if cut_after is not None and sent + len(chunk) > cut_after:
                self.wfile.write(chunk[: max(0, cut_after - sent)])
                self.wfile.flush()
                self.close_connection = True
                self.connection.shutdown(2)
                return
            if scen == "chunked":
                self.wfile.write(f"{len(chunk):x}\r\n".encode() + chunk + b"\r\n")
            else:
                self.wfile.write(chunk)
            sent += len(chunk)
            if rate:
                time.sleep(len(chunk) / rate)
        if scen == "chunked":
            self.wfile.write(b"0\r\n\r\n")


def relay(a, b):
    """Copies bytes both ways until one side closes."""
    socks = [a, b]
    try:
        while True:
            ready, _, _ = select.select(socks, [], [], 60)
            if not ready:
                return
            for s_ in ready:
                data = s_.recv(65536)
                if not data:
                    return
                (b if s_ is a else a).sendall(data)
    except OSError:
        pass
    finally:
        a.close()
        b.close()


class ProxyHandler(BaseHTTPRequestHandler):
    """Forward proxy (GET for http, CONNECT for https), counting what goes through it."""
    protocol_version = "HTTP/1.1"
    login = None  # "user:pass" when a login is required
    count = 0

    def log_message(self, *a):
        pass

    def allowed(self):
        if not self.login:
            return True
        want = "Basic " + base64.b64encode(self.login.encode()).decode()
        if self.headers.get("Proxy-Authorization") == want:
            return True
        self.send_response(407)
        self.send_header("Proxy-Authenticate", 'Basic realm="proxy"')
        self.send_header("Content-Length", "0")
        self.end_headers()
        return False

    def do_CONNECT(self):
        if not self.allowed():
            return
        host, _, port = self.path.rpartition(":")
        try:
            upstream = socket.create_connection((host, int(port)), timeout=20)
        except OSError:
            self.send_response(502)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        with lock:
            counters["proxy"] += 1
        self.send_response(200, "Connection established")
        self.end_headers()
        self.close_connection = True
        relay(self.connection, upstream)

    def do_GET(self):
        if not self.allowed():
            return
        import http.client
        u = urllib.parse.urlsplit(self.path)
        with lock:
            counters["proxy"] += 1
        conn = http.client.HTTPConnection(u.hostname, u.port or 80)
        headers = {k: v for k, v in self.headers.items() if k.lower() not in ("proxy-connection", "connection")}
        conn.request("GET", u.path + ("?" + u.query if u.query else ""), headers=headers)
        r = conn.getresponse()
        self.send_response(r.status)
        for k, v in r.getheaders():
            if k.lower() not in ("connection", "transfer-encoding"):
                self.send_header(k, v)
        self.end_headers()
        try:
            while True:
                chunk = r.read(65536)
                if not chunk:
                    break
                self.wfile.write(chunk)
        except (ConnectionResetError, BrokenPipeError, ConnectionAbortedError):
            pass
        conn.close()


def socks5_server(port, login):
    """Minimal SOCKS5 (RFC 1928/1929): CONNECT only, optional username/password."""
    srv = socket.socket()
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", port))
    srv.listen(64)

    def handle(c):
        try:
            ver, n = c.recv(2)
            methods = c.recv(n)
            if login:
                if 2 not in methods:
                    c.sendall(b"\x05\xff")
                    return c.close()
                c.sendall(b"\x05\x02")
                c.recv(1)
                user = c.recv(c.recv(1)[0]).decode()
                pw = c.recv(c.recv(1)[0]).decode()
                ok = f"{user}:{pw}" == login
                c.sendall(b"\x01\x00" if ok else b"\x01\x01")
                if not ok:
                    return c.close()
            else:
                c.sendall(b"\x05\x00")
            ver, cmd, _, atyp = c.recv(4)
            if atyp == 1:
                host = socket.inet_ntoa(c.recv(4))
            elif atyp == 3:
                host = c.recv(c.recv(1)[0]).decode()
            else:
                host = socket.inet_ntop(socket.AF_INET6, c.recv(16))
            port_ = struct.unpack(">H", c.recv(2))[0]
            upstream = socket.create_connection((host, port_), timeout=20)
            with lock:
                counters["socks"] += 1
            c.sendall(b"\x05\x00\x00\x01" + socket.inet_aton("127.0.0.1") + struct.pack(">H", port))
            relay(c, upstream)
        except Exception:
            c.close()

    while True:
        conn, _ = srv.accept()
        threading.Thread(target=handle, args=(conn,), daemon=True).start()


if PROXY_PORT:
    class AuthProxy(ProxyHandler):
        login = "pu:pp"

    for port_, handler in ((PROXY_PORT, ProxyHandler), (PROXY_PORT + 1, AuthProxy)):
        threading.Thread(target=ThreadingHTTPServer(("127.0.0.1", port_), handler).serve_forever, daemon=True).start()
    threading.Thread(target=socks5_server, args=(PROXY_PORT + 2, None), daemon=True).start()
    threading.Thread(target=socks5_server, args=(PROXY_PORT + 3, "su:sp"), daemon=True).start()
if HTTPS_PORT:
    import datetime
    import ssl
    import tempfile
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec
    from cryptography.x509.oid import NameOID
    key = ec.generate_private_key(ec.SECP256R1())
    name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "127.0.0.1")])
    now = datetime.datetime.now(datetime.timezone.utc)
    import ipaddress
    cert = (x509.CertificateBuilder().subject_name(name).issuer_name(name).public_key(key.public_key())
            .serial_number(x509.random_serial_number()).not_valid_before(now - datetime.timedelta(days=1))
            .not_valid_after(now + datetime.timedelta(days=7))
            .add_extension(x509.SubjectAlternativeName([x509.IPAddress(ipaddress.ip_address("127.0.0.1")), x509.DNSName("localhost")]), False)
            .sign(key, hashes.SHA256()))
    tmp = tempfile.mkdtemp(prefix="kilat-scen-tls-")
    certfile, keyfile = f"{tmp}/cert.pem", f"{tmp}/key.pem"
    with open(certfile, "wb") as f:
        f.write(cert.public_bytes(serialization.Encoding.PEM))
    with open(keyfile, "wb") as f:
        f.write(key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8, serialization.NoEncryption()))
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(certfile, keyfile)
    tls = ThreadingHTTPServer(("127.0.0.1", HTTPS_PORT), Handler)
    tls.socket = ctx.wrap_socket(tls.socket, server_side=True)
    threading.Thread(target=tls.serve_forever, daemon=True).start()
ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
