# Binders Page UX Description

**Source file:** `frontend/src/routes/binders/+page.svelte`
**URL:** `/binders`

---

## 1. Page Purpose

The Binders page is the list view of the user's physical binders. It lets the
user create a new binder by name and shows every existing binder as a tile
with its name and card count. It is purely a list/create page — viewing,
editing card contents, and deletion happen on the binder detail page.

---

## 2. Layout & Key Regions

- **Title** — `<h1>` "Binders".
- **Create form (`.newform`)** — a text input ("New binder name…") and a
  "Create" submit button.
- **Inline error line** — red text shown below the form when a load or create
  fails.
- **Binder grid (`.grid`)** — a responsive auto-fill grid of binder tiles.
  Each `.tile` is an anchor showing the binder name and an "{n} cards" count.

---

## 3. User-Facing Actions

| Element | Action |
|---------|--------|
| New-binder name input | Holds the name for the new binder |
| "Create" button (or Enter in the input) | Submits the form — creates a binder, clears the input, reloads the list |
| Binder tile | Navigates to that binder's detail page |

---

## 4. Navigation

- **In:** "Binders" header nav link.
- **Out:** each binder tile is an `<a href="/binders/{id}">` → the binder
  detail page.

---

## 5. Loading / Empty / Error States

| State | Condition | Appearance |
|-------|-----------|------------|
| **Loading** | While `api.binders()` runs on mount | "Loading…" muted text |
| **Empty** | `binders.length === 0` after load | EmptyState "No binders yet." — "A binder files cards you own into pages — a master set, a trade folder, whatever sits on your shelf. Name one above to create your first." |
| **Error** | A load or create fails | Red error text below the create form; the form stays usable |
| **Populated** | Binders exist | The grid of binder tiles |

---

## 6. Notable Interactive Behaviors

- **Create flow** — the form's `submit` handler calls `preventDefault()` then
  `create()`. An empty/whitespace name is silently ignored (no error). On
  success the input is cleared and `load()` re-fetches the full list.
- **Busy guard** — the "Create" button is disabled while a create is in
  flight (`busy`).
- No modals; the page never deletes from here.
