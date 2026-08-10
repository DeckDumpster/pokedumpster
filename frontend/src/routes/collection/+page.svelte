<script lang="ts">
	import { onMount, untrack } from 'svelte';
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { api, SearchQueryError } from '$lib/api';
	import { facetHref } from '$lib/facets';
	import { variantLabel, variantTag, variants } from '$lib/variants.svelte';
	import { CONDITIONS } from '$lib/conditions';
	import { conditionMultiplier } from '$lib/conditions.svelte';
	import { money, count } from '$lib/format';
	import {
		SORT_LABELS,
		defaultDir,
		isSortKey,
		type SortDir,
		type SortKey
	} from '$lib/collectionSort';

	// Foil shimmer treatment for holo / reverse-holo / pattern-RH /
	// cosmos_holo variants — ranks 1..3 in the variants table. Stamps
	// (rank 4) and normals (rank 0) are matte.
	function isFoilVariant(code: string): boolean {
		const rank = variants.map[code]?.rank ?? 0;
		return rank >= 1 && rank <= 3;
	}
	import CardModal from '$lib/components/CardModal.svelte';
	import ValueHistoryModal from '$lib/components/ValueHistoryModal.svelte';
	import Pokeball from '$lib/components/Pokeball.svelte';
	import { Button, EmptyState, SectionHeader, Toolbar } from '$lib/components/ui';
	import type { CollectionRow } from '$lib/types/CollectionRow';
	import type { SearchRow } from '$lib/types/SearchRow';
	import type { SearchVocabulary } from '$lib/types/SearchVocabulary';
	import type { Binder } from '$lib/types/Binder';
	import type { Deck } from '$lib/types/Deck';

	// Server-side search results (one row per printing, owned or not). The
	// page's existing rendering is per-copy, so owned printings are flattened
	// back into per-copy rows (`rows`); a printing with no copies drives a
	// dimmed "missing" tile instead (see `sorted`).
	let searchRows = $state<SearchRow[]>([]);
	// Printings the query matches in total, and their condition-adjusted value.
	// The endpoint answers with one bounded page, so both describe the whole
	// result while `searchRows` holds at most PAGE_SIZE of it — the count line
	// and the pager speak from these, never from what is on screen.
	let searchTotal = $state(0);
	// Physical cards behind those printings, and what they are worth. A
	// printing you own three of is one row and three cards, so the count line
	// cannot read either number off the other.
	let searchTotalCopies = $state(0);
	let searchTotalValue = $state<number | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);
	// A query-language parse error, shown under the search box with a caret.
	let searchError = $state<{ message: string; position: number } | null>(null);

	// One rendered row per owned copy. Deliberately NOT CollectionRow: the
	// search payload carries only the fields the list draws (pd-lk8v), so
	// claiming that shape here would promise `attacks`/`artist` the server
	// no longer sends. Anchored to CollectionRow by Omit so a field added
	// there still has to be answered for here.
	type ListRow = Omit<CollectionRow, 'artist' | 'attacks' | 'variant_description'> & {
		attack_costs: string | null;
	};

	/** One ListRow per owned copy of a printing, in the server's copy order. */
	function copyRows(sr: SearchRow): ListRow[] {
		return sr.copies.map((cp) => ({
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
			supertype: sr.supertype,
			subtypes: sr.subtypes,
			types: sr.types,
			attack_costs: sr.attack_costs,
			market_price: sr.market_price,
			image_small: sr.image_small
		}));
	}

	const rows = $derived<ListRow[]>(searchRows.flatMap(copyRows));

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

	// --- Autocomplete: keyword aliases for a bare token, is:/has: values
	//     after the colon. Vocabulary comes from the data-driven registry
	//     (api.searchKeywords); nothing here is hardcoded. (pokedumpster-cx1) ---
	type AcItem = { insert: string; label: string; hint: string };
	const HAS_VALUES = ['ability', 'flavor', 'attack', 'weakness', 'resistance', 'retreat'];
	let vocab = $state<SearchVocabulary | null>(null);
	let acFocused = $state(false);
	let acDismissed = $state(false);
	let acIndex = $state(0);

	// The trailing whitespace-delimited token (what the caret is completing)
	// and the text before it.
	function tokenInfo(raw: string): { prefix: string; token: string } {
		const m = raw.match(/(\S*)$/);
		const token = m ? m[0] : '';
		return { prefix: raw.slice(0, raw.length - token.length), token };
	}

	const suggestions = $derived.by<AcItem[]>(() => {
		const v = vocab;
		if (!v) return [];
		const { token } = tokenInfo(searchRaw);
		if (!token) return [];
		const neg = token.startsWith('-') ? '-' : '';
		const bare = neg ? token.slice(1) : token;
		const colon = bare.indexOf(':');
		if (colon >= 0) {
			const kw = bare.slice(0, colon).toLowerCase();
			const partial = bare.slice(colon + 1).toLowerCase();
			let pool: { v: string; h: string }[] = [];
			if (kw === 'is') pool = v.flags.map((f) => ({ v: f.flag, h: f.help ?? '' }));
			else if (kw === 'has') pool = HAS_VALUES.map((x) => ({ v: x, h: '' }));
			else return [];
			return pool
				.filter((x) => x.v.toLowerCase().startsWith(partial))
				.slice(0, 8)
				.map((x) => ({ insert: `${neg}${kw}:${x.v}`, label: `${kw}:${x.v}`, hint: x.h }));
		}
		const lower = bare.toLowerCase();
		const items: AcItem[] = [];
		for (const k of v.keywords) {
			const alias = k.aliases.find((a) => a.toLowerCase().startsWith(lower));
			if (alias) {
				const op = k.operators.includes(':') ? ':' : (k.operators[0] ?? ':');
				items.push({ insert: `${neg}${alias}${op}`, label: `${alias}${op}`, hint: k.help ?? '' });
			}
		}
		return items.slice(0, 8);
	});

	const acOpen = $derived(acFocused && !acDismissed && suggestions.length > 0);

	function acceptSuggestion(item: AcItem) {
		const { prefix } = tokenInfo(searchRaw);
		acDismissed = false;
		acIndex = 0;
		onSearch(prefix + item.insert);
		searchInput?.focus();
	}

	function onSearchKeydown(e: KeyboardEvent) {
		if (!acOpen) return;
		const n = suggestions.length;
		if (e.key === 'ArrowDown') {
			e.preventDefault();
			acIndex = (acIndex + 1) % n;
		} else if (e.key === 'ArrowUp') {
			e.preventDefault();
			acIndex = (acIndex - 1 + n) % n;
		} else if (e.key === 'Enter') {
			e.preventDefault();
			acceptSuggestion(suggestions[Math.min(acIndex, n - 1)]);
		} else if (e.key === 'Escape') {
			e.preventDefault();
			acDismissed = true;
		}
	}
	function onSearch(value: string) {
		searchRaw = value;
		// Typing reopens the autocomplete and resets the highlight.
		acDismissed = false;
		acIndex = 0;
		clearTimeout(debounce);
		debounce = setTimeout(() => {
			query = value.trim();
			// A new query is a new result set: page 17 of the old one names
			// nothing in the new one, so committing a query lands on page 1.
			offset = 0;
			// Reflect the active query in the URL so refreshes + back-button
			// keep state. goto(replaceState:true), NOT $app/navigation's
			// replaceState: the latter only repaints the address bar and
			// stashes the *stale* page.url as the entry's restore key, so a
			// forward nav (card facet link) + Back drops the ?q=. goto
			// updates page.url so Back restores it. keepFocus so the search
			// box doesn't blur mid-type.
			if (typeof window !== 'undefined') {
				const url = new URL(window.location.href);
				if (query) url.searchParams.set('q', searchRaw.trim());
				else url.searchParams.delete('q');
				url.searchParams.delete('page');
				if (url.href !== window.location.href) {
					void goto(url, { replaceState: true, keepFocus: true, noScroll: true });
				}
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
		const all = page.url.searchParams.get('all') === '1';
		const p = pageParam(page.url.searchParams.get('page'));
		untrack(() => {
			if (q !== searchRaw) {
				clearTimeout(debounce);
				searchRaw = q;
				query = q.trim();
				// Close any open modal so the filtered list is visible.
				selectedCard = null;
			}
			// The "All cards" param has to resync from the URL too. A
			// client-side facet nav (e.g. a card-modal link carrying &all=1)
			// updates the URL without remounting, so `initialAllCards` never
			// re-runs — without this, the catalog stayed owned-only until a
			// manual reload even though the URL already said all=1.
			if (all !== allCards) {
				allCards = all;
			}
			// ?page= is the page cursor, so a reload or a pasted link lands on
			// the page it names. The pager writes the param itself before it
			// navigates, which makes this a no-op on its own moves; what it
			// catches is a URL arriving from outside.
			const fromUrl = (p - 1) * PAGE_SIZE;
			if (fromUrl !== offset) {
				offset = fromUrl;
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
		// Widening to the catalog renumbers every page — back to page 1.
		offset = 0;
		// Persist so refresh + back-button keep the choice (matches how
		// search ?q= is reflected). goto(replaceState:true) so the param
		// survives a forward nav + Back — see the onSearch note.
		if (typeof window !== 'undefined') {
			const url = new URL(window.location.href);
			if (allCards) url.searchParams.set('all', '1');
			else url.searchParams.delete('all');
			url.searchParams.delete('page');
			if (url.href !== window.location.href) {
				void goto(url, { replaceState: true, keepFocus: true, noScroll: true });
			}
		}
	}

	// --- Sort + paging =====================================================
	//
	// The list draws ONE page. `include_unowned=1` matches one row per printing
	// in the catalog — 56,635 of them — and rendering that many tiles is what
	// killed the tab (pd-tsqd); the endpoint's bound (pd-jsby) keeps the payload
	// small, and this keeps the DOM small, which are two different problems.
	//
	// It follows that the ORDER is the server's. A client that sorts the page it
	// happens to hold answers a different question than the one asked: "priciest
	// first" over page 1 of 227 ranks an arbitrary 250 rows. So `sortKey` is a
	// `sort=` value the endpoint understands (see $lib/collectionSort), the rows
	// render in the order they arrive, and changing the sort is a refetch.

	/** Rows per page. `search::MAX_LIMIT` is 1000; this is a DOM budget, not a
	    payload one — enough card art to scroll through, few enough to draw. */
	const PAGE_SIZE = 250;

	function readStoredSort(): { key: SortKey; dir: SortDir } {
		if (typeof window === 'undefined') return { key: 'name', dir: 'asc' };
		const k = localStorage.getItem('collection.sortKey');
		const d = localStorage.getItem('collection.sortDir');
		// A preference stored under the pre-paging client-side key names
		// ('nm', 'market', 'type') is not a `sort=` value; fall back rather
		// than ask the server for a sort it would silently ignore.
		return { key: isSortKey(k) ? k : 'name', dir: d === 'desc' ? 'desc' : 'asc' };
	}
	const _storedSort = readStoredSort();
	let sortKey = $state<SortKey>(_storedSort.key);
	let sortDir = $state<SortDir>(_storedSort.dir);
	$effect(() => {
		if (typeof window !== 'undefined') {
			localStorage.setItem('collection.sortKey', sortKey);
			localStorage.setItem('collection.sortDir', sortDir);
		}
	});

	// Rows skipped — the page cursor. Round-trips through ?page= so a refresh
	// or a shared link lands on the page you were looking at. replaceState,
	// like ?q= and ?all= before it: Back should leave the collection, not walk
	// back one page at a time through however many Nexts you clicked.
	const initialPage =
		typeof window !== 'undefined' ? pageParam(page.url.searchParams.get('page')) : 1;
	let offset = $state((initialPage - 1) * PAGE_SIZE);

	/** `?page=` as a 1-based page number; anything else is page 1. */
	function pageParam(raw: string | null): number {
		const n = Number(raw);
		return Number.isInteger(n) && n >= 1 ? n : 1;
	}

	const pageCount = $derived(Math.max(1, Math.ceil(searchTotal / PAGE_SIZE)));
	const pageNumber = $derived(Math.floor(offset / PAGE_SIZE) + 1);
	// 1-based inclusive range of the rows on screen, for the pager's readout.
	const rangeFrom = $derived(searchRows.length === 0 ? 0 : offset + 1);
	const rangeTo = $derived(offset + searchRows.length);

	/** Jump to a page, clamped, and reflect it in the URL. */
	function goToPage(n: number) {
		const next = Math.min(Math.max(n, 1), pageCount);
		const nextOffset = (next - 1) * PAGE_SIZE;
		if (nextOffset === offset) return;
		offset = nextOffset;
		writePageParam();
		// A new page starts at the top; keeping the scroll position would leave
		// you halfway down a grid you have not seen.
		if (typeof window !== 'undefined') window.scrollTo({ top: 0 });
	}

	function writePageParam() {
		if (typeof window === 'undefined') return;
		const url = new URL(window.location.href);
		if (offset === 0) url.searchParams.delete('page');
		else url.searchParams.set('page', String(pageNumber));
		if (url.href !== window.location.href) {
			void goto(url, { replaceState: true, keepFocus: true, noScroll: true });
		}
	}

	/** Anything that changes WHICH rows match sends you back to page 1 — page
	    17 of the old result is not page 17 of the new one. */
	function resetPaging() {
		if (offset === 0) return;
		offset = 0;
		writePageParam();
	}

	function sortBy(key: SortKey) {
		if (sortKey === key) sortDir = sortDir === 'asc' ? 'desc' : 'asc';
		else {
			sortKey = key;
			sortDir = defaultDir(key);
		}
		resetPaging();
	}

	/** Run the server-side search for the current query, toggle, sort and page. */
	async function runSearch() {
		loading = true;
		error = null;
		searchError = null;
		try {
			const res = await api.collectionSearch(
				query,
				sortKey,
				sortDir,
				allCards,
				PAGE_SIZE,
				offset
			);
			searchRows = res.rows;
			searchTotal = res.total;
			searchTotalCopies = res.total_copies;
			searchTotalValue = res.total_value;
			// Copies deleted under us can shorten the result past the cursor.
			// Land on the last page that still has rows rather than show an
			// empty grid over a non-zero total.
			if (res.rows.length === 0 && res.total > 0 && offset >= res.total) {
				goToPage(Math.ceil(res.total / PAGE_SIZE));
			}
		} catch (e) {
			if (e instanceof SearchQueryError) {
				searchError = { message: e.message, position: e.position };
				searchRows = [];
				searchTotal = 0;
				searchTotalCopies = 0;
				searchTotalValue = null;
			} else {
				error = e instanceof Error ? e.message : String(e);
			}
		} finally {
			loading = false;
		}
	}

	// Re-search whenever the committed query, the All-cards toggle, the sort or
	// the page changes (also fires once on mount).
	$effect(() => {
		void query;
		void allCards;
		void sortKey;
		void sortDir;
		void offset;
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
	// Collection value-over-time chart modal (pokedumpster-e1vo).
	let valueOpen = $state(false);
	function closeMenu() {
		menuOpen = false;
	}

	// Set by the modal's onMutate when a copy is added/removed/edited while
	// the modal is open. Closing only re-runs the search when this is set —
	// merely viewing a card (then closing) leaves the list untouched.
	let cardDirty = $state(false);

	/** Close the modal, re-running the search only if the modal mutated copies. */
	async function closeCard() {
		selectedCard = null;
		if (cardDirty) {
			cardDirty = false;
			await runSearch();
		}
	}

	onMount(async () => {
		// The $effect above runs the initial search; here we only load the
		// binder/deck lists used by the bulk-assign menus.
		try {
			[binders, decks, vocab] = await Promise.all([
				api.binders(),
				api.decks(),
				api.searchKeywords()
			]);
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
	});

	// The server applies the query now; `filtered` is just the owned copies it
	// returned (kept as a name so the rest of the page is unchanged).
	const filtered = $derived(rows);
	// Header total is the sum of *condition-adjusted* market values across the
	// whole result — `SearchPage.total_value`, not a sum of what is on screen.
	// Adding up one page would report page 1's worth under the whole query's
	// name; the per-row Value cells still apply the same multiplier, so this is
	// their sum across every page.
	const totalValue = $derived(searchTotalValue ?? 0);


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
	function openCard(row: ListRow) {
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
		set_name: string;
		set_ptcgo_code: string | null;
		set_symbol_url: string | null;
		number: string;
		name: string;
		rarity: string | null;
		supertype: string | null;
		subtypes: string | null;
		types: string | null;
		/** JSON `[{name, cost}]` — the Cost column's pips and their tooltip.
		    Attack text/damage live on the card modal's own fetch (pd-lk8v). */
		attack_costs: string | null;
		variant: string;
		condition: string;
		status: string;
		image_small: string | null;
	};

	function aggregate(input: ListRow[]): AggRow[] {
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
					set_name: r.set_name,
					set_ptcgo_code: r.set_ptcgo_code,
					set_symbol_url: r.set_symbol_url,
					number: r.number,
					name: r.name,
					rarity: r.rarity,
					supertype: r.supertype,
					subtypes: r.subtypes,
					types: r.types,
					attack_costs: r.attack_costs,
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
	// Reads `attack_costs`, the server's `[{name, cost}]` projection: name is
	// the line's tooltip, cost is the pips, and nothing else was ever drawn
	// here (pd-lk8v).
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

	// Unowned printings become qty-0 AggRows so one list covers owned +
	// unowned together (pokedumpster-ffq). They carry the catalog market price
	// (no condition to adjust → nm_unit === market_unit) so price sorts are
	// meaningful; market_total stays null (you own none → Value shows "—").
	function unownedRow(sr: SearchRow): AggRow {
		return {
			key: `missing|${sr.printing_id}`,
			ids: [],
			qty: 0,
			paid_total: null,
			nm_unit: sr.market_price,
			market_unit: sr.market_price,
			market_total: null,
			printing_id: sr.printing_id,
			card_id: sr.card_id,
			set_code: sr.set_code,
			set_name: sr.set_name,
			set_ptcgo_code: sr.set_ptcgo_code,
			set_symbol_url: sr.set_symbol_url,
			number: sr.number,
			name: sr.name,
			rarity: sr.rarity,
			supertype: sr.supertype,
			subtypes: sr.subtypes,
			types: sr.types,
			attack_costs: sr.attack_costs,
			variant: sr.variant,
			condition: '',
			status: 'unowned',
			image_small: sr.image_small
		};
	}

	// The rendered list, in the server's order.
	//
	// Walking `searchRows` rather than sorting a bag of AggRows is what makes
	// the page boundaries mean anything: the rows arrive already ordered by
	// `sort=`/`dir=` across the WHOLE result, and re-sorting the 250 in hand
	// would answer a different question — plus the client's idea of, say,
	// rarity order (a hardcoded rank table) is not the catalog's, so page 2
	// would start above where page 1 ended. Owned and unowned printings
	// interleave because the server interleaved them.
	//
	// One printing can still expand to several rows here: `aggregate` groups
	// its copies by (condition, status), and a printing held Near Mint and
	// Lightly Played is two rows with two different Adj. prices. The server
	// sorts printings, so it has nothing to say about which of those two comes
	// first — `groupCmp` below is the client completing an order the per-
	// printing sort cannot express, not re-sorting the page.
	const sorted = $derived.by(() => {
		const out: AggRow[] = [];
		for (const sr of searchRows) {
			if (sr.copies.length === 0) out.push(unownedRow(sr));
			else out.push(...aggregate(copyRows(sr)).sort(groupCmp));
		}
		return out;
	});

	/** Order two copy-groups OF THE SAME PRINTING by the active sort key.
	 *
	 *  Only the three condition-dependent keys separate them at all — the
	 *  groups share a printing, so name/set/rarity/price/… are identical and
	 *  the comparator ties, leaving them in the server's copy order. */
	function groupCmp(a: AggRow, b: AggRow): number {
		function keyed(r: AggRow): number | null {
			if (sortKey === 'adjusted') return r.market_unit;
			if (sortKey === 'value') return r.market_total;
			if (sortKey === 'qty') return r.qty;
			return null;
		}
		const va = keyed(a);
		const vb = keyed(b);
		if (va === null || vb === null) return 0;
		const cmp = va < vb ? -1 : va > vb ? 1 : 0;
		return sortDir === 'asc' ? cmp : -cmp;
	}


	// === Grid grouping ================================================
	//
	// Bigger art costs scroll, and scroll is only worth paying for if what
	// you scroll through is organised. So the grid cuts itself into labelled
	// sections along whatever the active sort already orders on, whenever
	// that field is a category a person browses by — set, rarity, class,
	// energy type, or a name's initial. The continuous fields (#, NM, Adj.)
	// group into nothing useful, so they stay one flat run.
	//
	// Every label is read straight off the row's own fields. No new
	// taxonomy, no bucket table: a heading here is a value the catalog
	// already stores, which is why this needs no schema of its own.
	function groupLabel(a: AggRow): string | null {
		switch (sortKey) {
			case 'name': {
				const c = a.name.trim().charAt(0).toUpperCase();
				return c >= 'A' && c <= 'Z' ? c : '#';
			}
			case 'set':
				return a.set_name;
			case 'rarity':
				return a.rarity ?? 'No rarity';
			case 'supertype':
				return a.supertype ?? 'Other';
			case 'etype':
				return parseJsonStrArr(a.types)[0] ?? 'No type';
			default:
				return null;
		}
	}

	/** `sorted`, cut into runs of equal group label. Concatenating the runs
	    reproduces `sorted` exactly — the grid's tile order is untouched, so
	    owned and unowned printings still interleave by the sort key. */
	const gridSections = $derived.by(() => {
		const out: { label: string | null; rows: AggRow[] }[] = [];
		for (const a of sorted) {
			const label = groupLabel(a);
			const last = out[out.length - 1];
			if (last && last.label === label) last.rows.push(a);
			else out.push({ label, rows: [a] });
		}
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

	// Table view: only the Name cell opens the modal; every other column is a
	// DSL-appropriate facet search (rarity:, set:, type:, …) reusing the same
	// builder as the card-page links (pokedumpster-ozm). In select mode the
	// whole row still toggles selection, so these handlers no-op and let the
	// click bubble to the row's selection handler.
	function openCardCell(e: MouseEvent, a: AggRow) {
		if (selectMode) return;
		e.stopPropagation();
		selectedCard = { set: a.set_code, number: a.number };
	}
	function facetCell(e: MouseEvent, field: string, value: string | null | undefined) {
		if (selectMode) return;
		e.stopPropagation();
		if (value) void goto(facetHref(field, value));
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

	// Set condition on every selected copy in one batch, then refresh once —
	// far quicker than opening each card modal in turn (pokedumpster-4s8). The
	// updates are independent, so fire them together.
	async function bulkSetCondition(condition: string) {
		if (!condition) return;
		const ids = [...selected];
		if (!ids.length) return;
		busy = true;
		error = null;
		try {
			await Promise.all(ids.map((id) => api.updateCopy(id, { condition })));
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

  One row, not two. The controls either side of the search are all small
  and all rare — a view toggle, a menu, a count — and stacking them on
  their own line doubled the height of the only chrome that is always on
  screen. They ride the search's line and wrap under it on a narrow
  viewport, which is what Toolbar's `wrap` already does.

  `surface="panel"` rather than the translucent sticky fill: the search
  input's boundary has to clear 3:1 (WCAG 1.4.11) against whatever is
  behind it, and it manages 2.61:1 on the saturated blue. The panel is
  the quieter ground anyway — it sits a hair off the page colour instead
  of banding it.
-->
<Toolbar
	class="topbar"
	direction="column"
	align="stretch"
	gap="sm"
	wrap={false}
	sticky
	surface="panel"
>
	<Toolbar gap="md">
		<a class="brand" href="/" aria-label="Home" title="Home">
			<span class="brandmark"><Pokeball size={26} /></span>
		</a>
		<div class="searchwrap">
			<input
				class="search"
				class:error={searchError !== null}
				data-testid="search-input"
				type="text"
				role="combobox"
				aria-expanded={acOpen}
				aria-controls="search-ac"
				autocomplete="off"
				placeholder={allCards ? 'Search all cards… (t:fire hp>=200)' : 'Search… (t:fire hp>=200)'}
				value={searchRaw}
				oninput={(e) => onSearch(e.currentTarget.value)}
				onkeydown={onSearchKeydown}
				onfocus={() => (acFocused = true)}
				onblur={() => setTimeout(() => (acFocused = false), 120)}
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
			{#if acOpen}
				<ul class="acmenu" id="search-ac" data-testid="search-autocomplete" role="listbox">
					{#each suggestions as s, i (s.label)}
						<li>
							<button
								type="button"
								class="acitem"
								class:active={i === acIndex}
								role="option"
								aria-selected={i === acIndex}
								onmousedown={(e) => {
									e.preventDefault();
									acceptSuggestion(s);
								}}
							>
								<span class="ackey">{s.label}</span>
								{#if s.hint}<span class="achint">{s.hint}</span>{/if}
							</button>
						</li>
					{/each}
				</ul>
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
		{#if searchRows.length > 0}
			<span class="bardiv" aria-hidden="true"></span>
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
						<a class="menuItem" href="/api/export/csv" download onclick={closeMenu}>Export CSV (ManaBox)</a>
						<a class="menuItem" href="/api/export/collectr/singles.csv" download onclick={closeMenu}>Export cards (Collectr)</a>
						<a class="menuItem" href="/api/export/collectr/sealed.csv" download onclick={closeMenu}>Export sealed (Collectr)</a>
						<a class="menuItem" href="/api/export/json" download onclick={closeMenu}>Export JSON (full backup)</a>
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
		<!-- Cards and money, both across the WHOLE result rather than the page
		     in front of you — `total_copies` and `total_value` off SearchPage.
		     Summing the page would report page 1's collection as the whole
		     one. The pager below is what says which slice is on screen. -->
		<button
			type="button"
			class="countline muted"
			onclick={() => (valueOpen = true)}
			title="Collection value over time"
		>
			{count(searchTotalCopies)}
			cards{#if totalValue > 0}, {money(totalValue)}{/if}
		</button>
	</Toolbar>
	{#if searchError}
		<div class="searcherr" data-testid="search-error" role="alert">
			<span class="errmsg">{searchError.message}</span>
			<span class="errpos">position {searchError.position}</span>
		</div>
	{/if}
</Toolbar>

{#if menuOpen}
	<div
		class="menuBackdrop"
		role="presentation"
		onclick={closeMenu}
	></div>
{/if}

<!-- The layout runs this route flush to the viewport edge so the bar above
     can pin edge to edge; everything below it gets its gutter back here.
     Card art needs a margin as much as it needs a gap. -->
<div class="content">
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
				disabled={busy}
				onchange={(e) => {
					bulkSetCondition(e.currentTarget.value);
					e.currentTarget.selectedIndex = 0;
				}}
			>
				<option value="">Set condition…</option>
				{#each CONDITIONS as c (c)}<option value={c}>{c}</option>{/each}
			</select>
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
		{#if searchError}
			<EmptyState
				title="That query didn't parse."
				description="Fix the query above to see results — the message under the box points at the character that stopped it."
			/>
		{:else if query}
			<EmptyState
				title="No cards match “{query}”."
				description={allCards
					? 'Nothing in the whole catalog matches. Check the spelling, or narrow one keyword at a time.'
					: 'Nothing you own matches. Turn on “All cards” to search the catalog instead of your collection.'}
			>
				{#snippet action()}
					<Button variant="ghost" href="/search-help">Search syntax</Button>
				{/snippet}
			</EmptyState>
		{:else if allCards}
			<EmptyState
				title="No cards in the catalog."
				description="The shared catalog has no cards in it, so there is nothing to search."
			/>
		{:else}
			<EmptyState
				title="Your collection is empty."
				description="Add cards from a set's binder view — click a slot and that printing is registered as a copy you own. Or turn on “All cards” to browse the catalog first."
			>
				{#snippet action()}
					<Button href="/browse">Browse sets</Button>
				{/snippet}
			</EmptyState>
		{/if}
	{:else if view === 'grid'}
		<!-- Grid lacks the table's sortable column headers, so it gets a
		     row of per-field buttons. Click an inactive button to switch
		     to that field (with a sensible default direction); click the
		     active one to toggle asc/desc. State is shared with the
		     table view (sortKey/sortDir) so flipping the view never
		     loses your sort.

		     The buttons share a min-width so the row reads as one control
		     rather than eight ragged pills, and the active one is a wash
		     with an accent edge — a solid crimson slab beside 4,763 pieces
		     of card art was the loudest thing on the page. -->
		<div class="gridsort" role="group" aria-label="Sort">
			<span class="gridsortlabel">Sort</span>
			{#snippet sortBtn(key: SortKey)}
				<button
					class="sortbtn"
					class:active={sortKey === key}
					aria-pressed={sortKey === key}
					onclick={() => sortBy(key)}
				>
					{SORT_LABELS[key]}
					{#if sortKey === key}
						<span class="caret">{sortDir === 'asc' ? '▲' : '▼'}</span>
					{/if}
				</button>
			{/snippet}
			{@render sortBtn('name')}
			{@render sortBtn('supertype')}
			{@render sortBtn('etype')}
			{@render sortBtn('rarity')}
			{@render sortBtn('set')}
			{@render sortBtn('number')}
			{@render sortBtn('price')}
			{@render sortBtn('adjusted')}
		</div>
		{@render pager()}
		<!-- Sections are runs of `sorted`, so tile order is exactly the sort
		     order — owned and unowned printings still interleave. -->
		<div class="cardgrid">
			{#each gridSections as sec, si (si)}
				{#if sec.label !== null}
					<div class="groupheader">
						<SectionHeader
							size="sm"
							title={sec.label}
							meta="{count(sec.rows.length)} {sec.rows.length === 1 ? 'card' : 'cards'}"
							divider
						/>
					</div>
				{/if}
				{#each sec.rows as a (a.key)}
					{#if a.ids.length === 0}
						<button
							class="cardtile missing"
							title="{a.name} · {(a.set_ptcgo_code ?? a.set_code).toUpperCase()} #{a.number} · click to add"
							onclick={() => (selectedCard = { set: a.set_code, number: a.number })}
						>
							{#if a.image_small}
								<img src={a.image_small} alt={a.name} loading="lazy" />
							{:else}
								<div class="tilenoart">{a.name}</div>
							{/if}
						</button>
					{:else}
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
					{/if}
				{/each}
			{/each}
		</div>
	{:else}
		{#snippet sortable(key: SortKey, extra: string, title?: string)}
			<th class="sortable {extra}" {title} onclick={() => sortBy(key)}>
				{SORT_LABELS[key]}
				{#if sortKey === key}
					<span class="caret">{sortDir === 'asc' ? '▲' : '▼'}</span>
				{/if}
			</th>
		{/snippet}
		{@render pager()}
		<div class="tableScroll">
		<table class="dd">
			<thead>
				<tr>
					{#if selectMode}
						<th class="cbcol">
							<input type="checkbox" checked={tableAllSelected} onchange={toggleTableAll} />
						</th>
					{/if}
					{@render sortable('qty', 'num qty')}
					{@render sortable('name', 'colflex')}
					{@render sortable('supertype', '')}
					{@render sortable('etype', 'center')}
					<th>Cost</th>
					{@render sortable('rarity', 'center')}
					{@render sortable('set', 'center')}
					{@render sortable('number', 'num')}
					{@render sortable('price', 'num', 'Near Mint market price (per copy)')}
					{@render sortable('adjusted', 'num', 'Condition-adjusted price (per copy)')}
					{@render sortable('value', 'num', 'Condition-adjusted value (× qty)')}
				</tr>
			</thead>
			<tbody>
				{#each sorted as a (a.key)}
					{#if a.ids.length === 0}
						<tr class="missing" data-printing={a.printing_id}>
							{#if selectMode}<td class="cbcol"></td>{/if}
							<td class="num qty"><span class="pricedash">—</span></td>
							<td class="colflex namecol" title="Open card" onclick={(e) => openCardCell(e, a)}>
								<div class="namecell">
									{#if a.image_small}
										<img class="cardthumb" src={a.image_small} alt="" loading="lazy" />
									{/if}
									<span class="cardname">{a.name}</span>
								</div>
							</td>
							<td class="fac" title="Find all {a.supertype ?? ''} cards" onclick={(e) => facetCell(e, 'supertype', a.supertype)}>{a.supertype ?? ''}</td>
							<td class="center fac" title="Find cards of this type" onclick={(e) => facetCell(e, 'type', parseJsonStrArr(a.types)[0])}>
								<span class="etypes">
									{#each parseJsonStrArr(a.types) as t (t)}
										<img class="energy" src={energyIcon(t)} alt={t} title={t} />
									{/each}
								</span>
							</td>
							<td>
								{#each parseAttacks(a.attack_costs) as att, i (i)}
									<span class="attackline" title={att.name}>
										{#each att.cost as cc, j (j)}
											<img class="energy" src={energyIcon(cc)} alt={cc} title={cc} />
										{/each}
									</span>
								{/each}
							</td>
							<td class="center fac" title="Find all {a.rarity ?? ''} cards" onclick={(e) => facetCell(e, 'rarity', a.rarity)}>
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
							<td class="center fac" title="Find all cards in this set" onclick={(e) => facetCell(e, 'set', a.set_code)}>
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
							<td class="num"><span class="pricedash">—</span></td>
						</tr>
					{:else}
					<tr
						data-printing={a.printing_id}
						class:picked={selectMode && groupChecked(a.ids)}
						onclick={() => { if (selectMode) toggleGroup(a.ids); }}
					>
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
						<td class="colflex namecol" title="Open card" onclick={(e) => openCardCell(e, a)}>
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
						<td class="fac" title="Find all {a.supertype ?? ''} cards" onclick={(e) => facetCell(e, 'supertype', a.supertype)}>
							<span class="typecell">
								<span class="typeMain">{typeMain(a)}</span>
								{#if typeSub(a)}<span class="typeSub">{typeSub(a)}</span>{/if}
							</span>
						</td>
						<td class="center fac" title="Find cards of this type" onclick={(e) => facetCell(e, 'type', parseJsonStrArr(a.types)[0])}>
							<span class="etypes">
								{#each parseJsonStrArr(a.types) as t (t)}
									<img class="energy" src={energyIcon(t)} alt={t} title={t} />
								{/each}
							</span>
						</td>
						<td>
							{#each parseAttacks(a.attack_costs) as att, i (i)}
								<span class="attackline" title={att.name}>
									{#each att.cost as c, j (j)}
										<img class="energy" src={energyIcon(c)} alt={c} title={c} />
									{/each}
								</span>
							{/each}
						</td>
						<td class="center fac" title="Find all {a.rarity ?? ''} cards" onclick={(e) => facetCell(e, 'rarity', a.rarity)}>
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
						<td class="center fac" title="Find all cards in this set" onclick={(e) => facetCell(e, 'set', a.set_code)}>
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
					{/if}
				{/each}
			</tbody>
		</table>
		</div>
	{/if}
	{#if searchRows.length > 0}
		{@render pager()}
	{/if}
{/if}
</div>

<!-- The pager. It is the reason the grid is survivable: `offset` moves, the
     page is REPLACED, and the DOM holds one page of tiles whatever `total`
     says. An append-as-you-scroll control would put the 56,635 back one
     screenful at a time, which is the bug (pd-tsqd), just slower.

     Same shape and wording as the binder pager on /browse/[set] — ← Prev,
     "Page n of m", Next → — plus first/last jumps, which that one does not
     need at three pages and this one does at 227. The range readout is here
     for the same reason: "Page 3 of 227" alone never says how much there is. -->
{#snippet pager()}
	{#if pageCount > 1}
		<nav class="pager" data-testid="search-pager" aria-label="Result pages">
			<Button
				variant="ghost"
				size="sm"
				disabled={pageNumber <= 1}
				onclick={() => goToPage(1)}
				aria-label="First page">«</Button
			>
			<Button
				variant="ghost"
				size="sm"
				disabled={pageNumber <= 1}
				onclick={() => goToPage(pageNumber - 1)}
				data-testid="pager-prev">← Prev</Button
			>
			<span class="pageno" data-testid="pager-range" aria-live="polite">
				Page {count(pageNumber)} of {count(pageCount)}
				<span class="pagerrange">· {count(rangeFrom)}–{count(rangeTo)} of {count(searchTotal)}</span>
			</span>
			<Button
				variant="ghost"
				size="sm"
				disabled={pageNumber >= pageCount}
				onclick={() => goToPage(pageNumber + 1)}
				data-testid="pager-next">Next →</Button
			>
			<Button
				variant="ghost"
				size="sm"
				disabled={pageNumber >= pageCount}
				onclick={() => goToPage(pageCount)}
				aria-label="Last page">»</Button
			>
		</nav>
	{/if}
{/snippet}

{#if selectedCard}
	<CardModal
		setCode={selectedCard.set}
		number={selectedCard.number}
		onClose={closeCard}
		onMutate={() => (cardDirty = true)}
	/>
{/if}

{#if valueOpen}
	<ValueHistoryModal onClose={() => (valueOpen = false)} />
{/if}

<style>
	.muted {
		color: var(--color-text-subtle);
	}
	.error {
		color: var(--color-danger-text);
	}

	/* --- DD-style top chrome ------------------------------------------- */

	/* The band itself is Toolbar `sticky surface="panel"` — position, fill,
	   rule and padding all belong to the primitive. What is left here is
	   what sits inside it. */

	/* Everything below the pinned bar. `main.flush` strips the layout's
	   padding for this route so the bar can pin edge to edge; the gutter
	   comes back here, where the content is. */
	.content {
		padding: var(--space-5) var(--space-6) var(--space-10);
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
	   wrapper carries the flex sizing rather than the input itself. The
	   basis is what makes the single row wrap gracefully: below ~44rem of
	   free space the search claims the first line on its own and the small
	   controls drop under it. The cap stops it from stretching into a
	   1920-wide trough that no query ever fills. */
	.searchwrap {
		flex: 1 1 22rem;
		max-width: 44rem;
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
		padding: var(--space-2) var(--space-8) var(--space-2) var(--space-3);
		background: var(--color-control-surface);
		border: 1px solid var(--color-control-border);
		border-radius: var(--radius-md);
		color: var(--color-control-text);
		font: inherit;
	}
	/* An invalid control reddens on the danger ramp, not the brand one —
	   the same answer the Field primitive gives for `error`. Crimson here
	   was the "delete reads as brand" muddle the two ramps exist to end. */
	.search.error {
		border-color: var(--color-danger);
	}
	.searcherr {
		display: flex;
		align-items: baseline;
		gap: var(--space-3);
		font-size: var(--text-sm);
		color: var(--color-danger-text);
	}
	.errpos {
		color: var(--color-text-subtle);
	}
	.helplink {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 1.5rem;
		height: 1.5rem;
		border: 1px solid var(--color-border);
		border-radius: var(--radius-round);
		color: var(--color-text-subtle);
		text-decoration: none;
		font-size: var(--text-md);
		flex-shrink: 0;
	}
	.helplink:hover {
		color: var(--color-warning-text);
		border-color: var(--color-warning);
	}
	.acmenu {
		position: absolute;
		top: calc(100% + 2px);
		left: 0;
		right: 0;
		z-index: 60;
		margin: var(--space-0);
		padding: var(--space-1);
		list-style: none;
		background: var(--color-surface-overlay);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-md);
		box-shadow: var(--shadow-lg);
		max-height: 16rem;
		overflow-y: auto;
	}
	.acitem {
		display: flex;
		align-items: baseline;
		gap: 0.6rem;
		width: 100%;
		text-align: left;
		background: none;
		border: none;
		border-radius: var(--radius-sm);
		padding: 0.35rem 0.5rem;
		color: var(--color-text);
		font: inherit;
		cursor: pointer;
	}
	.acitem.active,
	.acitem:hover {
		background: var(--color-surface-hover);
	}
	.ackey {
		font-family: var(--font-mono);
		color: var(--color-warning-text);
		white-space: nowrap;
	}
	.achint {
		color: var(--color-text-subtle);
		font-size: 0.82rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.searchclear {
		position: absolute;
		right: 0.4rem;
		top: 50%;
		transform: translateY(-50%);
		width: 1.4rem;
		height: 1.4rem;
		padding: var(--space-0);
		background: none;
		border: none;
		color: var(--color-text-subtle);
		font-size: var(--text-xl);
		line-height: 1;
		border-radius: var(--radius-round);
		cursor: pointer;
		display: inline-flex;
		align-items: center;
		justify-content: center;
	}
	.searchclear:hover {
		color: var(--color-text-accent);
		background: var(--color-surface-selected);
	}
	.alltoggle {
		display: inline-flex;
		align-items: center;
		gap: 0.3rem;
		color: var(--color-text-subtle);
		font-size: var(--text-md);
		white-space: nowrap;
		cursor: pointer;
	}
	.alltoggle input {
		cursor: pointer;
	}
	/* A hairline between the search cluster and the view/menu controls, so
	   one row still reads as two groups. Decorative, hence aria-hidden. */
	.bardiv {
		width: 1px;
		align-self: stretch;
		margin: var(--space-1) var(--space-1);
		background: var(--color-border);
		flex-shrink: 0;
	}
	.countline {
		margin: var(--space-0) var(--space-0) var(--space-0) auto;
		font-size: var(--text-md);
		background: none;
		border: none;
		padding: var(--space-0);
		font-family: inherit;
		cursor: pointer;
	}
	.countline:hover {
		color: var(--color-text);
		text-decoration: underline;
	}
	.tableScroll {
		overflow-x: auto;
	}
	.viewtoggle {
		display: flex;
	}
	.viewtoggle button {
		background: none;
		border: 1px solid var(--color-border);
		color: var(--color-text-subtle);
		padding: 0.25rem 0.55rem;
		font-size: var(--text-xl);
		line-height: 1;
		cursor: pointer;
	}
	.viewtoggle button:first-child {
		border-radius: var(--radius-md) 0 0 var(--radius-md);
	}
	.viewtoggle button:last-child {
		border-radius: 0 var(--radius-md) var(--radius-md) 0;
		border-left: none;
	}
	.viewtoggle button.on {
		background: var(--color-info-surface);
		color: var(--color-text);
	}
	.burger {
		background: none;
		border: 1px solid transparent;
		color: var(--color-text-subtle);
		font-size: 1.3rem;
		line-height: 1;
		padding: 0.25rem 0.55rem;
		cursor: pointer;
		border-radius: var(--radius-md);
	}
	.burger:hover {
		color: var(--color-text);
		border-color: var(--color-border);
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
		/* Anchored to the ⋯'s right edge, not its left: the button now rides
		   the right-hand end of a single-row bar, and a left-anchored menu
		   would hang off the viewport on a narrow one. */
		right: 0;
		z-index: 60;
		display: flex;
		flex-direction: column;
		/* Wide enough for "Export JSON (full backup)" on one line — the
		   labels are the width, not a round number. */
		min-width: 15rem;
		background: var(--color-surface-overlay);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-lg);
		padding: 0.3rem;
		box-shadow: var(--shadow-md);
	}
	.menuItem {
		background: none;
		border: none;
		color: var(--color-text);
		text-align: left;
		padding: var(--space-2) var(--space-3);
		font: inherit;
		font-size: var(--text-lg);
		white-space: nowrap;
		border-radius: var(--radius-sm);
		cursor: pointer;
		text-decoration: none;
		display: block;
	}
	.menuItem:hover {
		background: var(--color-surface-hover);
		color: var(--color-text-accent);
	}

	/* --- Grid view ------------------------------------------------------ */

	.gridsort {
		display: flex;
		gap: var(--space-2);
		align-items: center;
		flex-wrap: wrap;
		margin: var(--space-0) var(--space-0) var(--space-5);
		font-size: var(--text-md);
	}
	/* Above the list and below it, because a pager you have to scroll back up
	   to reach is a pager you use once. */
	.pager {
		display: flex;
		gap: var(--space-2);
		align-items: center;
		justify-content: center;
		flex-wrap: wrap;
		margin: var(--space-5) var(--space-0);
		font-size: var(--text-md);
	}
	.pageno {
		color: var(--color-text-subtle);
		white-space: nowrap;
		padding: var(--space-0) var(--space-2);
	}
	.pagerrange {
		color: var(--color-text-muted);
	}
	.gridsortlabel {
		font-size: var(--text-xs);
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--color-text-subtle);
		margin-right: var(--space-1);
	}
	.sortbtn {
		/* One width for every pill. The labels run from "#" to "Rarity",
		   and letting each shrink to its text turned a row of peers into a
		   ragged line whose widths encoded nothing. */
		min-width: 5.5rem;
		justify-content: center;
		background: none;
		border: 1px solid var(--color-border-subtle);
		color: var(--color-text-subtle);
		border-radius: var(--radius-pill);
		padding: var(--space-1) var(--space-3);
		font: inherit;
		font-size: var(--text-md);
		cursor: pointer;
		display: inline-flex;
		align-items: center;
		gap: var(--space-1);
		transition:
			background-color var(--dur-fast) var(--ease-standard),
			border-color var(--dur-fast) var(--ease-standard),
			color var(--dur-fast) var(--ease-standard);
	}
	.sortbtn:hover {
		border-color: var(--color-border-accent);
		color: var(--color-text);
	}
	.sortbtn:focus-visible {
		outline: none;
		box-shadow: var(--shadow-focus);
	}
	/* The active pill is a wash and an edge, not a slab. Solid crimson next
	   to card art read as the loudest element on a page whose whole job is
	   to show the art; the accent still marks the pill, at a weight that
	   loses to a Charizard. */
	.sortbtn.active {
		background: var(--color-surface-selected);
		border-color: var(--color-border-accent);
		color: var(--color-text-accent);
	}
	.sortbtn .caret {
		/* Inherit rather than take the global .caret accent: on the active
		   pill the label is already accent-coloured, and a second tone
		   inside one control is noise. */
		color: inherit;
		font-size: 0.65rem;
		opacity: 0.9;
	}
	/* Fewer cards per row, bigger art, real gutters. The tile floor steps up
	   with the viewport rather than staying at a phone-sized 130px on a
	   1920 monitor, where it produced fourteen columns of thumbnails. */
	.cardgrid {
		--tile-min: 150px;
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(var(--tile-min), 1fr));
		gap: var(--space-4);
		margin-top: var(--space-0);
	}
	@media (min-width: 1200px) {
		.cardgrid {
			--tile-min: 176px;
		}
	}
	@media (min-width: 1700px) {
		.cardgrid {
			--tile-min: 204px;
		}
	}
	/* A section label spans the grid. SectionHeader owns its own type and
	   rule; this only places it. */
	.groupheader {
		grid-column: 1 / -1;
	}
	.cardtile {
		position: relative;
		padding: var(--space-0);
		background: none;
		border: 2px solid transparent;
		border-radius: var(--radius-lg);
		cursor: pointer;
	}
	.cardtile img {
		width: 100%;
		display: block;
		aspect-ratio: 5 / 7;
		object-fit: contain;
		background: var(--color-surface-well);
		border-radius: var(--radius-md);
	}
	.cardtile.picked {
		border-color: var(--color-border-accent);
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
	   to the tile.

	   Both treatments are theme composites (--gradient-foil-*): a foil is
	   a card-game idiom, not a colour, so it belongs to the theme whole
	   rather than being reassembled from seven stops per component. */
	.cardtile.foil {
		overflow: hidden;
	}
	.cardtile.foil::before,
	.thumbwrap.foil::before {
		content: '';
		position: absolute;
		inset: 0;
		border-radius: inherit;
		background: var(--gradient-foil-spectrum);
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
		background: var(--gradient-foil-streak) 100% 100% / 240% 240%;
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
		border-radius: var(--radius-xs);
		overflow: hidden;
	}
	/* Quantity badge in the corner of an aggregated grid tile. */
	.qtybadge {
		position: absolute;
		top: 4px;
		right: 4px;
		background: var(--color-surface-sticky);
		color: var(--color-text);
		font-size: var(--text-xs);
		font-weight: var(--weight-bold);
		padding: 0.1rem 0.4rem;
		border-radius: var(--radius-pill);
		z-index: 3;
		pointer-events: none;
	}
	.tilenoart {
		aspect-ratio: 5 / 7;
		display: flex;
		align-items: center;
		justify-content: center;
		background: var(--color-surface-panel);
		border-radius: var(--radius-md);
		color: var(--color-text-subtle);
		font-size: var(--text-sm);
		padding: var(--space-2);
		text-align: center;
	}
	.tick {
		position: absolute;
		top: 5px;
		right: 5px;
		width: 22px;
		height: 22px;
		border-radius: var(--radius-round);
		background: var(--color-accent);
		color: var(--color-on-accent);
		font-size: var(--text-sm);
		display: flex;
		align-items: center;
		justify-content: center;
	}

	/* --- Multi-select bulk bar ---------------------------------------- */

	.bulkbar {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		flex-wrap: wrap;
		margin: var(--space-0) var(--space-0) var(--space-4);
		padding: var(--space-3) var(--space-4);
		background: var(--color-surface-panel);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-lg);
	}
	.bulkbar .count {
		font-size: var(--text-md);
		color: var(--color-text-accent);
		font-weight: var(--weight-semibold);
	}
	.bulkbar button,
	.bulkbar select {
		background: var(--color-info-surface);
		border: none;
		border-radius: var(--radius-md);
		color: var(--color-text);
		padding: 0.35rem 0.7rem;
		font-size: var(--text-sm);
		cursor: pointer;
	}
	.bulkbar button:hover:not(:disabled),
	.bulkbar select:hover:not(:disabled) {
		background: var(--color-accent);
		/* Dark ink on the crimson fill, as Button's `primary` does. */
		color: var(--color-on-accent);
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
		margin-top: var(--space-0);
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
		border-bottom: 1px solid var(--color-border);
		vertical-align: middle;
	}
	table.dd th {
		text-align: left;
		border-bottom: 2px solid var(--color-border);
		color: var(--color-text-subtle);
		font-size: var(--text-xs);
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
		background: var(--color-surface-accent-wash);
	}
	table.dd tbody tr.picked {
		background: var(--color-surface-selected);
	}
	/* Only the Name cell opens the card; the other columns are facet
	   searches (pokedumpster-ozm). Highlight whichever cell the pointer is
	   over so the per-column click target reads clearly. */
	table.dd tbody td.namecol:hover,
	table.dd tbody td.fac:hover {
		background: var(--color-surface-selected);
		color: var(--color-text);
	}
	/* Match the .cardtile.missing treatment so unowned catalog rows read
	   the same way in table view as in grid view. The dimming is carried
	   by the opacity + the grayscale filter, so the text itself can stay
	   on the AA-clearing subtle step. */
	table.dd tbody tr.missing {
		color: var(--color-text-subtle);
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
		background: var(--color-surface-page);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-sm);
		padding: 1px 6px;
		font-variant-numeric: tabular-nums;
	}
	.pricedash {
		color: var(--color-text-disabled);
	}
	.sortable {
		cursor: pointer;
		user-select: none;
	}
	.sortable:hover {
		color: var(--color-text);
	}
	.caret {
		color: var(--color-text-accent);
		font-size: 0.65rem;
		margin-left: 0.15rem;
	}
	.qty {
		font-weight: var(--weight-semibold);
		color: var(--color-text);
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
		border-radius: var(--radius-xs);
		flex-shrink: 0;
		background: var(--color-surface-well);
	}
	.cardname {
		font-weight: var(--weight-medium);
		color: var(--color-text);
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
		color: var(--color-text-muted);
		font-size: var(--text-md);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.typeSub {
		color: var(--color-text-subtle);
		font-size: var(--text-xs);
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
	   name has its tags glued to the last line of text.

	   Deliberately not the Badge primitive: these sit inline inside a
	   table cell tuned for density, and Badge's pill padding would reflow
	   the Name column. The tones below use the same state roles Badge's
	   `soft` variant resolves, so the two stay in step. */
	.tag {
		display: inline-block;
		vertical-align: middle;
		margin-left: 0.4rem;
		padding: 1px 4px;
		font-size: 0.62rem;
		font-weight: var(--weight-semibold);
		text-transform: uppercase;
		border-radius: var(--radius-xs);
		border: 1px solid;
		letter-spacing: 0.04em;
		white-space: nowrap;
	}
	.vtag {
		background: var(--color-surface-panel);
		color: var(--color-text-subtle);
		border-color: var(--color-border);
	}
	.stag.t-ordered {
		background: var(--color-warning-surface);
		color: var(--color-warning-text);
		border-color: var(--color-warning-border);
	}
	.stag.t-listed {
		background: var(--color-info-surface);
		color: var(--color-info-text);
		border-color: var(--color-info-border);
	}
	.stag.t-sold,
	.stag.t-traded,
	.stag.t-gifted {
		background: var(--color-success-surface);
		color: var(--color-success-text);
		border-color: var(--color-success-border);
	}
	.stag.t-removed,
	.stag.t-lost {
		background: var(--color-danger-surface);
		color: var(--color-danger-text);
		border-color: var(--color-danger-border);
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
		.content {
			padding: var(--space-3) var(--space-3) var(--space-8);
		}
		table.dd {
			font-size: var(--text-sm);
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
