<script lang="ts">
	import { onMount } from 'svelte';
	import { money, count } from '$lib/format';
	import { api } from '$lib/api';
	import Pokeball from '$lib/components/Pokeball.svelte';
	import SealedModal from '$lib/components/SealedModal.svelte';
	import { Badge, Button, EmptyState, Field, Panel, Toolbar } from '$lib/components/ui';
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
	// Which Badge tone a status wears. Was three `.t-<status>` CSS blocks
	// restating the same info/success/warning palettes by hand; the tone names
	// a state and the Badge resolves the fill, text and rule.
	// NOTE this is still display metadata living in the frontend, same smell as
	// `statusBadge` above — filed as pd-ex95 (tag and tone belong beside the
	// status in a table, the way variants.json sits beside a variant code).
	const STATUS_TONE: Record<string, 'info' | 'success' | 'warning' | 'neutral'> = {
		listed: 'info',
		sold: 'success',
		traded: 'success',
		gifted: 'success',
		opened: 'warning'
	};
	function statusTone(status: string): 'info' | 'success' | 'warning' | 'neutral' {
		return STATUS_TONE[status] ?? 'neutral';
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

<Toolbar class="topbar" direction="column" align="stretch" gap="sm" wrap={false} sticky>
	<Toolbar gap="sm" wrap={false}>
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
				<Button
					variant="link"
					class="searchclear"
					aria-label="Clear filter"
					title="Clear"
					onclick={() => (searchRaw = '')}>×</Button
				>
			{/if}
		</div>
		<Field
			inline
			type="checkbox"
			label="Show disposed"
			data-testid="show-all-toggle"
			title="Include opened, sold, traded, and gifted products"
			bind:checked={showAll}
		/>
	</Toolbar>
	<Toolbar gap="sm" wrap={false}>
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
			<Button
				variant="link"
				class="burger"
				onclick={() => (menuOpen = !menuOpen)}
				aria-label="Menu"
				aria-expanded={menuOpen}
				title="Menu">⋯</Button
			>
			{#if menuOpen}
				<Panel variant="overlay" elevation="md" padding="sm" class="menu" role="menu">
					<button class="menuItem" onclick={openAdd}>+ Add sealed product</button>
					<a class="menuItem" href="/api/export/collectr/sealed.csv" download onclick={closeMenu}
						>Export sealed (Collectr)</a
					>
				</Panel>
			{/if}
		</div>
		<span class="countline muted">
			{count(shown.length)} sealed{#if totalValue > 0}, {money(totalValue)}{/if}
		</span>
	</Toolbar>
</Toolbar>

{#if menuOpen}
	<div class="menuBackdrop" role="presentation" onclick={closeMenu}></div>
{/if}

{#if error}<p class="error">{error}</p>{/if}

{#if loading}
	<p class="muted">Loading…</p>
{:else if shown.length === 0}
	{#if query}
		<EmptyState title="No sealed products match “{searchRaw}”." />
	{:else if showAll}
		<EmptyState title="No sealed products yet." description="Add one from the ⋯ menu." />
	{:else}
		<EmptyState
			title="No sealed products in your active inventory."
			description="Add one from the ⋯ menu, or turn on “Show disposed”."
		/>
	{/if}
{:else if view === 'grid'}
	{#snippet sortBtn(key: string, label: string)}
		<Button
			variant={sortKey === key ? 'primary' : 'ghost'}
			size="sm"
			class="sortbtn"
			onclick={() => sortBy(key)}
		>
			{label}
			{#if sortKey === key}<span class="btncaret">{sortDir === 'asc' ? '▲' : '▼'}</span>{/if}
		</Button>
	{/snippet}
	<div class="gridsort">
		<Toolbar gap="sm">
			{@render sortBtn('product', 'Product')}
			{@render sortBtn('set', 'Set')}
			{@render sortBtn('category', 'Category')}
			{@render sortBtn('qty', 'Qty')}
			{@render sortBtn('market', 'Market')}
			{@render sortBtn('value', 'Value')}
			{@render sortBtn('date', 'Date')}
		</Toolbar>
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
				{#if e.quantity > 1}
					<Badge tone="info" variant="solid" size="sm" class="qtybadge">×{e.quantity}</Badge>
				{/if}
				{#if statusBadge(e.status)}
					<Badge tone={statusTone(e.status)} shape="tag" size="sm" class="statusbadge"
						>{statusBadge(e.status)}</Badge
					>
				{/if}
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
									<span class="cardname">{e.name}</span>{#if statusBadge(e.status)}<Badge
											tone={statusTone(e.status)}
											shape="tag"
											size="sm"
											class="stag"
											title={e.status}>{statusBadge(e.status)}</Badge
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
	<div class="addmodal">
		<Panel
			variant="overlay"
			elevation="lg"
			padding="lg"
			role="dialog"
			aria-modal="true"
			aria-label="Add sealed product"
		>
		<header class="addhead">
			<h3>Add sealed product</h3>
			<Button variant="link" class="x" onclick={() => (adding = false)} aria-label="Close"
				>×</Button
			>
		</header>
		{#if chosen}
			<p class="chosen">{chosen.name}</p>
			<Field
				label="Purchase price"
				type="number"
				min="0"
				step="0.01"
				class="addfield"
				bind:value={addPrice}
			/>
			<Toolbar class="addrow" justify="between" gap="sm">
				<Button variant="link" onclick={() => (chosen = null)}>← Back</Button>
				<Button disabled={busy} onclick={confirmAdd}>Add to collection</Button>
			</Toolbar>
		{:else}
			<Field
				type="text"
				placeholder="Search sealed products…"
				class="addfield"
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
		</Panel>
	</div>
{/if}

<style>
	/*
		What is left here after the migration is layout and geometry: where a
		box sits, how wide it is, what shape it holds. Every surface, fill,
		rule, text colour, radius and spacing step comes from the semantic
		token layer or from a primitive that owns it.

		The remaining bespoke controls — the search field with its clear-X, the
		grid/table segmented toggle, the burger menu's rows — are shapes the
		primitive set does not have yet. They are shared verbatim with
		/collection, so inventing a private variant here would be the second
		pattern this bead is meant to avoid — filed as pd-5fki instead.

		WHERE A PRIMITIVE IS PLACED. Svelte scopes a rule to the elements in
		this file, and a `class` handed to a component lands on markup this
		file does not own — so `.menu { position: absolute }` would compile to
		a selector that matches nothing. Placement of a primitive is therefore
		written as `:global()` nested under a scoped ancestor
		(`.burgerWrap :global(.menu)`), and where no ancestor exists the
		primitive gets a plain wrapper element that carries it. Never a bare
		`:global(.menu)` — that leaks the rule to every route.
	*/
	.muted {
		color: var(--color-text-subtle);
	}
	.error {
		color: var(--color-danger-text);
		padding: var(--space-0) var(--space-3);
	}

	/* --- DD-style top chrome (mirrors /collection) ---------------------
	   The band itself is Toolbar `sticky`: translucent chrome that lets rows
	   show through rather than stamping an opaque panel across the page. */
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
		padding: var(--space-2) var(--space-8) var(--space-2) var(--space-2);
		background: var(--color-control-surface);
		border: 1px solid var(--color-control-border);
		border-radius: var(--radius-md);
		color: var(--color-control-text);
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
	.countline {
		margin: var(--space-0) var(--space-0) var(--space-0) auto;
		font-size: var(--text-md);
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
		padding: var(--space-1) var(--space-2);
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
	.burgerWrap {
		position: relative;
		display: inline-flex;
	}
	.burgerWrap :global(.burger) {
		font-size: var(--text-2xl);
		line-height: 1;
		padding: var(--space-1) var(--space-2);
	}
	.menuBackdrop {
		position: fixed;
		inset: 0;
		z-index: 49;
	}
	/* Placement only; Panel `overlay` + elevation md is the popover surface. */
	.burgerWrap :global(.menu) {
		position: absolute;
		top: calc(100% + var(--space-1));
		left: 0;
		z-index: 60;
		display: flex;
		flex-direction: column;
		min-width: 200px;
	}
	.menuItem {
		background: none;
		border: none;
		color: var(--color-text);
		text-align: left;
		padding: var(--space-2) var(--space-3);
		font: inherit;
		font-size: var(--text-lg);
		border-radius: var(--radius-sm);
		cursor: pointer;
		text-decoration: none;
		display: block;
	}
	.menuItem:hover {
		background: var(--color-info-surface);
		color: var(--color-text-accent);
	}

	/* --- Grid view ----------------------------------------------------- */
	/* The row's own Toolbar lays the buttons out; this places the row. */
	.gridsort {
		margin: var(--space-2) var(--space-3);
	}
	.caret {
		color: var(--color-text-accent);
		font-size: var(--text-xs);
		margin-left: var(--space-0-5);
	}
	/* Inside a sort button the caret rides the button's own ink. */
	.btncaret {
		color: inherit;
		font-size: var(--text-xs);
		opacity: 0.9;
	}
	.cardgrid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
		gap: var(--space-2);
		padding: var(--space-0) var(--space-3) var(--space-4);
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
		aspect-ratio: 1 / 1;
		object-fit: contain;
		background: var(--color-surface-well);
		border-radius: var(--radius-md);
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
		background: var(--color-surface-panel);
		border-radius: var(--radius-md);
		color: var(--color-text-subtle);
		font-size: var(--text-sm);
		padding: var(--space-2);
		text-align: center;
	}
	/* Both badges are Badge primitives; the route only says where they sit. */
	.cardtile :global(.qtybadge) {
		position: absolute;
		top: var(--space-1);
		right: var(--space-1);
		pointer-events: none;
	}
	.cardtile :global(.statusbadge) {
		position: absolute;
		bottom: var(--space-1);
		left: var(--space-1);
		pointer-events: none;
	}

	/* --- Table view (DeckDumpster-style, shared with /collection) ------- */
	table.dd {
		width: 100%;
		border-collapse: collapse;
		font-size: var(--text-md);
		margin-top: var(--space-0);
	}
	table.dd th.colflex,
	table.dd td.colflex {
		width: 100%;
	}
	table.dd th,
	table.dd td {
		padding: var(--space-1) var(--space-2);
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
	/* Disposed rows recede but stay content: the subtle step of the text
	   ladder, not the decorative one (which is exempt from AA). */
	table.dd tbody tr.dim {
		color: var(--color-text-subtle);
		opacity: 0.7;
	}
	table.dd tbody tr.dim img {
		filter: grayscale(0.7) brightness(0.7);
	}
	table.dd td.cat {
		text-transform: capitalize;
		color: var(--color-text-muted);
	}
	.pricebox {
		display: inline-block;
		background: var(--color-surface-page);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-sm);
		padding: var(--space-px) var(--space-1);
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
	.qty {
		font-weight: var(--weight-semibold);
		color: var(--color-text);
	}
	.namecell {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		min-width: 0;
	}
	.cardthumb {
		width: 46px;
		height: 46px;
		object-fit: contain;
		border-radius: var(--radius-xs);
		flex-shrink: 0;
		background: var(--color-surface-well);
	}
	.cardname {
		font-weight: var(--weight-medium);
		color: var(--color-text);
	}
	.namebody {
		min-width: 0;
		line-height: var(--leading-tight);
	}
	/* Inline status marker — a Badge `tag`, placed. */
	.namebody :global(.stag) {
		margin-left: var(--space-1);
	}

	/* --- Add-from-catalog modal ---------------------------------------- */
	.backdrop {
		position: fixed;
		inset: 0;
		background: var(--color-scrim);
		z-index: 100;
	}
	/* Placement only; Panel `overlay` is the dialog surface. */
	.addmodal {
		position: fixed;
		top: 50%;
		left: 50%;
		transform: translate(-50%, -50%);
		z-index: 101;
		width: 440px;
		max-width: 92vw;
	}
	.addhead {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
	}
	h3 {
		margin: var(--space-0);
		color: var(--color-text-accent);
	}
	.addhead :global(.x) {
		font-size: var(--text-2xl);
	}
	.addmodal :global(.addfield) {
		width: 100%;
		margin: var(--space-2) var(--space-0);
	}
	.addmodal :global(.addrow) {
		margin-top: var(--space-2);
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
		color: var(--color-text);
		border: none;
		border-bottom: 1px solid var(--color-border);
		padding: var(--space-2) var(--space-1);
		text-align: left;
		cursor: pointer;
	}
	.result:hover {
		background: var(--color-surface-accent-wash);
	}
	.cat {
		color: var(--color-text-subtle);
		font-size: var(--text-sm);
		text-transform: capitalize;
	}
	.chosen {
		font-weight: var(--weight-bold);
		color: var(--color-text-accent);
	}
</style>
