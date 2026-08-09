# Batch Detail Page UX Description

**Source file:** `frontend/src/routes/batches/[id]/+page.svelte`
**URL pattern:** `/batches/{id}`

---

## 1. Page Purpose

The Batch Detail page shows a single ingestion batch — its name/type, when it
ran, optional notes, and the full list of cards it added. It is a read-only
record of one ingestion run.

---

## 2. Layout & Key Regions

- **Header** — `<h1>` showing the batch name (or, when unnamed, the batch
  type), a `.sub` metadata line ("{type} · {timestamp} · {n} cards"), and an
  optional italic notes line.
- **Card table** — columns Name, Set, #, Variant, Condition, Status. The Name
  cell links to the card detail page.

---

## 3. User-Facing Actions

| Element | Action |
|---------|--------|
| Card name link | Navigates to `/card/{set}/{number}` |

The page has no inputs, buttons, modals, or mutations.

---

## 4. Navigation

- **In:** batch Type links on `/batches`; the CSV import result's "View
  batch →" link.
- **Out:** card-name links → `/card/{set}/{number}`.

---

## 5. Loading / Empty / Error States

| State | Condition | Appearance |
|-------|-----------|------------|
| **Loading** | While `api.batchDetail()` runs | "Loading…" muted text |
| **Error** | The fetch rejects | "Failed to load batch: {message}" |
| **Empty batch** | `detail.cards.length === 0` | EmptyState "No cards in this batch." (header and metadata still render) |

---

## 6. Notable Interactive Behaviors

- **Reactive reload** — an `$effect` watches the `id` route param and
  re-fetches the batch detail whenever it changes.
- **Timestamp formatting** — `created_at` is trimmed to its first 16
  characters with `T` replaced by a space.
- No modals, no debounced inputs, no optimistic updates.
