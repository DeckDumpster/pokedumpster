<script lang="ts">
	import { page } from '$app/state';
	import { api } from '$lib/api';
	import VariantModal from '$lib/components/VariantModal.svelte';
	import { breadcrumbs } from '$lib/breadcrumbs.svelte';
	import { variantColor, variantLabel, variantRank } from '$lib/variants.svelte';
	import type { BinderPage } from '$lib/types/BinderPage';
	import type { BinderSlot } from '$lib/types/BinderSlot';

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
	let layout = $state(urlNum('layout', 9));
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
	let searchRaw = $state(initialQ);
	let search = $state(initialQ.trim());
	let searchDebounce: ReturnType<typeof setTimeout>;
	let searchInput = $state<HTMLInputElement | undefined>();
	let tab = $state(urlStr('tab', 'all'));

	const tabs = [
		{ key: 'all', label: 'All' },
		{ key: 'need', label: 'Need' }
	];

	// "Cards per row" exposes the underlying layout (4/9/12 pocket size)
	// as the user-facing axis the user actually cares about. Stepping
	// cycles through {2, 3, 4} → layout {4, 9, 12}.
	const CPR_TO_LAYOUT: Record<number, number> = { 2: 4, 3: 9, 4: 12 };
	function layoutToCpr(l: number): number {
		if (l === 4) return 2;
		if (l === 12) return 4;
		return 3;
	}
	function stepCpr(delta: number) {
		const next = Math.min(4, Math.max(2, layoutToCpr(layout) + delta));
		layout = CPR_TO_LAYOUT[next];
		pageNum = 1;
	}

	let selectedSlot = $state<BinderSlot | null>(null);

	// One binder-browse session per set visit groups its adds under a batch
	// (PLAN §6.7). The batch is created lazily on the first add so merely
	// looking at a set never leaves an empty batch behind.
	let sessionSet = $state<string | null>(null);
	let sessionBatchId = $state<number | null>(null);

	// Clear the page-supplied breadcrumb when this route unmounts so the
	// next route doesn't briefly inherit the previous set's leaf label.
	$effect(() => {
		return () => breadcrumbs.set(null);
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
				filter: tab
			});
			breadcrumbs.set([
				{ label: 'Browse', href: '/browse' },
				{ label: binder.set.name }
			]);
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	}

	function syncUrl() {
		if (typeof window === 'undefined') return;
		const url = new URL(window.location.href);
		const set = (k: string, v: string, dflt: string) => {
			if (v === dflt) url.searchParams.delete(k);
			else url.searchParams.set(k, v);
		};
		set('page', String(pageNum), '1');
		set('layout', String(layout), '9');
		set('secret', includeSecret ? '1' : '0', '1');
		set('subset', includeSubset ? '1' : '0', '1');
		set('promos', includePromos ? '1' : '0', '0');
		set('sort', sort, 'number');
		set('q', searchRaw.trim(), '');
		set('tab', tab, 'all');
		window.history.replaceState({}, '', url);
	}

	$effect(() => {
		void page.params.set;
		void pageNum;
		void layout;
		void includeSecret;
		void includeSubset;
		void includePromos;
		void sort;
		void search;
		void tab;
		syncUrl();
		load();
	});

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
				batch_id: sessionBatchId
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

	function columns(l: number): number {
		if (l === 4) return 2;
		if (l === 12) return 4;
		return 3;
	}

	// Always show the full binder-page column count — a 9-pocket page fits
	// three columns even on a phone (cards scale down, pkmn.gg-style).
	const cols = $derived(columns(layout));

	const sectionLabel: Record<string, string> = {
		base: '',
		secret: 'Secret Rares',
		subset: 'Subset',
		promo: 'Promos'
	};

	function pct(owned: number, total: number): number {
		return total > 0 ? Math.round((owned / total) * 100) : 0;
	}

	function resetPage() {
		pageNum = 1;
	}

	/** Bottom-pager click: change page and jump back to the top of the grid
	 *  so the user doesn't have to scroll up themselves. */
	function gotoPage(n: number) {
		pageNum = n;
		if (typeof window !== 'undefined') {
			window.scrollTo({ top: 0, behavior: 'smooth' });
		}
	}

	function onSearch(value: string) {
		searchRaw = value;
		clearTimeout(searchDebounce);
		searchDebounce = setTimeout(() => {
			search = value.trim();
			pageNum = 1;
		}, 250);
	}

	function setTab(key: string) {
		tab = key;
		pageNum = 1;
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
	<header>
		<a class="statslink" href="/browse/{binder.set.set_code}/stats">Set stats →</a>
		<div class="stats">
			<div class="stat">
				<span>Base {binder.base_owned}/{binder.base_total}</span>
				<div class="bar">
					<span style:width="{pct(binder.base_owned, binder.base_total)}%"></span>
				</div>
			</div>
			<div class="stat">
				<span>Master {binder.master_owned}/{binder.master_total}</span>
				<div class="bar">
					<span style:width="{pct(binder.master_owned, binder.master_total)}%"></span>
				</div>
			</div>
		</div>
	</header>

	<!-- Search gets its own row so it can stretch. -->
	<div class="searchrow">
		<div class="searchwrap">
			<input
				class="search"
				type="text"
				placeholder="Search this set…"
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
	</div>

	<div class="filterbar">
		<div class="tabs">
			{#each tabs as t (t.key)}
				<button
					class="tab"
					class:active={tab === t.key}
					onclick={() => setTab(t.key)}
				>
					{t.label}
				</button>
			{/each}
		</div>
		<!-- Cards per row stepper. Replaces the 'Layout' <select>; the
		     underlying state is still `layout` (4/9/12) so the backend
		     contract doesn't change. -->
		<div class="cpr">
			<span class="cpr-label">Cards per row</span>
			<button
				class="cpr-btn"
				disabled={layoutToCpr(layout) <= 2}
				onclick={() => stepCpr(-1)}
				aria-label="Fewer cards per row"
			>−</button>
			<span class="cpr-value">{layoutToCpr(layout)}</span>
			<button
				class="cpr-btn"
				disabled={layoutToCpr(layout) >= 4}
				onclick={() => stepCpr(1)}
				aria-label="More cards per row"
			>+</button>
		</div>
	</div>

	<div class="controls">
		<!-- Per-field sort buttons (DD-style). Click an inactive button
		     to switch sorts (default direction per field); click the
		     active button to toggle asc/desc. -->
		<div class="sortbtns">
			{#snippet sortBtn(key: SortKey, label: string)}
				<button
					class="sortbtn"
					class:active={sortKey === key}
					onclick={() => toggleSort(key)}
				>
					{label}
					{#if sortKey === key}
						<span class="caret">{sortDir === 'asc' ? '▲' : '▼'}</span>
					{/if}
				</button>
			{/snippet}
			{@render sortBtn('number', '#')}
			{@render sortBtn('name', 'Name')}
			{@render sortBtn('rarity', 'Rarity')}
			{@render sortBtn('price', 'Price')}
		</div>
		<!-- Section-include toggles. Secret usually has content on modern
		     sets but it's not something you flip every visit; Subset and
		     Promos are mostly empty on SV/ME-era sets — tuck all three
		     into an overflow menu so they don't take a row of real estate. -->
		<details class="overflow">
			<summary aria-label="More filters" title="More filters">⋯</summary>
			<div class="overflow-menu">
				<label
					><input
						type="checkbox"
						bind:checked={includeSecret}
						onchange={resetPage}
					/> Secret</label
				>
				<label
					><input
						type="checkbox"
						bind:checked={includeSubset}
						onchange={resetPage}
					/> Subset</label
				>
				<label
					><input
						type="checkbox"
						bind:checked={includePromos}
						onchange={resetPage}
					/> Promos</label
				>
			</div>
		</details>
		<span class="spacer"></span>
		<!-- Top pager — Prev/Next buttons hidden on mobile (.toppager-arrows),
		     the page counter stays visible. Bottom pager always shows both. -->
		<div class="toppager">
			<button
				class="toppager-arrows"
				disabled={binder.page <= 1}
				onclick={() => (pageNum = binder!.page - 1)}
			>← Prev</button>
			<span class="pageno">Page {binder.page} of {binder.total_pages}</span>
			<button
				class="toppager-arrows"
				disabled={binder.page >= binder.total_pages}
				onclick={() => (pageNum = binder!.page + 1)}
			>Next →</button>
		</div>
	</div>

	{#if error}<p class="error">{error}</p>{/if}

	{#if binder.slots.length === 0}
		<p class="muted">No cards in this view.</p>
	{:else}
		<div class="grid" style:grid-template-columns="repeat({cols}, 1fr)">
			{#each binder.slots as slot, i (slot.card_id)}
				{@const prevSection = i > 0 ? binder.slots[i - 1].section : 'base'}
				{#if slot.section !== prevSection && slot.section !== 'base'}
					<div class="divider">{sectionLabel[slot.section]}</div>
				{/if}
				<div class="slotwrap" class:missing={!ownedAny(slot)}>
					<button class="slot" onclick={() => (selectedSlot = slot)}>
						{#if slot.image_large}
							<img src={slot.image_large} alt={slot.name} loading="lazy" />
						{:else}
							<div class="noart">{slot.name}</div>
						{/if}
					</button>
					<!-- One colored pip per variant, sibling to the slot so the
					     pip buttons aren't nested inside the slot button (invalid
					     HTML). Empty when unowned, filled with the variant's
					     color when owned, count badge when owned > 1. -->
					<div class="vchips">
						{#each slot.printings
							.filter((p) => !p.deprecated)
							.slice()
							.sort((a, b) => variantRank(a.variant) - variantRank(b.variant)) as p (p.printing_id)}
							<button
								class="vchip"
								class:owned={p.owned_count > 0}
								style:--c={variantColor(p.variant)}
								title="{variantLabel(p.variant)}{p.owned_count > 0
									? ` ×${p.owned_count}`
									: ''} — click to add"
								aria-label="Add one {variantLabel(p.variant)}{p.owned_count > 0
									? ` (own ${p.owned_count})`
									: ''}"
								onclick={() => addToSlot(slot, p.printing_id)}
							>
								{#if p.owned_count > 1}<span class="vcount">{p.owned_count}</span>{/if}
							</button>
						{/each}
					</div>
				</div>
			{/each}
		</div>

		{#if binder.total_pages > 1}
			<div class="pager-bottom">
				<button disabled={binder.page <= 1} onclick={() => gotoPage(binder!.page - 1)}>
					← Prev
				</button>
				<span class="pageno">Page {binder.page} of {binder.total_pages}</span>
				<button
					disabled={binder.page >= binder.total_pages}
					onclick={() => gotoPage(binder!.page + 1)}
				>
					Next →
				</button>
			</div>
		{/if}
	{/if}
{/if}

{#if selectedSlot && binder}
	<VariantModal
		slot={selectedSlot}
		setCode={binder.set.set_code}
		onAdd={addCopy}
		onRemove={removeCopy}
		onClose={() => (selectedSlot = null)}
	/>
{/if}

<style>
	header {
		display: flex;
		gap: 2rem;
		align-items: baseline;
		flex-wrap: wrap;
	}
	.statslink {
		color: #e0e0e0;
		font-size: 0.85rem;
	}
	.statslink:hover {
		color: #e94560;
	}
	.muted {
		color: #888;
	}
	.error {
		color: #e94560;
	}
	.stats {
		display: flex;
		gap: 1.5rem;
	}
	.stat span {
		font-size: 0.85rem;
		color: #ccc;
	}
	.bar {
		width: 160px;
		height: 6px;
		background: #0f3460;
		border-radius: 3px;
		margin-top: 0.2rem;
		overflow: hidden;
	}
	.bar span {
		display: block;
		height: 100%;
		background: #e94560;
	}
	.controls {
		display: flex;
		gap: 1rem;
		align-items: center;
		flex-wrap: wrap;
		margin: 1rem 0;
		font-size: 0.85rem;
	}
	.sortbtns {
		display: flex;
		gap: 0.3rem;
		flex-wrap: wrap;
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
		font-size: 0.65rem;
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
		border-radius: 6px;
		background: #16213e;
		border: 1px solid #0f3460;
		color: #b8c1d9;
		font-size: 1.1rem;
		line-height: 1;
		user-select: none;
	}
	.overflow > summary::-webkit-details-marker {
		display: none;
	}
	.overflow > summary:hover {
		border-color: #e94560;
		color: #e94560;
	}
	.overflow[open] > summary {
		border-color: #e94560;
		color: #e94560;
	}
	.overflow-menu {
		position: absolute;
		top: calc(100% + 4px);
		left: 0;
		z-index: 5;
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
		padding: 0.6rem 0.8rem;
		background: #16213e;
		border: 1px solid #0f3460;
		border-radius: 6px;
		min-width: 140px;
		white-space: nowrap;
		box-shadow: 0 4px 12px rgba(0, 0, 0, 0.4);
	}
	.controls label {
		color: #ccc;
	}
	.filterbar {
		display: flex;
		gap: 1rem;
		align-items: center;
		flex-wrap: wrap;
		margin: 1rem 0 0.25rem;
	}
	.tabs {
		display: flex;
		gap: 0.25rem;
	}
	.tab {
		color: #888;
		padding: 0.35rem 0.9rem;
		font-size: 0.85rem;
	}
	.tab.active {
		background: #e94560;
		border-color: #e94560;
		color: #fff;
	}
	/* Search row — own line, full width. */
	.searchrow {
		margin: 1rem 0 0.5rem;
	}
	/* Search input + clear-X. Wrapper carries the row flex slot; the
	   input has extra right padding to leave room for the × button
	   without overlapping typed text. */
	.searchwrap {
		width: 100%;
		position: relative;
		display: flex;
		align-items: center;
	}
	/* Cards-per-row stepper. */
	.cpr {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		color: #ccc;
		margin-left: auto;
	}
	.cpr-label {
		font-size: 0.85rem;
		color: #888;
	}
	.cpr-btn {
		width: 28px;
		height: 28px;
		padding: 0;
		font-size: 1.1rem;
		line-height: 1;
		background: #16213e;
		border: 1px solid #0f3460;
		color: #e0e0e0;
		border-radius: 6px;
		cursor: pointer;
		display: inline-flex;
		align-items: center;
		justify-content: center;
	}
	.cpr-btn:hover:not(:disabled) {
		border-color: #e94560;
		color: #e94560;
	}
	.cpr-btn:disabled {
		opacity: 0.35;
		cursor: default;
	}
	.cpr-value {
		min-width: 1.2rem;
		text-align: center;
		font-variant-numeric: tabular-nums;
		font-weight: 600;
		color: #e0e0e0;
	}
	/* Top pager — Prev/Next arrows collapse on mobile, page counter stays. */
	.toppager {
		display: inline-flex;
		align-items: center;
		gap: 0.5rem;
	}
	@media (max-width: 540px) {
		.toppager-arrows {
			display: none;
		}
	}
	.search {
		flex: 1;
		min-width: 0;
		background: #1a1a2e;
		border: 1px solid #0f3460;
		color: #e0e0e0;
		border-radius: 6px;
		padding: 0.4rem 2rem 0.4rem 0.6rem;
		font: inherit;
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
	.spacer {
		flex: 1;
	}
	.pageno {
		color: #888;
	}
	.pager-bottom {
		display: flex;
		justify-content: center;
		align-items: center;
		gap: 0.75rem;
		margin: 1.5rem 0 0.5rem;
	}
	button {
		background: #16213e;
		border: 1px solid #0f3460;
		color: #e0e0e0;
		padding: 0.3rem 0.7rem;
		border-radius: 6px;
		cursor: pointer;
		font: inherit;
	}
	button:disabled {
		opacity: 0.4;
		cursor: default;
	}
	.grid {
		display: grid;
		gap: 0.75rem;
	}
	.divider {
		grid-column: 1 / -1;
		color: #e94560;
		font-size: 0.8rem;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		border-bottom: 1px solid #0f3460;
		padding-bottom: 0.2rem;
		margin-top: 0.5rem;
	}
	.slotwrap {
		display: flex;
		flex-direction: column;
		gap: 6px;
	}
	.slot {
		display: block;
		width: 100%;
		padding: 0;
		background: transparent;
		border: none;
		color: #e0e0e0;
		text-align: left;
		cursor: pointer;
	}
	/* Cards the user owns no printing of read as greyed-out. */
	.slotwrap.missing img,
	.slotwrap.missing .noart {
		filter: grayscale(0.9) brightness(0.62);
	}
	.slotwrap.missing {
		opacity: 0.82;
	}
	.slot img {
		width: 100%;
		display: block;
		aspect-ratio: 5 / 7;
		object-fit: contain;
		/* Card images have transparent rounded corners — let the body
		   color show through instead of painting black behind them. */
		background: transparent;
	}
	.noart {
		aspect-ratio: 5 / 7;
		display: flex;
		align-items: center;
		justify-content: center;
		font-size: 0.8rem;
		color: #888;
		padding: 0.5rem;
		text-align: center;
	}
	/* Color-coded variant chips centered below the card. Empty (border-
	   only) when unowned, filled with the variant's color when owned;
	   count badge appears when owned > 1. */
	.vchips {
		display: flex;
		justify-content: center;
		align-items: center;
		gap: 4px;
		flex-wrap: wrap;
	}
	.vchip {
		width: 20px;
		height: 20px;
		border-radius: 50%;
		border: 2px solid var(--c, #666);
		background: transparent;
		color: #fff;
		padding: 0;
		cursor: pointer;
		font: inherit;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		transition: transform 0.08s ease-out;
	}
	.vchip.owned {
		background: var(--c);
	}
	.vchip:hover {
		transform: scale(1.15);
	}
	.vcount {
		font-size: 0.62rem;
		font-weight: 700;
		line-height: 1;
		/* Dark text on light fills (yellow/silver/light blue); white text
		   on dark fills. Black with a thin white halo reads on both. */
		color: #0a0a1a;
		text-shadow: 0 0 2px rgba(255, 255, 255, 0.7);
	}

	/* Larger tap targets on touch-sized viewports (PLAN §6.9). */
	@media (max-width: 540px) {
		.controls {
			gap: 0.6rem 1rem;
		}
		.controls label,
		.controls button {
			font-size: 0.95rem;
			padding: 0.45rem 0.6rem;
		}
		.stats {
			gap: 1rem;
		}
		.bar {
			width: 120px;
		}
		/* Tighter binder so a full page fits — small gaps, thin borders,
		   no foot padding eating the card. */
		.grid {
			gap: 0.25rem;
		}
		.slotwrap {
			gap: 4px;
		}
	}
</style>
