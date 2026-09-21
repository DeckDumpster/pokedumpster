# What "log out" means across deployments

> **Verdict: BUILD IT — but as a fourth *declared* config value, never an
> inferred one, and only alongside an origin-side refusal of
> `/cdn-cgi/access/*`.** The affordance is absent by default and cannot be
> configured into the shape that fails silently.
>
> Spike bead `db-lxqx`. Written 2026-09-21. Proof-of-concept branch:
> `spike/db-lxqx-poc` at `aba8e39` — `poc/probe.sh`, `poc/issuer_predicate.py`,
> `poc/README.md`. **Unmerged, and local to the deployment box**: aeons carry
> no push credentials, so if that branch matters to you, push it or re-run the
> two scripts from the recipe in §8. Sources preserved verbatim under
> [`sources/`](sources/).

**Where this lives, and why not `deep-dives/`.** The bead asked for
`deep-dives/<topic>/RESULT.md`, in the shape the existing dives use, and that
is the shape this follows. It is under `docs/spikes/` instead because the
landing gate for a spike branch refuses a commit touching anything outside
that directory. If a later change moves it, `deep-dives/README.md`'s table is
where the row belongs.

---

## 1. The question

Multi-tenancy went live on prod on 2026-09-21 and the operator's first question
afterwards was how to sign out. There is no answer in the UI. The obvious
answer — link to `/cdn-cgi/access/logout` — is wrong outside production,
because pkdump verifies JWTs it never issues and the issuer is not Cloudflare
in every deployment.

**What would count as an answer:** a mechanism that (a) logs a real user out on
prod, (b) renders *nothing* rather than something broken where the issuer is
not Cloudflare, and (c) cannot be put into the state where a user clicks "log
out", believes it worked, and is still signed in. A previous attempt at this
(`db-7r13`) was closed unimplemented for proposing (b) via a condition that is
true everywhere.

---

## 2. What was found

Claims below are marked **[observed]** (measured or read out of this tree),
**[documented]** (read from a preserved upstream source), or **[inferred]**.

### 2.1 The silent-failure trap is real, and it is wider than the logout URL

**[observed — POC `poc/probe.sh`, run 2026-09-21 against `target/debug/pkdump`
built from this tree]** The real `pkdump serve` binary, over the committed UI
fixture, with Cloudflare Access verification ON and pointed at
`tests/lib/test_jwks.py` — the same configuration `tests/tenants/handles.sh`
runs. One process, one run:

| request | status | body |
|---|---|---|
| `GET /api/sets` — **valid JWT** (the control) | `200` | JSON |
| `GET /cdn-cgi/access/logout` | `200` | the SPA shell |
| `GET /cdn-cgi/access/get-identity` | `200` | the SPA shell |
| `GET /api/auth-capabilities` (a route that does not exist) | `200` | the SPA shell |
| `GET /api/sets` — no JWT | `401` | — |
| `GET /api/collection` — no JWT | `401` | — |
| `GET /api/backup-status` — no JWT | `200` | JSON |

The first row is the control and it is load-bearing: without it the `401`s
below are equally consistent with a server that was broken for some unrelated
reason, and §2 would read as a security property it had not demonstrated.

Two things follow. First, the trap the bead names is confirmed rather than
assumed: the router in `crates/pkdump-server/src/lib.rs` ends in
`.fallback(get(spa))`, so **every** unmatched path — `/cdn-cgi/...` included —
answers `200` with the application shell. A logout link pointing at a
same-origin path where nothing intercepts it does not 404, does not error, and
leaves the user signed in with the app rendered in front of them.

Second, the last row is the delivery path for the fix: `/api/backup-status`
answers `200` **while `/api` is otherwise fail-closed at that same instant**,
because `routes::public_api_router()` is mounted outside the two auth layers
(`lib.rs`, the `authenticated_api` / `public_api` split). A capability the
frontend must read *before* it has an identity has an existing, reviewed home.

### 2.2 "Cloudflare is the issuer" is not something this app can infer

**[observed]** Every issuer literal in the tree that reaches the verifier's
pinned `iss`:

