#!/usr/bin/env python3
"""Send a sequence of SQL statements to sqld over the Hrana HTTP pipeline.

All statements in one invocation run on a single stream (one connection), so a
BEGIN / ATTACH / SELECT / COMMIT sequence shares a transaction — which is what
sqld's ATTACH requires.

Namespace is selected by the Host header (sqld routes on the first label).
Exit code: 0 if every statement succeeded, 1 if any statement errored.
Prints "ROWS: <n>" after a statement that returns a result set.
Stdlib only — no jq, no libsql client needed.
"""
import argparse
import json
import sys
import urllib.error
import urllib.request


def cell(c):
    if not isinstance(c, dict):
        return repr(c)
    t = c.get("type")
    if t == "null":
        return "NULL"
    return str(c.get("value"))


def pipeline(base, host, sqls, path):
    body = {
        "baton": None,
        "requests": [{"type": "execute", "stmt": {"sql": s}} for s in sqls]
        + [{"type": "close"}],
    }
    req = urllib.request.Request(
        base + path,
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json", "Host": host},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=15) as r:
        return json.load(r)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="http://127.0.0.1:8080")
    ap.add_argument("--host", required=True, help="namespace host, e.g. tenant1.localhost")
    ap.add_argument("--sql", action="append", required=True, help="repeatable; runs in order")
    a = ap.parse_args()

    # Prefer Hrana v3, fall back to v2 on 404.
    last_err = None
    for path in ("/v3/pipeline", "/v2/pipeline"):
        try:
            resp = pipeline(a.base, a.host, a.sql, path)
            break
        except urllib.error.HTTPError as e:
            last_err = e
            if e.code == 404:
                continue
            print(f"HTTP {e.code} on {path}: {e.read().decode(errors='replace')}", file=sys.stderr)
            return 1
    else:
        print(f"pipeline endpoint not found: {last_err}", file=sys.stderr)
        return 1

    results = resp.get("results", [])
    failed = False
    for stmt, res in zip(a.sql, results):
        kind = res.get("type")
        if kind == "error":
            msg = res.get("error", {}).get("message", "?")
            print(f"  ✗ ERROR  {stmt!r}\n           -> {msg}")
            failed = True
            continue
        inner = res.get("response", {})
        if inner.get("type") == "execute":
            result = inner.get("result", {})
            cols = [c.get("name") for c in result.get("cols", [])]
            rows = result.get("rows", [])
            if cols and rows:
                print(f"  ✓ {stmt}")
                print("       " + " | ".join(cols))
                for row in rows:
                    print("       " + " | ".join(cell(c) for c in row))
                print(f"  ROWS: {len(rows)}")
            else:
                affected = result.get("affected_row_count", 0)
                print(f"  ✓ {stmt}   (affected={affected})")
        else:
            print(f"  ✓ {stmt}")

    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
