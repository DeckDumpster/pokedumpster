# Binder Detail Page UX Description

**Source file:** `frontend/src/routes/binders/[id]/+page.svelte`
**URL pattern:** `/binders/{id}`

---

## 1. Page Purpose

The Binder Detail page shows a single binder's contents and lets the user
manage it: add cards into the binder from the collection, remove individual
cards (which un-assigns them, leaving them in the collection), and delete the
binder entirely.

---

## 2. Layout & Key Regions

- **Header** — the binder name (`<h1>`) and an `.actions` group with a
  "+ Add cards" button and a red "Delete binder" button.
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
| "+ Add cards" button | Opens the collection picker modal targeting this binder |
| "Delete binder" button | `confirm()`s, then deletes the binder and navigates back to `/binders` |
| "Remove" link (per row) | Un-assigns that copy from the binder via `moveCopy(id, {})`; the copy stays in the collection |
| Card name link | Navigates to `/card/{set}/{number}` |

### Collection Picker Modal (`CollectionPicker.svelte`)

A centered modal titled "Add cards to {binder name}". It loads the full
collection, offers a "Search your collection…" text box, and lists matching
rows as checkbox labels showing card name, "{set_code} · {variant}", and a
"where" tag ("here", "in a binder", or "in a deck") when the copy is already
assigned. A footer shows "{n} selected" and an "Add {n} to {binder name}"
button. It closes via the "×" button or the Escape key.

---

## 4. Navigation

- **In:** binder tiles on `/binders`.
- **Out:** card-name links → `/card/{set}/{number}`; deleting the binder
  programmatically navigates to `/binders`.

---

## 5. Loading / Empty / Error States

| State | Condition | Appearance |
|-------|-----------|------------|
| **Loading** | First load, before `detail` is set | "Loading…" muted text |
| **Error (no data)** | The detail fetch fails and `detail` is still null | "Failed to load binder: {message}" |
| **Inline error** | A mutation fails after the page loads | Red error text below the header; the page stays visible |
| **Empty binder** | `detail.cards.length === 0` | EmptyState "No cards in this binder." with an **Add cards** button that opens the same CollectionPicker as the header control |

The picker modal has its own "Loading collection…", error, and "No matching
cards." states.

---

## 6. Notable Interactive Behaviors

- **Reactive reload** — an `$effect` watches the `id` route param and reloads
  the binder detail.
- **No optimistic updates** — `remove()` and the picker's assign both set a
  `busy` flag, perform the mutation, then re-run `load()` to refresh from the
  server.
- **Picker assign loops** — selecting multiple cards in the picker issues one
  `moveCopy` call per selected copy, then fires the `onAssigned` callback
  (which reloads the page) and closes the modal.
- **Delete** — uses the browser `confirm()` dialog; the message notes the
  cards stay in the collection.
