# Batches Page UX Description

**Source file:** `frontend/src/routes/batches/+page.svelte`
**URL:** `/batches`

---

## 1. Page Purpose

The Batches page lists every ingestion run the user has ever made — manual
entry, binder clicks, CSV imports, and orders. It is the full audit list of
batches (unlike the Recent page, which shows only the latest 15). The user can
filter the list by batch type and drill into any batch's detail page.

---

## 2. Layout & Key Regions

- **Header** — `<h1>` "Batches" and a muted line, "Every ingestion run —
  manual entry, binder clicks, imports, orders."
- **Type filter** — a labelled `<select>` ("Type") with an "All" option plus
  one option per distinct batch type present in the data.
- **Batches table** — columns Type, Name, Cards, When. The Type cell links to
  the batch detail page.

---

## 3. User-Facing Actions

| Element | Action |
|---------|--------|
| Type `<select>` | Filters the table to a single batch type, or "All" |
| Batch type link (per row) | Navigates to that batch's detail page |

---

## 4. Navigation

- **In:** the "Import" header nav goes to `/ingest/csv`; the Batches page is
  reached directly by URL or from links such as the CSV import result's "View
  batch →".
- **Out:** each Type cell is an `<a href="/batches/{id}">` → the batch detail
  page.

---

## 5. Loading / Empty / Error States

| State | Condition | Appearance |
|-------|-----------|------------|
| **Loading** | While `api.batches()` runs on mount | "Loading…" muted text |
| **Error** | The fetch rejects | "Failed to load batches: {message}" |
| **Empty** | `batches.length === 0` after load | "No batches yet." (the type filter is not shown) |
| **Empty filter result** | Batches exist but none match the chosen type | The table renders with no body rows |

---

## 6. Notable Interactive Behaviors

- **Derived filter options** — the type `<select>` options come from the
  distinct `batch_type` values across the loaded batches, sorted.
- **Live filtering** — the shown list is a `$derived` value; choosing a type
  filters the table immediately with no request.
- **Timestamp formatting** — `created_at` is trimmed to its first 16
  characters with the `T` replaced by a space.
- No modals, no mutations — read-only.
