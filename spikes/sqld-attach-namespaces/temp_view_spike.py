#!/usr/bin/env python3
"""TEMP-VIEW spike: does PokeDumpster's "ATTACH + TEMP VIEWs once, then query
unqualified" pattern survive sqld server mode?

Background: locally (rusqlite) the user DB ATTACHes shared.sqlite for the
connection's whole life and exposes catalog tables via TEMP VIEWs, so queries
join unqualified. In sqld, ATTACH must be issued inside a transaction. The open
questions:

  Q1 (foundational): is ATTACH connection-scoped (survives COMMIT on a held
       connection) or transaction-scoped (released at COMMIT)?
  Q2: does a TEMP VIEW over an attached namespace work intra-transaction?
  Q3: does a TEMP VIEW persist across transactions on a HELD connection, and
       resolve once the catalog is (re-)attached?
  Q4: is a TEMP VIEW invisible on a DIFFERENT connection (per-connection scope)?

A "connection" in Hrana == a stream identified by a baton. The Rust `libsql`
client's Connection maps to exactly this held-baton stream, so the baton-held
scenarios below model real client behavior.

Self-contained given a running sqld with --enable-namespaces + admin API.
Stdlib only.
"""
import argparse
import json
import sys
import urllib.error
import urllib.request

BASE = "http://127.0.0.1:18080"
ADMIN = "http://127.0.0.1:19090"

GREEN, RED, CYAN, DIM, RESET = "\033[1;32m", "\033[1;31m", "\033[1;36m", "\033[2m", "\033[0m"


