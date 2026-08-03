# Set Stats Page UX Description

**Source file:** `frontend/src/routes/browse/[set]/stats/+page.svelte`
**URL pattern:** `/browse/{set}/stats` (e.g. `/browse/sv3pt5/stats`)

---

## 1. Page Purpose

The Set Stats page is a read-only analytics dashboard for a single set. It
summarizes how complete the user's collection of that set is (numbered-set and
master-set completion), what the set is worth (owned value vs. full-set market
value), and how completion breaks down by rarity.

---

## 2. Layout & Key Regions

- **Header** — the set name (`<h1>`) with its series below, and a "Binder
  view →" link.
- **Cards row (`.cards`)** — two analytics cards side by side:
  - **Completion** — two labelled progress bars: "Numbered set"
    (owned_cards / total_cards) and "Master set" (owned_printings /
    total_printings), each showing "{owned} / {total} · {pct}%".
  - **Value** — three figures: "Owned" (owned value in dollars), "Full set"
    (market value of the complete set), and "of set value" (owned as a
    percentage of full-set value).
- **Rarity split card** — a `<h2>` "Rarity split" and a table with columns
  Rarity, Owned, Total, and a Progress cell containing a small bar plus a
  percentage.

---

## 3. User-Facing Actions

| Element | Action |
|---------|--------|
| "Binder view →" link | Navigates to this set's binder browse page |

The page has no inputs, buttons, modals, or mutations — it is entirely
read-only.

---

## 4. Navigation

- **In:** the "Set stats →" link on the binder browse page.
- **Out:** "Binder view →" → `/browse/{set}`.

---

## 5. Loading / Empty / Error States

| State | Condition | Appearance |
|-------|-----------|------------|
| **Loading** | While `api.setAnalytics()` runs | "Loading…" muted text |
| **Error** | The analytics fetch rejects | "Failed to load set stats: {message}" |
| **Empty rarity split** | `stats.rarities.length === 0` | Small EmptyState "No cards catalogued." inside the Rarity split card; the rest of the page still renders |

---

## 6. Notable Interactive Behaviors

- **Reactive reload** — an `$effect` watches the set route param and re-fetches
  analytics whenever it changes.
- **Percentage helper** — `pct()` rounds `owned / total * 100`, guarding
  against division by zero, and drives every progress bar fill width and the
  "of set value" figure.
- No modals, no debounced inputs, no optimistic updates.
