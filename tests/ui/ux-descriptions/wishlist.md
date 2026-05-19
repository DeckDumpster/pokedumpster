# Wishlist Page UX Description

**Source file:** `frontend/src/routes/wishlist/+page.svelte`
**URL:** `/wishlist`

---

## 1. Page Purpose

The Wishlist page tracks cards the user wants to acquire. The user adds a wish
by entering a set code and collector number (with an optional priority and max
price), sees their open wishes in a table, can mark a wish fulfilled or reopen
it, and can remove wishes. A toggle reveals already-fulfilled wishes.

---

## 2. Layout & Key Regions

- **Title** — `<h1>` "Wishlist".
- **Add form (`.addform`)** — four labelled inputs (Set, Number, Priority, Max
  price) and an "Add wish" submit button.
- **Show-fulfilled toggle** — a checkbox labelled "Show fulfilled".
- **Inline error line** — red text on failure.
- **Wishlist table** — columns Card, Set, #, Priority, Max, and an action cell
  with "Fulfill"/"Reopen" and "Remove" link buttons. The Card cell links to
  the card detail page; fulfilled rows render dimmed.

---

## 3. User-Facing Actions

| Element | Action |
|---------|--------|
| Set / Number inputs | Identify the card to wish for (both required) |
| Priority input | Optional numeric priority |
| Max price input | Optional max price |
| "Add wish" button (or Enter) | Looks up the card by set+number, then adds the wish and reloads |
| "Show fulfilled" checkbox | Includes/excludes fulfilled wishes from the table |
| "Fulfill" link (per row) | Marks an open wish fulfilled |
| "Reopen" link (per row) | Reopens a fulfilled wish |
| "Remove" link (per row) | Deletes the wish |
| Card name link | Navigates to `/card/{set}/{number}` |

---

## 4. Navigation

- **In:** "Wishlist" header nav link; the collection page's bulk "Add to
  wishlist" action and the card detail page write wishes that surface here.
- **Out:** card-name links → `/card/{set}/{number}`.

---

## 5. Loading / Empty / Error States

| State | Condition | Appearance |
|-------|-----------|------------|
| **Loading** | While the wishlist fetch runs | "Loading…" muted text |
| **Empty (open)** | No wishes and "Show fulfilled" is off | "Nothing on your wishlist yet." |
| **Empty (all)** | No wishes and "Show fulfilled" is on | "Nothing on your wishlist." |
| **Validation error** | "Add wish" pressed with a blank set or number | "Set code and collector number are required." (no request sent) |
| **Inline error** | A lookup, add, or row action fails | Red error text below the toggle |

---

## 6. Notable Interactive Behaviors

- **Reactive reload** — an `$effect` watches the "Show fulfilled" toggle and
  re-fetches the wishlist (passing the toggle) whenever it changes.
- **Add resolves the card first** — `add()` calls `api.card(set, number)` to
  resolve a `card_id`, then `addWish`; a bad set/number surfaces as an inline
  error. On success all four inputs reset.
- **Row actions share a helper** — Fulfill, Reopen, and Remove all run through
  `act()`, which sets `busy`, runs the call, then reloads. All row buttons are
  disabled while `busy`.
- **Optional fields** — priority and max price are omitted from the request
  when blank/zero.
- No modals — the add form is inline.
