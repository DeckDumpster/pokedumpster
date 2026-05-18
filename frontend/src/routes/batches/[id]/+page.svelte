<script lang="ts">
	import { page } from '$app/state';
	import { api, variantLabel } from '$lib/api';
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
						<td>{card.set_code}</td>
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
		color: #e94560;
		margin-bottom: 0.25rem;
	}
	.sub {
		color: #888;
		font-size: 0.85rem;
		margin: 0;
	}
	.notes {
		color: #aaa;
		font-style: italic;
	}
	.muted {
		color: #888;
	}
	.error {
		color: #e94560;
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
