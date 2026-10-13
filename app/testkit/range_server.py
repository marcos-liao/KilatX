"""Tiny HTTP server with Range support and throttling, for testing the downloader.
Usage: python range_server.py <dir> <port> [bytes_per_sec_per_conn]
"""
import os, re, sys, time
from http.server import ThreadingHTTPServer, BaseHTTPRequestHandler

ROOT, PORT = sys.argv[1], int(sys.argv[2])
RATE = int(sys.argv[3]) if len(sys.argv) > 3 else 0


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_GET(self):
        path = os.path.join(ROOT, os.path.basename(self.path.split("?")[0]))
        if not os.path.isfile(path):
            self.send_error(404)
            return
        size = os.path.getsize(path)
        start, end = 0, size - 1
        m = re.match(r"bytes=(\d+)-(\d*)", self.headers.get("Range", ""))
        if m and "norange" not in self.path:
            start = int(m.group(1))
            end = int(m.group(2)) if m.group(2) else size - 1
            self.send_response(206)
            self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
        else:
            self.send_response(200)
        self.send_header("Content-Length", str(end - start + 1))
        self.send_header("Accept-Ranges", "bytes")
        self.end_headers()
        with open(path, "rb") as f:
            f.seek(start)
            left = end - start + 1
            try:
                while left > 0:
                    chunk = f.read(min(65536, left))
                    self.wfile.write(chunk)
                    left -= len(chunk)
                    if RATE:
                        time.sleep(len(chunk) / RATE)
            except (ConnectionResetError, BrokenPipeError, ConnectionAbortedError):
                pass


ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
