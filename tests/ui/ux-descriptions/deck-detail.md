# Deck Detail Page UX Description

**Source file:** `frontend/src/routes/decks/[id]/+page.svelte`
**URL pattern:** `/decks/{id}`

---

## 1. Page Purpose

The Deck Detail page shows a single deck's contents and lets the user manage
it: change the deck's lifecycle state, add cards into the deck from the
collection, remove individual cards (un-assigning them but leaving them in the
collection), and delete the deck entirely.

---

## 2. Layout & Key Regions

- **Header** — a left block with the deck name (`<h1>`) and a `.sub` line
  showing the owner and format (when set) plus a "Lifecycle" `<select>`; and a
  right `.actions` group with a "+ Add cards" button and a red "Delete deck"
  button.
- **Inline error line** — red text shown below the header on mutation failure.
- **Card table** — columns Name, Set, #, Variant, Condition, and an action
  cell with a "Remove" link button. The Name cell links to the card detail
  page.
- **Collection picker modal** — opened by "+ Add cards" (the shared
  `CollectionPicker`).

---

## 3. User-Facing Actions

| Element | Action |
|---------|--------|
| "Lifecycle" `<select>` | Updates the deck's state — one of idea, ready, built |
| "+ Add cards" button | Opens the collection picker modal targeting this deck |
| "Delete deck" button | `confirm()`s, then deletes the deck and navigates back to `/decks` |
| "Remove" link (per row) | Un-assigns that copy from the deck via `moveCopy(id, {})`; the copy stays in the collection |
| Card name link | Navigates to `/card/{set}/{number}` |

### Collection Picker Modal (`CollectionPicker.svelte`)

The shared modal, here titled "Add cards to {deck name}". It loads the full
collection, offers a "Search your collection…" search box, lists matching
copies as checkboxes (with a "where" tag for copies already assigned), and has
a footer with the selected count and an "Add {n} to {deck name}" button. It
closes via the "×" button or the Escape key.

---

## 4. Navigation

- **In:** deck tiles on `/decks`.
- **Out:** card-name links → `/card/{set}/{number}`; deleting the deck
  programmatically navigates to `/decks`.

---

## 5. Loading / Empty / Error States

| State | Condition | Appearance |
|-------|-----------|------------|
| **Loading** | First load, before `detail` is set | "Loading…" muted text |
| **Error (no data)** | The detail fetch fails and `detail` is still null | "Failed to load deck: {message}" |
| **Inline error** | A mutation fails after the page loads | Red error text below the header |
| **Empty deck** | `detail.cards.length === 0` | EmptyState "No cards in this deck." with an **Add cards** button that opens the same CollectionPicker as the header control |

---

## 6. Notable Interactive Behaviors

- **Reactive reload** — an `$effect` watches the `id` route param and reloads
  the deck detail.
- **No optimistic updates** — `changeState()`, `remove()`, and the picker's
  assign each set a `busy` flag, perform the mutation, then re-run `load()` to
  refresh from the server.
- **Delete** — uses the browser `confirm()` dialog; the message notes the
  cards stay in the collection.