def _post(url, host, body):
    req = urllib.request.Request(
        url, data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json", **({"Host": host} if host else {})},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=15) as r:
        raw = r.read().decode(errors="replace").strip()
    return json.loads(raw) if raw else {}


# ---- admin setup -----------------------------------------------------------
def create_ns(ns):
    try:
        _post(f"{ADMIN}/v1/namespaces/{ns}/create", None, {})
    except urllib.error.HTTPError as e:
        if e.code not in (200, 409):  # 409 = already exists
            raise


def enable_attach(ns):
    with urllib.request.urlopen(f"{ADMIN}/v1/namespaces/{ns}/config", timeout=15) as r:
        cfg = json.load(r)
    cfg["allow_attach"] = True
    _post(f"{ADMIN}/v1/namespaces/{ns}/config", None, cfg)


# ---- hrana pipeline with baton threading -----------------------------------
def pipeline(host, sqls, baton=None, close=False):
    """Run sqls on one stream. If close=False, stream stays open and a baton is
    returned to continue on the SAME connection. Returns (steps, baton).
    steps: list of dicts {sql, ok(bool), rows(list[list]|None), cols, error}."""
    reqs = [{"type": "execute", "stmt": {"sql": s}} for s in sqls]
    if close:
        reqs.append({"type": "close"})
    for path in ("/v3/pipeline", "/v2/pipeline"):
        try:
            resp = _post(f"{BASE}{path}", host, {"baton": baton, "requests": reqs})
            break
        except urllib.error.HTTPError as e:
            if e.code == 404:
                continue
            # surface server rejection (e.g. 403 attach-forbidden) as a failed step
            return ([{"sql": sqls[0] if sqls else "", "ok": False, "rows": None,
                      "cols": [], "error": f"HTTP {e.code}: {e.read().decode(errors='replace')}"}], baton)
    steps = []
    for sql, res in zip(sqls, resp.get("results", [])):
        if res.get("type") == "error":
            steps.append({"sql": sql, "ok": False, "rows": None, "cols": [],
                          "error": res.get("error", {}).get("message", "?")})
            continue
        inner = res.get("response", {})
        result = inner.get("result", {}) if inner.get("type") == "execute" else {}
        cols = [c.get("name") for c in result.get("cols", [])]
        rows = [[(c.get("value") if isinstance(c, dict) and c.get("type") != "null" else "NULL")
                 for c in row] for row in result.get("rows", [])]
        steps.append({"sql": sql, "ok": True, "rows": rows if cols else None, "cols": cols, "error": None})
    return steps, resp.get("baton")


def show(steps):
    for s in steps:
        if not s["ok"]:
            print(f"    {RED}✗{RESET} {s['sql'].strip()[:70]}")
            print(f"        {RED}{s['error']}{RESET}")
        elif s["rows"] is not None:
            print(f"    {GREEN}✓{RESET} {s['sql'].strip()[:70]}")
            print(f"        {DIM}{' | '.join(s['cols'])}{RESET}")
            for r in s["rows"]:
                print(f"        {' | '.join(map(str, r))}")
        else:
            print(f"    {GREEN}✓{RESET} {s['sql'].strip()[:70]}")


def first_error(steps):
    for s in steps:
        if not s["ok"]:
            return s["error"]
    return None


def last_rows(steps):
    for s in reversed(steps):
        if s["ok"] and s["rows"] is not None:
            return s["rows"]
    return None


def main():
    global BASE, ADMIN
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default=BASE)
    ap.add_argument("--admin", default=ADMIN)
    a = ap.parse_args()
    BASE, ADMIN = a.base, a.admin

    print(f"{CYAN}== Setup: namespaces + seed =={RESET}")
    create_ns("catalog")
    create_ns("tenant1")
    enable_attach("catalog")
    pipeline("catalog.localhost", [
        "CREATE TABLE IF NOT EXISTS cards(id INTEGER PRIMARY KEY, name TEXT)",
        "DELETE FROM cards",
        "INSERT INTO cards(id,name) VALUES (1,'Pikachu'),(2,'Charizard'),(3,'Mew')",
    ], close=True)
    pipeline("tenant1.localhost", [
        "CREATE TABLE IF NOT EXISTS collection(id INTEGER PRIMARY KEY, card_id INTEGER, condition TEXT)",
        "DELETE FROM collection",
        "INSERT INTO collection(id,card_id,condition) VALUES (10,2,'NM'),(11,3,'LP'),(12,2,'MP')",
    ], close=True)
    print(f"  {GREEN}seeded catalog + tenant1{RESET}")

    H = "tenant1.localhost"
    results = {}

    def open_attached():
        """Open a held connection and ATTACH catalog once. Returns the baton."""
        _, baton = pipeline(H, ["BEGIN", 'ATTACH "catalog" AS cat', "COMMIT"], close=False)
        return baton

    def close_conn(baton):
        if baton:
            pipeline(H, [], baton=baton, close=True)

    # -- S0: is ATTACH connection-scoped or transaction-scoped? --------------
    print(f"\n{CYAN}== S0: is ATTACH connection-scoped? =={RESET}")
    print(f"  {DIM}held connection: BEGIN;ATTACH;COMMIT, then SELECT cat.cards OUTSIDE any txn{RESET}")
    baton = open_attached()
    steps, baton = pipeline(H, ["SELECT id,name FROM cat.cards ORDER BY id"], baton=baton, close=False)
    show(steps)
    results["attach_conn_scoped"] = steps[0]["ok"] and last_rows(steps) is not None
    close_conn(baton)

    # -- S1: CREATE TEMP VIEW supported at all? ------------------------------
    print(f"\n{CYAN}== S1: CREATE TEMP VIEW supported? =={RESET}")
    steps, _ = pipeline(H, ["BEGIN", 'ATTACH "catalog" AS cat',
                            "CREATE TEMP VIEW v_t AS SELECT id,name FROM cat.cards", "COMMIT"], close=True)
    show(steps)
    results["temp_view"] = not any(s["sql"].startswith("CREATE TEMP VIEW") and not s["ok"] for s in steps)

    # -- S2: CREATE TEMPORARY VIEW supported? --------------------------------
    print(f"\n{CYAN}== S2: CREATE TEMPORARY VIEW supported? =={RESET}")
    steps, _ = pipeline(H, ["BEGIN", 'ATTACH "catalog" AS cat',
                            "CREATE TEMPORARY VIEW v_ty AS SELECT id,name FROM cat.cards", "COMMIT"], close=True)
    show(steps)
    results["temporary_view"] = not any(s["sql"].startswith("CREATE TEMPORARY VIEW") and not s["ok"] for s in steps)

    # -- S3: PERMANENT view in tenant DB referencing the attached catalog ----
    print(f"\n{CYAN}== S3: permanent CREATE VIEW referencing cat.*, used on a FRESH connection =={RESET}")
    print(f"  {DIM}create the view once (conn A), then JOIN through it on a new conn B{RESET}")
    stepsA, _ = pipeline(H, ["BEGIN", 'ATTACH "catalog" AS cat',
                             "DROP VIEW IF EXISTS v_cards",
                             "CREATE VIEW v_cards AS SELECT id,name FROM cat.cards", "COMMIT"], close=True)
    show(stepsA)
    results["perm_view_create"] = not any(s["sql"].startswith("CREATE VIEW") and not s["ok"] for s in stepsA)
    print(f"  {DIM}-- new connection B: ATTACH then query unqualified through the permanent view --{RESET}")
    stepsB, _ = pipeline(H, ["BEGIN", 'ATTACH "catalog" AS cat',
                             "SELECT col.id, c.name, col.condition "
                             "FROM collection col JOIN v_cards c ON c.id = col.card_id ORDER BY col.id",
                             "COMMIT"], close=True)
    show(stepsB)
    rowsB = last_rows(stepsB)
    results["perm_view_join"] = (first_error(stepsB) is None and rowsB is not None and len(rowsB) == 3)
    print(f"  {DIM}-- conn C: query the permanent view withOUT attaching catalog (expect failure) --{RESET}")
    stepsC, _ = pipeline(H, ["SELECT * FROM v_cards LIMIT 1"], close=True)
    show(stepsC)
    results["perm_view_needs_attach"] = first_error(stepsC) is not None

    # -- S4: bare qualified alias, no view layer (baseline) ------------------
    print(f"\n{CYAN}== S4: qualified `cat.` reference, no view layer (baseline) =={RESET}")
    steps, _ = pipeline(H, ["BEGIN", 'ATTACH "catalog" AS cat',
                            "SELECT col.id, c.name FROM collection col JOIN cat.cards c "
                            "ON c.id = col.card_id ORDER BY col.id", "COMMIT"], close=True)
    show(steps)
    results["qualified_alias"] = (first_error(steps) is None and last_rows(steps) is not None)

    # -- summary -------------------------------------------------------------
    print(f"\n{CYAN}== FINDINGS =={RESET}")
    labels = {
        "attach_conn_scoped":   "ATTACH is connection-scoped (sticks after COMMIT; attach once per conn)",
        "temp_view":            "CREATE TEMP VIEW supported",
        "temporary_view":       "CREATE TEMPORARY VIEW supported",
        "perm_view_create":     "CREATE VIEW (permanent) over an attached ns succeeds",
        "perm_view_join":       "permanent view joins correctly on a fresh attached connection",
        "perm_view_needs_attach": "permanent view requires catalog attached at use time",
        "qualified_alias":      "qualified `cat.` reference works (no view layer)",
    }
    for k, label in labels.items():
        tag = f"{GREEN}YES{RESET}" if results.get(k) else f"{RED}NO {RESET}"
        print(f"  [{tag}] {label}")

    print(f"\n{CYAN}== ARCHITECTURE GUIDANCE =={RESET}")
    if results["temp_view"] or results["temporary_view"]:
        print("  TEMP/TEMPORARY views ARE supported — closest to the current rusqlite")
        print("  pattern: attach once at open, recreate temp views, query unqualified.")
    elif results["attach_conn_scoped"] and results["perm_view_join"]:
        print("  RECOMMENDED PATTERN:")
        print("   * TEMP views are unsupported (can't be replicated), but ATTACH is")
        print("     connection-scoped and PERMANENT views over cat.* work.")
        print("   * Define permanent catalog views ONCE in the tenant schema_user.sql")
        print("     (e.g. CREATE VIEW cards AS SELECT * FROM cat.cards).")
        print("   * At each connection open: `BEGIN; ATTACH \"catalog\" AS cat; COMMIT` once.")
        print("   * Query code stays UNQUALIFIED against the views — minimal change.")
        print("   The TEMP-VIEW-at-open step becomes ATTACH-at-open; views move to schema.")
    elif results["attach_conn_scoped"] and results["qualified_alias"]:
        print("  ATTACH is connection-scoped but permanent views over cat.* don't work.")
        print("  Drop the view layer; attach once at open and qualify catalog refs with")
        print("  the `cat.` alias in queries.")
    else:
        print("  Catalog refs must be qualified with `cat.` inside an ATTACH transaction;")
        print("  no persistent view layer is available.")

    return 0 if (results["qualified_alias"] or results["perm_view_join"]) else 1


if __name__ == "__main__":
    sys.exit(main())
