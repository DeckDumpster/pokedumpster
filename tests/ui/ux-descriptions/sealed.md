# Sealed Page UX Description

**Source file:** `frontend/src/routes/sealed/+page.svelte`
**URL:** `/sealed`

---

## 1. Page Purpose

The Sealed page tracks the user's sealed-product inventory — booster boxes,
bundles, sealed decks, and similar products. It lists owned sealed products
with their status, lets the user add a new sealed product (found via a search
modal), change a product's status, and remove a product from the collection.

---

## 2. Layout & Key Regions

- **Header** — `<h1>` "Sealed collection" and a "+ Add sealed" button.
- **Show-opened toggle** — a checkbox labelled "Show opened / disposed" that
  expands the table to include non-active products.
- **Inline error line** — red text on failure.
- **Sealed table** — columns Product, Category, Qty, Paid, Status, and an
  action cell with a "Remove" link. The Status cell is a `<select>`.
  Non-active rows render dimmed.
- **Add modal** — a centered modal with two sub-states (search, then
  confirm).

---

## 3. User-Facing Actions

| Element | Action |
|---------|--------|
| "+ Add sealed" button | Opens the add-sealed modal |
| "Show opened / disposed" checkbox | Switches the table between active-only and all products |
| Status `<select>` (per row) | Changes a product's status — one of owned, listed, sold, traded, gifted, opened |
| "Remove" link (per row) | `confirm()`s, then deletes the sealed entry |
| Modal search input | Searches sealed products (requires ≥ 2 characters) |
| Modal search result | Selects a product, advancing the modal to the confirm step |
| Modal "Purchase price" input | Optional price for the new entry |
| Modal "← Back" link | Returns from the confirm step to the search step |
| Modal "Add to collection" button | Adds the chosen product and reloads the list |
| Modal "×" button | Closes the modal |

---

## 4. Navigation

- **In:** "Sealed" header nav link.
- **Out:** no in-page links to other routes; only the global header.

---

## 5. Loading / Empty / Error States

| State | Condition | Appearance |
|-------|-----------|------------|
| **Loading** | While `api.sealedCollection()` runs on mount | "Loading…" muted text |
| **Empty (active)** | Products exist but none active, "Show opened" off | EmptyState "Nothing sealed in your active inventory." with an **Add sealed product** button |
| **Empty (all)** | `entries.length === 0` — nothing logged at all | EmptyState "No sealed products yet." with an **Add sealed product** button |
| **Filter matches nothing** | A query with no match | EmptyState "No sealed products match “&lt;query&gt;”." |
| **Error** | A load or mutation fails | Red error text below the toggle |
| **Modal — no results** | Search ≥ 2 chars but no products match | Small EmptyState "No matching products." inside the modal list |

---

## 6. Notable Interactive Behaviors

- **Two-step add modal** — the modal starts in search mode (a text box plus a
  scrollable result list); picking a result advances to confirm mode (the
  chosen product name plus a purchase-price input). On a successful add the
  modal closes and all of its fields reset.
- **Live search** — the modal search fires on every `oninput`; queries shorter
  than 2 characters clear the result list without a request.
- **Active vs. all** — the `shown` list is derived: when "Show opened" is off
  it filters to statuses owned/listed; otherwise it shows everything, dimming
  non-active rows.
- **No optimistic updates** — add, status change, and remove each set `busy`,
  perform the mutation, then re-run `load()`.
- The modal is not dismissible by backdrop click or Escape — only the "×"
  button closes it.
