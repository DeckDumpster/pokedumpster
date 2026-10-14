# scripts/

Tools that sit alongside the main app — pkmn.gg ingest, etc.

## `pkmngg_export.user.js` — pkmn.gg → PokeDumpster CSV (recommended)

A Tampermonkey / Violentmonkey userscript that runs *inside* a logged-in
pkmn.gg tab. This is the recommended path: it inherits the browser
session you already have, which is the one thing the headless fetcher
below cannot do (see pokedumpster-p6y.3).

### Install

1. Install Tampermonkey or Violentmonkey.
2. Add `scripts/pkmngg_export.user.js` as a new script.
3. Open pkmn.gg, logged in. A panel appears bottom-right showing how
   many API responses have been captured.

### Use

Open your collection, then click **Export**. The script walks pkmn.gg's
collection API to the end — it does *not* depend on you scrolling the
grid — and downloads:

- `pokedumpster-pkmngg-<ts>.csv` — upload at `/ingest/csv`, format
  **PokeDumpster (pkmn.gg export)**.
- `pokedumpster-pkmngg-<ts>.json` — the lossless capture: every API
  response it saw, `Authorization` and `Cookie` redacted so the file is
  safe to send on.
- `pkmngg-tcgplayer-mass-entry-<ts>.txt` and `pkmngg-ptcg-live-<ts>.txt`
  — only when pkmn.gg supplied the codes, which it does on the
  collection endpoint. See "Exit path" below.

### How it gets the data

Preferred, and confirmed by recon rather than guessed:

```
GET api.tcg.gg/pkmn/v1/page/u/<username>/collection?pageSize=60[&cursor=…]
  → {cards: [{card, variant, quantity}], nextCursor, facets, showPrivate}
```

Cursor-paginated, `pageSize` caps at 60; a ~330-card collection is six
requests. The script discovers the username from the route the app
already called, from `/u/<name>` in the address bar, or from the
`auth/me` payload, and replays the app's own bearer token — so
`showPrivate` covers cards a public profile view would omit.

If that route ever moves, it falls back to replaying whichever captured
request actually yielded rows, advancing whatever pagination it uses.
Both paths are fed by the same capture layer, which keeps every JSON
response the page makes from `document-start` onward.

If it reports no cards, click **Dump capture** and send that JSON back.
It contains everything needed to teach the extractor a new API shape —
which is the recovery mechanism, not a nicety.

### What survives the trip

pkmn.gg models less per card than PokeDumpster does (RESEARCH.md §3.2),
so most of the mapping is about not inventing data:

| Column | Source |
|---|---|
| `set_code` / `ptcgo_code` | the printing's set |
| `number` | collector number, verbatim |
| `variant` | pkmn.gg's printing name, mapped to `variant.rs`'s codes |
| `condition` | always `Near Mint` — pkmn.gg does not track condition |
| `language` | English / Japanese / … from the card identity |
| `quantity` | per printing |
| `purchase_price`, `currency` | always blank — pkmn.gg does not track cost basis |
| `notes` | graded-copy details and private notes, which have no column of their own |

A variant PokeDumpster's catalog doesn't recognise is exported verbatim
rather than coerced to `normal`, so the row parks in `import_unresolved`
where you can see it. The console lists any such variants after an
export — that list is the diff to apply to `VARIANT_MAP`.

### Exit path off PokeDumpster

Every card in the collection payload carries `tcgPlayerMassEntry`
(`"Charizard ex - 006/165 [MEW]"`) and `tcgLiveCode`
(`"Charizard ex MEW 6"`). The script emits both as quantity-prefixed
text files, so the collection lands in TCGplayer Mass Entry and PTCG
Live format with no name-matching and no fuzzy resolution — the
identifiers are handed to us. These are also what the extractor reads
identity out of when the payload has no `set`/`number` field of its own,
which makes it robust against pkmn.gg renaming fields underneath.

### Known limits

- Entries carrying an id — or a `(card.id, variant)` pair, which the
  collection endpoint always supplies — are reconciled exactly. Only
  entries with *neither* fall back to taking the largest per-response
  quantity rather than summing, so a card appearing in both the grid and
  a "recently added" strip isn't double-counted. That fallback can
  under-count; the export summary reports how many rows reached it, and
  on the collection endpoint the answer is zero.
- A full page reload clears the capture buffer. Export before reloading.
- **Terms of service.** pkmn.gg's ToS §11 prohibits automated access
  flatly, with no written-permission carve-out, and separately prohibits
  harvesting information about others. Reading *your own* collection
  from *your own* logged-in session is the defensible posture and is
  what this script is for. The same endpoint will happily serve other
  people's public profiles without any authentication at all; don't.

