# CSV Import Page UX Description

**Source file:** `frontend/src/routes/ingest/csv/+page.svelte`
**URL:** `/ingest/csv`

---

## 1. Page Purpose

The CSV Import page brings an entire collection in from an external export —
ManaBox or TCGplayer. The user picks the source format, supplies the CSV
(either by uploading a file or by pasting text), previews a resolution report
that classifies every row as matched or unmatched, then commits the import.
Committing adds the matched cards and links them to a new ingestion batch.

---

## 2. Layout & Key Regions

- **Header** — `<h1>` "Import CSV" and a muted line linking to the other add-
  cards routes (manual entry, paste an order).
- **Form (`.form`)** — a Format `<select>` (ManaBox / TCGplayer), a "CSV file"
  file input, a "…or paste CSV text" textarea, and an `.actions` row with a
  "Preview" button and a "Import" button.
- **Inline error line** — red text on failure.
- **Result banner** — after a commit, a banner reporting added/skipped counts
  with a "View batch →" link.
- **Preview section** — after a preview, a summary line plus up to two tables:
  "Unmatched rows" (Line, Set, #, Variant, Reason) and "Matched rows" (Line,
  Card, Set, #, Variant, Condition).

---

## 3. User-Facing Actions

| Element | Action |
|---------|--------|
| Format `<select>` | Chooses the import parser — ManaBox or TCGplayer |
| "CSV file" input | Reads a chosen file's text into the content field |
| Paste textarea | Lets the user paste CSV text directly; editing it clears any prior preview/result |
| "Preview" button | Requests a resolution report without importing |
| "Import" button | Commits the import; the button label reflects the matched count ("Import N cards") |
| "View batch →" link | Navigates to the created batch's detail page |
| "manual entry" / "paste an order" links | Navigate to `/ingest/manual` and `/ingest/order` |

---

## 4. Navigation

- **In:** the "Import" header nav link points here.
- **Out:** the result banner's "View batch →" → `/batches/{batch_id}`; the
  intro line links to `/ingest/manual` and `/ingest/order`.

---

## 5. Loading / Empty / Error States

| State | Condition | Appearance |
|-------|-----------|------------|
| **Initial** | No content yet | Both buttons disabled (Preview needs content; Import needs a preview with matches) |
| **Busy** | A preview or commit is running | Both buttons disabled |
| **Error** | A preview or commit fails | Red error text below the form |
| **Preview — all matched** | `unmatched.length === 0` | Summary shows only "{n} matched"; only the Matched table renders |
| **Preview — some unmatched** | Unmatched rows exist | Summary adds "· {n} unmatched"; the Unmatched table renders above the Matched table |
| **Result** | A commit succeeded | The result banner with added/skipped counts and the batch link |

---

## 6. Notable Interactive Behaviors

- **File vs. paste** — choosing a file reads its text into the same `content`
  field used by the textarea; the filename is remembered and passed to the
  commit. Editing the textarea clears any stale preview/result so the user
  can't import against outdated content.
- **Preview gates Import** — the "Import" button stays disabled until a
  preview exists with at least one matched row; its label updates to "Import
  {matched} cards" once a preview is available.
- **Preview and commit are mutually exclusive results** — a successful preview
  clears any prior result; a successful commit clears the preview.
- No modal — the whole flow is an inline form plus result tables.
