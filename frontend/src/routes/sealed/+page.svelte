<script lang="ts">
	import { onMount } from 'svelte';
	import { api } from '$lib/api';
	import { money, count } from '$lib/format';
	import type { SealedEntry } from '$lib/types/SealedEntry';
	import type { SealedProduct } from '$lib/types/SealedProduct';

	const STATUSES = ['owned', 'listed', 'sold', 'traded', 'gifted', 'opened'];
	const ACTIVE = new Set(['owned', 'listed']);

	let entries = $state<SealedEntry[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let showAll = $state(false);
	let busy = $state(false);

	// Add modal.
	let adding = $state(false);
	let search = $state('');
	let results = $state<SealedProduct[]>([]);
	let chosen = $state<SealedProduct | null>(null);
	let price = $state('');

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

	const shown = $derived(showAll ? entries : entries.filter((e) => ACTIVE.has(e.status)));

	async function runSearch() {
		if (search.trim().length < 2) {
			results = [];
			return;
		}
		try {
			results = await api.sealedProducts(search.trim());
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
	}

	async function add() {
		if (!chosen) return;
		busy = true;
		error = null;
		try {
			await api.addSealed({
				product_id: chosen.product_id,
				purchase_price: price ? Number(price) : undefined
			});
			adding = false;
			chosen = null;
			search = '';
			results = [];
			price = '';
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}

	async function setStatus(id: number, status: string) {
		busy = true;
		error = null;
		try {
			await api.updateSealed(id, { status });
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}

	async function remove(id: number) {
		if (!confirm('Remove this sealed product from your collection?')) return;
		busy = true;
		try {
			await api.deleteSealed(id);
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}
</script>

<svelte:head><title>Sealed — PokeDumpster</title></svelte:head>

<header>
	<h1>Sealed collection</h1>
	<button onclick={() => (adding = true)}>+ Add sealed</button>
</header>

<label class="toggle">
	<input type="checkbox" bind:checked={showAll} /> Show opened / disposed
</label>

{#if error}<p class="error">{error}</p>{/if}

{#if loading}
	<p class="muted">Loading…</p>
{:else if shown.length === 0}
	<p class="muted">No sealed products{showAll ? '' : ' in your active inventory'}.</p>
{:else}
	<table>
		<thead>
			<tr><th>Product</th><th>Category</th><th>Qty</th><th>Paid</th><th>Status</th><th></th></tr>
		</thead>
		<tbody>
			{#each shown as e (e.id)}
				<tr class:dim={!ACTIVE.has(e.status)}>
					<td>{e.name}</td>
					<td>{e.category}</td>
					<td>{count(e.quantity)}</td>
					<td>{money(e.purchase_price)}</td>
					<td>
						<select
							value={e.status}
							disabled={busy}
							onchange={(ev) => setStatus(e.id, ev.currentTarget.value)}
						>
							{#each STATUSES as s (s)}<option value={s}>{s}</option>{/each}
						</select>
					</td>
					<td><button class="link" disabled={busy} onclick={() => remove(e.id)}>Remove</button></td>
				</tr>
			{/each}
		</tbody>
	</table>
{/if}

{#if adding}
	<div class="backdrop"></div>
	<div class="modal" role="dialog" aria-modal="true" aria-label="Add sealed product">
		<header>
			<h3>Add sealed product</h3>
			<button class="x" onclick={() => (adding = false)} aria-label="Close">×</button>
		</header>
		{#if chosen}
			<p class="chosen">{chosen.name}</p>
			<label>Purchase price <input type="number" min="0" step="0.01" bind:value={price} /></label>
			<div class="row">
				<button class="link" onclick={() => (chosen = null)}>← Back</button>
				<button class="primary" disabled={busy} onclick={add}>Add to collection</button>
			</div>
		{:else}
			<input
				class="search"
				type="text"
				placeholder="Search sealed products…"
				bind:value={search}
				oninput={runSearch}
			/>
			<div class="list">
				{#each results as p (p.product_id)}
					<button class="result" onclick={() => (chosen = p)}>
						<span>{p.name}</span>
						<span class="cat">{p.category}</span>
					</button>
				{/each}
				{#if search.trim().length >= 2 && results.length === 0}
					<p class="muted">No matching products.</p>
				{/if}
			</div>
		{/if}
	</div>
{/if}

<style>
	header {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
	}
	h1 {
		color: #e94560;
	}
	.muted {
		color: #888;
	}
	.error {
		color: #e94560;
	}
	.toggle {
		display: block;
		font-size: 0.85rem;
		color: #ccc;
		margin: 0.5rem 0 1rem;
	}
	button {
		background: #e94560;
		border: none;
		color: #fff;
		padding: 0.4rem 0.8rem;
		border-radius: 6px;
		cursor: pointer;
	}
	button.link {
		background: none;
		color: #888;
		padding: 0;
	}
	button.link:hover {
		color: #e94560;
	}
	button:disabled {
		opacity: 0.5;
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
	tr.dim {
		opacity: 0.5;
	}
	select {
		background: #1a1a2e;
		border: 1px solid #0f3460;
		color: #e0e0e0;
		border-radius: 6px;
		padding: 0.15rem;
	}
	.backdrop {
		position: fixed;
		inset: 0;
		background: rgba(0, 0, 0, 0.6);
		z-index: 100;
	}
	.modal {
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
	.modal header {
		display: flex;
		justify-content: space-between;
	}
	h3 {
		margin: 0;
		color: #e94560;
	}
	.x {
		background: none;
		color: #888;
		font-size: 1.4rem;
	}
	.search,
	.modal input[type='number'] {
		width: 100%;
		padding: 0.5rem;
		margin: 0.5rem 0;
		background: #1a1a2e;
		border: 1px solid #0f3460;
		border-radius: 6px;
		color: #e0e0e0;
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
		border-bottom: 1px solid #0f3460;
		border-radius: 0;
		text-align: left;
	}
	.result:hover {
		background: rgba(233, 69, 96, 0.1);
	}
	.cat {
		color: #888;
		font-size: 0.8rem;
	}
	.chosen {
		font-weight: 700;
		color: #e94560;
	}
	.modal label {
		display: block;
		font-size: 0.85rem;
		color: #888;
	}
	.row {
		display: flex;
		justify-content: space-between;
		align-items: center;
		margin-top: 0.5rem;
	}
	.primary {
		background: #e94560;
	}
</style>