### Measured against the live catalog

A real 2,383-row export (2,557 copies) resolved against prod's catalog on
2026-09-21, replaying `pkdump-db::import::resolve`'s own queries read-only:

| | rows resolved |
|---|---|
| before the variant-slug fix | 2,245 / 2,383 (94.2%) |
| after | **2,351 / 2,383 (98.7%)** |

The 32 that still park, and why none of them is fixable in this script:

- **11 `stamp`** — set-specific promo stamps (`stamp_journey_together`,
  `stamp_black_bolt`, `stamp_prismatic_evolutions`, …). Each card has
  exactly one, but choosing it needs the catalog.
- **10 `holidaystamp`** — carry `cosmos_holo_trick_or_trade` rather than
  `stamp_trick_or_trade`; pkmn.gg uses one slug for both.
- **5 `jumbo`**, **2 `holofoilalternate`** — zero printings catalog-wide;
  not modelled at all.
- **2 `holo`** on `sv4pt5` #7/#8 — the catalog has no holo printing for
  those cards.
- **2 cards** — `svp` #175/#176 are not in the catalog.

All of these land in the unresolved queue, where they can be resolved by
hand. None of them blocks the import.

### Tests

The extraction and mapping logic — everything testable without a live
pkmn.gg account — is covered by:

```bash
node scripts/tests/pkmngg_export.test.js
```

## `pkmngg_fetch.js` — headless-browser scraper (blocked)

**Does not currently reach a real collection.** Signing in with an emailed
magic link lands in an empty shadow account gated behind `/auth/newuser`,
not the user's real collection (pokedumpster-p6y.3). Kept because it still
works if you hand it a storage-state export from an already-logged-in
browser, and because the recon it did — API host `api.tcg.gg/pkmn/v1/…`,
the `/auth/email-confirm` interstitial, the `/collections` + `/lists` nav
shell — is what the userscript above is built on.

`p6y.3` attributed the failure to NextAuth treating magic-link identity as
separate from OAuth identity. That diagnosis is **unconfirmed and may rest
on a stale premise**: the site is Next.js *Pages Router* with SWR, and the
API uses a bespoke `Authorization: Bearer` scheme minted from an HttpOnly
cookie via `POST /v1/auth/refresh` — not NextAuth's own session handling.
The observed symptom is real; the mechanism behind it is a lead, not a
conclusion. Nobody has retried the login since.

Note also that the public path needs none of this: plain `curl` against
`api.tcg.gg` and the site's own pages returns 200 with no bot-wall, so
Playwright is not load-bearing for unauthenticated reads.

A headless-browser scraper. Drives Playwright (re-using the install at
`tests/ui/node_modules/playwright`) against pkmn.gg, captures every
JSON response the page makes while you'd normally browse the
collection, flattens the lot into a PokeDumpster-native CSV ready for
`/ingest/csv`.

### First run — magic-link bootstrap

pkmn.gg login is passwordless: you request a link, the site emails it.
First run, pass the link from that email — the script visits it once,
saves the resulting browser session, then runs the collection scrape:

```bash
node scripts/pkmngg_fetch.js --link "https://pkmn.gg/.../?token=..."
```

### Subsequent runs

Storage state is saved to `~/.pkdump/pkmngg-state.json` (chmod 600).
Re-runs use it without bothering you for a fresh link until the
session expires:

```bash
node scripts/pkmngg_fetch.js
# → ./pokedumpster-pkmngg-<timestamp>.csv
```

Then upload at PokeDumpster's `/ingest/csv` with format
**PokeDumpster (pkmn.gg export)**.

### Options

```
--link URL       Magic-link URL from the login email (single-use).
--storage PATH   Storage state file. Default: ~/.pkdump/pkmngg-state.json
--out FILE       Output CSV. Default: ./pokedumpster-pkmngg-<ts>.csv
--debug FILE     Log of every captured JSON response (URL + truncated
                 body). Default: ~/.pkdump/pkmngg-debug.log
--url URL        Where to land after login. Default: https://pkmn.gg/
--headed         Run a visible browser (debugging the flow).
--help, -h       Print this help.
```

The `--debug` log is the recovery mechanism: if zero rows come out,
pkmn.gg's API shape changed (or the script's heuristic doesn't match
it). Paste a relevant entry from the log back into the repo so the
flattener can be taught the new shape.

### CSV columns

Matches `crates/pkdump-core/src/import/pokedumpster.rs`:

```
set_code, ptcgo_code, number, variant, condition,
language, quantity, purchase_price, currency, source, notes
```
