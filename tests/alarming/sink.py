#!/usr/bin/env python3
"""A stand-in for healthchecks.io and Pushover: records every request it gets.

tests/alarming/run.sh points PKDUMP_BACKUP_PING_URL and PUSHOVER_API_URL here so
the gate can assert what the alarming layers ACTUALLY SENT, rather than reading
the scripts and reasoning about what they would send. That distinction is the
whole point of the bead this was written for — every layer had been reviewed and
none of them had ever fired.

One JSON object per request, appended to the log:
    {"method": "GET", "path": "/hc/<token>/fail", "body": ""}

Usage: sink.py <port> <logfile>
"""

import http.server
import json
import sys

PORT = int(sys.argv[1])
LOGFILE = sys.argv[2]


class Recorder(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _record(self, method):
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length).decode("utf-8", "replace") if length else ""
        with open(LOGFILE, "a", encoding="utf-8") as fh:
            fh.write(json.dumps({"method": method, "path": self.path, "body": body}) + "\n")
            fh.flush()
        payload = b'{"status":1}'
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):  # noqa: N802 - http.server's interface
        self._record("GET")

    def do_POST(self):  # noqa: N802
        self._record("POST")

    def log_message(self, *args):
        pass  # the log file IS the output; stderr noise would drown the gate


if __name__ == "__main__":
    http.server.ThreadingHTTPServer(("127.0.0.1", PORT), Recorder).serve_forever()