| value | where | how it reaches the verifier |
|---|---|---|
| `https://test.cloudflareaccess.com` | `crates/pkdump-server/src/access.rs:461` | `AccessConfig.team_domain` in `TestAccessFixture` — every `cargo test` of the auth path |
| `https://test.cloudflareaccess.com` | `tests/lib/test_jwks.py:81` | → `PKDUMP_ACCESS_TEAM_DOMAIN` at `tests/tenants/handles.sh:242` — the shipped image, container tier |
| `https://myteam.cloudflareaccess.com` | `deploy/config-lib.sh:32` | the scaffold placeholder an operator fills in |

`poc/issuer_predicate.py` evaluates the bead's cheap candidate — *default the
logout URL when the issuer is a `cloudflareaccess.com` team domain* — over
exactly that list:

```
  True   https://test.cloudflareaccess.com    crates/pkdump-server/src/access.rs:461
  True   https://test.cloudflareaccess.com    tests/lib/test_jwks.py:81
  True   https://myteam.cloudflareaccess.com  deploy/config-lib.sh:32

VERDICT: the predicate is True for EVERY configured issuer in the tree.
         It does not distinguish production from anything else.
```

This is `db-7r13`'s bug with one more step in it. The test fixture names itself
`test.cloudflareaccess.com` **on purpose** — it also serves its JWKS at
`/cdn-cgi/access/certs` — so that non-prod runs the same verification path
without the code having to know it is a test. That deliberate fidelity is
exactly what destroys the heuristic: the issuer string is a fixture value, not
a fact about who is in front of the origin.

There is no other signal available. `access.rs` holds the team domain, the
audience, the JWKS URL and a decoding key; none of them says whether a
Cloudflare edge is terminating requests. **Nothing the app can currently
observe distinguishes prod from a test instance.**

### 2.3 Cloudflare's logout is a real revocation — at the edge

