<script lang="ts">
	import { page } from '$app/state';
	import { api, variantLabel } from '$lib/api';
	import VariantModal from '$lib/components/VariantModal.svelte';
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
	let sort = $state(urlStr('sort', 'number'));
	let searchRaw = $state(initialQ);
	let search = $state(initialQ.trim());
	let searchDebounce: ReturnType<typeof setTimeout>;
	let tab = $state(urlStr('tab', 'all'));
	// Pill toggle, mutually exclusive with the tab — when on, the binder
	// query asks for the broader "incomplete" filter (any printing not
	// owned), regardless of the underlying tab.
	let incompleteOnly = $state(urlBool('incomplete', false));

	const tabs = [
		{ key: 'all', label: 'All' },
		{ key: 'have', label: 'Have' },
		{ key: 'need', label: 'Need' },
		{ key: 'dupes', label: 'Dupes' }
	];

	let selectedSlot = $state<BinderSlot | null>(null);

	// One binder-browse session per set visit groups its adds under a batch
	// (PLAN §6.7). The batch is created lazily on the first add so merely
	// looking at a set never leaves an empty batch behind.
	let sessionSet = $state<string | null>(null);
	let sessionBatchId = $state<number | null>(null);

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
				filter: incompleteOnly ? 'incomplete' : tab
			});
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
		set('incomplete', incompleteOnly ? '1' : '0', '0');
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
		void incompleteOnly;
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

	/** Distinct color per variant treatment, so each colored pip below
	 *  the slot is unambiguous at a glance. Ball patterns mirror the
	 *  ball's real-world color; treatments without a natural color get a
	 *  consistent fallback. */
	function variantColor(variant: string): string {
		switch (variant) {
			case 'normal':
				return '#bbbbbb';
			case 'holo':
				return '#f0c878';
			case 'reverse_holo':
				return '#a0c4f0';
			case 'pokeball_rh':
				return '#e94560';
			case 'masterball_rh':
				return '#9c5fb5';
			case 'quickball_rh':
				return '#4a8df0';
			case 'duskball_rh':
				return '#3a3a52';
			case 'loveball_rh':
				return '#f478a0';
			case 'friendball_rh':
				return '#5cb85c';
			case 'energy_symbol_rh':
				return '#ffd24a';
			case 'team_rocket_rh':
				return '#2f1b1b';
			case 'first_ed_holo':
			case 'first_ed_normal':
				return '#d4af37';
			case 'unlimited_holo':
				return '#aa7733';
			default:
				return '#b88cc0';
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
{#if loading && !binder}
	<p class="muted">Loading…</p>
{:else if error && !binder}
	<p class="error">Failed to load binder: {error}</p>
{:else if binder}
	<header>
		<h1>{binder.set.name}</h1>
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

	<div class="filterbar">
		<div class="tabs">
			{#each tabs as t (t.key)}
				<button
					class="tab"
					class:active={!incompleteOnly && tab === t.key}
					onclick={() => setTab(t.key)}
				>
					{t.label}
				</button>
			{/each}
			<button
				class="tab pill"
				class:active={incompleteOnly}
				title="Show only slots that are missing at least one variant"
				onclick={() => {
					incompleteOnly = !incompleteOnly;
					pageNum = 1;
				}}
			>
				Incomplete only
			</button>
		</div>
		<input
			class="search"
			type="text"
			placeholder="Search this set…"
			value={searchRaw}
			oninput={(e) => onSearch(e.currentTarget.value)}
		/>
	</div>

	<div class="controls">
		<label><input type="checkbox" bind:checked={includeSecret} onchange={resetPage} /> Secret</label>
		<label><input type="checkbox" bind:checked={includeSubset} onchange={resetPage} /> Subset</label>
		<label><input type="checkbox" bind:checked={includePromos} onchange={resetPage} /> Promos</label>
		<label>
			Layout
			<select bind:value={layout} onchange={resetPage}>
				<option value={4}>4-pocket</option>
				<option value={9}>9-pocket</option>
				<option value={12}>12-pocket</option>
			</select>
		</label>
		<label>
			Sort
			<select bind:value={sort} onchange={resetPage}>
				<option value="number">Number ↑</option>
				<option value="number_desc">Number ↓</option>
				<option value="price">Price ↓</option>
				<option value="name">Name A→Z</option>
				<option value="rarity">Rarity (grouped)</option>
			</select>
		</label>
		<span class="spacer"></span>
		<button disabled={binder.page <= 1} onclick={() => (pageNum = binder!.page - 1)}>← Prev</button>
		<span class="pageno">Page {binder.page} of {binder.total_pages}</span>
		<button
			disabled={binder.page >= binder.total_pages}
			onclick={() => (pageNum = binder!.page + 1)}
		>
			Next →
		</button>
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
						{#each slot.printings.filter((p) => !p.deprecated) as p (p.printing_id)}
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
	h1 {
		color: #e94560;
		margin: 0;
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
	/* Distinct from the regular tabs — pill is an additive filter, not a
	   mutually-exclusive view choice. */
	.tab.pill {
		border-radius: 999px;
		margin-left: 0.5rem;
		font-size: 0.78rem;
	}
	.search {
		flex: 1;
		min-width: 160px;
		max-width: 320px;
		background: #1a1a2e;
		border: 1px solid #0f3460;
		color: #e0e0e0;
		border-radius: 6px;
		padding: 0.4rem 0.6rem;
		font: inherit;
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
	select {
		background: #1a1a2e;
		border: 1px solid #0f3460;
		color: #e0e0e0;
		border-radius: 6px;
		padding: 0.2rem;
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
		background: #0d1424;
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
