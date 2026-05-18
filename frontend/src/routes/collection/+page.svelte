<script lang="ts">
	import { onMount } from 'svelte';
	import { api, variantLabel } from '$lib/api';
	import type { CollectionRow } from '$lib/types/CollectionRow';

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

	onMount(async () => {
		try {
			rows = await api.collection();
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
			<input
				class="search"
				type="text"
				placeholder="Search cards…"
				value={searchRaw}
				oninput={(e) => onSearch(e.currentTarget.value)}
			/>
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
			<p class="muted">{filtered.length} of {rows.length} cards</p>
			{#if rows.length === 0}
				<p class="muted">Your collection is empty. Add cards from a set's binder view.</p>
			{:else}
				<table>
					<thead>
						<tr>
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
							<tr>
								<td><a href="/card/{row.set_code}/{row.number}">{row.name}</a></td>
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
	.search {
		width: 100%;
		padding: 0.5rem;
		background: #1a1a2e;
		border: 1px solid #0f3460;
		border-radius: 6px;
		color: #e0e0e0;
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
