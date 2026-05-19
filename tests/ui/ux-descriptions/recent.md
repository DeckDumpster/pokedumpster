# Recent Page UX Description

**Source file:** `frontend/src/routes/recent/+page.svelte`
**URL:** `/recent`

---

## 1. Page Purpose

The Recent page is a compact activity feed of the user's most recent ingestion
batches — manual entries, binder clicks, imports, orders. It shows the latest
15 batches as a timeline; each entry can be expanded inline to reveal the cards
that batch added.

---

## 2. Layout & Key Regions

- **Header** — `<h1>` "Recent activity" and a muted instruction line, "Your
  most recent ingestion batches. Click one to see its cards."
- **Inline error line** — red text on failure.
- **Timeline (`ul.timeline`)** — one `<li>` per batch. Each batch row is a
  full-width button (`.head`) showing a caret (▸ collapsed / ▾ expanded), the
  batch type, the batch name, an "{n} cards" count, and a timestamp.
- **Expanded card list** — when a batch is expanded, an indented `ul.cards`
  lists each card with a name link and a meta line ("{set_code} · {variant} ·
  {status}").

---

## 3. User-Facing Actions

| Element | Action |
|---------|--------|
| Batch header button | Toggles that batch's expanded card list (lazy-loads cards on first expand) |
| Card name link | Navigates to `/card/{set}/{number}` |

---

## 4. Navigation

- **In:** "Recent" header nav link.
- **Out:** card-name links inside expanded batches → `/card/{set}/{number}`.
  Note there is no link to the full per-batch detail page or to `/batches`
  from here.

---

## 5. Loading / Empty / Error States

| State | Condition | Appearance |
|-------|-----------|------------|
| **Loading** | While the batches fetch runs on mount | "Loading…" muted text |
| **Empty** | `batches.length === 0` after load | "No activity yet." |
| **Error** | The list fetch (or a per-batch detail fetch) fails | Red error text near the top |
| **Expanding** | A batch is expanded but its cards haven't arrived | Indented "Loading…" muted text under the batch row |
| **Expanded — empty batch** | An expanded batch has no cards | Indented "No cards." |

---

## 6. Notable Interactive Behaviors

- **Lazy detail loading** — `toggle()` adds the batch id to the `expanded`
  set; on first expansion it fetches the batch detail and caches it in a
  `Map`, so re-expanding the same batch never re-fetches.
- **Capped list** — the page requests only the 15 most recent batches.
- No modals; expansion is purely inline accordion behavior.
