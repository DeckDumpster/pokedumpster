#!/usr/bin/env python3
"""A stand-in for tcgcsv.com and api.pokemontcg.io that publishes nothing.

tests/refresh/tenant_bytes.sh points a real `pkdump data refresh` here. The
gate's question is what the refresh WRITES, not what it imports, and the two
are separable: a refresh whose upstreams have no new sets, no groups and no
prices still runs every local phase — variants, sub-type map, bundles, search
metadata, set discovery, promo synthesis, variant expansion, symbols, and the
materialized latest_prices — and still reaches the end, which is where the
tenant-database write used to be.

Empty is therefore the right fixture, and it is also the fast one: a refresh
against the real tcgcsv.com walks ~180 English and ~450 Japanese groups at two
requests each, and would make this gate depend on somebody else's uptime.

Every route answers a well-formed empty result, in the shape each client
parses:

    GET /tcgplayer/<category>/groups          -> {"results": []}
    GET /v2/sets                              -> {"data": []}

Anything else is a 404 with the path in it, recorded in the log. A refresh that
starts asking for something this file does not know about should read as a
FAILURE of the gate's assumptions, not as a quiet fallback — so the 404 body
says so, and the log is what the gate prints when the refresh exits non-zero.

One JSON object per request, appended to the log:
    {"method": "GET", "path": "/tcgplayer/3/groups", "status": 200}

Usage: upstream.py <port> <logfile>
"""

import http.server
import json
import re
import sys

PORT = int(sys.argv[1])
LOGFILE = sys.argv[2]

# TCGCSV: the group list for a category. `pkdump data refresh` asks for
# category 3 (English) and category 85 (Pokémon Japan); with no groups in
# either, the per-group product and price fetches never happen at all.
TCGCSV_GROUPS = re.compile(r"^/tcgplayer/\d+/groups$")

# pokemontcg.io: the set list `import_tail` walks. With none, it fetches no
# cards either — every card fetch is nested inside a set the catalog lacks.
PTCGIO_SETS = re.compile(r"^/v2/sets$")


class Upstream(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):  # noqa: N802 - http.server's interface
        path = self.path.split("?", 1)[0]
        if path == "/ready":
            self._reply(200, b"ready")
        elif TCGCSV_GROUPS.match(path):
            self._reply(200, b'{"results":[],"success":true,"errors":[]}')
        elif PTCGIO_SETS.match(path):
            self._reply(200, b'{"data":[],"page":1,"pageSize":250,"count":0,"totalCount":0}')
        else:
            self._reply(
                404,
                json.dumps(
                    {
                        "error": f"tests/refresh/upstream.py has no route for {path}",
                        "hint": "the refresh asked an upstream something this fixture "
                        "does not model — the gate's assumptions have drifted",
                    }
                ).encode(),
            )

    def _reply(self, status, payload):
        with open(LOGFILE, "a", encoding="utf-8") as fh:
            fh.write(
                json.dumps({"method": "GET", "path": self.path, "status": status}) + "\n"
            )
            fh.flush()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *args):
        pass  # the log file IS the output; stderr noise would drown the gate


if __name__ == "__main__":
    # Bound on every interface, not just loopback: the refresh runs in a
    # container and reaches this through host.containers.internal.
    http.server.ThreadingHTTPServer(("", PORT), Upstream).serve_forever()
