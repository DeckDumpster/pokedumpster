# Card Detail Page UX Description

**Source file:** `frontend/src/routes/card/[set]/[number]/+page.svelte`
**URL pattern:** `/card/{set}/{number}` (e.g. `/card/sv3pt5/6`)

---

## 1. Page Purpose

The Card Detail page is the canonical full view of a single Pokémon card
identified by its set code and collector number. It shows the card art and
metadata, lists every printing/variant of that card with owned counts and
market prices and an "Add" button per printing, and lists every copy the user
owns of this card with inline controls to change a copy's variant, status, and
binder/deck assignment.

---

## 2. Layout & Key Regions

- **Detail header (`.detail`)** — a two-column flex:
  - **Art (`.art`)** — the large card image, or a "No image" placeholder box.
  - **Info (`.info`)** — the card name (`<h1>`), a subtitle line of
    "{set_code} · #{number} · {rarity}", a definition list of optional fields
    (Type/supertype, HP, Energy/types, Artist), and flavor text if present.

- **Printings section** — `<h2>` "Printings" and a table with columns
  Variant, Owned, Market, and an action cell holding a "+ Add" button per row.
  Deprecated printings render dimmed and their Add button is disabled.

- **Your copies section** — `<h2>` "Your copies (N)" and, if any copies exist,
  a table with columns Variant, Condition, Status, Location, Paid. The
  Variant, Status, and Location cells are `<select>` controls.

---

## 3. User-Facing Actions

| Element | Action |
|---------|--------|
| "+ Add" button (printings table) | Adds a new copy of that printing to the collection with `source: 'manual'` |
| Variant `<select>` (copies table) | Re-points the copy at a different printing of the same card |
| Status `<select>` (copies table) | Changes the copy's status — one of owned, ordered, listed, sold, removed, traded, gifted, lost |
| Location `<select>` (copies table) | Assigns the copy to a binder, a deck, or "Unassigned" |

There are no modals on this page.

---

## 4. Navigation

- **In:** card-name links from the collection, binders, decks, orders,
  batches, recent, wishlist, manual-entry pages, and the variant modal's
  "Full card details" link.
- **Out:** no in-page links to other routes; the global header nav is the
  only outbound navigation.

---

## 5. Loading / Empty / Error States

| State | Condition | Appearance |
|-------|-----------|------------|
| **Loading** | First load, before `detail` is set | "Loading…" muted text |
| **Error (no data)** | Initial fetch fails and `detail` is still null | "Failed to load card: {message}" |
| **Inline error** | A mutation fails while `detail` is already shown | Red error text shown both below the detail header and above the copies table; the page content stays visible |
| **No copies** | `detail.copies.length === 0` | "You don't own this card yet." under the "Your copies (0)" header |

---

## 6. Notable Interactive Behaviors

- **Reactive reload on route change** — an `$effect` watches the `set` and
  `number` route params and re-runs `load()`, which fires three parallel
  fetches (card detail, binders, decks).
- **All mutations are non-optimistic** — `addCopy`, `changeVariant`,
  `changeStatus`, and `assignCopy` all run through `withBusy()`, which sets a
  `busy` flag (disabling all controls), performs the mutation, then re-runs
  `load()` to refresh the whole page from the server.
- **Location encoding** — the Location `<select>` encodes its value as
  `b:{id}` for a binder, `d:{id}` for a deck, or empty for unassigned;
  `assignCopy` decodes this into the appropriate `moveCopy` body.
- **Optional metadata fields** — Type, HP, Energy, Artist, and flavor text
  each render only when present on the card; `types` is parsed from a JSON
  string list.
