# Browse (Set Picker) Page UX Description

**Source file:** `frontend/src/routes/browse/+page.svelte`
**URL:** `/browse`

---

## 1. Page Purpose

The Browse page is the entry point to binder-page browsing — PokeDumpster's
headline feature. It presents every set in the catalog as a grid of tiles, each
showing the set's symbol, name, series, owned/total card count, and a
completion progress bar. The user searches/scans for a set and clicks its tile
to open that set's binder view.

---

## 2. Layout & Key Regions

- **Header** — `<h1>` "Browse sets" and a muted instruction line, "Pick a set
  to open its binder view."
- **Search input** — a text box, placeholder "Search sets…", capped at ~360px
  width.
- **Result count** — a muted "{filtered} of {total} sets" line.
- **Set grid (`.grid`)** — a responsive auto-fill grid of set tiles. Each
  `.tile` is an anchor containing: the set symbol image (if any), the set
  name (`.title`), the series (`.series`), an "{owned} / {total} cards"
  count, and a `.bar` progress bar whose fill width is the owned percentage.

---

## 3. User-Facing Actions

| Element | Action |
|---------|--------|
| Search input | Filters the grid (case-insensitive) by set name or series substring |
| Set tile | Navigates to that set's binder view |

---

## 4. Navigation

- **In:** "Browse" header nav link.
- **Out:** each set tile is an `<a href="/browse/{set_code}">` → the binder
  browse page for that set.

---

## 5. Loading / Empty / Error States

| State | Condition | Appearance |
|-------|-----------|------------|
| **Loading** | While `api.sets()` runs on mount | "Loading…" muted text |
| **Error** | The sets fetch rejects | "Failed to load sets: {message}" |
| **No matches** | Sets exist but none match the search | The grid renders empty; the count line reads "0 of N sets" |

There is no distinct "no sets at all" empty state — an empty catalog simply
renders an empty grid.

---

## 6. Notable Interactive Behaviors

- **Live (non-debounced) search** — the search input is `bind:value`-bound and
  the filtered list is a `$derived` value, so the grid updates on every
  keystroke.
- **Progress bar** — `pct()` computes `round(owned / total * 100)`, guarding
  against division by zero; the result drives the inline `width` style of the
  bar fill.
- No modals, no mutations — this page only reads and navigates.
