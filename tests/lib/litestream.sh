#!/usr/bin/env bash
# litestream.sh — "does this replica hold data?", once, for every harness.
# Sourced, never executed.
#
# ── WHY THIS FILE EXISTS (pd-reyy, and pd-nt1k before it) ───────────────────
# `litestream ltx -level all <url>` has THREE outcomes and the obvious predicate
# collapses them into two — wrongly, at both ends. Measured against v0.5.16:
#
#   the replica holds LTX files   exit 0, a column header AND one row per file
#   the prefix has never been     exit 0, THE COLUMN HEADER ALONE
#     written to
#   the query could not be made   exit 1, NOTHING on stdout, `Error: …` on stderr
#     (creds, network, S3)
#
# So `ltx … 2>/dev/null | grep -q .` — the predicate three harnesses used — reads
# an EMPTY replica as full and an UNREACHABLE S3 as empty. Both halves have cost
# a real gate:
#
#   * pd-nt1k: tests/alarming/run.sh said "replicating" while the sidecar had
#     exited at startup and nothing had ever reached the bucket, so a dead
#     sidecar reached §3 wearing a green §2. Fixed there, in that one file.
#   * pd-reyy: tests/litestream/recreate.sh §4 reported `and the replica outlives
#     it — the retention window is open (expected yes, got no)` on CI run
#     32973744522 attempt 3 — and §6, SECONDS LATER and against the same URL,
#     restored old alice's card from that replica, complete and
#     `integrity_check = ok`. The replica was never gone. One `ltx` call had
#     failed to reach MinIO, and a failed query is the only way that predicate
#     can say "no". Six occurrences, six re-runs, and each one re-ran a suite
#     that had proved the opposite three assertions later.
#   * And the quiet half of the same bug, never yet seen as a failure because it
#     cannot fail: tests/litestream/drill.sh's `every tenant is replicating from
#     the one sidecar` passed the instant S3 answered, whether or not a single
#     byte had been replicated. A vacuous green is not a flake; nothing reports
#     it at all.
#
# THE THREE ANSWERS ARE KEPT APART, because two of them call for opposite
# responses: an empty replica is a fact about the data, and a failed query is a
# fact about the network. `deploy/backup-check.sh::ltx_list` has kept them apart
# on the production path since it was written ("'we could not ask' and 'the
# answer is fine' must never be the same outcome"); this is that idea for the
# harnesses, in the one place a fourth harness will find it.
#
# THE QUERY IS RETRIED; THE ANSWER IS NOT. `ltx_listing` re-asks a query that
# FAILED, up to LTX_QUERY_ATTEMPTS times — that is what closes pd-reyy, and it
# masks nothing: a replica that is genuinely empty answers `empty` on the first
# attempt and is never re-asked, and a replica that is genuinely gone stays gone
# however many times it is listed. Retrying the ANSWER would be the mistake this
# whole file is about.
#
# THE LISTING IS PARSED FORMAT-AGNOSTICALLY, by the SHAPE of a TXID — 16 hex
# characters, which every row carries twice (min_txid, max_txid) and no column
# header, level, size or timestamp can be mistaken for. The column order has
# shifted across litestream versions; `deploy/backup-check.sh::ltx_max_txid`
# reads it this way for that reason and this agrees with it deliberately, so a
# harness and the production checker cannot come to different conclusions about
# one bucket.
#
# tests/lib/litestream_test.sh is the gate: it drives these functions against
# recorded `ltx` output for all three outcomes, proves the retry, and asserts on
# the TREE that no harness has grown a fourth copy of the inverted predicate.

# How many times a FAILED query is re-asked, and how long between attempts. A
# knob for the self-test, not a tuning parameter: three attempts is enough to
# ride out a hiccup and few enough that a genuinely unreachable bucket is
# reported in seconds rather than minutes.
LTX_QUERY_ATTEMPTS=${LTX_QUERY_ATTEMPTS:-3}
LTX_QUERY_RETRY_SECONDS=${LTX_QUERY_RETRY_SECONDS:-2}

# ltx_listing <runner> <url> — one `ltx -level all` listing.
#
#   stdout  the listing, when the query was answered
#   stderr  the last non-blank line litestream printed, when it was not
#   return  0 answered / 1 could not ask
#
# <runner> is the CALLER's own litestream invocation, by function name — every
# harness wears different podman flags (its own network, its own credential
# mounts, its own image variable) and none of that belongs here. The runner is
# called as `<runner> ltx -level all <url>`, which is the shape `ls_run` and
# `ls_cli` already have.
ltx_listing() {
	local runner="$1" url="$2"
	local err out attempt
	err="$(mktemp "${TMPDIR:-/tmp}/pd-ltx.XXXXXX")"
	for (( attempt = 1; attempt <= LTX_QUERY_ATTEMPTS; attempt++ )); do
		if out="$("$runner" ltx -level all "$url" 2>"$err")"; then
			rm -f "$err"
			printf '%s\n' "$out"
			return 0
		fi
		if [ "$attempt" -lt "$LTX_QUERY_ATTEMPTS" ]; then
			sleep "$LTX_QUERY_RETRY_SECONDS"
		fi
	done
	grep -v '^[[:space:]]*$' "$err" | tail -n1 >&2
	rm -f "$err"
	return 1
}

