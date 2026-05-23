# scripts/

Tools that sit alongside the main app — pkmn.gg ingest, etc.

## `pkmngg_fetch.js` — pkmn.gg → PokeDumpster CSV (recommended)

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

## `pkmngg_export.user.js` — Tampermonkey export (legacy)

A Tampermonkey userscript that runs *inside* a logged-in pkmn.gg tab
and offers an Export CSV button. Kept around as a manual fallback if
the headless scraper ever can't establish a session, but
`pkmngg_fetch.js` is the recommended path.
