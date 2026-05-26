<script lang="ts">
	import { onMount } from 'svelte';
	import { api } from '$lib/api';
	import type { SetSummary } from '$lib/types/SetSummary';

	let sets = $state<SetSummary[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let search = $state('');

	onMount(async () => {
		try {
			sets = await api.sets();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	});

	const filtered = $derived(
		sets.filter((s) => {
			const q = search.trim().toLowerCase();
			return !q || s.name.toLowerCase().includes(q) || s.series.toLowerCase().includes(q);
		})
	);

	function pct(s: SetSummary): number {
		return s.total_cards > 0 ? Math.round((s.owned_cards / s.total_cards) * 100) : 0;
	}
	function basePct(s: SetSummary): number {
		if (s.base_total_cards == null || s.base_owned_cards == null || s.base_total_cards === 0)
			return 0;
		return Math.round((s.base_owned_cards / s.base_total_cards) * 100);
	}
</script>

<svelte:head><title>Browse sets — PokeDumpster</title></svelte:head>

<h1>Browse sets</h1>
<p class="muted">Pick a set to open its binder view.</p>

{#if loading}
	<p class="muted">Loading…</p>
{:else if error}
	<p class="error">Failed to load sets: {error}</p>
{:else}
	<input class="search" type="text" placeholder="Search sets…" bind:value={search} />
	<p class="muted">{filtered.length} of {sets.length} sets</p>
	<div class="grid">
		{#each filtered as set (set.set_code)}
			<a class="tile" href="/browse/{set.set_code}">
				{#if set.symbol_url}
					<img class="symbol" src={set.symbol_url} alt="" />
				{/if}
				<div class="title">{set.name}</div>
				<div class="series">{set.series}</div>
				{#if set.base_total_cards != null && set.base_owned_cards != null}
					<div class="count">Base {set.base_owned_cards} / {set.base_total_cards}</div>
					<div class="bar base"><span style:width="{basePct(set)}%"></span></div>
				{/if}
				<div class="count">Master {set.owned_cards} / {set.total_cards}</div>
				<div class="bar"><span style:width="{pct(set)}%"></span></div>
			</a>
		{/each}
	</div>
{/if}

<style>
	h1 {
		color: #e94560;
		margin-bottom: 0.25rem;
	}
	.muted {
		color: #888;
	}
	.error {
		color: #e94560;
	}
	.search {
		width: 100%;
		max-width: 360px;
		padding: 0.5rem;
		background: #1a1a2e;
		border: 1px solid #0f3460;
		border-radius: 6px;
		color: #e0e0e0;
		margin: 0.5rem 0;
	}
	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(190px, 1fr));
		gap: 1rem;
	}
	.tile {
		display: block;
		background: #16213e;
		border: 2px solid #0f3460;
		border-radius: 10px;
		padding: 1rem;
		text-decoration: none;
		color: #e0e0e0;
		transition: border-color 0.15s;
	}
	.tile:hover {
		border-color: #e94560;
	}
	.symbol {
		height: 28px;
		margin-bottom: 0.4rem;
	}
	.title {
		font-weight: 700;
		color: #e94560;
	}
	.series {
		font-size: 0.8rem;
		color: #888;
		margin: 0.1rem 0 0.5rem;
	}
	.count {
		font-size: 0.85rem;
	}
	.bar {
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
	/* Base-set bar is the "completion you actually care about" measure;
	   green to distinguish from the red Master bar. */
	.bar.base span {
		background: #4caf72;
	}
	.count + .bar.base {
		margin-bottom: 0.35rem;
	}
</style>
