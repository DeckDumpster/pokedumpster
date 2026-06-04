<script lang="ts">
	import { onMount, untrack } from 'svelte';
	import { page } from '$app/state';
	import { replaceState } from '$app/navigation';
	import { api, SearchQueryError } from '$lib/api';
	import { variantLabel, variantTag, variants } from '$lib/variants.svelte';
	import { conditionMultiplier } from '$lib/conditions';
	import { money, count } from '$lib/format';

	// Foil shimmer treatment for holo / reverse-holo / pattern-RH /
	// cosmos_holo variants — ranks 1..3 in the variants table. Stamps
	// (rank 4) and normals (rank 0) are matte.
	function isFoilVariant(code: string): boolean {
		const rank = variants.map[code]?.rank ?? 0;
		return rank >= 1 && rank <= 3;
	}
	import CardModal from '$lib/components/CardModal.svelte';
	import Pokeball from '$lib/components/Pokeball.svelte';
	import type { CollectionRow } from '$lib/types/CollectionRow';
	import type { SearchRow } from '$lib/types/SearchRow';
	import type { Binder } from '$lib/types/Binder';
	import type { Deck } from '$lib/types/Deck';

	// Server-side search results (one row per printing, owned or not). The
	// page's existing rendering is per-copy, so owned printings are flattened
	// back into CollectionRow[] (`rows`); unowned printings (owned_count === 0)
	// drive the dimmed "missing" tiles via `unownedCatalog`.
	let searchRows = $state<SearchRow[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	// A query-language parse error, shown under the search box with a caret.
	let searchError = $state<{ message: string; position: number } | null>(null);

	const rows = $derived<CollectionRow[]>(
		searchRows.flatMap((sr) =>
			sr.copies.map((cp) => ({
				id: cp.id,
				printing_id: sr.printing_id,
				condition: cp.condition,
				language: cp.language,
				purchase_price: cp.purchase_price,
				sale_price: null,
				acquired_at: cp.acquired_at,
				source: '',
				notes: null,
				status: cp.status,
				graded: cp.graded,
				binder_id: cp.binder_id,
				deck_id: cp.deck_id,
				variant: sr.variant,
				card_id: sr.card_id,
				set_code: sr.set_code,
				set_name: sr.set_name,
				set_ptcgo_code: sr.set_ptcgo_code,
				set_symbol_url: sr.set_symbol_url,
				number: sr.number,
				name: sr.name,
				rarity: sr.rarity,
				artist: sr.artist,
				supertype: sr.supertype,
				subtypes: sr.subtypes,
				types: sr.types,
				attacks: sr.attacks,
				market_price: sr.market_price,
				image_small: sr.image_small,
				variant_description: sr.variant_description
			}))
		)
	);
	// Printings the user doesn't own — surfaced when "All cards" is on or the
	// query uses is:missing. Rendered as dimmed tiles (SearchRow carries every
	// field the missing-tile markup reads).
	const unownedCatalog = $derived(searchRows.filter((sr) => sr.owned_count === 0));

	// Debounced search. Initial value comes from ?q= so clickable facets
	// on the card-detail page (artist, set, energy type, rarity, …) can
	// land here with the filter pre-applied.
	const initialQuery =
		typeof window !== 'undefined' ? (page.url.searchParams.get('q') ?? '') : '';
	let searchRaw = $state(initialQuery);
	// The committed (debounced) query sent to the server. Empty = owned default.
	let query = $state(initialQuery.trim());
	let debounce: ReturnType<typeof setTimeout>;
	let searchInput = $state<HTMLInputElement | undefined>();
	function onSearch(value: string) {
		searchRaw = value;
		clearTimeout(debounce);
		debounce = setTimeout(() => {
			query = value.trim();
			// Reflect the active query in the URL so refreshes + back-button
			// keep state. SvelteKit's replaceState (not window.history's)
			// keeps the router's internal state aligned with the URL —
			// raw window.history.replaceState corrupts the history entry
			// so a later popstate from a navigated-away page can leave
			// SvelteKit unable to re-render this route.
			if (typeof window !== 'undefined') {
				const url = new URL(window.location.href);
				if (query) url.searchParams.set('q', searchRaw.trim());
				else url.searchParams.delete('q');
				replaceState(url, {});
			}
		}, 200);
	}

	// Re-sync from the URL whenever it changes — covers the case where the
	// user clicks a facet link in the card modal while already on
	// /collection. SvelteKit's client router updates the URL but doesn't
	// remount us, so the initial-value read above never re-runs. `untrack`
	// keeps this effect from refiring when the user types into the box
	// (which writes searchRaw + the URL itself); only an external URL
	// change triggers the resync.
	$effect(() => {
		const q = page.url.searchParams.get('q') ?? '';
		untrack(() => {
			if (q !== searchRaw) {
				clearTimeout(debounce);
				searchRaw = q;
				query = q.trim();
				// Close any open modal so the filtered list is visible.
				selectedCard = null;
			}
		});
	});

	// "All cards" toggle widens the server search from owned-only to the whole
	// catalog (include_unowned=1). Unowned printings come back with
	// owned_count === 0 and render as dimmed "missing" tiles — the same visual
	// treatment /browse/[set] uses for unowned binder slots.
	const initialAllCards =
		typeof window !== 'undefined' && page.url.searchParams.get('all') === '1';
	let allCards = $state(initialAllCards);
	function toggleAllCards() {
		allCards = !allCards;
		// Persist so refresh + back-button keep the choice (matches how
		// search ?q= is reflected).
		if (typeof window !== 'undefined') {
			const url = new URL(window.location.href);
			if (allCards) url.searchParams.set('all', '1');
			else url.searchParams.delete('all');
			replaceState(url, {});
		}
	}

	/** Run the server-side search for the current query + toggle. */
	async function runSearch() {
		loading = true;
		error = null;
		searchError = null;
		try {
			searchRows = await api.collectionSearch(query, undefined, undefined, allCards);
		} catch (e) {
			if (e instanceof SearchQueryError) {
				searchError = { message: e.message, position: e.position };
				searchRows = [];
			} else {
				error = e instanceof Error ? e.message : String(e);
			}
		} finally {
			loading = false;
		}
	}

	// Re-search whenever the committed query or the All-cards toggle changes
	// (also fires once on mount).
	$effect(() => {
		void query;
		void allCards;
		runSearch();
	});

	// --- Multi-select bulk operations. ---
	let binders = $state<Binder[]>([]);
	let decks = $state<Deck[]>([]);
	let selectMode = $state(false);
	let selected = $state(new Set<number>());
	let busy = $state(false);

	// Grid (card images) vs. table view — persisted across reloads in
	// localStorage so refreshes don't snap back to the default.
	function readStoredView(): 'grid' | 'table' {
		if (typeof window === 'undefined') return 'grid';
		const v = localStorage.getItem('collection.view');
		return v === 'table' || v === 'grid' ? v : 'grid';
	}
	let view = $state<'grid' | 'table'>(readStoredView());
	$effect(() => {
		if (typeof window !== 'undefined') {
			localStorage.setItem('collection.view', view);
		}
	});
	let selectedCard = $state<{ set: string; number: string } | null>(null);

	// Burger menu (Export CSV, Select) lives in the sticky top bar.
	let menuOpen = $state(false);
	function closeMenu() {
		menuOpen = false;
	}

	// Column sort for the table view — persisted across reloads in
	// localStorage so refreshes don't snap back to the default (mirrors
	// the view-mode persistence above).
	const SORT_KEYS = ['name', 'type', 'etype', 'rarity', 'set', 'number', 'nm', 'market', 'value', 'qty'];
	function readStoredSort(): { key: string; dir: 'asc' | 'desc' } {
		if (typeof window === 'undefined') return { key: 'name', dir: 'asc' };
		const k = localStorage.getItem('collection.sortKey');
		const d = localStorage.getItem('collection.sortDir');
		return {
			key: k && SORT_KEYS.includes(k) ? k : 'name',
			dir: d === 'desc' ? 'desc' : 'asc'
		};
	}
	const _storedSort = readStoredSort();
	let sortKey = $state(_storedSort.key);
	let sortDir = $state<'asc' | 'desc'>(_storedSort.dir);
	$effect(() => {
		if (typeof window !== 'undefined') {
			localStorage.setItem('collection.sortKey', sortKey);
			localStorage.setItem('collection.sortDir', sortDir);
		}
	});

	function sortBy(key: string) {
		if (sortKey === key) {
			sortDir = sortDir === 'asc' ? 'desc' : 'asc';
		} else {
			sortKey = key;
			// Counts and money default to high→low; everything else low→high.
			sortDir =
				key === 'qty' || key === 'nm' || key === 'market' || key === 'value' ? 'desc' : 'asc';
		}
	}

	/** Close the modal and re-run the search — the modal may have mutated copies. */
	async function closeCard() {
		selectedCard = null;
		await runSearch();
	}

	onMount(async () => {
		// The $effect above runs the initial search; here we only load the
		// binder/deck lists used by the bulk-assign menus.
		try {
			[binders, decks] = await Promise.all([api.binders(), api.decks()]);
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
	});

	// The server applies the query now; `filtered` is just the owned copies it
	// returned (kept as a name so the rest of the page is unchanged).
	const filtered = $derived(rows);
	// Header total is the sum of *condition-adjusted* market values across
	// the filtered rows, so it equals the sum of the per-row Value cells
	// shown below (which also apply the multiplier).
	const totalValue = $derived(
		filtered.reduce((s, r) => s + (r.market_price ?? 0) * conditionMultiplier(r.condition), 0)
	);


	function toggleSelectMode() {
		selectMode = !selectMode;
		if (!selectMode) selected = new Set();
	}

	function toggleRow(id: number) {
		const next = new Set(selected);
		if (next.has(id)) next.delete(id);
		else next.add(id);
		selected = next;
	}

	/** Open a row's card in the detail modal — unless we're multi-selecting. */
	function openCard(row: CollectionRow) {
		if (selectMode) toggleRow(row.id);
		else selectedCard = { set: row.set_code, number: row.number };
	}

	// === Table view: aggregate per-copy rows so the table can show Qty. ===

	type AggRow = {
		key: string;
		ids: number[];
		qty: number;
		paid_total: number | null;
		/** Per-copy raw Near Mint market price (every row in the group shares
		    a printing, so this is condition-independent). */
		nm_unit: number | null;
		/** Per-copy condition-adjusted market price (nm_unit × the copy's
		    condition multiplier). */
		market_unit: number | null;
		/** Sum across all copies in the group — market_unit × qty. */
		market_total: number | null;
		printing_id: string;
		card_id: string;
		set_code: string;
		set_ptcgo_code: string | null;
		set_symbol_url: string | null;
		number: string;
		name: string;
		rarity: string | null;
		supertype: string | null;
		subtypes: string | null;
		types: string | null;
		attacks: string | null;
		variant: string;
		condition: string;
		status: string;
		image_small: string | null;
	};

	function aggregate(input: CollectionRow[]): AggRow[] {
		const map = new Map<string, AggRow>();
		for (const r of input) {
			const key = `${r.printing_id}|${r.condition}|${r.status}`;
			// market_price is always NM market from the API; condition-adjust
			// it here so the Value column reflects the copy's actual worth,
			// not the NM headline (pokedumpster-qtp).
			const condValue =
				r.market_price != null ? r.market_price * conditionMultiplier(r.condition) : null;
			const existing = map.get(key);
			if (existing) {
				existing.ids.push(r.id);
				existing.qty += 1;
				if (r.purchase_price != null) {
					existing.paid_total = (existing.paid_total ?? 0) + r.purchase_price;
				}
				if (condValue != null) {
					existing.market_total = (existing.market_total ?? 0) + condValue;
				}
			} else {
				map.set(key, {
					key,
					ids: [r.id],
					qty: 1,
					paid_total: r.purchase_price,
					nm_unit: r.market_price,
					market_unit: condValue,
					market_total: condValue,
					printing_id: r.printing_id,
					card_id: r.card_id,
					set_code: r.set_code,
					set_ptcgo_code: r.set_ptcgo_code,
					set_symbol_url: r.set_symbol_url,
					number: r.number,
					name: r.name,
					rarity: r.rarity,
					supertype: r.supertype,
					subtypes: r.subtypes,
					types: r.types,
					attacks: r.attacks,
					variant: r.variant,
					condition: r.condition,
					status: r.status,
					image_small: r.image_small
				});
			}
		}
		return [...map.values()];
	}

	function parseJsonStrArr(s: string | null): string[] {
		if (!s) return [];
		try {
			const v: unknown = JSON.parse(s);
			return Array.isArray(v) ? v.map(String) : [];
		} catch {
			return [];
		}
	}

	// Attack list — one line of energy pips per attack in the Cost column.
	type Attack = { name: string; cost: string[] };
	function parseAttacks(s: string | null): Attack[] {
		if (!s) return [];
		try {
			const v: unknown = JSON.parse(s);
			if (!Array.isArray(v)) return [];
			return v.map((a) => {
				const obj = a as { name?: unknown; cost?: unknown };
				const cost = Array.isArray(obj.cost) ? obj.cost.map(String) : [];
				return { name: String(obj.name ?? ''), cost };
			});
		} catch {
			return [];
		}
	}

	function typeMain(a: AggRow): string {
		return a.supertype ?? '';
	}
	function typeSub(a: AggRow): string {
		return parseJsonStrArr(a.subtypes).join(' ');
	}

	// Energy-type icons live under /static/energy/<lowercase>.png — pulled
	// from pkmn.gg (see static/energy/README.md). "Free" — a zero-energy
	// attack cost the card art draws as a clear circle — maps to the
	// colorless icon, and anything else unknown falls back to colorless
	// rather than 404 → broken-image.
	const KNOWN_ENERGY = new Set([
		'grass',
		'fire',
		'water',
		'lightning',
		'psychic',
		'fighting',
		'darkness',
		'metal',
		'fairy',
		'dragon',
		'colorless'
	]);
	function energyIcon(type: string): string {
		const t = type.toLowerCase();
		return `/energy/${KNOWN_ENERGY.has(t) ? t : 'colorless'}.png`;
	}

	// Map a catalog rarity string ("Special Illustration Rare",
	// "MEGA_ATTACK_RARE") to the kebab-slug filename under static/rarity/
	// where the matching SVG lives. Same lowercase + dash rule as the
	// fetcher in static/rarity/README.md, so a refresh from pkmn.gg is
	// fire-and-forget.
	function rarityIconSrc(rarity: string | null): string | null {
		if (!rarity) return null;
		const slug = rarity.toLowerCase().replace(/[ ._]/g, '-');
		return `/rarity/${slug}.svg`;
	}

	// The classic ●/◆/★ glyphs are pure shapes filling their viewBox, so
	// they read much larger than the detailed illustration/hyper/secret
	// icons at the same render size. Scale just those three down to match.
	function isBasicRarity(rarity: string | null): boolean {
		return (
			rarity === 'Common' ||
			rarity === 'Uncommon' ||
			rarity === 'Rare' ||
			rarity === 'Illustration Rare' ||
			rarity === 'ACE SPEC Rare' ||
			rarity === 'Rare ACE'
		);
	}

	const RARITY_RANK: Record<string, number> = {
		Common: 1,
		Uncommon: 2,
		Rare: 3,
		Promo: 4,
		'Classic Collection': 4,
		'Rare Holo': 5,
		'Radiant Rare': 6,
		'Rare Holo EX': 7,
		'Rare Holo GX': 7,
		'Rare Holo V': 7,
		'Double Rare': 7,
		'Rare Holo VMAX': 8,
		'Rare Holo VSTAR': 8,
		'Ultra Rare': 8,
		'Amazing Rare': 9,
		'Rare Shiny': 9,
		'Rare Shiny GX': 9,
		'Illustration Rare': 10,
		// Mega Attack Rare slots between IR and SIR in the printed set
		// numbering for Mega Evolution sets (me1..me4, me2pt5).
		'Mega Attack Rare': 11,
		'Trainer Gallery Rare Holo': 11,
		'Rare Secret': 12,
		'Rare Rainbow': 12,
		'Special Illustration Rare': 13,
		'Hyper Rare': 14,
		'Rare Holo Star': 14,
		'Mega Hyper Rare': 15
	};
	// Upstream rarity strings arrive in both 'Title Case' and
	// 'SCREAMING_SNAKE' (e.g. MEGA_ATTACK_RARE) depending on which feed
	// catalogued the card. Canonicalise to title case before any lookup
	// so the rank table doesn't need both spellings.
	function canonicalRarity(r: string): string {
		return r
			.toLowerCase()
			.replace(/_/g, ' ')
			.split(' ')
			.filter((w) => w.length > 0)
			.map((w) => w.charAt(0).toUpperCase() + w.slice(1))
			.join(' ');
	}
	function rarityRank(r: string | null): number {
		if (!r) return 0;
		return RARITY_RANK[canonicalRarity(r)] ?? 6;
	}

	function numberKey(n: string): number {
		const m = n.match(/(\d+)/);
		return m ? parseInt(m[1], 10) : 0;
	}

	function sortValue(a: AggRow, key: string): number | string {
		switch (key) {
			case 'qty':
				return a.qty;
			case 'name':
				return a.name.toLowerCase();
			case 'type':
				return `${typeMain(a)} ${typeSub(a)}`.toLowerCase();
			case 'etype':
				return parseJsonStrArr(a.types).join(' ').toLowerCase();
			case 'set':
				return (a.set_ptcgo_code ?? a.set_code).toLowerCase();
			case 'number':
				return numberKey(a.number);
			case 'rarity':
				return rarityRank(a.rarity);
			case 'nm':
				return a.nm_unit ?? -1;
			case 'market':
				return a.market_unit ?? -1;
			case 'value':
				return a.market_total ?? -1;
			default:
				return 0;
		}
	}

	const aggregated = $derived(aggregate(filtered));
	const sorted = $derived.by(() => {
		const out = [...aggregated];
		out.sort((a, b) => {
			const va = sortValue(a, sortKey);
			const vb = sortValue(b, sortKey);
			const cmp = va < vb ? -1 : va > vb ? 1 : 0;
			return sortDir === 'asc' ? cmp : -cmp;
		});
		return out;
	});


	function groupChecked(ids: number[]): boolean {
		// An unowned printing has no copies — never "checked" (empty .every is true).
		return ids.length > 0 && ids.every((id) => selected.has(id));
	}
	function toggleGroup(ids: number[]) {
		const all = groupChecked(ids);
		const next = new Set(selected);
		for (const id of ids) {
			if (all) next.delete(id);
			else next.add(id);
		}
		selected = next;
	}
	function openGroup(a: AggRow) {
		if (selectMode) toggleGroup(a.ids);
		else selectedCard = { set: a.set_code, number: a.number };
	}

	// Non-owned statuses surface as a small badge next to the card name —
	// the column itself is gone (no point spamming "owned" on every row).
	function statusBadge(status: string): string | null {
		switch (status) {
			case 'owned':
				return null;
			case 'ordered':
				return 'ORD';
			case 'listed':
				return 'LST';
			case 'sold':
				return 'SLD';
			case 'traded':
				return 'TRD';
			case 'gifted':
				return 'GFT';
			case 'lost':
				return 'LOST';
			case 'removed':
				return 'RMV';
			default:
				return status.slice(0, 3).toUpperCase();
		}
	}

	// The header checkbox in the table selects/clears every owned aggregated
	// row (unowned printings have no copies to select).
	const ownedSorted = $derived(sorted.filter((a) => a.ids.length > 0));
	const tableAllSelected = $derived(
		ownedSorted.length > 0 && ownedSorted.every((a) => groupChecked(a.ids))
	);
	function toggleTableAll() {
		if (tableAllSelected) {
			selected = new Set();
		} else {
			const next = new Set<number>();
			for (const a of ownedSorted) for (const id of a.ids) next.add(id);
			selected = next;
		}
	}

	// The grid still operates per copy; its header checkbox sees raw rows.
	const allSelected = $derived(
		filtered.length > 0 && filtered.every((r) => selected.has(r.id))
	);

	/** Re-run the search after a bulk mutation, then drop the selection. */
	async function refresh() {
		await runSearch();
		selected = new Set();
	}

	async function bulkDelete() {
		const ids = [...selected];
		if (!ids.length || !confirm(`Delete ${ids.length} selected ${ids.length === 1 ? 'copy' : 'copies'}?`))
			return;
		busy = true;
		try {
			await api.bulkDelete(ids);
			await refresh();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}

	async function bulkAssign(field: 'binder_id' | 'deck_id', value: string) {
		const id = Number(value);
		if (!id) return;
		const ids = [...selected];
		busy = true;
		try {
			for (const copyId of ids) {
				await api.moveCopy(copyId, { [field]: id });
			}
			await refresh();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}

	async function bulkWishlist() {
		// One wish per distinct card — selecting two copies of a card wishes it once.
		const seen = new Set<string>();
		const wishes = rows
			.filter((r) => selected.has(r.id))
			.filter((r) => (seen.has(r.card_id) ? false : (seen.add(r.card_id), true)));
		busy = true;
		try {
			for (const r of wishes) {
				await api.addWish({ card_id: r.card_id, printing_id: r.printing_id });
			}
			selected = new Set();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}
</script>

<svelte:head><title>Collection — PokeDumpster</title></svelte:head>

<svelte:window
	onkeydown={(e) => {
		if (e.key === 'Escape') menuOpen = false;
	}}
/>

<!--
  DD-style chrome: a sticky top bar with brand → home, the search
  pinned to the viewport, glyph view toggle, and Export/Select hidden
  behind a burger. The global nav is suppressed for this page by
  +layout.svelte.
-->
<header class="topbar">
	<div class="row row1">
		<a class="brand" href="/" aria-label="Home" title="Home">
			<span class="brandmark"><Pokeball size={26} /></span>
		</a>
		<div class="searchwrap">
			<input
				class="search"
				class:error={searchError !== null}
				data-testid="search-input"
				type="text"
				placeholder={allCards ? 'Search all cards… (t:fire hp>=200)' : 'Search… (t:fire hp>=200)'}
				value={searchRaw}
				oninput={(e) => onSearch(e.currentTarget.value)}
				bind:this={searchInput}
			/>
			{#if searchRaw}
				<button
					class="searchclear"
					type="button"
					aria-label="Clear search"
					title="Clear"
					onclick={() => {
						onSearch('');
						searchInput?.focus();
					}}
				>×</button>
			{/if}
		</div>
		<label class="alltoggle" title="Search the full card catalog, not just your collection">
			<input
				type="checkbox"
				data-testid="all-cards-toggle"
				checked={allCards}
				onchange={toggleAllCards}
			/>
			All cards
		</label>
		<a class="helplink" data-testid="search-help-link" href="/search-help" title="Search syntax help"
			>?</a
		>
	</div>
	{#if searchError}
		<div class="row searcherr" data-testid="search-error" role="alert">
			<span class="errmsg">{searchError.message}</span>
			<span class="errpos">position {searchError.position}</span>
		</div>
	{/if}
	<div class="row row2">
		{#if searchRows.length > 0}
			<div class="viewtoggle" role="group" aria-label="View">
				<button
					class:on={view === 'grid'}
					data-testid="view-grid"
					onclick={() => (view = 'grid')}
					aria-label="Grid view"
					title="Grid"
				>▦</button>
				<button
					class:on={view === 'table'}
					data-testid="view-table"
					onclick={() => (view = 'table')}
					aria-label="Table view"
					title="Table"
				>≡</button>
			</div>
			<div class="burgerWrap">
				<button
					class="burger"
					onclick={() => (menuOpen = !menuOpen)}
					aria-label="Menu"
					aria-expanded={menuOpen}
					title="Menu"
				>⋯</button>
				{#if menuOpen}
					<div class="menu" role="menu">
						<a class="menuItem" href="/api/export/csv" download onclick={closeMenu}>Export CSV</a>
						<button
							class="menuItem"
							onclick={() => {
								toggleSelectMode();
								closeMenu();
							}}
						>
							{selectMode ? 'Cancel select' : 'Select'}
						</button>
					</div>
				{/if}
			</div>
		{/if}
		<p class="countline muted">
			{count(filtered.length)}
			cards{#if totalValue > 0}, {money(totalValue)}{/if}
		</p>
	</div>
</header>
<div class="topbarSpacer" aria-hidden="true"></div>

{#if menuOpen}
	<div
		class="menuBackdrop"
		role="presentation"
		onclick={closeMenu}
	></div>
{/if}

{#if loading}
	<p class="muted">Loading…</p>
{:else if error}
	<p class="error">Failed to load collection: {error}</p>
{:else}

	{#if selectMode && selected.size > 0}
		<div class="bulkbar">
			<span class="count">{selected.size} selected</span>
			<button disabled={busy} onclick={bulkDelete}>Delete</button>
			<select
				disabled={busy || binders.length === 0}
				onchange={(e) => {
					bulkAssign('binder_id', e.currentTarget.value);
					e.currentTarget.selectedIndex = 0;
				}}
			>
				<option value="">Assign to binder…</option>
				{#each binders as b (b.id)}<option value={b.id}>{b.name}</option>{/each}
			</select>
			<select
				disabled={busy || decks.length === 0}
				onchange={(e) => {
					bulkAssign('deck_id', e.currentTarget.value);
					e.currentTarget.selectedIndex = 0;
				}}
			>
				<option value="">Assign to deck…</option>
				{#each decks as d (d.id)}<option value={d.id}>{d.name}</option>{/each}
			</select>
			<button disabled={busy} onclick={bulkWishlist}>Add to wishlist</button>
		</div>
	{/if}

	{#if searchRows.length === 0}
		<p class="muted">
			{#if searchError}Fix the query above to see results.{:else if query}No cards match
				<code>{query}</code>.{:else if allCards}No cards in the catalog.{:else}Your collection is
				empty. Add cards from a set's binder view, or turn on “All cards”.{/if}
		</p>
	{:else if view === 'grid'}
		<!-- Grid lacks the table's sortable column headers, so it gets a
		     row of per-field buttons. Click an inactive button to switch
		     to that field (with a sensible default direction); click the
		     active one to toggle asc/desc. State is shared with the
		     table view (sortKey/sortDir) so flipping the view never
		     loses your sort. -->
		<div class="gridsort">
			{#snippet sortBtn(key: string, label: string)}
				<button
					class="sortbtn"
					class:active={sortKey === key}
					onclick={() => sortBy(key)}
				>
					{label}
					{#if sortKey === key}
						<span class="caret">{sortDir === 'asc' ? '▲' : '▼'}</span>
					{/if}
				</button>
			{/snippet}
			{@render sortBtn('name', 'Name')}
			{@render sortBtn('type', 'Class')}
			{@render sortBtn('etype', 'Type')}
			{@render sortBtn('rarity', 'Rarity')}
			{@render sortBtn('set', 'Set')}
			{@render sortBtn('number', '#')}
			{@render sortBtn('nm', 'NM')}
			{@render sortBtn('market', 'Adj.')}
		</div>
		<div class="cardgrid">
			{#each sorted as a (a.key)}
				<button
					class="cardtile"
					class:picked={selectMode && groupChecked(a.ids)}
					class:foil={isFoilVariant(a.variant)}
					title="{a.name} · {variantLabel(a.variant)}{a.qty > 1 ? ` ×${a.qty}` : ''}"
					onclick={() => openGroup(a)}
				>
					{#if a.image_small}
						<img src={a.image_small} alt={a.name} loading="lazy" />
					{:else}
						<div class="tilenoart">{a.name}</div>
					{/if}
					{#if a.qty > 1}<span class="qtybadge">×{a.qty}</span>{/if}
					{#if selectMode && groupChecked(a.ids)}<span class="tick">✓</span>{/if}
				</button>
			{/each}
			{#each unownedCatalog as c (c.card_id)}
				<button
					class="cardtile missing"
					title="{c.name} · {(c.set_ptcgo_code ?? c.set_code).toUpperCase()} #{c.number} · click to add"
					onclick={() => (selectedCard = { set: c.set_code, number: c.number })}
				>
					{#if c.image_small}
						<img src={c.image_small} alt={c.name} loading="lazy" />
					{:else}
						<div class="tilenoart">{c.name}</div>
					{/if}
				</button>
			{/each}
		</div>
	{:else}
		{#snippet sortable(key: string, label: string, extra: string, title?: string)}
			<th class="sortable {extra}" {title} onclick={() => sortBy(key)}>
				{label}
				{#if sortKey === key}
					<span class="caret">{sortDir === 'asc' ? '▲' : '▼'}</span>
				{/if}
			</th>
		{/snippet}
		<div class="tableScroll">
		<table class="dd">
			<thead>
				<tr>
					{#if selectMode}
						<th class="cbcol">
							<input type="checkbox" checked={tableAllSelected} onchange={toggleTableAll} />
						</th>
					{/if}
					{@render sortable('qty', 'Qty', 'num qty')}
					{@render sortable('name', 'Name', 'colflex')}
					{@render sortable('type', 'Class', '')}
					{@render sortable('etype', 'Type', 'center')}
					<th>Cost</th>
					{@render sortable('rarity', 'Rarity', 'center')}
					{@render sortable('set', 'Set', 'center')}
					{@render sortable('number', '#', 'num')}
					{@render sortable('nm', 'NM', 'num', 'Near Mint market price (per copy)')}
					{@render sortable(
						'market',
						'Adj.',
						'num',
						'Condition-adjusted price (per copy)'
					)}
					{@render sortable('value', 'Value', 'num', 'Condition-adjusted value (× qty)')}
				</tr>
			</thead>
			<tbody>
				{#each sorted as a (a.key)}
					<tr class:picked={selectMode && groupChecked(a.ids)} onclick={() => openGroup(a)}>
						{#if selectMode}
							<td class="cbcol" onclick={(e) => e.stopPropagation()}>
								<input
									type="checkbox"
									checked={groupChecked(a.ids)}
									onchange={() => toggleGroup(a.ids)}
								/>
							</td>
						{/if}
						<td class="num qty">{a.qty}</td>
						<td class="colflex">
							<div class="namecell">
								{#if a.image_small}
									<span class="thumbwrap" class:foil={isFoilVariant(a.variant)}>
										<img class="cardthumb" src={a.image_small} alt="" loading="lazy" />
									</span>
								{/if}
								<span class="namebody"
									><span class="cardname">{a.name}</span>{#if a.variant !== 'normal'}<span
											class="tag vtag"
											title={variantLabel(a.variant)}>{variantTag(a.variant)}</span
										>{/if}{#if statusBadge(a.status)}<span
											class="tag stag t-{a.status}"
											title={a.status}>{statusBadge(a.status)}</span
										>{/if}</span
								>
							</div>
						</td>
						<td>
							<span class="typecell">
								<span class="typeMain">{typeMain(a)}</span>
								{#if typeSub(a)}<span class="typeSub">{typeSub(a)}</span>{/if}
							</span>
						</td>
						<td class="center">
							<span class="etypes">
								{#each parseJsonStrArr(a.types) as t (t)}
									<img class="energy" src={energyIcon(t)} alt={t} title={t} />
								{/each}
							</span>
						</td>
						<td>
							{#each parseAttacks(a.attacks) as att, i (i)}
								<span class="attackline" title={att.name}>
									{#each att.cost as c, j (j)}
										<img class="energy" src={energyIcon(c)} alt={c} title={c} />
									{/each}
								</span>
							{/each}
						</td>
						<td class="center">
							{#if a.rarity}
								{@const src = rarityIconSrc(a.rarity)}
								{#if src}
									<img
										class="rarityicon"
										class:basic={isBasicRarity(a.rarity)}
										{src}
										alt={a.rarity}
										title={a.rarity}
										onerror={(e) =>
											((e.currentTarget as HTMLImageElement).style.display = 'none')}
									/>
								{/if}
							{/if}
						</td>
						<td class="center">
							{#if a.set_symbol_url}
								<img
									class="setsym"
									src={a.set_symbol_url}
									alt="{(a.set_ptcgo_code ?? a.set_code).toUpperCase()}/{a.set_code}"
									title="{(a.set_ptcgo_code ?? a.set_code).toUpperCase()}/{a.set_code}"
								/>
							{:else}
								<span title={a.set_code}>
									{(a.set_ptcgo_code ?? a.set_code).toUpperCase()}
								</span>
							{/if}
						</td>
						<td class="num">{a.number}</td>
						<td class="num">
							{#if a.nm_unit != null}
								<span class="pricebox">{money(a.nm_unit)}</span>
							{:else}
								<span class="pricedash">—</span>
							{/if}
						</td>
						<td class="num">
							{#if a.market_unit != null}
								<span class="pricebox">{money(a.market_unit)}</span>
							{:else}
								<span class="pricedash">—</span>
							{/if}
						</td>
						<td class="num">
							{#if a.market_total != null}
								<span class="pricebox">{money(a.market_total)}</span>
							{:else}
								<span class="pricedash">—</span>
							{/if}
						</td>
					</tr>
				{/each}
				{#each unownedCatalog as c (c.card_id)}
					<tr
						class="missing"
						onclick={() => (selectedCard = { set: c.set_code, number: c.number })}
					>
						{#if selectMode}<td class="cbcol"></td>{/if}
						<td class="num qty"><span class="pricedash">—</span></td>
						<td class="colflex">
							<div class="namecell">
								{#if c.image_small}
									<img class="cardthumb" src={c.image_small} alt="" loading="lazy" />
								{/if}
								<span class="cardname">{c.name}</span>
							</div>
						</td>
						<td>{c.supertype ?? ''}</td>
						<td class="center">
							<span class="etypes">
								{#each parseJsonStrArr(c.types) as t (t)}
									<img class="energy" src={energyIcon(t)} alt={t} title={t} />
								{/each}
							</span>
						</td>
						<td>
							{#each parseAttacks(c.attacks) as att, i (i)}
								<span class="attackline" title={att.name}>
									{#each att.cost as cc, j (j)}
										<img class="energy" src={energyIcon(cc)} alt={cc} title={cc} />
									{/each}
								</span>
							{/each}
						</td>
						<td class="center">
							{#if c.rarity}
								{@const src = rarityIconSrc(c.rarity)}
								{#if src}
									<img
										class="rarityicon"
										class:basic={isBasicRarity(c.rarity)}
										{src}
										alt={c.rarity}
										title={c.rarity}
										onerror={(e) =>
											((e.currentTarget as HTMLImageElement).style.display = 'none')}
									/>
								{/if}
							{/if}
						</td>
						<td class="center">
							{#if c.set_symbol_url}
								<img
									class="setsym"
									src={c.set_symbol_url}
									alt="{(c.set_ptcgo_code ?? c.set_code).toUpperCase()}/{c.set_code}"
									title="{(c.set_ptcgo_code ?? c.set_code).toUpperCase()}/{c.set_code}"
								/>
							{:else}
								<span title={c.set_code}>
									{(c.set_ptcgo_code ?? c.set_code).toUpperCase()}
								</span>
							{/if}
						</td>
						<td class="num">{c.number}</td>
						<td class="num"><span class="pricedash">—</span></td>
						<td class="num"><span class="pricedash">—</span></td>
						<td class="num"><span class="pricedash">—</span></td>
					</tr>
				{/each}
			</tbody>
		</table>
		</div>
	{/if}
{/if}

{#if selectedCard}
	<CardModal
		setCode={selectedCard.set}
		number={selectedCard.number}
		onClose={closeCard}
		onNavigate={(s, n) => (selectedCard = { set: s, number: n })}
	/>
{/if}

<style>
	.muted {
		color: #888;
	}
	.error {
		color: #e94560;
	}

	/* --- DD-style top chrome ------------------------------------------- */

	.topbar {
		/* Sticky (not fixed) so the bar takes its own height in the page
		   flow — no manual spacer needed, and the bar can never overlap
		   the first row of results. Horizontal table scroll is owned by
		   .tableScroll, so the page itself doesn't scroll horizontally
		   and the bar stays put. */
		position: sticky;
		top: 0;
		z-index: 50;
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		padding: 0.4rem 0.7rem 0.45rem;
		background: #16213e;
		border-bottom: 1px solid #0f3460;
	}
	.topbarSpacer {
		display: none;
	}
	.row {
		display: flex;
		align-items: center;
		gap: 0.5rem;
	}
	/* Toggles + ⋯ hug the left; countline rides flush right via the
	   `.countline { margin-left: auto }` rule below. Vertical alignment
	   inherits from `.row { align-items: center }`. */
	.row2 {
		justify-content: flex-start;
	}
	.brand {
		display: inline-flex;
		align-items: center;
		text-decoration: none;
		flex-shrink: 0;
	}
	.brandmark {
		width: 26px;
		height: 26px;
		display: block;
	}
	.brand:hover .brandmark {
		filter: brightness(1.2);
	}
	/* The input inherits the row's flex slot via its wrapper, so the
	   wrapper carries the flex sizing rather than the input itself. */
	.searchwrap {
		flex: 1;
		min-width: 0;
		position: relative;
		display: flex;
		align-items: center;
	}
	.search {
		flex: 1;
		min-width: 0;
		/* Extra right padding leaves room for the clear button without
		   it overlapping typed text. */
		padding: 0.45rem 2rem 0.45rem 0.6rem;
		background: #1a1a2e;
		border: 1px solid #0f3460;
		border-radius: 6px;
		color: #e0e0e0;
		font: inherit;
	}
	.search.error {
		border-color: #e94560;
	}
	.searcherr {
		gap: 0.6rem;
		font-size: 0.82rem;
		color: #ff8a8a;
	}
	.errpos {
		color: #9aa0bd;
	}
	.helplink {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 1.5rem;
		height: 1.5rem;
		border: 1px solid #0f3460;
		border-radius: 50%;
		color: #9aa0bd;
		text-decoration: none;
		font-size: 0.85rem;
		flex-shrink: 0;
	}
	.helplink:hover {
		color: #ffd66b;
		border-color: #ffd66b;
	}
	.searchclear {
		position: absolute;
		right: 0.4rem;
		top: 50%;
		transform: translateY(-50%);
		width: 1.4rem;
		height: 1.4rem;
		padding: 0;
		background: none;
		border: none;
		color: #888;
		font-size: 1.1rem;
		line-height: 1;
		border-radius: 50%;
		cursor: pointer;
		display: inline-flex;
		align-items: center;
		justify-content: center;
	}
	.searchclear:hover {
		color: #e94560;
		background: rgba(233, 69, 96, 0.12);
	}
	.alltoggle {
		display: inline-flex;
		align-items: center;
		gap: 0.3rem;
		color: #888;
		font-size: 0.85rem;
		white-space: nowrap;
		cursor: pointer;
	}
	.alltoggle input {
		cursor: pointer;
	}
	.countline {
		margin: 0 0 0 auto;
		font-size: 0.85rem;
	}
	.tableScroll {
		overflow-x: auto;
	}
	.viewtoggle {
		display: flex;
	}
	.viewtoggle button {
		background: none;
		border: 1px solid #0f3460;
		color: #888;
		padding: 0.25rem 0.55rem;
		font-size: 1.1rem;
		line-height: 1;
		cursor: pointer;
	}
	.viewtoggle button:first-child {
		border-radius: 6px 0 0 6px;
	}
	.viewtoggle button:last-child {
		border-radius: 0 6px 6px 0;
		border-left: none;
	}
	.viewtoggle button.on {
		background: #0f3460;
		color: #e0e0e0;
	}
	.burger {
		background: none;
		border: 1px solid transparent;
		color: #888;
		font-size: 1.3rem;
		line-height: 1;
		padding: 0.25rem 0.55rem;
		cursor: pointer;
		border-radius: 6px;
	}
	.burger:hover {
		color: #e0e0e0;
		border-color: #0f3460;
	}

	.menuBackdrop {
		position: fixed;
		inset: 0;
		z-index: 49;
	}
	/* Burger wrapper anchors the dropdown to the button itself — no
	   matter how tall the topbar grows, the menu lands just below the
	   ⋯ and never overlays it. */
	.burgerWrap {
		position: relative;
		display: inline-flex;
	}
	.menu {
		position: absolute;
		top: calc(100% + 4px);
		left: 0;
		z-index: 60;
		display: flex;
		flex-direction: column;
		min-width: 180px;
		background: #16213e;
		border: 1px solid #0f3460;
		border-radius: 8px;
		padding: 0.3rem;
		box-shadow: 0 4px 14px rgba(0, 0, 0, 0.4);
	}
	.menuItem {
		background: none;
		border: none;
		color: #e0e0e0;
		text-align: left;
		padding: 0.45rem 0.7rem;
		font: inherit;
		font-size: 0.9rem;
		border-radius: 5px;
		cursor: pointer;
		text-decoration: none;
		display: block;
	}
	.menuItem:hover {
		background: #0f3460;
		color: #e94560;
	}

	/* --- Grid view ------------------------------------------------------ */

	.gridsort {
		display: flex;
		gap: 0.4rem;
		align-items: center;
		flex-wrap: wrap;
		margin: 0 0 0.5rem;
		font-size: 0.85rem;
	}
	.sortbtn {
		background: #16213e;
		border: 1px solid #0f3460;
		color: #888;
		border-radius: 6px;
		padding: 0.3rem 0.7rem;
		font: inherit;
		cursor: pointer;
		display: inline-flex;
		align-items: center;
		gap: 0.3rem;
	}
	.sortbtn:hover {
		border-color: #e94560;
		color: #e0e0e0;
	}
	.sortbtn.active {
		background: #e94560;
		border-color: #e94560;
		color: #fff;
	}
	.sortbtn .caret {
		/* Override the global .caret rule (#e94560) which collides with
		   the active sortbtn's red background. Inheriting the button's
		   color gives gray on inactive buttons and white on the active
		   one, both readable. */
		color: inherit;
		font-size: 0.65rem;
		opacity: 0.9;
	}
	.cardgrid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(130px, 1fr));
		gap: 0.4rem;
		margin-top: 0;
	}
	.cardtile {
		position: relative;
		padding: 0;
		background: none;
		border: 2px solid transparent;
		border-radius: 8px;
		cursor: pointer;
	}
	.cardtile img {
		width: 100%;
		display: block;
		aspect-ratio: 5 / 7;
		object-fit: contain;
		background: #0d1424;
		border-radius: 6px;
	}
	.cardtile.picked {
		border-color: #e94560;
	}
	/* Catalog rows the user doesn't own — mirror the .missing treatment
	   the browse view uses for empty binder slots. */
	.cardtile.missing img,
	.cardtile.missing .tilenoart {
		filter: grayscale(0.9) brightness(0.62);
	}
	.cardtile.missing {
		opacity: 0.82;
	}
	/* Foil treatment for holo / reverse-holo / pattern-RH / cosmos
	   variants. Two pseudo-elements stacked over the card image:
	   ::before paints a rainbow tint via mix-blend-mode, ::after
	   animates a diagonal sheen across the surface. Pointer-events
	   none so clicks still hit the underlying button. The .cardtile
	   already has rounded corners + overflow handled by its img;
	   give the foil overlays the same border-radius and clip them
	   to the tile. */
	.cardtile.foil {
		overflow: hidden;
	}
	.cardtile.foil::before,
	.thumbwrap.foil::before {
		content: '';
		position: absolute;
		inset: 0;
		border-radius: inherit;
		background: repeating-linear-gradient(
			135deg,
			rgba(255, 0, 0, 0.08),
			rgba(255, 165, 0, 0.08) 5%,
			rgba(255, 255, 0, 0.08) 10%,
			rgba(0, 200, 0, 0.08) 15%,
			rgba(0, 140, 255, 0.08) 20%,
			rgba(130, 0, 255, 0.08) 25%,
			rgba(255, 0, 200, 0.08) 30%,
			rgba(255, 0, 0, 0.08) 33.33%
		);
		mix-blend-mode: color;
		pointer-events: none;
		z-index: 1;
	}
	.cardtile.foil::after,
	.thumbwrap.foil::after {
		content: '';
		position: absolute;
		inset: 0;
		border-radius: inherit;
		background:
			linear-gradient(
				135deg,
				transparent 46%,
				rgba(255, 255, 255, 0.18) 49%,
				rgba(255, 255, 255, 0.18) 51%,
				transparent 54%
			)
			100% 100% / 240% 240%;
		pointer-events: none;
		z-index: 2;
		animation: foil-streak 3.2s ease-in-out infinite;
	}
	@keyframes foil-streak {
		0% {
			background-position: 100% 100%;
		}
		15%,
		100% {
			background-position: 0 0;
		}
	}
	/* Wrapper around the small table-row thumbnail so foil pseudos
	   have somewhere to attach (an <img> can't host pseudo-elements). */
	.thumbwrap {
		position: relative;
		display: inline-block;
		flex-shrink: 0;
		border-radius: 3px;
		overflow: hidden;
	}
	/* Quantity badge in the corner of an aggregated grid tile. */
	.qtybadge {
		position: absolute;
		top: 4px;
		right: 4px;
		background: rgba(15, 52, 96, 0.95);
		color: #fff;
		font-size: 0.72rem;
		font-weight: 700;
		padding: 0.1rem 0.4rem;
		border-radius: 999px;
		z-index: 3;
		pointer-events: none;
	}
	.ownbadge {
		position: absolute;
		top: 4px;
		right: 4px;
		background: #0f3460;
		color: #9fe7a0;
		font-size: 0.7rem;
		padding: 1px 5px;
		border-radius: 4px;
		font-weight: 600;
	}
	.tilenoart {
		aspect-ratio: 5 / 7;
		display: flex;
		align-items: center;
		justify-content: center;
		background: #16213e;
		border-radius: 6px;
		color: #888;
		font-size: 0.8rem;
		padding: 0.5rem;
		text-align: center;
	}
	.tick {
		position: absolute;
		top: 5px;
		right: 5px;
		width: 22px;
		height: 22px;
		border-radius: 50%;
		background: #e94560;
		color: #fff;
		font-size: 0.8rem;
		display: flex;
		align-items: center;
		justify-content: center;
	}

	/* --- Multi-select bulk bar ---------------------------------------- */

	.bulkbar {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
		margin: 0.6rem 0;
		padding: 0.6rem 0.8rem;
		background: #16213e;
		border: 1px solid #0f3460;
		border-radius: 8px;
	}
	.bulkbar .count {
		font-size: 0.85rem;
		color: #e94560;
		font-weight: 600;
	}
	.bulkbar button,
	.bulkbar select {
		background: #0f3460;
		border: none;
		border-radius: 6px;
		color: #e0e0e0;
		padding: 0.35rem 0.7rem;
		font-size: 0.8rem;
		cursor: pointer;
	}
	.bulkbar button:hover:not(:disabled),
	.bulkbar select:hover:not(:disabled) {
		background: #e94560;
	}
	.bulkbar button:disabled,
	.bulkbar select:disabled {
		opacity: 0.5;
		cursor: default;
	}

	/* --- Table view (DeckDumpster-style) ------------------------------ */

	table.dd {
		/* Span the full container so the header underline reaches the
		   right edge on wide viewports. The Name column (`.colflex`)
		   absorbs all leftover width — the longest text column is the
		   natural one to flex — while other columns stay content-sized. */
		width: 100%;
		border-collapse: collapse;
		font-size: 0.9rem;
		margin-top: 0;
	}
	/* The flex column: `width: 100%` under `table-layout: auto` makes
	   this column claim all leftover horizontal space. Today it's the
	   Name column. */
	table.dd th.colflex,
	table.dd td.colflex {
		width: 100%;
	}
	table.dd th,
	table.dd td {
		padding: 0.35rem 0.6rem;
		border-bottom: 1px solid #0f3460;
		vertical-align: middle;
	}
	table.dd th {
		text-align: left;
		border-bottom: 2px solid #0f3460;
		color: #888;
		font-size: 0.72rem;
		text-transform: uppercase;
		white-space: nowrap;
	}
	table.dd th.num,
	table.dd td.num {
		text-align: right;
		font-variant-numeric: tabular-nums;
	}
	table.dd th.center,
	table.dd td.center {
		text-align: center;
	}
	table.dd tbody tr {
		cursor: pointer;
	}
	table.dd tbody tr:hover {
		background: rgba(233, 69, 96, 0.07);
	}
	table.dd tbody tr.picked {
		background: rgba(233, 69, 96, 0.14);
	}
	/* Match the .cardtile.missing treatment so unowned catalog rows read
	   the same way in table view as in grid view. */
	table.dd tbody tr.missing {
		color: #777;
		opacity: 0.82;
	}
	table.dd tbody tr.missing img {
		filter: grayscale(0.9) brightness(0.62);
	}
	table.dd .cbcol {
		width: 1.5rem;
		text-align: center;
	}
	table.dd th.qty,
	table.dd td.qty {
		width: 1.5rem;
		padding-left: 0.35rem;
		padding-right: 0.35rem;
	}
	/* DD-style pill around per-row price values; '—' stays unstyled so empty
	   prices don't draw a box. */
	.pricebox {
		display: inline-block;
		background: #1a1a2e;
		border: 1px solid #0f3460;
		border-radius: 4px;
		padding: 1px 6px;
		font-variant-numeric: tabular-nums;
	}
	.pricedash {
		color: #555;
	}
	.sortable {
		cursor: pointer;
		user-select: none;
	}
	.sortable:hover {
		color: #e0e0e0;
	}
	.caret {
		color: #e94560;
		font-size: 0.65rem;
		margin-left: 0.15rem;
	}
	.qty {
		font-weight: 600;
		color: #e0e0e0;
	}
	.namecell {
		display: flex;
		align-items: center;
		gap: 0.55rem;
		min-width: 0;
	}
	.cardthumb {
		width: 110px;
		height: 36px;
		object-fit: cover;
		object-position: top center;
		border-radius: 3px;
		flex-shrink: 0;
		background: #0d1424;
	}
	.cardname {
		font-weight: 500;
		color: #e0e0e0;
	}
	/* Wrap name + inline tags so the tags flow with the text. Without this
	   wrapper they sit as flex siblings of .namecell, and a long-wrapping
	   name shrinks to fill the remaining width, pushing the tags to the
	   right edge of the cell instead of abutting the name. */
	.namebody {
		min-width: 0;
		line-height: 1.25;
	}
	/* Type cell: main type on top, subtypes on a smaller second line —
	   matching DeckDumpster's .type-cell / .type-sub split. */
	.typecell {
		display: inline-flex;
		flex-direction: column;
		gap: 1px;
		line-height: 1.2;
		max-width: 180px;
	}
	.typeMain {
		color: #ddd;
		font-size: 0.85rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.typeSub {
		color: #777;
		font-size: 0.72rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.attackline {
		display: flex;
		gap: 3px;
		align-items: center;
		line-height: 1;
		margin: 1px 0;
	}
	.etypes {
		display: inline-flex;
		gap: 2px;
		align-items: center;
	}
	.energy {
		width: 16px;
		height: 16px;
		display: inline-block;
		vertical-align: middle;
	}

	/* Inline tags (variant, non-owned status) — DD card-tag pattern.
	   inline-block + vertical-align lets the pill keep its shape while
	   flowing as inline content inside .namebody, so a wrapped card
	   name has its tags glued to the last line of text. */
	.tag {
		display: inline-block;
		vertical-align: middle;
		margin-left: 0.4rem;
		padding: 1px 4px;
		font-size: 0.62rem;
		font-weight: 600;
		text-transform: uppercase;
		border-radius: 3px;
		border: 1px solid;
		letter-spacing: 0.04em;
		white-space: nowrap;
	}
	.vtag {
		background: #16213e;
		color: #9ab3d8;
		border-color: #0f3460;
	}
	.stag.t-ordered {
		background: #5c3a1a;
		color: #f0c878;
		border-color: #8c5a2a;
	}
	.stag.t-listed {
		background: #1a3a5c;
		color: #78c8f0;
		border-color: #2a5a8c;
	}
	.stag.t-sold,
	.stag.t-traded,
	.stag.t-gifted {
		background: #1a5c3a;
		color: #7ee8b0;
		border-color: #2a8c5a;
	}
	.stag.t-removed,
	.stag.t-lost {
		background: #5c1a2a;
		color: #f08888;
		border-color: #8c2a3a;
	}

	/* Set cell: just the symbol; alt/title carries "PFL/me2". */
	.setsym {
		height: 22px;
		width: auto;
		max-width: 36px;
		object-fit: contain;
		vertical-align: middle;
	}

	/* Real Pokémon rarity icons live under static/rarity/, sourced from
	   pkmn.gg (see static/rarity/README.md). Sized to sit comfortably in
	   one table-row height. */
	.rarityicon {
		width: 22px;
		height: 22px;
		display: inline-block;
		vertical-align: middle;
	}
	/* Common / Uncommon / Rare are simple filled shapes — at 22px they
	   read much heavier than the detailed illustration/hyper/secret icons.
	   Halve them so the column feels visually balanced. */
	.rarityicon.basic {
		width: 11px;
		height: 11px;
	}

	/* On a phone the table is just a denser version of itself — no row
	   reflow yet (it would clash with click-to-sort headers). */
	@media (max-width: 540px) {
		table.dd {
			font-size: 0.8rem;
		}
		.cardthumb {
			width: 70px;
			height: 26px;
		}
		.typecell {
			font-size: 0.75rem;
		}
		table.dd th,
		table.dd td {
			padding: 0.3rem 0.35rem;
		}
	}
</style>
