<script lang="ts">
	import { onMount } from 'svelte';
	import { api, variantLabel } from '$lib/api';
	import CardModal from '$lib/components/CardModal.svelte';
	import type { CollectionRow } from '$lib/types/CollectionRow';
	import type { CollectionView } from '$lib/types/CollectionView';
	import type { Binder } from '$lib/types/Binder';
	import type { Deck } from '$lib/types/Deck';

	let rows = $state<CollectionRow[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	// Debounced search.
	let searchRaw = $state('');
	let search = $state('');
	let debounce: ReturnType<typeof setTimeout>;
	function onSearch(value: string) {
		searchRaw = value;
		clearTimeout(debounce);
		debounce = setTimeout(() => (search = value.trim().toLowerCase()), 200);
	}

	// Facet selections.
	let selRarity = $state(new Set<string>());
	let selSet = $state(new Set<string>());
	let selVariant = $state(new Set<string>());

	function toggled(set: Set<string>, value: string): Set<string> {
		const next = new Set(set);
		if (next.has(value)) next.delete(value);
		else next.add(value);
		return next;
	}

	// --- Saved views: a named filter config (search + facet selections). ---
	type ViewFilters = { search: string; rarity: string[]; set: string[]; variant: string[] };
	let savedViews = $state<CollectionView[]>([]);
	let activeViewId = $state<number | null>(null);

	function currentFilters(): ViewFilters {
		return {
			search: searchRaw,
			rarity: [...selRarity],
			set: [...selSet],
			variant: [...selVariant]
		};
	}

	function applyView(id: number | null) {
		activeViewId = id;
		if (id == null) return;
		const view = savedViews.find((v) => v.id === id);
		if (!view) return;
		const f = JSON.parse(view.filters_json) as Partial<ViewFilters>;
		onSearch(f.search ?? '');
		selRarity = new Set(f.rarity ?? []);
		selSet = new Set(f.set ?? []);
		selVariant = new Set(f.variant ?? []);
	}

	async function saveView() {
		const name = prompt('Name this view:')?.trim();
		if (!name) return;
		try {
			const id = await api.createView({
				name,
				description: null,
				filters_json: JSON.stringify(currentFilters())
			});
			savedViews = await api.views();
			activeViewId = id;
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
	}

	async function deleteView() {
		if (activeViewId == null) return;
		const view = savedViews.find((v) => v.id === activeViewId);
		if (!view || !confirm(`Delete view "${view.name}"?`)) return;
		try {
			await api.deleteView(view.id);
			savedViews = await api.views();
			activeViewId = null;
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
	}

	// --- Multi-select bulk operations. ---
	let binders = $state<Binder[]>([]);
	let decks = $state<Deck[]>([]);
	let selectMode = $state(false);
	let selected = $state(new Set<number>());
	let busy = $state(false);

	// Grid (card images) vs. table view, and the card-detail modal.
	let view = $state<'grid' | 'table'>('grid');
	let selectedCard = $state<{ set: string; number: string } | null>(null);

	/** Open a row's card in the detail modal — unless we're multi-selecting. */
	function openCard(row: CollectionRow) {
		if (selectMode) {
			toggleRow(row.id);
		} else {
			selectedCard = { set: row.set_code, number: row.number };
		}
	}

	/** Close the modal and re-fetch — the modal may have mutated copies. */
	async function closeCard() {
		selectedCard = null;
		try {
			rows = await api.collection();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
	}

	onMount(async () => {
		try {
			[rows, savedViews, binders, decks] = await Promise.all([
				api.collection(),
				api.views(),
				api.binders(),
				api.decks()
			]);
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	});

	function facetValues(pick: (r: CollectionRow) => string | null | undefined): string[] {
		return [...new Set(rows.map(pick).filter((v): v is string => !!v))].sort();
	}
	const rarities = $derived(facetValues((r) => r.rarity));
	const sets = $derived(facetValues((r) => r.set_code));
	const variants = $derived(facetValues((r) => r.variant));

	const filtered = $derived(
		rows.filter((r) => {
			if (search && !r.name.toLowerCase().includes(search)) return false;
			if (selRarity.size && !(r.rarity && selRarity.has(r.rarity))) return false;
			if (selSet.size && !selSet.has(r.set_code)) return false;
			if (selVariant.size && !selVariant.has(r.variant)) return false;
			return true;
		})
	);

	function price(p: number | null): string {
		return p == null ? '—' : `$${p.toFixed(2)}`;
	}

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

	// The header checkbox selects/clears every currently-filtered row.
	const allSelected = $derived(
		filtered.length > 0 && filtered.every((r) => selected.has(r.id))
	);
	function toggleAll() {
		selected = allSelected ? new Set() : new Set(filtered.map((r) => r.id));
	}

	/** Re-fetch the collection after a bulk mutation, then drop the selection. */
	async function refresh() {
		rows = await api.collection();
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

<h1>Collection</h1>

{#if loading}
	<p class="muted">Loading…</p>
{:else if error}
	<p class="error">Failed to load collection: {error}</p>
{:else}
	<div class="layout">
		<aside class="sidebar">
			<section class="views">
				<h3>Saved views</h3>
				{#if savedViews.length}
					<select
						class="viewpick"
						value={activeViewId ?? ''}
						onchange={(e) =>
							applyView(e.currentTarget.value ? Number(e.currentTarget.value) : null)}
					>
						<option value="">— none —</option>
						{#each savedViews as v (v.id)}
							<option value={v.id}>{v.name}</option>
						{/each}
					</select>
				{/if}
				<div class="viewbtns">
					<button onclick={saveView}>Save current…</button>
					{#if activeViewId != null}
						<button onclick={deleteView}>Delete</button>
					{/if}
				</div>
			</section>
			{#snippet facet(title: string, values: string[], selected: Set<string>, set: (s: Set<string>) => void, label: (v: string) => string)}
				{#if values.length}
					<section>
						<h3>{title}</h3>
						{#each values as value (value)}
							<label class="check">
								<input
									type="checkbox"
									checked={selected.has(value)}
									onchange={() => set(toggled(selected, value))}
								/>
								{label(value)}
							</label>
						{/each}
					</section>
				{/if}
			{/snippet}
			{@render facet('Rarity', rarities, selRarity, (s) => (selRarity = s), (v) => v)}
			{@render facet('Set', sets, selSet, (s) => (selSet = s), (v) => v)}
			{@render facet('Variant', variants, selVariant, (s) => (selVariant = s), variantLabel)}
		</aside>

		<main class="content">
			<input
				class="search"
				type="text"
				placeholder="Search cards…"
				value={searchRaw}
				oninput={(e) => onSearch(e.currentTarget.value)}
			/>
			<div class="toolbar">
				<p class="muted">{filtered.length} of {rows.length} cards</p>
				<span class="spacer"></span>
				{#if rows.length > 0}
					<div class="viewtoggle">
						<button class:on={view === 'grid'} onclick={() => (view = 'grid')}>Grid</button>
						<button class:on={view === 'table'} onclick={() => (view = 'table')}>Table</button>
					</div>
					<a class="ghost" href="/api/export/csv" download>Export CSV</a>
					<button class="ghost" onclick={toggleSelectMode}>
						{selectMode ? 'Cancel' : 'Select'}
					</button>
				{/if}
			</div>

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

			{#if rows.length === 0}
				<p class="muted">Your collection is empty. Add cards from a set's binder view.</p>
			{:else if view === 'grid'}
				<div class="cardgrid">
					{#each filtered as row (row.id)}
						<button
							class="cardtile"
							class:picked={selectMode && selected.has(row.id)}
							title="{row.name} · {variantLabel(row.variant)}"
							onclick={() => openCard(row)}
						>
							{#if row.image_small}
								<img src={row.image_small} alt={row.name} loading="lazy" />
							{:else}
								<div class="tilenoart">{row.name}</div>
							{/if}
							{#if selectMode && selected.has(row.id)}<span class="tick">✓</span>{/if}
						</button>
					{/each}
				</div>
			{:else}
				<table>
					<thead>
						<tr>
							{#if selectMode}
								<th class="cbcol">
									<input type="checkbox" checked={allSelected} onchange={toggleAll} />
								</th>
							{/if}
							<th class="thumbcol"></th>
							<th>Name</th>
							<th>Set</th>
							<th>#</th>
							<th>Variant</th>
							<th>Rarity</th>
							<th>Condition</th>
							<th>Status</th>
							<th>Paid</th>
						</tr>
					</thead>
					<tbody>
						{#each filtered as row (row.id)}
							<tr class:picked={selectMode && selected.has(row.id)}>
								{#if selectMode}
									<td class="cbcol">
										<input
											type="checkbox"
											checked={selected.has(row.id)}
											onchange={() => toggleRow(row.id)}
										/>
									</td>
								{/if}
								<td class="thumbcol">
									<button class="thumb" onclick={() => openCard(row)} aria-label={row.name}>
										{#if row.image_small}
											<img src={row.image_small} alt={row.name} loading="lazy" />
										{:else}
											<span class="thumbnoart">?</span>
										{/if}
									</button>
								</td>
								<td>
									<button class="linkish" onclick={() => openCard(row)}>{row.name}</button>
								</td>
								<td>{row.set_code}</td>
								<td>{row.number}</td>
								<td>{variantLabel(row.variant)}</td>
								<td>{row.rarity ?? '—'}</td>
								<td>{row.condition}</td>
								<td>{row.status}</td>
								<td>{price(row.purchase_price)}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			{/if}
		</main>
	</div>
{/if}

{#if selectedCard}
	<CardModal setCode={selectedCard.set} number={selectedCard.number} onClose={closeCard} />
{/if}

<style>
	h1 {
		color: #e94560;
	}
	.muted {
		color: #888;
	}
	.error {
		color: #e94560;
	}
	.layout {
		display: flex;
		gap: 1.5rem;
		align-items: flex-start;
	}
	.sidebar {
		flex: 0 0 200px;
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}
	.sidebar section h3 {
		margin: 0 0 0.4rem;
		font-size: 0.8rem;
		text-transform: uppercase;
		color: #888;
	}
	.search,
	.viewpick {
		width: 100%;
		padding: 0.5rem;
		background: #1a1a2e;
		border: 1px solid #0f3460;
		border-radius: 6px;
		color: #e0e0e0;
	}
	.viewpick {
		margin-bottom: 0.5rem;
	}
	.content > .search {
		margin-bottom: 0.6rem;
	}
	.viewbtns {
		display: flex;
		gap: 0.4rem;
		flex-wrap: wrap;
	}
	.viewbtns button {
		flex: 1;
		padding: 0.35rem;
		font-size: 0.8rem;
		background: #0f3460;
		border: none;
		border-radius: 6px;
		color: #e0e0e0;
		cursor: pointer;
	}
	.viewbtns button:hover {
		background: #e94560;
	}
	.check {
		display: block;
		font-size: 0.85rem;
		padding: 0.1rem 0;
	}
	.content {
		flex: 1;
		min-width: 0;
	}
	.toolbar {
		display: flex;
		align-items: center;
		gap: 0.75rem;
	}
	.toolbar .muted {
		margin: 0;
	}
	.spacer {
		flex: 1;
	}
	.ghost {
		background: none;
		border: 1px solid #0f3460;
		color: #e0e0e0;
		border-radius: 6px;
		padding: 0.3rem 0.8rem;
		font-size: 0.85rem;
		cursor: pointer;
		text-decoration: none;
		display: inline-block;
	}
	.ghost:hover {
		border-color: #e94560;
		color: #e94560;
	}
	.viewtoggle {
		display: flex;
	}
	.viewtoggle button {
		background: none;
		border: 1px solid #0f3460;
		color: #888;
		padding: 0.3rem 0.7rem;
		font-size: 0.85rem;
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
	.cardgrid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(130px, 1fr));
		gap: 0.8rem;
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
	.linkish {
		background: none;
		border: none;
		color: #e0e0e0;
		cursor: pointer;
		font: inherit;
		padding: 0;
		text-align: left;
	}
	.linkish:hover {
		color: #e94560;
	}
	.thumbcol {
		width: 46px;
	}
	.thumb {
		background: none;
		border: none;
		padding: 0;
		cursor: pointer;
		display: block;
	}
	.thumb img {
		width: 40px;
		aspect-ratio: 5 / 7;
		object-fit: contain;
		background: #0d1424;
		border-radius: 3px;
		display: block;
	}
	.thumbnoart {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 40px;
		aspect-ratio: 5 / 7;
		background: #16213e;
		border-radius: 3px;
		color: #888;
	}

	/* Stack the facet sidebar above the content on narrow screens. */
	@media (max-width: 640px) {
		.layout {
			flex-direction: column;
		}
		.sidebar {
			flex: 0 0 auto;
			width: 100%;
		}
	}
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
	.cbcol {
		width: 1.5rem;
		text-align: center;
	}
	tbody tr.picked {
		background: rgba(233, 69, 96, 0.12);
	}
	table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.9rem;
	}
	th {
		text-align: left;
		padding: 0.4rem 0.6rem;
		border-bottom: 2px solid #0f3460;
		color: #888;
		font-size: 0.75rem;
		text-transform: uppercase;
	}
	td {
		padding: 0.4rem 0.6rem;
		border-bottom: 1px solid #0f3460;
	}
	tbody tr:hover {
		background: rgba(233, 69, 96, 0.06);
	}
	a {
		color: #e0e0e0;
	}
	a:hover {
		color: #e94560;
	}
</style>
