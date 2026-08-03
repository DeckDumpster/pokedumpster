<script lang="ts">
	import { onMount } from 'svelte';
	import { api } from '$lib/api';
	import { count } from '$lib/format';
	import { Button, EmptyState } from '$lib/components/ui';
	import type { Batch } from '$lib/types/Batch';

	let batches = $state<Batch[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let typeFilter = $state('');

	onMount(async () => {
		try {
			batches = await api.batches();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	});

	const types = $derived([...new Set(batches.map((b) => b.batch_type))].sort());
	const shown = $derived(batches.filter((b) => !typeFilter || b.batch_type === typeFilter));
</script>

<svelte:head><title>Batches — PokeDumpster</title></svelte:head>

<h1>Batches</h1>
<p class="muted">Every ingestion run — manual entry, binder clicks, imports, orders.</p>

{#if loading}
	<p class="muted">Loading…</p>
{:else if error}
	<p class="error">Failed to load batches: {error}</p>
{:else if batches.length === 0}
	<EmptyState
		title="No batches yet."
		description="A batch is written for you every time cards enter the collection — nothing to record until the first ones do."
	>
		{#snippet action()}
			<Button href="/browse">Browse sets</Button>
		{/snippet}
	</EmptyState>
{:else}
	<label class="filter">
		Type
		<select bind:value={typeFilter}>
			<option value="">All</option>
			{#each types as t (t)}<option value={t}>{t}</option>{/each}
		</select>
	</label>
	{#if shown.length === 0}
		<EmptyState
			size="sm"
			title="No {typeFilter} batches."
			description="Every other type is still here — clear the filter to see them."
		>
			{#snippet action()}
				<Button variant="ghost" size="sm" onclick={() => (typeFilter = '')}>Clear filter</Button>
			{/snippet}
		</EmptyState>
	{:else}
		<table>
			<thead>
				<tr><th>Type</th><th>Name</th><th>Cards</th><th>When</th></tr>
			</thead>
			<tbody>
				{#each shown as batch (batch.id)}
					<tr>
						<td><a href="/batches/{batch.id}">{batch.batch_type}</a></td>
						<td>{batch.name ?? '—'}</td>
						<td>{count(batch.card_count)}</td>
						<td>{batch.created_at.slice(0, 16).replace('T', ' ')}</td>
					</tr>
				{/each}
			</tbody>
		</table>
	{/if}
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
	.filter {
		display: inline-flex;
		gap: 0.4rem;
		align-items: center;
		font-size: 0.85rem;
		color: #888;
		margin-bottom: 0.5rem;
	}
	select {
		background: #1a1a2e;
		border: 1px solid #0f3460;
		color: #e0e0e0;
		border-radius: 6px;
		padding: 0.2rem;
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
	a {
		color: #e0e0e0;
	}
	a:hover {
		color: #e94560;
	}
</style>
