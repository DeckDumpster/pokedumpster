# Orders Page UX Description

**Source file:** `frontend/src/routes/orders/+page.svelte`
**URL:** `/orders`

---

## 1. Page Purpose

The Orders page is the list view of imported purchase orders. Each order
groups the cards bought in a single transaction (from TCGplayer, eBay, an LGS,
etc.). The page shows every order in a table and links to the order import
flow and to each order's detail page.

---

## 2. Layout & Key Regions

- **Header** — `<h1>` "Orders" and a red "+ Import order" link button.
- **Orders table** — columns Source, Seller, Date, Cards, Total. The Source
  cell links to the order detail page. The Date cell shows the order date, or
  falls back to the creation date.

---

## 3. User-Facing Actions

| Element | Action |
|---------|--------|
| "+ Import order" link | Navigates to the order import page |
| Order source link (per row) | Navigates to that order's detail page |

The page has no inputs, modals, or mutations.

---

## 4. Navigation

- **In:** "Orders" header nav link.
- **Out:** "+ Import order" → `/ingest/order`; each Source cell is an
  `<a href="/orders/{id}">` → the order detail page.

---

## 5. Loading / Empty / Error States

| State | Condition | Appearance |
|-------|-----------|------------|
| **Loading** | While `api.orders()` runs on mount | "Loading…" muted text |
| **Error** | The orders fetch rejects | "Failed to load orders: {message}" |
| **Empty** | `orders.length === 0` after load | EmptyState "No orders yet." with an **Import an order** button to /ingest/order |
| **Populated** | Orders exist | The orders table |

---

## 6. Notable Interactive Behaviors

- **Money formatting** — `money()` formats a numeric total as `$X.XX`, or "—"
  when null.
- **Date fallback** — when an order has no explicit `order_date`, the table
  shows the first 10 characters of `created_at` instead.
- No modals, no debounced search, no optimistic updates — this is a read-only
  list.
