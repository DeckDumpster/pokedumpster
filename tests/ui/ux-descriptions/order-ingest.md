# Order Ingest (Import Order) Page UX Description

**Source file:** `frontend/src/routes/ingest/order/+page.svelte`
**URL:** `/ingest/order`

---

## 1. Page Purpose

The Order Ingest page lets the user record a purchase order — its source,
seller, date, total, and notes — together with the cards it contained. The
user builds the card list line by line, optionally pre-filling lines by
pasting raw order text, resolves each line to a specific printing via a
set+number lookup, then commits the whole order. Committing creates the order
and navigates to its detail page.

---

## 2. Layout & Key Regions

- **Header** — `<h1>` "Import order".
- **Order metadata section (`.meta`)** — a Source `<select>` (tcgplayer,
  ebay, pokemoncenter, lgs, other) and text/date/number inputs for Seller,
  Order date, Total, and Notes.
- **Paste section (`.paste`)** — a textarea for raw order text and a "Parse"
  button.
- **Lines section** — a header "Cards ({ready}/{total} ready)" with a "+ Add
  line" button, then one editable `.line` row per card. Each line has: Set and
  # inputs, a "Look up" button, a resolved card name + variant `<select>` (or
  a name hint, or "—"), a Quantity input, a Price input, and an "✕" remove
  button. Lines that have resolved a printing get a green left border.
- **Inline error line** — red text on failure.
- **Commit button** — "Commit order ({n} card type(s))".

---

## 3. User-Facing Actions

| Element | Action |
|---------|--------|
| Source `<select>` & metadata inputs | Set the order's source, seller, date, total, notes |
| "Parse" button | Best-effort parses pasted text into pre-filled lines, then clears the textarea |
| "+ Add line" button | Appends a blank card line |
| Per-line "Look up" button | Resolves that line's set+number to a card and its printings |
| Per-line variant `<select>` | Picks which printing of the resolved card the line refers to |
| Per-line Quantity / Price inputs | Set the line's quantity and per-card purchase price |
| Per-line "✕" button | Removes that line |
| "Commit order" button | Creates the order from all resolved lines and navigates to the order detail page |

---

## 4. Navigation

- **In:** the "+ Import order" link on `/orders`.
- **Out:** a successful commit programmatically navigates to
  `/orders/{id}`.

---

## 5. Loading / Empty / Error States

| State | Condition | Appearance |
|-------|-----------|------------|
| **No lines** | `lines.length === 0` | "Add a line, or paste order text above." |
| **Per-line error** | A line's lookup fails or its set/number is blank | A red per-line error message under that line |
| **Commit validation** | "Commit order" pressed with no resolved line | "Resolve at least one line (look up its card) before committing." |
| **Commit error** | The create-order request fails | Red error text above the commit button |

---

## 6. Notable Interactive Behaviors

- **Paste parsing** — `parse()` runs a regex over each pasted line to extract
  quantity, a name hint, and a price; matched lines are appended (the name
  hint is informational only — the user still must resolve set+number). Lines
  the regex can't parse are skipped.
- **Per-line lookup** — resolving a line populates its card name and printings
  and defaults the variant to the first printing; failure clears those fields
  and shows a per-line error.
- **`ready` is derived** — only lines with a chosen `printingId` count as
  ready; the header count and the commit button's enabled state both follow
  it. The commit button is also disabled while a commit is in flight.
- No modal — the whole flow is an inline multi-step form.
