<script lang="ts">
	import { onMount } from 'svelte';
	import { api, variantLabel } from '$lib/api';
	import CardModal from '$lib/components/CardModal.svelte';
	import type { CollectionRow } from '$lib/types/CollectionRow';
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
			[rows, binders, decks] = await Promise.all([
				api.collection(),
				api.binders(),
				api.decks()
			]);
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	});

	const filtered = $derived(
		rows.filter((r) => !search || r.name.toLowerCase().includes(search))
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
	.search {
		width: 100%;
		max-width: 480px;
		padding: 0.5rem;
		background: #1a1a2e;
		border: 1px solid #0f3460;
		border-radius: 6px;
		color: #e0e0e0;
		margin-bottom: 0.6rem;
	}
	.toolbar {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		flex-wrap: wrap;
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
		margin-top: 0.8rem;
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
	table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.9rem;
		margin-top: 0.8rem;
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
	tbody tr.picked {
		background: rgba(233, 69, 96, 0.12);
	}
</style>