**[documented — [`sources/cloudflare-session-management.md`](sources/cloudflare-session-management.md) line 200; upstream <https://developers.cloudflare.com/cloudflare-one/access-controls/access-settings/session-management/>, retrieved 2026-09-21]**

> This action revokes the user's session across all applications. Access will
> immediately clear the authorization cookie from the user's browser, and all
> previously issued tokens will stop being accepted in 20-30 seconds. The only
> difference between these two URLs is which domain the authorization cookie is
> deleted from. For example, going to
> `<your-application-domain>/cdn-cgi/access/logout` will remove the application
> cookie and make the logout action feel more instantaneous.

Three consequences.

- **Both logout URLs revoke globally.** The app-domain form and the
  `<team>.cloudflareaccess.com` form differ only in which cookie the browser
  drops. There is no per-application logout: *"end users cannot log themselves
  out on a per-application basis"* (same source).
- **The app-domain form is the better one for this app**, because the 20-30s
  during which the app cookie is still in the browser is itself a "I clicked
  log out and I am still signed in" window — the precise experience this bead
  exists to prevent.
- **The revocation binds the edge, not this origin.** `access.rs` verifies
  RS256 + `iss` + `aud` + `exp` + `nbf` against a cached JWKS and makes no call
  to Cloudflare. **[observed]** On any request path that does not traverse the
  Cloudflare edge, an already-issued token keeps working until `exp`, logged
  out or not.

**[documented — same source, lines 55 and 83]** `exp` is the Access
*application session duration*: default **24 hours**, settable from immediate
timeout to one month. The *global* session duration (how often the IdP
re-prompts) is separately defaulted to 24 hours.

### 2.4 The origin is published on the host, and this box is not prod

**[observed]** `deploy/pkdump.container` carries `PublishPort={{PORT}}:8080`;
`CLAUDE.md` says the operator reaches the box over WireGuard. If both hold on
the prod box there is a request path with no Cloudflare edge on it, and on that
path the 20-30s revocation does not apply — the window is the full `exp`.

**[could not check]** I could not confirm either fact. The box this spike ran
on is `agent-swarm`: `systemctl --user list-units` shows no `pkdump-prod`
unit, `~/.config/containers/systemd/` is empty, and `~/.config/pkdump/` holds
`alerts.env`, `lake.env` and `store.env` but no instance directory and no
`access.env`. Prod's Access session duration is likewise console-only.
Escalated to the operator (question with defaults: 24h, and treat the direct
path as a real residual). **Nothing in the recommendation turns on either
answer.**

### 2.5 There is no staging deployment

**[observed]** Question 1 of the bead asks for staging's identity story
concretely. There isn't one. `staging` appears in this tree exactly three
times, all hypothetical: `deploy/alert-gate.sh:30` offers
`PKDUMP_ALERT_INSTANCES="prod staging"` as an override *"for a second real
deployment"*, and `tests/deploy/run.sh:3338` installs an instance named
`staging` to exercise that override. No unit, no port, no `access.env`, no
documented hostname.

So the concrete non-prod identity story is the one the container tier runs:
`tests/lib/test_jwks.py` mints a fresh RSA-2048 keypair per invocation, serves
its JWKS at `/cdn-cgi/access/certs`, and `tests/tenants/handles.sh` hands the
resulting tokens to `curl` with `-H Cf-Access-Jwt-Assertion`. **There is no
browser, no cookie, no login flow and therefore nothing a logout could end.**
`deploy/setup.sh --test` scaffolds `access.env` fully commented out, so an
ordinary CI instance runs with no Access at all.

That makes the design target sharper than the bead assumed: the question is not
"what does logout mean in staging" but "how does an environment with **no
session concept whatsoever** render nothing, mechanically, without the code
having to recognise itself as a test".

### 2.6 `/cdn-cgi/` is Cloudflare's, and the origin should never see it

**[documented — [`sources/cloudflare-cdn-cgi-endpoint.md`](sources/cloudflare-cdn-cgi-endpoint.md); upstream <https://developers.cloudflare.com/fundamentals/reference/cdn-cgi-endpoint/>]**

> This endpoint is managed and served by Cloudflare. It cannot be modified or
> customized.

**[inferred]** In a Cloudflare deployment the edge answers `/cdn-cgi/access/*`
and never proxies it, so an origin-side refusal of that prefix is unreachable
in prod and reachable in exactly the environments where the path is a lie. I
could not verify this against prod from here; it is named as an acceptance
criterion (§7, AC-11) rather than assumed.

---

## 3. The options

### Option A — a fourth *declared* env var, absent by default

`PKDUMP_ACCESS_LOGOUT_URL`, independent of the three that exist, with **no
default and no derivation**. Unset means no affordance at all. Surfaced to the
frontend through a route on `routes::public_api_router()`, so it is readable
before the client has an identity.

- **Cost — ~285 lines across 8 files.** Derived by counting the analogous
  pieces in this tree rather than estimating: `routes/backup.rs`, the exact
  template for a new public route, is **75 lines** including its doc header;
  `tests/tenants/handles.sh` is **295 lines for 8 sections**, ~37 lines each.
  Budget: ~80 lines of Rust (one `AccessConfig` field plus its startup
  validation, a ~30-line `routes/auth.rs` with a `ts-rs` type, a ~10-line
  `/cdn-cgi/access/{*rest}` refusal, wiring), ~90 lines of Rust tests, ~40
  lines of Svelte in `+layout.svelte` and `api.ts` (the TypeScript type is
  generated free by `cargo test`), ~45 lines of bash for two new
  `handles.sh` sections, ~25 lines of docs in `deploy/TENANTS.md` and the
  `deploy/config-lib.sh` scaffold.
- **Risk — an operator sets it wrong.** The failure is bounded by the startup
  validation in §4: a relative path (the only form that the SPA fallback can
  swallow) is refused before the server binds. A wrong *host* still resolves to
  a browser-visible error rather than to this app. The residual is an operator
  copying prod's `access.env` onto a non-Cloudflare instance, which navigates
  the user to prod — visible, not silent.

### Option B — infer it from the issuer (`*.cloudflareaccess.com`)

- **Cost — ~10 lines, no new configuration.** Genuinely the cheapest thing that
  could work.
- **Risk — fatal, and measured.** §2.2: the predicate is `True` for all three
  configured issuers in the tree. It renders a Cloudflare-only link in every
  environment that runs the auth path, which is `db-7r13`'s rejected design
  reached by a different route. **Reject.**

### Option C — the app owns its own session cookie

Issue an app-signed cookie after the first verified JWT; logout clears it.

- **Cost — 400-800 lines plus a key-custody runbook and a restore scenario.**
  Basis: the comparable in-tree component for "this app holds a secret" is
  `crates/pkdump-keys` at **2,248 lines** with a **245-line** `deploy/KEYS.md`
  behind it, and that is for a key that signs nothing user-facing. A session
  signer is smaller, but it inherits the same obligations: a key file at mode
  600, a password-manager backup, a rotation story, and a new scenario in
  `deploy/RESTORE.md`.
- **Risk — it crosses a load-bearing boundary and still does not log anyone
  out.**
  - `VerifiedIdentity`'s constructor is private to `access`, and
    `deploy/TENANTS.md`'s isolation argument rests on it: *"A request cannot
    reach a database without a `VerifiedIdentity` in scope … its constructor is
    private."* A cookie path is a **second source** of that type, so the
    property becomes "two things can mint identity" and every argument built on
    it needs re-making.
  - And the logout it buys is fake. §2.3's two-token table: the global session
    token lives on the team domain and the application token on the app
    hostname. Clearing an app-owned cookie touches neither, so the next request
    re-authenticates at the edge and hands the app a fresh valid JWT for the
    same identity — a button that logs you out and instantly back in. **Reject
    on mechanism, not on effort.**

### Option D — do nothing; document the URL in the runbook

- **Cost — 0 lines of code, ~10 lines in `deploy/TENANTS.md`.**
- **Risk — the operator's question stays unanswered in the UI**, and the
  "signed in as" gap in `deploy/TENANTS.md` §"What is not here yet" stays open.
  But it is never *wrong*, in any deployment. This is the honest baseline and
  the fallback if §6's assumption fails.

---

## 4. Recommendation

**Option A, with two non-negotiable constraints and one addition.**

**A fourth variable, `PKDUMP_ACCESS_LOGOUT_URL`.**

1. **It is declared, never derived.** Not from `PKDUMP_ACCESS_TEAM_DOMAIN`, not
   from a hostname, not from a build flag. §2.2 is the evidence: the issuer is
   a fixture value in every environment that runs the auth path.
2. **Absent is legal and means absent.** Unset → the capability route reports
   `null` → the UI renders *nothing*. Not a disabled button, not a tooltip,
   not a placeholder. There is no session to end in a `curl`-driven container
   gate, and the right amount of chrome for that is none.
3. **Set-but-invalid is a startup refusal, and the validation is the whole
   safety argument.** The value must parse as an absolute URL with scheme
   `https` and a non-empty host. **A relative path is refused by name**, with a
   message saying that it would be served by this application's own SPA
   fallback and the logout would silently do nothing. This is what makes the
   silent state *unconfigurable* rather than merely discouraged: the one URL
   shape the fallback can swallow cannot be accepted.
   It follows the all-or-none idiom already in `AccessConfig::from_env_if_configured`
   — setting it while the other three are unset is a misconfiguration too.
4. **Prod sets the app-domain form**, `https://<app-host>/cdn-cgi/access/logout`.
   Both forms revoke globally (§2.3); this one also drops the app cookie
   immediately, and the 20-30s window the team-domain form leaves *is* the
   "still signed in after clicking log out" experience being designed against.

**Plus: the origin refuses `/cdn-cgi/access/*` itself.** ~10 lines: an explicit
route ahead of the fallback returning a non-200 with a plain-text body naming
the cause. This is the addition that earns the recommendation, because it
attacks the trap at its root instead of at its configuration. Today that prefix
returns `200` and the app shell (§2.1, measured), so **any** deployment where
somebody reaches for a same-origin Cloudflare path — this design, a runbook, a
bookmark, a future `get-identity` line — fails visibly instead of silently. In
a Cloudflare deployment the edge answers first and the refusal is unreachable
(§2.6).

**Delivery to the frontend** is a route on `routes::public_api_router()`
answering `{"logout_url": <string|null>}`. §2.1 measured that this mount
answers `200` with no JWT while `/api` is otherwise 401, which is the property
required — the client must ask before it has an identity. The response is the
same for every caller and **carries no identity**: a "signed in as" line needs
the email and the email must not go on an unauthenticated route, so that half
belongs on an authenticated route and is a separate decision.

**On invalidation (bead question 3):** logout *does* invalidate on prod — at
the edge, in 20-30 seconds, across all applications (§2.3). This app performs
no revocation check of its own and cannot: it has no signing key, no session
store, and no call to Cloudflare. So the honest statement is *"the edge revokes;
this origin trusts `exp`"*, and the shortest window it tolerates is the Access
application session duration — assumed to be Cloudflare's 24h default
(escalated; §2.4). Nothing about the recommendation changes if that number is
different; what changes is the size of the residual in §5.

---

## 5. The one load-bearing assumption

> **A deployment's operator knows whether an identity gateway is in front of
> the origin, and the app does not and cannot.**

Everything above follows from it. Configuration is the only channel that
carries that knowledge (Option A), inference cannot (Option B, measured), and
an app-owned session would be the app trying to answer a question about its own
deployment topology (Option C).

**Named residuals**, none of which the recommendation resolves:

- **A direct path to the origin has no revocation.** If prod's published port is
  reachable over WireGuard (§2.4, unverified), a logged-out-but-unexpired token
  is accepted there for the full session duration. This is true today and is
  not made worse by shipping a logout button; it is filed separately rather
  than folded in.
- **Copying `access.env` between instances** puts prod's absolute logout URL on
  a non-Cloudflare box. The user is navigated to prod — visible, but confusing.
- **The `/cdn-cgi/access/*` refusal is unreachable in prod by inference, not by
  measurement** (§2.6). AC-11 turns that into a manual check.

---

## 6. The falsifier

The recommendation is wrong if **any of these becomes true**, and each is
checkable:

1. **The issuer becomes discriminating.** If `tests/lib/test_jwks.py` and
   `access.rs`'s `TestAccessFixture` ever stop naming a `cloudflareaccess.com`
   issuer, Option B becomes viable and the fourth env var is dead weight.
   Check: re-run `poc/issuer_predicate.py`; it exits non-zero the moment the
   predicate discriminates.
2. **The SPA fallback stops swallowing unmatched paths.** If `lib.rs` grows a
   real 404 for unmatched non-`/api` routes, the trap disappears and the
   startup URL validation loses most of its value (the button would fail
   loudly on its own). Check: re-run `poc/probe.sh` §1; it fails when
   `/cdn-cgi/access/logout` stops answering `200` with the shell.
3. **A second real deployment appears with a non-Cloudflare browser login
   flow** — the staging box §2.5 says does not exist. Then `logout_url` stops
   being a Cloudflare path in practice, which the design already permits (it is
   an opaque absolute URL), but "signed in as" and session lifetime would need
   re-answering per issuer.
4. **Cloudflare changes the logout contract.** The 20-30s revocation and
   "revokes across all applications" are quoted from a doc page dated
   *Last updated Sep 4, 2026*. Check: diff
   [`sources/cloudflare-session-management.md`](sources/cloudflare-session-management.md)
   against the live page.
5. **Cloudflare begins proxying `/cdn-cgi/*` to the origin.** The
   origin-side refusal would then break prod's logout outright. AC-11 catches
   it once; it would need re-checking on any change to the edge configuration.

---

## 7. Acceptance criteria for the build bead

Concrete enough to carry. **AC-3, AC-7 and AC-10 are the negative cases** — a
gate that only exercises the configured path passes just as happily with the
condition inverted, which is how `db-7r13` nearly shipped.

**Configuration**

- **AC-1** `PKDUMP_ACCESS_LOGOUT_URL` is read as a fourth independent value.
  Unset is legal.
- **AC-2** Set while the other three Access vars are unset → the server refuses
  to start, extending the existing all-or-none rule and naming the variable.
- **AC-3 (negative)** Set to `/cdn-cgi/access/logout` — a relative path — → the
  server refuses to start, and the message says that the path would be served
  by this application's own SPA fallback. Also refused: `http://` and any value
  with no host. Seen red: delete the validation and the test fails.
- **AC-4 (drift ratchet)** No code path constructs a logout URL from
  `PKDUMP_ACCESS_TEAM_DOMAIN`. Stated over the tree, not over one call site —
  the failure mode is a helper nobody has written yet, and §2.2 shows why a
  reviewer would accept the derivation as obviously correct.

**Delivery**

- **AC-5** The capability route is mounted in `routes::public_api_router()` and
  answers `200` with `{"logout_url": <string|null>}` **with no JWT, while the
  Access layer is active and `/api/collection` answers 401 in the same run**.
- **AC-6** The response carries nothing tenant-identifying — no email, no
  handle, no `sub`. It is byte-identical for every caller.

**Affordance**

- **AC-7 (negative)** `logout_url: null` → the DOM contains no logout control
  at all. Asserted at **both viewports**, per `pd-4tce`: a one-viewport
  approval is a stale baseline, not a smaller one.
- **AC-8** `logout_url` present → an anchor whose `href` is that string
  verbatim, not rewritten, relativised, or suffixed.

**The loud failure**

- **AC-9** `GET /cdn-cgi/access/logout` against the shipped image returns a
  non-200 whose body is not the SPA shell. Today it is `200` + the shell
  (§2.1), so this is a fail-first assertion.
- **AC-10 (both arms in one gate)** `tests/tenants/handles.sh` is the home: it
  already runs the shipped image with Access ON and a non-Cloudflare issuer,
  which is the environment this whole spike is about. One section with the var
  unset (capability `null`, no control, `/cdn-cgi/access/logout` refused) and
  one with it set to a fake absolute URL (reported verbatim).
- **AC-11 (manual, on prod, before this is called done)** The operator confirms
  `https://<app-host>/cdn-cgi/access/logout` still signs them out — i.e. the
  edge still intercepts it and the origin's refusal is unreachable. No gate on
  the build box can make this check (§2.4).

**Documentation**

- **AC-12** `deploy/TENANTS.md` gains the variable, prod's value, and the
  sentence that absent means absent; `deploy/config-lib.sh`'s `access.env`
  scaffold gains a commented fourth line. `deploy/TENANTS.md` §"What is not
  here yet" is updated to say the logout half has landed and the "signed in as"
  half has not.

**Explicitly out of scope**, and it is a separate decision: the "signed in as"
line. It needs the verified email, the email must not travel on an
unauthenticated route (AC-6), and `/cdn-cgi/access/get-identity` is the same
same-origin trap as the logout path.

---

## 8. Reproducing the measurements

```bash
git checkout spike/db-lxqx-poc
python3 poc/issuer_predicate.py                 # §2.2, exits non-zero if the
                                                #   predicate ever discriminates
PATH="$HOME/.cargo/bin:$PATH" cargo build -p pkdump-cli --bin pkdump
bash poc/probe.sh target/debug/pkdump           # §2.1
```

`probe.sh` stands the real binary up against the committed UI fixture with
Access configured to point at `tests/lib/test_jwks.py` — the same shape
`tests/tenants/handles.sh` runs, minus the container. It needs `python3` with
`pyjwt` and `cryptography` (present on this box: 2.10.1 / 46.0.5).

**Timing, for whoever sizes the follow-on work.** This checkout had no
`target/` at all, so this is a genuinely cold build with a warm
`~/.cargo/registry`:

| step | wall clock |
|---|---|
| `cargo build -p pkdump-cli --bin pkdump`, cold `target/` | **11m 29s** (`user` 6m 15s, `sys` 57s) |
| `poc/probe.sh` — JWKS server, `tenant adopt`, server start, 12 requests | **~3s** |
| `poc/issuer_predicate.py` | instant, no build |

The compile dominates by three orders of magnitude, and most of it is
`aws-lc-sys` building its C sources. Anyone iterating on the build bead should
expect the *first* cycle to cost ~12 minutes and everything after it to cost
seconds.

**One trap worth passing on.** `/usr/bin/cargo` on this box is rustc 1.93.1 and
`rust-toolchain.toml` pins 1.94, so a bare `cargo build` fails in under a
second with *"rustc 1.93.1 is not supported by the following packages"* naming
twenty `aws-*` crates — which reads like a dependency problem and is not one.
`~/.cargo/bin` (the rustup shim, which honours the pin) is not on the default
`PATH`. Put it there first.

## 9. What could not be established

Marked out so a later reader does not mistake silence for a finding:

- **Prod's Access application session duration** — console-only, escalated
  (§2.4). Assumed to be Cloudflare's documented 24h default.
- **Whether prod's published port is reachable without the Cloudflare edge**
  — not observable from this box (§2.4). Filed as `db-a5g0`.
- **That `/cdn-cgi/access/*` is never proxied to the origin in a Cloudflare
  deployment** — documented as Cloudflare-managed and not customizable (§2.6),
  but inferred rather than measured against prod. AC-11 is the check.
- **What `GET /cdn-cgi/access/logout` returns to an unauthenticated caller at
  the edge** — not probed; doing so would have meant hitting the operator's
  production hostname from an agent box, which is not this bead's to do.
