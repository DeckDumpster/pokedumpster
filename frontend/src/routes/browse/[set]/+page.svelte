<script lang="ts">
	import { page } from '$app/state';
	import { api } from '$lib/api';
	import type { BinderPage } from '$lib/types/BinderPage';

	let binder = $state<BinderPage | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);

	let pageNum = $state(1);
	let layout = $state(9);
	let includeSecret = $state(true);
	let includeSubset = $state(true);
	let includePromos = $state(false);

	async function load() {
		const set = page.params.set;
		if (!set) return;
		loading = true;
		error = null;
		try {
			binder = await api.binder(set, {
				page: pageNum,
				layout,
				secret: includeSecret,
				subset: includeSubset,
				promos: includePromos
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
		load();
	});

	function columns(l: number): number {
		if (l === 4) return 2;
		if (l === 12) return 4;
		return 3;
	}

	const sectionLabel: Record<string, string> = {
		base: '',
		secret: 'Secret Rares',
		subset: 'Subset',
		promo: 'Promos'
	};

	function pct(owned: number, total: number): number {
		return total > 0 ? Math.round((owned / total) * 100) : 0;
	}

	// Reset to page 1 whenever the filters change.
	function resetPage() {
		pageNum = 1;
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

	{#if binder.slots.length === 0}
		<p class="muted">No cards in this view.</p>
	{:else}
		<div class="grid" style:grid-template-columns="repeat({columns(layout)}, 1fr)">
			{#each binder.slots as slot, i (slot.card_id)}
				{@const prevSection = i > 0 ? binder.slots[i - 1].section : 'base'}
				{#if slot.section !== prevSection && slot.section !== 'base'}
					<div class="divider">{sectionLabel[slot.section]}</div>
				{/if}
				<a class="slot" href="/card/{binder.set.set_code}/{slot.number}">
					{#if slot.image_large}
						<img src={slot.image_large} alt={slot.name} loading="lazy" />
					{:else}
						<div class="noart">{slot.name}</div>
					{/if}
					<div class="foot">
						<span class="num">{slot.number}</span>
						<div class="pips">
							{#each slot.printings.filter((p) => !p.deprecated) as p (p.printing_id)}
								<span class="pip" class:owned={p.owned_count > 0}></span>
							{/each}
						</div>
					</div>
				</a>
			{/each}
		</div>
	{/if}
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
		background: #16213e;
		border: 2px solid #0f3460;
		border-radius: 8px;
		overflow: hidden;
		text-decoration: none;
		color: #e0e0e0;
		transition: border-color 0.15s;
	}
	.slot:hover {
		border-color: #e94560;
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
		justify-content: space-between;
		align-items: center;
		padding: 0.3rem 0.5rem;
	}
	.num {
		font-size: 0.8rem;
		color: #888;
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
</style>
