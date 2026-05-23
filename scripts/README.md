# scripts/

## `pkmngg_export.user.js` — pkmn.gg → PokeDumpster CSV

A Tampermonkey / Violentmonkey userscript that runs on a logged-in
**pkmn.gg** page and downloads a CSV importable by PokeDumpster's
`/ingest/csv` (format **PokeDumpster**).

### Install

1. Install **[Tampermonkey][tm]** (Chrome / Firefox) or
   **[Violentmonkey][vm]** (open-source equivalent).
2. Open `scripts/pkmngg_export.user.js` in the browser and click the
   "Install" prompt — or paste it into a new userscript by hand.

[tm]: https://www.tampermonkey.net/
[vm]: https://violentmonkey.github.io/

### Use

1. Log in to pkmn.gg in the same browser profile and open your
   collection.
2. A small floating **Export CSV** button appears bottom-right. Click
   it. The script auto-scrolls the page (to materialise lazy rows),
   collects the rendered cards, then downloads
   `pokedumpster-pkmngg-<timestamp>.csv`.
3. In PokeDumpster, go to **/ingest/csv**, pick format **PokeDumpster
   (pkmn.gg export)**, upload the file, preview, commit.

### How it works

The script tries two strategies, in order:

1. **API capture** — `window.fetch` is hooked; any pkmn.gg response that
   looks like a collection page (URL contains `collection` / `card` /
   `inventory` / `owned`) is buffered. On click, every captured blob is
   walked recursively for objects that look like collection rows
   (a set identifier + a card number).
2. **DOM scrape** — fallback for when the API capture finds nothing.
   Card tiles + table rows are scanned for set / number /
   variant / quantity in data-attributes, with the `/cards/<set>/<number>`
   link href used as a last resort.

The two-pass design keeps it working when pkmn.gg redesigns its
chrome but keeps the JSON API stable, and vice-versa. If both
strategies come up empty, the script logs to the browser console and
shows an alert so you can paste an example response back here for us
to teach the script.

### CSV shape

Matches the PokeDumpster-native parser at
`crates/pkdump-core/src/import/pokedumpster.rs`:

```text
set_code, ptcgo_code, number, variant, condition,
language, quantity, purchase_price, currency, source, notes
```

`set_code` (the pokemontcg.io id) and `ptcgo_code` (the collector-facing
3-letter code) are both written when the page exposes them — the
importer resolves either. `variant` is mapped to PokeDumpster's flat
variant enum (`holo`, `reverse_holo`, `first_ed_holo`, …); anything
unrecognised passes through verbatim so a manual fix-up is possible.
