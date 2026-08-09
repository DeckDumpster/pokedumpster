# Binder Browse Page UX Description

**Source file:** `frontend/src/routes/browse/[set]/+page.svelte`
**URL pattern:** `/browse/{set}` (e.g. `/browse/sv3pt5`)

---

## 1. Page Purpose

The Binder Browse page is PokeDumpster's headline feature: it renders a set as
virtual binder pages — a grid of card "slots", one per card number — and lets
the user register the printings they own by clicking a slot and adding a
variant. It shows base-set and master-set completion progress, supports
4/9/12-pocket page layouts, paginates through the set, and can include or
exclude secret rares, subsets, and promos. Adds made during a single set visit
are grouped under one ingestion batch created lazily on the first add.

---

## 2. Layout & Key Regions

- **Header** — the set name (`<h1>`), a "Set stats →" link, and a `.stats`
  block with two progress bars: "Base {owned}/{total}" and
  "Master {owned}/{total}".
- **Controls bar** — three checkboxes (Secret, Subset, Promos), a Layout
  `<select>` (4-pocket / 9-pocket / 12-pocket), a spacer, a "← Prev" button, a
  "Page X of Y" indicator, and a "Next →" button.
- **Slot grid (`.grid`)** — a CSS grid whose column count derives from the
  pocket layout (4→2, 9→3, 12→4 columns), capped on narrow viewports. Each
  slot is a button showing the card image (or a name placeholder), the card
  number, and a row of "pips" — one per non-deprecated printing, filled when
  that printing is owned.
- **Section dividers** — full-width labels ("Secret Rares", "Subset",
  "Promos") inserted between slots when the section changes.
- **Variant modal** — opened on slot click (the shared `VariantModal`).
- **Toast** — a fixed bottom-center bar shown after an add, with an "Undo"
  button.

---

## 3. User-Facing Actions

| Element | Action |
|---------|--------|
| Secret / Subset / Promos checkboxes | Include or exclude those card sections; toggling resets to page 1 |
| Layout `<select>` | Switches between 4/9/12-pocket layouts; resets to page 1 |
| "← Prev" / "Next →" buttons | Page through the set; disabled at the first/last page |
| "Set stats →" link | Navigates to the set stats page |
| Slot button | Opens the variant modal for that card |
| Variant modal "+ Add" | Adds a copy of that printing (see Variant Modal below) |
| Variant modal "Full card details →" | Navigates to the card detail page |
| Toast "Undo" button | Deletes the just-added copy |

### Variant Modal (`VariantModal.svelte`)

A centered modal (a bottom sheet below 540px wide) titled "#{number} ·
{name}". It lists every printing of the card with: the variant label, an
"{n} owned" count (green when > 0), the market price, and a "+ Add" button
(disabled for deprecated printings). It closes via the "×" button or the
Escape key, and has a "Full card details →" link to `/card/{set}/{number}`.

---

## 4. Navigation

- **In:** set tiles on `/browse`; the "Binder view →" link on the set stats
  page.
- **Out:** "Set stats →" → `/browse/{set}/stats`; the variant modal's "Full
  card details →" → `/card/{set}/{number}`.

---

## 5. Loading / Empty / Error States

| State | Condition | Appearance |
|-------|-----------|------------|
| **Loading** | First load, before `binder` is set | "Loading…" muted text |
| **Error (no data)** | The binder fetch fails before any data loads | "Failed to load binder: {message}" |
| **Inline error** | A reload or an add fails while a binder is already shown | Red error text shown above the slot grid; the page stays visible |
| **Empty view** | `binder.slots.length === 0`, no search, "Missing only" off | EmptyState "No cards in this view." (every section excluded) |
| **Search matches nothing** | `binder.slots.length === 0` with a search | EmptyState "No cards match “&lt;query&gt;”." with a **Clear search** button |
| **Nothing missing** | `binder.slots.length === 0` with "Missing only" on | Success-toned EmptyState "Nothing missing here." with a **Show every card** button that turns the filter off |

---

## 6. Notable Interactive Behaviors

- **Reactive reload** — an `$effect` watches the set param plus every control
  (page number, layout, all three section toggles) and re-fetches the binder
  page whenever any changes.
- **Lazy batch creation** — a binder-browse session groups its adds under a
  single batch (PLAN §6.7). The batch is created only on the *first* add of a
  visit, so merely viewing a set never leaves an empty batch behind. Changing
  to a different set resets the session and clears the cached batch id.
- **Optimistic add** — clicking "+ Add" immediately increments the printing's
  `owned_count` (so the modal count and the slot pip update at once), then
  fires the create call. On failure the increment is reverted and an error is
  shown.
- **Undo toast** — every successful add shows a toast for 6 seconds with the
  card name and an Undo button. Undo deletes the created copy and decrements
  the owned count. A new add resets the toast timer.
- **Responsive layout** — the grid column count is capped to 1 below 480px and
  to 2 below 768px; the variant modal becomes a bottom sheet below 540px.
- **Pip rendering** — pips are drawn only for non-deprecated printings; a pip
  is filled (`.owned`) when its printing's `owned_count > 0`.
