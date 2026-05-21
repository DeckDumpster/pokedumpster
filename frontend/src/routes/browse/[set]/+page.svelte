<script lang="ts">
	import { page } from '$app/state';
	import { api } from '$lib/api';
	import VariantModal from '$lib/components/VariantModal.svelte';
	import type { BinderPage } from '$lib/types/BinderPage';
	import type { BinderSlot } from '$lib/types/BinderSlot';

	let binder = $state<BinderPage | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);

	let pageNum = $state(1);
	let layout = $state(9);
	let includeSecret = $state(true);
	let includeSubset = $state(true);
	let includePromos = $state(false);

	// Sort, in-set search, and the ownership tab — all server-side, since
	// the binder is paginated (a client-side sort would only touch one page).
	let sort = $state('number');
	let searchRaw = $state('');
	let search = $state('');
	let searchDebounce: ReturnType<typeof setTimeout>;
	let tab = $state('all');

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
				filter: tab
			});
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
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
		load();
	});

	// Add a copy of a printing — the binder modal's "+". Optimistic: the pip
	// and the modal's count update at once; a failure reverts.
	async function addCopy(printingId: string) {
		const slot = selectedSlot;
		if (!slot) return;
		const printing = slot.printings.find((p) => p.printing_id === printingId);
		if (!printing) return;
		printing.owned_count += 1;
		try {
			if (sessionBatchId === null) {
				sessionBatchId = await api.createBatch({
					batch_type: 'binder_browse',
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
				<button class="tab" class:active={tab === t.key} onclick={() => setTab(t.key)}>
					{t.label}
				</button>
			{/each}
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
				<option value="rarity">Rarity ↓</option>
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
				<button
					class="slot"
					class:missing={!ownedAny(slot)}
					onclick={() => (selectedSlot = slot)}
				>
					{#if slot.image_large}
						<img src={slot.image_large} alt={slot.name} loading="lazy" />
					{:else}
						<div class="noart">{slot.name}</div>
					{/if}
					<div class="foot">
						<div class="pips">
							{#each slot.printings.filter((p) => !p.deprecated) as p (p.printing_id)}
								<span class="pip" class:owned={p.owned_count > 0}></span>
							{/each}
						</div>
					</div>
				</button>
			{/each}
		</div>
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
	.slot {
		display: block;
		width: 100%;
		padding: 0;
		background: #16213e;
		border: 2px solid #0f3460;
		border-radius: 8px;
		overflow: hidden;
		color: #e0e0e0;
		text-align: left;
		cursor: pointer;
	}
	/* Cards the user owns no printing of read as greyed-out. */
	.slot.missing img,
	.slot.missing .noart {
		filter: grayscale(0.9) brightness(0.62);
	}
	.slot.missing {
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
	.foot {
		display: flex;
		justify-content: center;
		align-items: center;
		padding: 0.3rem 0.5rem;
	}
	.pips {
		display: flex;
		gap: 3px;
	}
	.pip {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		border: 1px solid #0f3460;
		background: transparent;
	}
	.pip.owned {
		background: #e94560;
		border-color: #e94560;
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
		.slot {
			border-width: 1px;
		}
		.foot {
			padding: 0.2rem 0.3rem;
		}
	}
</style>
