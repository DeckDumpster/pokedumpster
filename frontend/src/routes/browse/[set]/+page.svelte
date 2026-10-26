<script lang="ts">
	import { page } from '$app/state';
	import { goto, afterNavigate, beforeNavigate } from '$app/navigation';
	import { api } from '$lib/api';
	import VariantModal from '$lib/components/VariantModal.svelte';
	import TcgExportModal from '$lib/components/TcgExportModal.svelte';
	import { breadcrumbs } from '$lib/breadcrumbs.svelte';
	import {
		Button,
		CardTile,
		EmptyState,
		Field,
		Panel,
		ProgressBar,
		SectionHeader,
		Toolbar
	} from '$lib/components/ui';
	import { variantColor, variantLabel, variantSortCmp } from '$lib/variants.svelte';
	import type { BinderPage } from '$lib/types/BinderPage';
	import type { BinderSlot } from '$lib/types/BinderSlot';
	import type { ComponentProps } from 'svelte';
	type TileRarity = ComponentProps<typeof CardTile>['rarity'];

	let binder = $state<BinderPage | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);

	// Hydrate every page/layout/filter knob from the URL so reloads + shared
	// links land in the same view. (PLAN §6.8 keys the binder cache on
	// this state.) syncUrl() below writes any subsequent changes back.
	const urlBool = (k: string, dflt: boolean): boolean => {
		if (typeof window === 'undefined') return dflt;
		const v = page.url.searchParams.get(k);
		if (v === null) return dflt;
		return v === '1' || v === 'true';
	};
	const urlNum = (k: string, dflt: number): number => {
		if (typeof window === 'undefined') return dflt;
		const v = page.url.searchParams.get(k);
		const n = v == null ? NaN : Number(v);
		return Number.isFinite(n) && n > 0 ? n : dflt;
	};
	const urlStr = (k: string, dflt: string): string => {
		if (typeof window === 'undefined') return dflt;
		return page.url.searchParams.get(k) ?? dflt;
	};

	let pageNum = $state(urlNum('page', 1));
	// Cards per row is a pure UI choice — decoupled from binder pocket
	// sizes. The backend's `layout` (cards per page) derives from cols:
	// always 3 rows per page so denser grids → bigger pages.
	let cols = $state(Math.min(10, Math.max(1, urlNum('cols', 3))));
	const layout = $derived(cols * 3);
	let includeSecret = $state(urlBool('secret', true));
	let includeSubset = $state(urlBool('subset', true));
	let includePromos = $state(urlBool('promos', false));

	// Sort, in-set search, and the ownership tab — all server-side, since
	// the binder is paginated (a client-side sort would only touch one page).
	const initialQ = urlStr('q', '');
	// Sort state lives as (key, dir) and serializes to a flat URL value
	// for backend backcompat: bare key when direction matches the key's
	// default; otherwise key + '_asc' or '_desc'. Mirrors DeckDumpster's
	// per-field button row UX.
	type SortKey = 'number' | 'name' | 'price' | 'rarity';
	const SORT_KEYS = ['number', 'name', 'price', 'rarity'] as const;
	const DEFAULT_DIR: Record<SortKey, 'asc' | 'desc'> = {
		number: 'asc',
		name: 'asc',
		price: 'desc',
		rarity: 'desc'
	};
	function parseSort(raw: string): { key: SortKey; dir: 'asc' | 'desc' } {
		const suffix = raw.endsWith('_desc') ? '_desc' : raw.endsWith('_asc') ? '_asc' : '';
		const key = (suffix ? raw.slice(0, -suffix.length) : raw) as SortKey;
		if (!SORT_KEYS.includes(key)) return { key: 'number', dir: 'asc' };
		const dir: 'asc' | 'desc' =
			suffix === '_desc' ? 'desc' : suffix === '_asc' ? 'asc' : DEFAULT_DIR[key];
		return { key, dir };
	}
	const initialSort = parseSort(urlStr('sort', 'number'));
	let sortKey = $state<SortKey>(initialSort.key);
	let sortDir = $state<'asc' | 'desc'>(initialSort.dir);
	const sort = $derived.by(() =>
		sortDir === DEFAULT_DIR[sortKey] ? sortKey : `${sortKey}_${sortDir}`
	);
	function toggleSort(key: SortKey) {
		if (sortKey === key) {
			sortDir = sortDir === 'asc' ? 'desc' : 'asc';
		} else {
			sortKey = key;
			sortDir = DEFAULT_DIR[key];
		}
		pageNum = 1;
	}
	// Two-tier state: `searchRaw` mirrors the input character-by-character,
	// `search` is the debounced value that drives the URL + API load. The
	// per-keystroke effect below schedules the debounce; the load effect
	// only sees the debounced one.
	let searchRaw = $state(initialQ);
	let search = $state(initialQ.trim());
	let searchDebounce: ReturnType<typeof setTimeout>;
	let searchInput = $state<HTMLInputElement | undefined>();
	// Stop-gap filter until the unified search lands (pokedumpster-dzf).
	// Default off — show the full set. When on, restricts the binder to
	// cards the user owns no printing of. Inverted equivalent of the
	// 'All cards' toggle on /collection.
	let missingOnly = $state(urlBool('missing', false));

	function stepCols(delta: number) {
		const next = Math.min(10, Math.max(1, cols + delta));
		if (next === cols) return;
		cols = next;
		pageNum = 1;
	}

	let selectedSlot = $state<BinderSlot | null>(null);
	// "Buy missing" → TCGplayer Mass Entry export modal.
	let showTcgExport = $state(false);

	// Default condition for new copies on this page. Sticky across modal
	// opens within the same visit (the modal binds to this), defaults back
	// to NM on full reload. Both addCopy (modal "+") and addToSlot
	// (inline pip "+") consume it, so flipping the picker in the modal
	// also tags subsequent pip clicks until the user changes it back.
	let condition = $state('Near Mint');

	// One binder-browse session per set visit groups its adds under a batch
	// (PLAN §6.7). The batch is created lazily on the first add so merely
	// looking at a set never leaves an empty batch behind.
	let sessionSet = $state<string | null>(null);
	let sessionBatchId = $state<number | null>(null);

	// Keep the breadcrumb in sync with the latest known label for this set.
	// Uses binder.set.name once the API has resolved, the URL param as a
	// placeholder before that, so the first paint after navigating here
	// never falls back to the URL-derived "Browse › Base1" — and the
	// "Base1 → Base" upgrade happens in the same frame as the data load
	// since $effect.pre runs before the DOM updates. No unmount cleanup
	// needed — the breadcrumbs store is path-keyed.
	$effect.pre(() => {
		const setCode = page.params.set;
		if (!setCode) return;
		breadcrumbs.set([
			{ label: 'Browse', href: '/browse' },
			{ label: binder?.set.name ?? setCode }
		]);
	});

	async function load() {
		const set = page.params.set;
		if (!set) return;
		if (set !== sessionSet) {
			sessionSet = set;
			sessionBatchId = null;
		}
		loading = true;
		error = null;
		selectedSlot = null;
		try {
			binder = await api.binder(set, {
				page: pageNum,
				layout,
				secret: includeSecret,
				subset: includeSubset,
				promos: includePromos,
				sort,
				q: search,
				filter: missingOnly ? 'need' : 'all'
			});
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
			// A pager action asked to re-center at the top — do it now that the
			// new page's slots are in the DOM. rAF gives layout one frame to
			// settle so the scroll lands (mirrors the popstate restore above).
			if (scrollTopPending && typeof window !== 'undefined') {
				scrollTopPending = false;
				// Instant jump (not smooth): a page advance swaps the whole grid,
				// so animating a scroll through replaced content is both odd and
				// leaves the page moving while the next interaction fires. Mirrors
				// the popstate scroll restore below.
				requestAnimationFrame(() => window.scrollTo(0, 0));
			}
		}
	}

	// SvelteKit's replaceState reads from the kit router's internal page
	// store, which isn't initialised until *after* hydration completes
	// — onMount is too early on a hard refresh and the call blows up
	// with "Cannot read properties of undefined (reading '$set')".
	// afterNavigate fires on the initial navigation once the router has
	// settled, which is the right hook for this gate. On a client-side
	// nav from /browse the router is already up, so afterNavigate fires
	// promptly and there's no observable delay.
	let routerReady = $state(false);

	// Scroll restoration on Back. The binder data loads client-side (async),
	// so on a popstate return the page is briefly 0px tall and the router's
	// native scroll restoration lands at the top before the slots render. We
	// stash window.scrollY per URL as we leave, then re-apply it once the data
	// has rendered (pokedumpster-3m3).
	const SCROLL_KEY = 'browse-scroll';
	const urlKey = (u: URL) => u.pathname + u.search;
	function readScroll(): Record<string, number> {
		try {
			return JSON.parse(sessionStorage.getItem(SCROLL_KEY) ?? '{}') as Record<string, number>;
		} catch {
			return {};
		}
	}
	let pendingScroll: number | null = null;
	// Set by a pager action; consumed in load()'s finally to re-center the
	// viewport at the top of the grid once the new page has rendered.
	let scrollTopPending = false;

	beforeNavigate((nav) => {
		if (typeof window === 'undefined' || !nav.from) return;
		const store = readScroll();
		store[urlKey(nav.from.url)] = window.scrollY;
		sessionStorage.setItem(SCROLL_KEY, JSON.stringify(store));
	});

	afterNavigate((nav) => {
		routerReady = true;
		if (typeof window === 'undefined') return;
		pendingScroll =
			nav.type === 'popstate' && nav.to ? (readScroll()[urlKey(nav.to.url)] ?? null) : null;
	});

	// Re-apply the saved scroll once the binder finishes loading (the slots are
	// in the DOM, so the page is tall enough to scroll to). One frame of slack
	// lets layout settle first.
	$effect(() => {
		if (loading || pendingScroll == null) return;
		const y = pendingScroll;
		pendingScroll = null;
		requestAnimationFrame(() => window.scrollTo(0, y));
	});

	function syncUrl() {
		if (typeof window === 'undefined' || !routerReady) return;
		const url = new URL(window.location.href);
		const set = (k: string, v: string, dflt: string) => {
			if (v === dflt) url.searchParams.delete(k);
			else url.searchParams.set(k, v);
		};
		set('page', String(pageNum), '1');
		set('cols', String(cols), '3');
		set('secret', includeSecret ? '1' : '0', '1');
		set('subset', includeSubset ? '1' : '0', '1');
		set('promos', includePromos ? '1' : '0', '0');
		set('sort', sort, 'number');
		set('q', search, '');
		set('missing', missingOnly ? '1' : '0', '0');
		// Persist via goto(replaceState:true), NOT $app/navigation's
		// replaceState. replaceState(url, …) only repaints the address bar;
		// it stashes page.url.href (SvelteKit's *current* URL, still stale)
		// as the entry's restore key, so navigating forward to a card page
		// and pressing Back restores the pre-param URL — the page/filter
		// reverts to its default. goto updates page.url properly, so the
		// correct URL is what Back restores. keepFocus so the search input
		// doesn't blur mid-type; noScroll so paging keeps its own scroll
		// handling. Same route + no url-dependent load() → no data refetch.
		if (url.href !== window.location.href) {
			void goto(url, { replaceState: true, keepFocus: true, noScroll: true });
		}
	}

	$effect(() => {
		void page.params.set;
		void pageNum;
		void cols;
		void includeSecret;
		void includeSubset;
		void includePromos;
		void sort;
		void search;
		void missingOnly;
		syncUrl();
		load();
	});

	// Shimmer only what the user actually holds in a foil treatment
	// (`variants.foil`, data — not a list of codes here).
	function ownedFoil(slot: BinderSlot): boolean {
		return slot.printings.some((p) => p.foil && p.owned_count > 0);
	}

	// Add a copy of a printing — the binder modal's "+". Optimistic: the pip
	// and the modal's count update at once; a failure reverts.
	async function addCopy(printingId: string) {
		const slot = selectedSlot;
		if (!slot) return;
		await addToSlot(slot, printingId);
	}

	/** Like addCopy but for slots not currently open in the modal — drives
	 *  the inline Reg/RH/Holo quick-toggle checkboxes in each slot footer. */
	async function addToSlot(slot: BinderSlot, printingId: string) {
		const printing = slot.printings.find((p) => p.printing_id === printingId);
		if (!printing) return;
		printing.owned_count += 1;
		try {
			if (sessionBatchId === null) {
				sessionBatchId = await api.createBatch({
					batch_type: 'binder_click',
					name: binder?.set.name ?? null
				});
			}
			await api.addCopy({
				printing_id: printingId,
				source: 'binder_click',
				batch_id: sessionBatchId,
				condition
			});
		} catch (e) {
			printing.owned_count -= 1; // revert
			error = e instanceof Error ? e.message : String(e);
		}
	}

	// Remove the most recent copy of a printing — the binder modal's "−".
	async function removeCopy(printingId: string) {
		const slot = selectedSlot;
		if (!slot) return;
		const printing = slot.printings.find((p) => p.printing_id === printingId);
		if (!printing || printing.owned_count <= 0) return;
		printing.owned_count -= 1;
		try {
			await api.removeCopyByPrinting(printingId);
		} catch (e) {
			printing.owned_count += 1; // revert
			error = e instanceof Error ? e.message : String(e);
		}
	}

	const sectionLabel: Record<string, string> = {
		base: '',
		secret: 'Secret Rares',
		subset: 'Subset',
		promo: 'Promos'
	};

	// Bundles (e.g. TTBB) are single-section containers — the
	// secret/subset/promo toggles don't apply, and per-slot prices come
	// straight from one TTBB-specific printing, so sort-by-price still
	// makes sense. Drives the rendering branches below.
	const isBundle = $derived(binder?.set.kind === 'bundle');

	function resetPage() {
		pageNum = 1;
	}

	/** Pager click / arrow key: change page and re-center the viewport at the
	 *  top of the grid so paging from the bottom row lands on the top row
	 *  rather than leaving the user scrolled to the bottom. The scroll is
	 *  deferred to load()'s finally (after the new slots render) — scrolling
	 *  immediately here is unreliable because load() swaps the grid content
	 *  mid-animation, which cancels the scroll (pokedumpster-not). */
	function gotoPage(n: number) {
		if (n === pageNum) return;
		pageNum = n;
		scrollTopPending = true;
	}

	// Reschedule the debounced `search` update whenever the raw input changes.
	// Kept in its own effect so the load+syncUrl effect below only depends on
	// the debounced value — otherwise every keystroke would refire load().
	$effect(() => {
		void searchRaw;
		clearTimeout(searchDebounce);
		searchDebounce = setTimeout(() => {
			const next = searchRaw.trim();
			if (next === search) return;
			search = next;
			pageNum = 1;
		}, 250);
	});

	function clearSearch() {
		clearTimeout(searchDebounce);
		searchRaw = '';
		if (search !== '') {
			search = '';
			pageNum = 1;
		}
		searchInput?.focus();
	}

	/** Whether the user owns at least one printing of this slot's card. */
	function ownedAny(slot: BinderSlot): boolean {
		return slot.printings.some((p) => p.owned_count > 0);
	}
