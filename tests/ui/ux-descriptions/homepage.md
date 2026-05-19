# Homepage UX Description

**Source file:** `frontend/src/routes/+page.svelte`
**URL:** `/`

---

## 1. Page Purpose

The homepage is the root landing page of the PokeDumpster web app. It is
intentionally minimal: it shows the app name and a one-line description, and
directs the user to pick a section from the global navigation bar. It carries
no data, no fetches, and no interactive controls of its own.

---

## 2. Layout & Key Regions

The page content is two static elements rendered inside the shared layout's
`<main>` region:

| Region | Element | Content |
|--------|---------|---------|
| Title | `<h1>` | "PokeDumpster" |
| Description | `<p>` | "A Pokémon TCG collection tracker. Choose a section from the navigation above." |

Above this (provided by `+layout.svelte`, present on every page) is the global
header: the red "PokeDumpster" brand link and a horizontal nav with links to
Home, Collection, Browse, Binders, Decks, Sealed, Wishlist, Orders, Recent,
and Import.

---

## 3. User-Facing Actions

The page itself has no buttons, inputs, modals, or forms. The only actionable
elements visible while on this page are the global header links.

---

## 4. Navigation

| Element | Target | Notes |
|---------|--------|-------|
| "PokeDumpster" brand link (header) | `/` | Reloads the homepage |
| "Home" nav link | `/` | — |
| "Collection" nav link | `/collection` | — |
| "Browse" nav link | `/browse` | — |
| "Binders" nav link | `/binders` | — |
| "Decks" nav link | `/decks` | — |
| "Sealed" nav link | `/sealed` | — |
| "Wishlist" nav link | `/wishlist` | — |
| "Orders" nav link | `/orders` | — |
| "Recent" nav link | `/recent` | — |
| "Import" nav link | `/ingest/csv` | — |

There is no breadcrumb and no outbound link in the page body.

---

## 5. Loading / Empty / Error States

The page is fully static markup. There is no loading state, no empty state,
and no error state — it always renders identically and immediately.

---

## 6. Notable Interactive Behaviors

None. No modals, no debounced search, no optimistic updates, no async work.
The page is purely a navigational signpost.