# replica_state <runner> <url> — exactly one of:
#
#   data            the replica holds LTX files
#   empty           the prefix exists as far as S3 is concerned and holds none
#   error: <line>   the query could not be made; <line> is litestream's own
#
# One word, so a caller can `check "…" "data" "$(replica_state ls_run "$url")"`
# and have the FAIL line name which of the two failures it is. The message rides
# on the same line deliberately: a diagnostic printed anywhere else arrives
# detached from the assertion it explains, and this is a function that only ever
# runs inside `$(…)`, where nothing it sets in a variable survives.
replica_state() {
	local out err rc
	err="$(mktemp "${TMPDIR:-/tmp}/pd-ltxstate.XXXXXX")"
	out="$(ltx_listing "$1" "$2" 2>"$err")" && rc=0 || rc=$?
	if [ "$rc" -ne 0 ]; then
		printf 'error: %s\n' "$(tr '\n' ' ' <"$err" | cut -c1-200)"
		rm -f "$err"
		return 0
	fi
	rm -f "$err"
	if grep -qE '\b[0-9a-fA-F]{16}\b' <<<"$out"; then
		echo data
	else
		echo empty
	fi
}

# replica_holds_data <runner> <url> — the predicate, for `wait_until`.
#
# TRUE only for `data`. An error is not an absence, but it is not a presence
# either, so a poll goes on polling; a caller that has to tell `empty` from
# `error` asks `replica_state` and prints the answer.
replica_holds_data() { [ "$(replica_state "$1" "$2")" = data ]; }

# pitr_marker [lead_seconds] — name an instant a point-in-time restore can
# actually resolve. Echoes the timestamp; sleeps first, deliberately.
#
# ── WHY THE SLEEP IS PART OF NAMING A MARKER ────────────────────────────────
# `date -u +…%SZ` truncates DOWN to the start of the second it was taken in, so
# a marker taken at 00:49:54.9 IS 00:49:54.000 — up to a second EARLIER than the
# instant it was meant to name. If the replica only became restorable at
# 00:49:54.3, that marker sits BEFORE the oldest instant the replica covers, and
# Litestream refuses it outright:
#
#     Error: timestamp does not exist
#
# which surfaces in a harness as `expected 1, got <no restored db>` — a restore
# that produced no file at all, and one that is immune to retrying because the
# marker is a constant.
#
# It is reachable exactly when replication is FAST, because that is when the
# marker lands in the same second the data arrived. With E the instant the
# replica became restorable and M the moment the marker is taken, waiting for
# M >= E + 1s gives floor(M) >= M - 1s >= E — the marker provably sits at or
# after the oldest covered instant, whatever the truncation does. So: CALL THIS
# ONLY ONCE THE THING YOU WILL RESTORE TO IS ALREADY IN THE REPLICA, and leave
# the second it arrived in before naming an instant inside it.
#
# The caller still has to wait on the OTHER side: the marker names the whole
# second it was taken in, so a write meant to fall after it has to land in a
# later one, or a restore at the marker may legitimately include it. There is no
# condition to poll for that — it is the clock's resolution — so it stays a
# `sleep 2` at the call site, where it reads next to the write it is separating.
#
# ── WHY IT IS A FUNCTION AND NOT A NOTE ─────────────────────────────────────
# It was a note. tests/litestream/run.sh §3 carried the derivation above in a
# 20-line comment and the `sleep 1` that follows from it, and §4b — a registry
# PITR in the SAME FILE — took its marker with a bare `date`. That was safe only
# because §4b's marker was taken late, against a long-established replica; §3's
# comment said so in as many words. Then the fix for an unrelated race put a
# `wait_until`-poll for `registry_replicated` immediately before it, which
# moved that marker onto the exact edge the comment describes, and the
# registry's two PITR assertions went red in CI while a local run passed 61/0.
#
# A rule that lives in a comment protects the section the comment is in. This is
# that rule as the only way to name a marker; litestream_test.sh §7 is the
# ratchet that keeps a sixth call site from spelling it by hand.
pitr_marker() {
	sleep "${1:-1}"
	date -u +%Y-%m-%dT%H:%M:%SZ
}