</script>

<svelte:head><title>{binder ? binder.set.name : 'Binder'} — PokeDumpster</title></svelte:head>

<!--
  Arrow-key paging: ← / → moves between binder pages. Skip when the
  user is typing in an input/textarea or has the variant modal open
  (modal owns its own Escape handler; arrows would feel wrong there).
-->
<svelte:window
	onkeydown={(e) => {
		if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return;
		if (e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
		if (selectedSlot) return;
		const target = e.target as HTMLElement | null;
		const tag = target?.tagName;
		if (tag === 'INPUT' || tag === 'TEXTAREA' || target?.isContentEditable) return;
		if (!binder) return;
		if (e.key === 'ArrowLeft' && binder.page > 1) {
			e.preventDefault();
			gotoPage(binder.page - 1);
		} else if (e.key === 'ArrowRight' && binder.page < binder.total_pages) {
			e.preventDefault();
			gotoPage(binder.page + 1);
		}
	}}
/>
{#if loading && !binder}
	<p class="muted">Loading…</p>
{:else if error && !binder}
	<p class="error">Failed to load binder: {error}</p>
{:else if binder}
	<div class="binderpage">
		<header>
			<a class="statslink" href="/browse/{binder.set.set_code}/stats">Set stats →</a>
			<div class="stats">
				<!-- tone="complete" on every meter here, not the primitive's
				     default accent: green is ownership across the whole app,
				     and /browse already draws these same two figures green on
				     its set tiles. Clicking a tile used to swap them to
				     crimson for the identical number (pd-2lk2). -->
				{#if isBundle}
					<!-- Bundles have a single section: base == master. Show one
					     progress bar instead of two identical ones. -->
					<ProgressBar
						class="stat"
						tone="complete"
						label="Collected {binder.master_owned}/{binder.master_total}"
						value={binder.master_owned}
						max={binder.master_total}
					/>
				{:else}
					<ProgressBar
						class="stat"
						tone="complete"
						label="Base {binder.base_owned}/{binder.base_total}"
						value={binder.base_owned}
						max={binder.base_total}
					/>
					<ProgressBar
						class="stat"
						tone="complete"
						label="Master {binder.master_owned}/{binder.master_total}"
						value={binder.master_owned}
						max={binder.master_total}
					/>
				{/if}
			</div>
		</header>

		<!-- Search + 'Missing only' share a row. On laptop the checkbox
		     hugs the right edge; on narrow viewports it wraps below. -->
		<Toolbar class="searchrow" gap="md">
			<div class="searchwrap">
				<input
					class="search"
					type="text"
					placeholder="Search this set…"
					bind:value={searchRaw}
					bind:this={searchInput}
				/>
				{#if searchRaw}
					<Button
						variant="link"
						class="searchclear"
						aria-label="Clear search"
						title="Clear"
						onclick={clearSearch}>×</Button
					>
				{/if}
			</div>
			<!-- Stop-gap until the unified search lands (pokedumpster-dzf):
			     restricts the binder to cards the user owns no printing of.
			     Default off → show the whole set. -->
			<Field
				inline
				type="checkbox"
				label="Missing only"
				bind:checked={missingOnly}
				onchange={() => (pageNum = 1)}
			/>
		</Toolbar>

		<!-- Per-field sort buttons (DD-style). # already orders by
		     rarity (set numbering groups by rarity tier) so the
		     standalone Rarity button is redundant; Name sorting is
		     subsumed by the search input. -->
		{#snippet sortBtn(key: SortKey, label: string)}
			<Button
				variant={sortKey === key ? 'primary' : 'ghost'}
				size="sm"
				class="sortbtn"
				onclick={() => toggleSort(key)}
			>
				{label}
				{#if sortKey === key}
					<span class="caret">{sortDir === 'asc' ? '▲' : '▼'}</span>
				{/if}
			</Button>
		{/snippet}

		<Toolbar class="controls" gap="lg">
			<Toolbar gap="sm">
				{@render sortBtn('number', '#')}
				{@render sortBtn('price', 'Price')}
			</Toolbar>
			<!-- Cards per row stepper. Pure UI choice (1..10) — page size
			     derives from it (cols × 3 rows per page) so the backend's
			     pagination stays consistent at 3 visible rows. -->
			<div class="cpr">
				<span class="cpr-label">Columns</span>
				<Button
					variant="ghost"
					size="sm"
					class="cpr-btn"
					disabled={cols <= 1}
					onclick={() => stepCols(-1)}
					aria-label="Fewer cards per row">−</Button
				>
				<span class="cpr-value">{cols}</span>
				<Button
					variant="ghost"
					size="sm"
					class="cpr-btn"
					disabled={cols >= 10}
					onclick={() => stepCols(1)}
					aria-label="More cards per row">+</Button
				>
			</div>
			<!-- Section-include toggles. Secret usually has content on modern
			     sets but it's not something you flip every visit; Subset and
			     Promos are mostly empty on SV/ME-era sets — tuck all three
			     into an overflow menu so they don't take a row of real estate.
			     Hidden for bundles since they have a single section. -->
			{#if !isBundle}
				<details class="overflow">
					<summary aria-label="More filters" title="More filters">⋯</summary>
					<Panel variant="overlay" elevation="md" padding="sm" class="overflow-menu">
						<Field
							inline
							type="checkbox"
							label="Secret"
							bind:checked={includeSecret}
							onchange={resetPage}
						/>
						<Field
							inline
							type="checkbox"
							label="Subset"
							bind:checked={includeSubset}
							onchange={resetPage}
						/>
						<Field
							inline
							type="checkbox"
							label="Promos"
							bind:checked={includePromos}
							onchange={resetPage}
						/>
					</Panel>
				</details>
				<!-- One-click: collect every missing card as a TCGplayer Mass
				     Entry list. Real sets only — bundles span many home sets. -->
				<Button
					variant="ghost"
					size="sm"
					onclick={() => (showTcgExport = true)}
					title="Build a TCGplayer Mass Entry list of every card you're missing"
					>🛒 Buy missing</Button
				>
			{/if}
			<span class="spacer"></span>
			<!-- Top pager — entire row hidden on mobile (the bottom pager
			     handles paging there); arrow keys work either way. -->
			<div class="toppager">
				<Button
					variant="ghost"
					size="sm"
					disabled={binder.page <= 1}
					onclick={() => gotoPage(binder!.page - 1)}>← Prev</Button
				>
				<span class="pageno">Page {binder.page} of {binder.total_pages}</span>
				<Button
					variant="ghost"
					size="sm"
					disabled={binder.page >= binder.total_pages}
					onclick={() => gotoPage(binder!.page + 1)}>Next →</Button
				>
			</div>
		</Toolbar>

		{#if error}<p class="error">{error}</p>{/if}

		{#if binder.slots.length === 0}
			{#if search}
				<EmptyState
					title="No cards match “{search}”."
					description="The search reads card names and collector numbers inside this set only."
				>
					{#snippet action()}
						<Button variant="ghost" onclick={clearSearch}>Clear search</Button>
					{/snippet}
				</EmptyState>
			{:else if missingOnly}
				<EmptyState
					tone="success"
					title="Nothing missing here."
					description="You own a printing of every card in this view. Turn off “Missing only” to see the whole set."
				>
					{#snippet action()}
						<Button variant="ghost" onclick={() => ((missingOnly = false), resetPage())}>
							Show every card
						</Button>
					{/snippet}
				</EmptyState>
			{:else}
				<EmptyState
					title="No cards in this view."
					description="Every section this set has is switched off — turn Secret, Subset or Promos back on in the ⋯ menu."
				/>
			{/if}
		{:else}
			<div class="grid" style:grid-template-columns="repeat({cols}, minmax(0, 1fr))">
				{#each binder.slots as slot, i (slot.card_id)}
					{@const prevSection = i > 0 ? binder.slots[i - 1].section : 'base'}
					{#if slot.section !== prevSection && slot.section !== 'base'}
						<SectionHeader
							class="sectionbreak"
							tone="accent"
							divider
							title={sectionLabel[slot.section]}
						/>
					{/if}
					<CardTile
						name={slot.name}
						image={slot.image_large}
						number={slot.number}
						rarity={(slot.rarity_grp ?? 'unranked') as TileRarity}
						foil={ownedFoil(slot)}
						price={slot.display_price !== null ? `$${slot.display_price.toFixed(2)}` : null}
						missing={!ownedAny(slot)}
						pips={slot.printings
							.filter((p) => !p.deprecated)
							.slice()
							.sort((a, b) => variantSortCmp(a.variant, b.variant))
							.map((p) => ({
								id: p.printing_id,
								label: variantLabel(p.variant),
								color: variantColor(p.variant),
								owned: p.owned_count
							}))}
						onselect={() => (selectedSlot = slot)}
						onpip={(pip) => addToSlot(slot, pip.id)}
					/>
				{/each}
			</div>

			{#if binder.total_pages > 1}
				<div class="pager-bottom">
					<Button
						variant="ghost"
						size="sm"
						disabled={binder.page <= 1}
						onclick={() => gotoPage(binder!.page - 1)}>← Prev</Button
					>
					<span class="pageno">Page {binder.page} of {binder.total_pages}</span>
					<Button
						variant="ghost"
						size="sm"
						disabled={binder.page >= binder.total_pages}
						onclick={() => gotoPage(binder!.page + 1)}>Next →</Button
					>
				</div>
			{/if}
		{/if}
	</div>
{/if}

{#if selectedSlot && binder}
	<VariantModal
		slot={selectedSlot}
		setCode={binder.set.set_code}
		bind:condition
		onAdd={addCopy}
		onRemove={removeCopy}
		onClose={() => (selectedSlot = null)}
	/>
{/if}

{#if showTcgExport && binder}
	<TcgExportModal setCode={binder.set.set_code} onClose={() => (showTcgExport = false)} />
{/if}


<style>
	/*
		Only layout and geometry are left here — where a box sits, how wide it
		is, what shape it holds. Surfaces, fills, rules, text colour, radius,
		spacing and elevation all arrive through the semantic token layer or
		through a primitive that owns them.

		WHERE A PRIMITIVE IS PLACED. Svelte scopes a rule to the elements in
		this file, and a `class` handed to a component lands on markup this
		file does not own — so `.controls { margin: 1rem 0 }` would compile to
		a selector matching nothing. Placement of a primitive is therefore
		written as `:global()` nested under a scoped ancestor; `.binderpage`
		exists to be that ancestor for the page's top-level rows. Never a bare
		`:global(.controls)` — that leaks the rule to every route.

		The search field's clear-X are shapes the
		primitive set does not have yet, and are shared verbatim with
		/collection and /sealed; filed as pd-5fki rather than grown into a
		private variant here.
	*/
	header {
		display: flex;
		gap: var(--space-8);
		align-items: baseline;
		flex-wrap: wrap;
	}
	.statslink {
		color: var(--color-text);
		font-size: var(--text-md);
	}
	.statslink:hover {
		color: var(--color-text-accent);
	}
	.muted {
		color: var(--color-text-subtle);
	}
	.error {
		color: var(--color-danger-text);
	}
	.stats {
		display: flex;
		gap: var(--space-6);
	}
	/* Both meters are ProgressBar; the route only says how wide they are. */
	.stats :global(.stat) {
		width: 160px;
	}
	.binderpage :global(.controls) {
		margin: var(--space-4) var(--space-0);
		font-size: var(--text-md);
	}
	.caret {
		font-size: var(--text-xs);
		opacity: 0.9;
	}
	.overflow {
		position: relative;
	}
	.overflow > summary {
		list-style: none;
		cursor: pointer;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 28px;
		height: 28px;
		border-radius: var(--radius-md);
		background: var(--color-surface-panel);
		border: 1px solid var(--color-border);
		color: var(--color-text-muted);
		font-size: var(--text-xl);
		line-height: 1;
		user-select: none;
	}
	.overflow > summary::-webkit-details-marker {
		display: none;
	}
	.overflow > summary:hover {
		border-color: var(--color-border-accent);
		color: var(--color-text-accent);
	}
	.overflow[open] > summary {
		border-color: var(--color-border-accent);
		color: var(--color-text-accent);
	}
	/* Placement only; Panel `overlay` + elevation md is the popover surface. */
	.overflow :global(.overflow-menu) {
		position: absolute;
		top: calc(100% + var(--space-1));
		left: 0;
		z-index: 5;
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		min-width: 140px;
		white-space: nowrap;
	}
	/* Search row — search input + 'Missing only' checkbox on the same
	   line. Search takes all the slack via flex:1; the checkbox hugs
	   the right edge and wraps below on narrow viewports. */
	.binderpage :global(.searchrow) {
		margin: var(--space-4) var(--space-0) var(--space-2);
	}
	/* Search input + clear-X. flex:1 lets it absorb all available
	   width; min-width:0 stops it from refusing to shrink below its
	   intrinsic placeholder width on tight viewports. */
	.searchwrap {
		flex: 1;
		min-width: 220px;
		position: relative;
		display: flex;
		align-items: center;
	}
	/* Cards-per-row stepper. */
	.cpr {
		display: inline-flex;
		align-items: center;
		gap: var(--space-2);
		color: var(--color-text-muted);
		margin-left: auto;
	}
	.cpr-label {
		font-size: var(--text-md);
		color: var(--color-text-subtle);
	}
	.cpr :global(.cpr-btn) {
		width: 28px;
		height: 28px;
		padding: var(--space-0);
		font-size: var(--text-xl);
		line-height: 1;
	}
	.cpr-value {
		min-width: 1.2rem;
		text-align: center;
		font-variant-numeric: tabular-nums;
		font-weight: var(--weight-semibold);
		color: var(--color-text);
	}
	/* Top pager — page counter + Prev/Next arrows. Both hidden on
	   mobile (the bottom pager handles paging there). */
	.toppager {
		display: inline-flex;
		align-items: center;
		gap: var(--space-2);
	}
	@media (max-width: 540px) {
		.toppager {
			display: none;
		}
	}
	.search {
		flex: 1;
		min-width: 0;
		background: var(--color-control-surface);
		border: 1px solid var(--color-control-border);
		color: var(--color-control-text);
		border-radius: var(--radius-md);
		padding: var(--space-2) var(--space-8) var(--space-2) var(--space-2);
		font: inherit;
	}
	.search::placeholder {
		color: var(--color-control-placeholder);
	}
	.search:focus-visible {
		outline: none;
		border-color: var(--color-border-focus);
		box-shadow: var(--shadow-focus);
	}
	/* Geometry only — the Button primitive paints it. */
	.searchwrap :global(.searchclear) {
		position: absolute;
		right: var(--space-2);
		top: 50%;
		transform: translateY(-50%);
		width: 1.4rem;
		height: 1.4rem;
		font-size: var(--text-xl);
		line-height: 1;
		border-radius: var(--radius-round);
	}
	.spacer {
		flex: 1;
	}
	.pageno {
		color: var(--color-text-subtle);
	}
	.pager-bottom {
		display: flex;
		justify-content: center;
		align-items: center;
		gap: var(--space-3);
		margin: var(--space-6) var(--space-0) var(--space-2);
	}
	.grid {
		display: grid;
		gap: var(--space-3);
	}
	/* The section label is a SectionHeader; the grid only says it spans. */
	.grid :global(.sectionbreak) {
		grid-column: 1 / -1;
	}
	/* Larger tap targets on touch-sized viewports (PLAN §6.9). */
	@media (max-width: 540px) {
		.binderpage :global(.controls) {
			gap: var(--space-2) var(--space-4);
		}
		.binderpage :global(.controls button) {
			font-size: var(--text-lg);
			padding: var(--space-2) var(--space-2);
		}
		.stats {
			gap: var(--space-4);
		}
		.stats :global(.stat) {
			width: 120px;
		}
		/* Tighter binder so a full page fits — small gaps, thin borders,
		   no foot padding eating the card. */
		.grid {
			gap: var(--space-1);
		}
	}
</style>
