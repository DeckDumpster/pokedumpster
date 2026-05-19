# Order Detail Page UX Description

**Source file:** `frontend/src/routes/orders/[id]/+page.svelte`
**URL pattern:** `/orders/{id}`

---

## 1. Page Purpose

The Order Detail page shows a single imported order — its source, seller,
order number, date, card count, and total — and lists every card in the order
with its purchase price and status. Its key action is "receive": marking still
-pending (status `ordered`) cards as received in one click.

---

## 2. Layout & Key Regions

- **Header** — a left block with the order title (`<h1>` "{source} ·
  {seller}") and a `.sub` metadata line ("#{order_number} · {date} · {n}
  cards · {total}"); and a right side showing either a "Receive {n} card(s)"
  button (when pending cards remain) or an "All received" badge.
- **Inline error line** — red text on a receive failure.
- **Card table** — columns Name, Set, #, Variant, Paid, Status. The Name cell
  links to the card detail page.

---

## 3. User-Facing Actions

| Element | Action |
|---------|--------|
| "Receive {n} card(s)" button | Marks all pending (`ordered`) cards in this order as received, then reloads |
| Card name link | Navigates to `/card/{set}/{number}` |

The button is replaced by a static "All received" label once no pending cards
remain.

---

## 4. Navigation

- **In:** order Source links on `/orders`; the order import flow navigates
  here after committing an order.
- **Out:** card-name links → `/card/{set}/{number}`.

---

## 5. Loading / Empty / Error States

| State | Condition | Appearance |
|-------|-----------|------------|
| **Loading** | First load, before `detail` is set | "Loading…" muted text |
| **Error (no data)** | The detail fetch fails and `detail` is still null | "Failed to load order: {message}" |
| **Inline error** | A receive fails after the page loads | Red error text below the header |

There is no dedicated empty state — an order always has at least its metadata
and card table.

---

## 6. Notable Interactive Behaviors

- **Reactive reload** — an `$effect` watches the `id` route param and reloads
  the order detail.
- **Pending count is derived** — `pendingCount` counts cards with status
  `ordered`; the header conditionally shows the Receive button or the "All
  received" label based on it.
- **Receive** is non-optimistic: it sets `busy`, calls `receiveOrder`, then
  re-runs `load()` to refresh card statuses.
- **Date fallback** — when `order_date` is absent, the metadata line uses the
  first 10 characters of `created_at`.
