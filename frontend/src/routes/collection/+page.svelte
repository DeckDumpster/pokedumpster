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

	// Column sort for the table view.
	let sortKey = $state('name');
	let sortDir = $state<'asc' | 'desc'>('asc');

	function sortBy(key: string) {
		if (sortKey === key) {
			sortDir = sortDir === 'asc' ? 'desc' : 'asc';
		} else {
			sortKey = key;
			// Counts and money default to high→low; everything else low→high.
			sortDir = key === 'qty' || key === 'market' ? 'desc' : 'asc';
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
	const totalValue = $derived(
		filtered.reduce((s, r) => s + (r.market_price ?? 0), 0)
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
			const existing = map.get(key);
			if (existing) {
				existing.ids.push(r.id);
				existing.qty += 1;
				if (r.purchase_price != null) {
					existing.paid_total = (existing.paid_total ?? 0) + r.purchase_price;
				}
				if (r.market_price != null) {
					existing.market_total = (existing.market_total ?? 0) + r.market_price;
				}
			} else {
				map.set(key, {
					key,
					ids: [r.id],
					qty: 1,
					paid_total: r.purchase_price,
					market_total: r.market_price,
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

	// Short tag in the Name cell. The two common foil treatments collapse
	// to single-letter badges; rarer treatments keep room for their name.
	function variantTag(code: string): string {
		if (code === 'holo') return 'H';
		if (code === 'reverse_holo') return 'R';
		return variantLabel(code);
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
		'Trainer Gallery Rare Holo': 11,
		'Rare Secret': 12,
		'Rare Rainbow': 12,
		'Special Illustration Rare': 13,
		'Hyper Rare': 14,
		'Rare Holo Star': 14
	};
	function rarityRank(r: string | null): number {
		if (!r) return 0;
		return RARITY_RANK[r] ?? 6;
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
			case 'market':
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
		return ids.every((id) => selected.has(id));
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

	// The header checkbox in the table selects/clears every aggregated row.
	const tableAllSelected = $derived(
		sorted.length > 0 && sorted.every((a) => groupChecked(a.ids))
	);
	function toggleTableAll() {
		if (tableAllSelected) {
			selected = new Set();
		} else {
			const next = new Set<number>();
			for (const a of sorted) for (const id of a.ids) next.add(id);
			selected = next;
		}
	}

	// The grid still operates per copy; its header checkbox sees raw rows.
	const allSelected = $derived(
		filtered.length > 0 && filtered.every((r) => selected.has(r.id))
	);

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
		<p class="muted">
			{filtered.length}
			cards{#if totalValue > 0}, ${totalValue.toFixed(2)}{/if}
		</p>
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
		{#snippet sortable(key: string, label: string, extra: string)}
			<th class="sortable {extra}" onclick={() => sortBy(key)}>
				{label}
				{#if sortKey === key}
					<span class="caret">{sortDir === 'asc' ? '▲' : '▼'}</span>
				{/if}
			</th>
		{/snippet}
		<table class="dd">
			<thead>
				<tr>
					{#if selectMode}
						<th class="cbcol">
							<input type="checkbox" checked={tableAllSelected} onchange={toggleTableAll} />
						</th>
					{/if}
					{@render sortable('qty', 'Qty', 'num')}
					{@render sortable('name', 'Name', '')}
					{@render sortable('type', 'Class', '')}
					{@render sortable('etype', 'Type', 'center')}
					<th>Cost</th>
					{@render sortable('rarity', 'Rarity', 'center')}
					{@render sortable('set', 'Set', 'center')}
					{@render sortable('number', '#', 'num')}
					{@render sortable('market', 'Market', 'num')}
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
						<td>
							<div class="namecell">
								{#if a.image_small}
									<img class="cardthumb" src={a.image_small} alt="" loading="lazy" />
								{/if}
								<span class="cardname">{a.name}</span>
								{#if a.variant !== 'normal'}
									<span class="tag vtag" title={variantLabel(a.variant)}>
										{variantTag(a.variant)}
									</span>
								{/if}
								{#if statusBadge(a.status)}
									<span class="tag stag t-{a.status}" title={a.status}>
										{statusBadge(a.status)}
									</span>
								{/if}
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
							{a.market_total != null ? `$${a.market_total.toFixed(2)}` : '—'}
						</td>
					</tr>
				{/each}
			</tbody>
		</table>
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

	/* --- Grid view ------------------------------------------------------ */

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
		width: 100%;
		border-collapse: collapse;
		font-size: 0.9rem;
		margin-top: 0.8rem;
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
	table.dd .cbcol {
		width: 1.5rem;
		text-align: center;
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

	/* Inline tags (variant, non-owned status) — DD card-tag pattern. */
	.tag {
		padding: 1px 4px;
		font-size: 0.62rem;
		font-weight: 600;
		text-transform: uppercase;
		border-radius: 3px;
		border: 1px solid;
		letter-spacing: 0.04em;
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
