# Manual Entry Page UX Description

**Source file:** `frontend/src/routes/ingest/manual/+page.svelte`
**URL:** `/ingest/manual`

---

## 1. Page Purpose

The Manual Entry page lets the user add cards to the collection one at a time
by looking up a card by set code and collector number. After a lookup the page
shows the resolved card and a table of its printings; clicking "+ Add" on a
printing adds a copy at the chosen condition. A running session log records
every card added during the visit.

---

## 2. Layout & Key Regions

- **Header** — `<h1>` "Manual entry" and a muted instruction line.
- **Lookup form (`.lookup`)** — three labelled controls: a "Set code" text
  input, a "Number" text input, and a "Condition" `<select>`; plus a "Look up"
  submit button.
- **Inline error line** — red text on failure.
- **Resolved card section** — appears after a successful lookup: a card
  thumbnail and heading (the name links to the card detail page), and a table
  of printings with columns Variant, Owned, and an action cell with a "+ Add"
  button. Deprecated printings render dimmed.
- **Session log section** — appears once at least one card has been added:
  `<h2>` "This session (N)" and a green list of "Added {name} · {variant} ·
  {condition}" lines, newest first.

---

## 3. User-Facing Actions

| Element | Action |
|---------|--------|
| Set code / Number inputs | Identify the card to look up |
| Condition `<select>` | Sets the condition applied to copies added next — Near Mint, Lightly Played, Moderately Played, Heavily Played, Damaged |
| "Look up" button (or Enter) | Resolves the card by set+number |
| "+ Add" button (per printing) | Adds a copy of that printing at the current condition (`source: 'manual_id'`) |
| Resolved card name link | Navigates to `/card/{set}/{number}` |

---

## 4. Navigation

- **In:** reached by direct URL (the header "Import" link points at
  `/ingest/csv`, which links here).
- **Out:** the resolved card name links to `/card/{set}/{number}`.

---

## 5. Loading / Empty / Error States

| State | Condition | Appearance |
|-------|-----------|------------|
| **Initial** | Before any lookup | Only the form is shown — no resolved card, no log |
| **Validation error** | "Look up" pressed with a blank set or number | "Enter a set code and collector number." (no request) |
| **Lookup error** | The card lookup fails | Red error text; no resolved-card section |
| **Add error** | An add fails | Red error text; the resolved card stays visible |

---

## 6. Notable Interactive Behaviors

- **Owned counts refresh after each add** — `add()` re-fetches the card after
  a successful add so the printings table's "Owned" column stays current
  without leaving the page.
- **Session log** — purely client-side; it prepends each added card and is
  lost on navigation/reload.
- **Busy guard** — the "Look up" and "+ Add" buttons are disabled while a
  request is in flight.
- No modals.
