<script lang="ts">
	import { page } from '$app/state';
	import { api } from '$lib/api';
	import { variantLabel } from '$lib/variants.svelte';
	import type { BatchDetail } from '$lib/types/BatchDetail';

	let detail = $state<BatchDetail | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);

	$effect(() => {
		const id = Number(page.params.id);
		if (!id) return;
		loading = true;
		error = null;
		api
			.batchDetail(id)
			.then((d) => (detail = d))
			.catch((e) => (error = e instanceof Error ? e.message : String(e)))
			.finally(() => (loading = false));
	});
</script>

<svelte:head><title>Batch — PokeDumpster</title></svelte:head>

{#if loading}
	<p class="muted">Loading…</p>
{:else if error}
	<p class="error">Failed to load batch: {error}</p>
{:else if detail}
	<h1>{detail.batch.name ?? detail.batch.batch_type}</h1>
	<p class="sub">
		{detail.batch.batch_type} · {detail.batch.created_at.slice(0, 16).replace('T', ' ')} ·
		{detail.cards.length} cards
	</p>
	{#if detail.batch.notes}<p class="notes">{detail.batch.notes}</p>{/if}

	{#if detail.cards.length === 0}
		<p class="muted">No cards in this batch.</p>
	{:else}
		<table>
			<thead>
				<tr><th>Name</th><th>Set</th><th>#</th><th>Variant</th><th>Condition</th><th>Status</th></tr>
			</thead>
			<tbody>
				{#each detail.cards as card (card.id)}
					<tr>
						<td><a href="/card/{card.set_code}/{card.number}">{card.name}</a></td>
						<td><a href="/browse/{card.set_code}">{card.set_name}</a></td>
						<td>{card.number}</td>
						<td>{variantLabel(card.variant)}</td>
						<td>{card.condition}</td>
						<td>{card.status}</td>
					</tr>
				{/each}
			</tbody>
		</table>
	{/if}
{/if}

<style>
	h1 {
		color: var(--color-text-accent);
		margin-bottom: 0.25rem;
	}
	.sub {
		color: var(--color-text-subtle);
		font-size: 0.85rem;
		margin: 0;
	}
	.notes {
		color: var(--color-text-subtle);
		font-style: italic;
	}
	.muted {
		color: var(--color-text-subtle);
	}
	.error {
		color: var(--color-text-accent);
	}
	table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.9rem;
		margin-top: 1rem;
	}
	th {
		text-align: left;
		padding: 0.4rem 0.6rem;
		border-bottom: 2px solid var(--color-border);
		color: var(--color-text-subtle);
		font-size: 0.75rem;
		text-transform: uppercase;
	}
	td {
		padding: 0.4rem 0.6rem;
		border-bottom: 1px solid var(--color-border);
	}
	a {
		color: var(--color-text);
	}
	a:hover {
		color: var(--color-text-accent);
	}
</style>
