# Decks Page UX Description

**Source file:** `frontend/src/routes/decks/+page.svelte`
**URL:** `/decks`

---

## 1. Page Purpose

The Decks page is the list view of the user's decks. It lets the user create a
new deck (with an optional owner) and shows every existing deck as a tile with
its name, lifecycle state, optional owner, and card count.

---

## 2. Layout & Key Regions

- **Title** — `<h1>` "Decks".
- **Create form (`.newform`)** — a "New deck name…" text input, an "Owner
  (optional)" text input, and a "Create" submit button.
- **Inline error line** — red text shown below the form on load/create
  failure.
- **Deck grid (`.grid`)** — a responsive auto-fill grid of deck tiles. Each
  `.tile` shows the deck name, a `.meta` line with a state badge (styled per
  state — e.g. "built" green, "ready" amber) and the owner if set, and an
  "{n} cards" count.

---

## 3. User-Facing Actions

| Element | Action |
|---------|--------|
| New-deck name input | Holds the name for the new deck (required) |
| Owner input | Optional owner for the new deck |
| "Create" button (or Enter) | Submits the form — creates a deck, clears both inputs, reloads the list |
| Deck tile | Navigates to that deck's detail page |

---

## 4. Navigation

- **In:** "Decks" header nav link.
- **Out:** each deck tile is an `<a href="/decks/{id}">` → the deck detail
  page.

---

## 5. Loading / Empty / Error States

| State | Condition | Appearance |
|-------|-----------|------------|
| **Loading** | While `api.decks()` runs on mount | "Loading…" muted text |
| **Empty** | `decks.length === 0` after load | EmptyState "No decks yet." — "A deck holds the copies you've committed to a list, so they stop counting as loose collection. Name one above to create your first." |
| **Error** | A load or create fails | Red error text below the create form |
| **Populated** | Decks exist | The grid of deck tiles |

---

## 6. Notable Interactive Behaviors

- **Create flow** — the form's `submit` handler calls `preventDefault()` then
  `create()`. An empty/whitespace name is silently ignored. The owner field is
  trimmed and omitted when blank. On success both inputs clear and the list
  reloads.
- **Busy guard** — the "Create" button is disabled while a create is in
  flight.
- **State badge styling** — the tile's state badge gets a `state-{state}`
  class so different lifecycle states render with different colours.
