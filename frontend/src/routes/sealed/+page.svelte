<script lang="ts">
	import { onMount } from 'svelte';
	import { money, count } from '$lib/format';
	import { api } from '$lib/api';
	import Pokeball from '$lib/components/Pokeball.svelte';
	import SealedModal from '$lib/components/SealedModal.svelte';
	import type { SealedEntry } from '$lib/types/SealedEntry';
	import type { SealedProduct } from '$lib/types/SealedProduct';

	// Statuses you still physically hold — the default view, and the set the
	// collection total sums over. Everything else (sold/traded/gifted/opened)
	// is disposed inventory, shown only when "Show opened / disposed" is on.
	const ACTIVE = new Set(['owned', 'listed']);

	let entries = $state<SealedEntry[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let showAll = $state(false);

	async function load() {
		try {
			entries = await api.sealedCollection();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	}
	onMount(load);

	// --- Client-side text filter. Sealed has no card-DSL, so this is a plain
	//     substring match over the product name, its set, and its category. ---
	let searchRaw = $state('');
	const query = $derived(searchRaw.trim().toLowerCase());
	function matches(e: SealedEntry): boolean {
		if (!query) return true;
		return (
			e.name.toLowerCase().includes(query) ||
			(e.set_code ?? '').toLowerCase().includes(query) ||
			e.category.toLowerCase().includes(query)
		);
	}

	const shown = $derived(
		entries.filter((e) => (showAll ? true : ACTIVE.has(e.status))).filter(matches)
	);

	// Collection total: market × quantity across the held (owned/listed)
	// products currently shown. Market price is data (latest_sealed_prices),
	// never hardcoded; a product with no snapshot contributes nothing.
	const totalValue = $derived(
		shown.reduce((s, e) => s + (ACTIVE.has(e.status) ? (e.market_price ?? 0) * e.quantity : 0), 0)
	);

	// --- Grid vs. table view, persisted across reloads (mirrors /collection). ---
	function readStoredView(): 'grid' | 'table' {
		if (typeof window === 'undefined') return 'table';
		const v = localStorage.getItem('sealed.view');
		return v === 'table' || v === 'grid' ? v : 'table';
	}
	let view = $state<'grid' | 'table'>(readStoredView());
	$effect(() => {
		if (typeof window !== 'undefined') localStorage.setItem('sealed.view', view);
	});

	// --- Column sort, persisted like the view mode. ---
	const SORT_KEYS = ['product', 'set', 'category', 'qty', 'paid', 'market', 'value', 'status', 'date'];
	function readStoredSort(): { key: string; dir: 'asc' | 'desc' } {
		if (typeof window === 'undefined') return { key: 'product', dir: 'asc' };
		const k = localStorage.getItem('sealed.sortKey');
		const d = localStorage.getItem('sealed.sortDir');
		return { key: k && SORT_KEYS.includes(k) ? k : 'product', dir: d === 'desc' ? 'desc' : 'asc' };
	}
	const _storedSort = readStoredSort();
	let sortKey = $state(_storedSort.key);
	let sortDir = $state<'asc' | 'desc'>(_storedSort.dir);
	$effect(() => {
		if (typeof window !== 'undefined') {
			localStorage.setItem('sealed.sortKey', sortKey);
			localStorage.setItem('sealed.sortDir', sortDir);
		}
	});
	function sortBy(key: string) {
		if (sortKey === key) {
			sortDir = sortDir === 'asc' ? 'desc' : 'asc';
		} else {
			sortKey = key;
			// Counts / money default high→low; text low→high.
			sortDir = key === 'qty' || key === 'paid' || key === 'market' || key === 'value' || key === 'date' ? 'desc' : 'asc';
		}
	}
	function sortValue(e: SealedEntry, key: string): number | string {
		switch (key) {
			case 'product':
				return e.name.toLowerCase();
			case 'set':
				return (e.set_code ?? '').toLowerCase();
			case 'category':
				return e.category.toLowerCase();
			case 'qty':
				return e.quantity;
			case 'paid':
				return e.purchase_price ?? -1;
			case 'market':
				return e.market_price ?? -1;
			case 'value':
				return e.market_price != null ? e.market_price * e.quantity : -1;
			case 'status':
				return e.status;
			case 'date':
				return e.added_at;
			default:
				return 0;
		}
	}
	const sorted = $derived.by(() => {
		const out = [...shown];
		out.sort((a, b) => {
			const va = sortValue(a, sortKey);
			const vb = sortValue(b, sortKey);
			const cmp = va < vb ? -1 : va > vb ? 1 : 0;
			return sortDir === 'asc' ? cmp : -cmp;
		});
		return out;
	});

	// Non-'owned' statuses surface as a small badge (owned is the default and
	// gets none). Mirrors the collection table's status pills.
	function statusBadge(status: string): string | null {
		switch (status) {
			case 'owned':
				return null;
			case 'listed':
				return 'LST';
			case 'sold':
				return 'SLD';
			case 'traded':
				return 'TRD';
			case 'gifted':
				return 'GFT';
			case 'opened':
				return 'OPN';
			default:
				return status.slice(0, 3).toUpperCase();
		}
	}
	function value(e: SealedEntry): number | null {
		return e.market_price != null ? e.market_price * e.quantity : null;
	}

	// --- Detail/edit modal. Closing re-runs the list only when the modal
	//     mutated something (mirrors /collection's cardDirty). ---
	let selected = $state<SealedEntry | null>(null);
	let dirty = $state(false);
	async function closeModal() {
		selected = null;
		if (dirty) {
			dirty = false;
			await load();
		}
	}

	// --- Top-bar burger + add-from-catalog flow. ---
	let menuOpen = $state(false);
	function closeMenu() {
		menuOpen = false;
	}

	let adding = $state(false);
	let addSearch = $state('');
	let results = $state<SealedProduct[]>([]);
	let chosen = $state<SealedProduct | null>(null);
	let addPrice = $state('');
	let busy = $state(false);

	async function runAddSearch() {
		if (addSearch.trim().length < 2) {
			results = [];
			return;
		}
		try {
			results = await api.sealedProducts(addSearch.trim());
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
	}
	function openAdd() {
		adding = true;
		closeMenu();
	}
	async function confirmAdd() {
		if (!chosen) return;
		busy = true;
		error = null;
		try {
			await api.addSealed({
				product_id: chosen.product_id,
				purchase_price: addPrice ? Number(addPrice) : undefined
			});
			adding = false;
			chosen = null;
			addSearch = '';
			results = [];
			addPrice = '';
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}
</script>

<svelte:head><title>Sealed — PokeDumpster</title></svelte:head>

<svelte:window
	onkeydown={(e) => {
		if (e.key === 'Escape') menuOpen = false;
	}}
/>

<header class="topbar">
	<div class="row row1">
		<a class="brand" href="/" aria-label="Home" title="Home">
			<span class="brandmark"><Pokeball size={26} /></span>
		</a>
		<div class="searchwrap">
			<input
				class="search"
				data-testid="search-input"
				type="text"
				autocomplete="off"
				placeholder="Filter sealed… (name, set, category)"
				bind:value={searchRaw}
			/>
			{#if searchRaw}
				<button
					class="searchclear"
					type="button"
					aria-label="Clear filter"
					title="Clear"
					onclick={() => (searchRaw = '')}>×</button
				>
			{/if}
		</div>
		<label class="alltoggle" title="Include opened, sold, traded, and gifted products">
			<input type="checkbox" data-testid="show-all-toggle" bind:checked={showAll} />
			Show disposed
		</label>
	</div>
	<div class="row row2">
		<div class="viewtoggle" role="group" aria-label="View">
			<button
				class:on={view === 'grid'}
				data-testid="view-grid"
				onclick={() => (view = 'grid')}
				aria-label="Grid view"
				title="Grid">▦</button
			>
			<button
				class:on={view === 'table'}
				data-testid="view-table"
				onclick={() => (view = 'table')}
				aria-label="Table view"
				title="Table">≡</button
			>
		</div>
		<div class="burgerWrap">
			<button
				class="burger"
				onclick={() => (menuOpen = !menuOpen)}
				aria-label="Menu"
				aria-expanded={menuOpen}
				title="Menu">⋯</button
			>
			{#if menuOpen}
				<div class="menu" role="menu">
					<button class="menuItem" onclick={openAdd}>+ Add sealed product</button>
					<a class="menuItem" href="/api/export/collectr/sealed.csv" download onclick={closeMenu}
						>Export sealed (Collectr)</a
					>
				</div>
			{/if}
		</div>
		<span class="countline muted">
			{count(shown.length)} sealed{#if totalValue > 0}, {money(totalValue)}{/if}
		</span>
	</div>
</header>

{#if menuOpen}
	<div class="menuBackdrop" role="presentation" onclick={closeMenu}></div>
{/if}

{#if error}<p class="error">{error}</p>{/if}

{#if loading}
	<p class="muted">Loading…</p>
{:else if shown.length === 0}
	<p class="muted">
		{#if query}No sealed products match <code>{searchRaw}</code>.{:else if showAll}No sealed products
			yet. Add one from the ⋯ menu.{:else}No sealed products in your active inventory. Add one from
			the ⋯ menu, or turn on “Show disposed”.{/if}
	</p>
{:else if view === 'grid'}
	<div class="gridsort">
		{#snippet sortBtn(key: string, label: string)}
			<button class="sortbtn" class:active={sortKey === key} onclick={() => sortBy(key)}>
				{label}
				{#if sortKey === key}<span class="caret">{sortDir === 'asc' ? '▲' : '▼'}</span>{/if}
			</button>
		{/snippet}
		{@render sortBtn('product', 'Product')}
		{@render sortBtn('set', 'Set')}
		{@render sortBtn('category', 'Category')}
		{@render sortBtn('qty', 'Qty')}
		{@render sortBtn('market', 'Market')}
		{@render sortBtn('value', 'Value')}
		{@render sortBtn('date', 'Date')}
	</div>
	<div class="cardgrid">
		{#each sorted as e (e.id)}
			<button
				class="cardtile"
				class:dim={!ACTIVE.has(e.status)}
				title="{e.name}{e.quantity > 1 ? ` ×${e.quantity}` : ''}"
				onclick={() => (selected = e)}
			>
				{#if e.image_url}
					<img src={e.image_url} alt={e.name} loading="lazy" />
				{:else}
					<div class="tilenoart">{e.name}</div>
				{/if}
				{#if e.quantity > 1}<span class="qtybadge">×{e.quantity}</span>{/if}
				{#if statusBadge(e.status)}<span class="statusbadge t-{e.status}">{statusBadge(e.status)}</span>{/if}
			</button>
		{/each}
	</div>
{:else}
	{#snippet sortable(key: string, label: string, extra: string, title?: string)}
		<th class="sortable {extra}" {title} onclick={() => sortBy(key)}>
			{label}
			{#if sortKey === key}<span class="caret">{sortDir === 'asc' ? '▲' : '▼'}</span>{/if}
		</th>
	{/snippet}
	<div class="tableScroll">
		<table class="dd">
			<thead>
				<tr>
					{@render sortable('product', 'Product', 'colflex')}
					{@render sortable('set', 'Set', 'center')}
					{@render sortable('category', 'Category', '')}
					{@render sortable('qty', 'Qty', 'num qty')}
					{@render sortable('paid', 'Paid', 'num', 'Purchase price')}
					{@render sortable('market', 'Market', 'num', 'Latest TCGplayer market price (per unit)')}
					{@render sortable('value', 'Value', 'num', 'Market × quantity')}
					{@render sortable('status', 'Status', 'center')}
					{@render sortable('date', 'Date', 'num')}
				</tr>
			</thead>
			<tbody>
				{#each sorted as e (e.id)}
					<tr class:dim={!ACTIVE.has(e.status)} onclick={() => (selected = e)}>
						<td class="colflex namecol">
							<div class="namecell">
								{#if e.image_url}
									<img class="cardthumb" src={e.image_url} alt="" loading="lazy" />
								{/if}
								<span class="namebody">
									<span class="cardname">{e.name}</span>{#if statusBadge(e.status)}<span
											class="tag stag t-{e.status}"
											title={e.status}>{statusBadge(e.status)}</span
										>{/if}
								</span>
							</div>
						</td>
						<td class="center">{e.set_code ? e.set_code.toUpperCase() : '—'}</td>
						<td class="cat">{e.category.replace(/_/g, ' ')}</td>
						<td class="num qty">{count(e.quantity)}</td>
						<td class="num">
							{#if e.purchase_price != null}<span class="pricebox">{money(e.purchase_price)}</span
								>{:else}<span class="pricedash">—</span>{/if}
						</td>
						<td class="num">
							{#if e.market_price != null}<span class="pricebox">{money(e.market_price)}</span
								>{:else}<span class="pricedash">—</span>{/if}
						</td>
						<td class="num">
							{#if value(e) != null}<span class="pricebox">{money(value(e))}</span>{:else}<span
									class="pricedash">—</span
								>{/if}
						</td>
						<td class="center">{e.status}</td>
						<td class="num">{e.added_at.slice(0, 10)}</td>
					</tr>
				{/each}
			</tbody>
		</table>
	</div>
{/if}

{#if selected}
	<SealedModal entry={selected} onClose={closeModal} onMutate={() => (dirty = true)} />
{/if}

{#if adding}
	<div class="backdrop" role="presentation" onclick={() => (adding = false)}></div>
	<div class="addmodal" role="dialog" aria-modal="true" aria-label="Add sealed product">
		<header class="addhead">
			<h3>Add sealed product</h3>
			<button class="x" onclick={() => (adding = false)} aria-label="Close">×</button>
		</header>
		{#if chosen}
			<p class="chosen">{chosen.name}</p>
			<label class="addlabel"
				>Purchase price <input type="number" min="0" step="0.01" bind:value={addPrice} /></label
			>
			<div class="addrow">
				<button class="link" onclick={() => (chosen = null)}>← Back</button>
				<button class="primary" disabled={busy} onclick={confirmAdd}>Add to collection</button>
			</div>
		{:else}
			<input
				class="addsearch"
				type="text"
				placeholder="Search sealed products…"
				bind:value={addSearch}
				oninput={runAddSearch}
			/>
			<div class="list">
				{#each results as p (p.product_id)}
					<button class="result" onclick={() => (chosen = p)}>
						<span>{p.name}</span>
						<span class="cat">{p.category.replace(/_/g, ' ')}</span>
					</button>
				{/each}
				{#if addSearch.trim().length >= 2 && results.length === 0}
					<p class="muted">No matching products.</p>
				{/if}
			</div>
		{/if}
	</div>
{/if}

<style>
	.muted {
		color: #888;
	}
	.error {
		color: #e94560;
		padding: 0 0.7rem;
	}

	/* --- DD-style top chrome (mirrors /collection) --------------------- */
	.topbar {
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
	.row {
		display: flex;
		align-items: center;
		gap: 0.5rem;
	}
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
		padding: 0.45rem 2rem 0.45rem 0.6rem;
		background: #1a1a2e;
		border: 1px solid #0f3460;
		border-radius: 6px;
		color: #e0e0e0;
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
	.burgerWrap {
		position: relative;
		display: inline-flex;
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
	.menu {
		position: absolute;
		top: calc(100% + 4px);
		left: 0;
		z-index: 60;
		display: flex;
		flex-direction: column;
		min-width: 200px;
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

	/* --- Grid view ----------------------------------------------------- */
	.gridsort {
		display: flex;
		gap: 0.4rem;
		align-items: center;
		flex-wrap: wrap;
		margin: 0.5rem 0.7rem;
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
		color: inherit;
		font-size: 0.65rem;
		opacity: 0.9;
	}
	.cardgrid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
		gap: 0.5rem;
		padding: 0 0.7rem 1rem;
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
		aspect-ratio: 1 / 1;
		object-fit: contain;
		background: #0d1424;
		border-radius: 6px;
	}
	.cardtile.dim img,
	.cardtile.dim .tilenoart {
		filter: grayscale(0.7) brightness(0.7);
	}
	.tilenoart {
		aspect-ratio: 1 / 1;
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
		pointer-events: none;
	}
	.statusbadge {
		position: absolute;
		bottom: 4px;
		left: 4px;
		font-size: 0.62rem;
		font-weight: 700;
		text-transform: uppercase;
		padding: 1px 5px;
		border-radius: 4px;
		border: 1px solid;
		pointer-events: none;
	}

	/* --- Table view (DeckDumpster-style, shared with /collection) ------- */
	table.dd {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.9rem;
		margin-top: 0;
	}
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
	table.dd tbody tr.dim {
		color: #777;
		opacity: 0.7;
	}
	table.dd tbody tr.dim img {
		filter: grayscale(0.7) brightness(0.7);
	}
	table.dd td.cat {
		text-transform: capitalize;
		color: #bbb;
	}
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
		width: 46px;
		height: 46px;
		object-fit: contain;
		border-radius: 3px;
		flex-shrink: 0;
		background: #0d1424;
	}
	.cardname {
		font-weight: 500;
		color: #e0e0e0;
	}
	.namebody {
		min-width: 0;
		line-height: 1.25;
	}

	/* Inline status pills — same palette as /collection's .stag. */
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
	.t-listed {
		background: #1a3a5c;
		color: #78c8f0;
		border-color: #2a5a8c;
	}
	.t-sold,
	.t-traded,
	.t-gifted {
		background: #1a5c3a;
		color: #7ee8b0;
		border-color: #2a8c5a;
	}
	.t-opened {
		background: #5c3a1a;
		color: #f0c878;
		border-color: #8c5a2a;
	}

	/* --- Add-from-catalog modal ---------------------------------------- */
	.backdrop {
		position: fixed;
		inset: 0;
		background: rgba(0, 0, 0, 0.6);
		z-index: 100;
	}
	.addmodal {
		position: fixed;
		top: 50%;
		left: 50%;
		transform: translate(-50%, -50%);
		z-index: 101;
		width: 440px;
		max-width: 92vw;
		background: #16213e;
		border: 2px solid #0f3460;
		border-radius: 12px;
		padding: 1.25rem;
	}
	.addhead {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
	}
	h3 {
		margin: 0;
		color: #e94560;
	}
	.x {
		background: none;
		border: none;
		color: #888;
		font-size: 1.4rem;
		cursor: pointer;
	}
	.x:hover {
		color: #e94560;
	}
	.addsearch,
	.addmodal input[type='number'] {
		width: 100%;
		padding: 0.5rem;
		margin: 0.5rem 0;
		background: #1a1a2e;
		border: 1px solid #0f3460;
		border-radius: 6px;
		color: #e0e0e0;
		box-sizing: border-box;
	}
	.list {
		max-height: 300px;
		overflow-y: auto;
	}
	.result {
		display: flex;
		justify-content: space-between;
		width: 100%;
		background: none;
		color: #e0e0e0;
		border: none;
		border-bottom: 1px solid #0f3460;
		padding: 0.5rem 0.25rem;
		text-align: left;
		cursor: pointer;
	}
	.result:hover {
		background: rgba(233, 69, 96, 0.1);
	}
	.cat {
		color: #888;
		font-size: 0.8rem;
		text-transform: capitalize;
	}
	.chosen {
		font-weight: 700;
		color: #e94560;
	}
	.addlabel {
		display: block;
		font-size: 0.85rem;
		color: #888;
	}
	.addrow {
		display: flex;
		justify-content: space-between;
		align-items: center;
		margin-top: 0.5rem;
	}
	.link {
		background: none;
		border: none;
		color: #888;
		cursor: pointer;
		padding: 0;
	}
	.link:hover {
		color: #e94560;
	}
	.primary {
		background: #e94560;
		border: none;
		color: #fff;
		padding: 0.4rem 0.8rem;
		border-radius: 6px;
		cursor: pointer;
	}
	.primary:disabled {
		opacity: 0.5;
	}
</style>
