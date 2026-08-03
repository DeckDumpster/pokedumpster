# Collection Page UX Description

**Source file:** `frontend/src/routes/collection/+page.svelte`
**URL:** `/collection`

---

## 1. Page Purpose

The Collection page is the master table view of every physical card the user
owns or has owned. Because PokeDumpster keeps strictly one row per physical
card (no quantity aggregation), each table row is a single copy. The page lets
the user search and filter the collection with faceted sidebar controls, save
and re-apply named filter configurations ("saved views"), export the
collection to CSV, and perform bulk operations (delete, assign to a
binder/deck, add to wishlist) on multiple selected copies.

---

## 2. Layout & Key Regions

When loaded, the page renders an `<h1>` "Collection" followed by a two-column
`.layout`:

- **Sidebar (`aside.sidebar`, ~200px)** — stacked sections:
  - **Saved views** — a `<select>` of saved views (only shown if any exist),
    plus a "Save current…" button and (when a view is active) a "Delete"
    button.
  - **Search input** — a text box, placeholder "Search cards…".
  - **Facet sections** — one per facet that has values: "Rarity", "Set",
    "Variant". Each is a list of checkboxes built from the distinct values
    present in the loaded rows.

- **Content (`main.content`)** — a toolbar, an optional bulk-action bar, and
  the collection table:
  - **Toolbar** — a "{filtered} of {total} cards" count, an "Export CSV"
    link, and a "Select" / "Cancel" button (the last two appear only when the
    collection is non-empty).
  - **Bulk bar** — appears only in select mode with at least one row checked.
  - **Table** — columns: (checkbox, in select mode), Name, Set, #, Variant,
    Rarity, Condition, Status, Paid. The Name cell links to the card detail
    page.

---

## 3. User-Facing Actions

| Element | Action |
|---------|--------|
| Search input | Debounced (200 ms) — filters the table by card name substring |
| Rarity / Set / Variant checkboxes | Toggle facet membership; filters are AND-ed across facets, OR-ed within a facet |
| Saved views `<select>` | Applies the chosen view's saved search + facet selections; "— none —" clears the active view (but does not clear filters) |
| "Save current…" button | `prompt()`s for a name, then saves the current filter config as a new view |
| "Delete" button | `confirm()`s, then deletes the active saved view |
| "Export CSV" link | Downloads `/api/export/csv` |
| "Select" / "Cancel" button | Toggles multi-select mode; cancelling clears the selection |
| Header checkbox (select mode) | Selects/clears every currently-filtered row |
| Per-row checkbox (select mode) | Toggles that row's selection |
| Bulk "Delete" button | `confirm()`s, then bulk-deletes selected copies |
| Bulk "Assign to binder…" `<select>` | Moves every selected copy into the chosen binder |
| Bulk "Assign to deck…" `<select>` | Moves every selected copy into the chosen deck |
| Bulk "Add to wishlist" button | Adds one wish per distinct card among the selection |
| Card name link | Navigates to `/card/{set}/{number}` |

---

## 4. Navigation

- **In:** "Collection" header nav link; card-name links from many other pages.
- **Out:** each row's Name cell links to the card detail page
  (`/card/{set_code}/{number}`). The "Export CSV" anchor points at the API
  endpoint `/api/export/csv` (a download, not a page).
- The global header is always present.

---

## 5. Loading / Empty / Error States

| State | Condition | Appearance |
|-------|-----------|------------|
| **Loading** | Initial mount, while four parallel fetches run | "Loading…" muted text; no layout shown |
| **Error** | Any of the mount fetches reject | "Failed to load collection: {message}" |
| **Empty collection** | `rows.length === 0`, no query | Sidebar + toolbar render; content shows the EmptyState "Your collection is empty." with "Add cards from a set's binder view — click a slot and that printing is registered as a copy you own. Or turn on “All cards” to browse the catalog first." and a **Browse sets** button to /browse. The Export CSV link and Select button are hidden when there are no rows |
| **Search matches nothing** | `searchRows.length === 0` with a query | EmptyState "No cards match “&lt;query&gt;”." — the description tells you to turn on “All cards” when it is off; a **Search syntax** button links to /search-help |
| **Unparseable query** | `searchError` set | EmptyState "That query didn't parse." pointing at the error message under the search box |
| **Empty filter result** | Rows exist but none match | The table renders with no body rows; the toolbar count reads "0 of N cards" |
| **Inline error** | A saved-view or bulk operation fails after load | The error message is surfaced via the `error` state and shown as red text |

---

## 6. Notable Interactive Behaviors

- **Debounced search** — keystrokes update a raw value immediately but the
  effective search term only after a 200 ms idle, via `setTimeout`.
- **Saved views** — a view stores `{search, rarity, set, variant}` serialized
  to `filters_json`. Applying a view rehydrates all four. Note that selecting
  "— none —" clears `activeViewId` but leaves the current filters in place.
- **Facet values are derived** — the Rarity/Set/Variant checkbox lists come
  from the distinct values present in the loaded rows, sorted; a facet section
  is hidden entirely if it has no values.
- **Bulk operations** are not optimistic: after a bulk delete/assign/wishlist,
  the page re-fetches the full collection (`refresh()`) and drops the
  selection. The bulk `<select>`s reset their `selectedIndex` to 0 after each
  use so they read as a placeholder again.
- **Bulk assign loops** one `moveCopy` call per selected copy; the assign
  `<select>`s are disabled when there are no binders/decks respectively.
- **Bulk wishlist deduplicates** by `card_id` so selecting two copies of the
  same card creates only one wish.
- The page has no modal of its own; all confirmations use the browser's
  native `confirm()` / `prompt()` dialogs.
