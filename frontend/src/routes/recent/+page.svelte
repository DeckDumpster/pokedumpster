<script lang="ts">
	import { onMount } from 'svelte';
	import { api } from '$lib/api';
	import { variantLabel } from '$lib/variants.svelte';
	import { Button, EmptyState } from '$lib/components/ui';
	import type { Batch } from '$lib/types/Batch';
	import type { BatchDetail } from '$lib/types/BatchDetail';

	let batches = $state<Batch[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	// Lazily-loaded card lists, keyed by batch id.
	let expanded = $state(new Set<number>());
	let details = $state(new Map<number, BatchDetail>());

	onMount(async () => {
		try {
			batches = await api.batches(15);
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	});

	async function toggle(id: number) {
		const next = new Set(expanded);
		if (next.has(id)) {
			next.delete(id);
		} else {
			next.add(id);
			if (!details.has(id)) {
				try {
					const d = await api.batchDetail(id);
					details = new Map(details).set(id, d);
				} catch (e) {
					error = e instanceof Error ? e.message : String(e);
				}
			}
		}
		expanded = next;
	}
</script>

<svelte:head><title>Recent — PokeDumpster</title></svelte:head>

<h1>Recent activity</h1>
<p class="muted">Your most recent ingestion batches. Click one to see its cards.</p>

{#if error}<p class="error">{error}</p>{/if}

{#if loading}
	<p class="muted">Loading…</p>
{:else if batches.length === 0}
	<EmptyState
		title="No activity yet."
		description="Nothing has been added to the collection, so there's no history to walk back through. Page through a set's binder and this fills in."
	>
		{#snippet action()}
			<Button href="/browse">Browse sets</Button>
		{/snippet}
	</EmptyState>
{:else}
	<ul class="timeline">
		{#each batches as batch (batch.id)}
			<li>
				<button class="head" onclick={() => toggle(batch.id)}>
					<span class="caret">{expanded.has(batch.id) ? '▾' : '▸'}</span>
					<span class="type">{batch.batch_type}</span>
					<span class="name">{batch.name ?? ''}</span>
					<span class="count">{batch.card_count} cards</span>
					<span class="when">{batch.created_at.slice(0, 16).replace('T', ' ')}</span>
				</button>
				{#if expanded.has(batch.id)}
					{@const d = details.get(batch.id)}
					{#if !d}
						<p class="muted indent">Loading…</p>
					{:else if d.cards.length === 0}
						<EmptyState
							size="sm"
							title="No cards in this batch."
							description="The run was recorded, but nothing came out of it."
						/>
					{:else}
						<ul class="cards">
							{#each d.cards as card (card.id)}
								<li>
									<a href="/card/{card.set_code}/{card.number}">{card.name}</a>
									<span class="cardmeta">
										{card.set_name} · {variantLabel(card.variant)} · {card.status}
									</span>
								</li>
							{/each}
						</ul>
					{/if}
				{/if}
			</li>
		{/each}
	</ul>
{/if}

<style>
	h1 {
		color: var(--color-text-accent);
		margin-bottom: 0.25rem;
	}
	.muted {
		color: var(--color-text-subtle);
	}
	.error {
		color: var(--color-text-accent);
	}
	.indent {
		padding-left: 1.5rem;
	}
	.timeline {
		list-style: none;
		padding: 0;
	}
	.timeline > li {
		border-bottom: 1px solid var(--color-border);
	}
	.head {
		display: flex;
		gap: 0.75rem;
		align-items: baseline;
		width: 100%;
		background: none;
		border: none;
		color: var(--color-text);
		cursor: pointer;
		padding: 0.6rem 0.25rem;
		text-align: left;
		font: inherit;
	}
	.head:hover {
		background: var(--color-surface-accent-wash);
	}
	.caret {
		color: var(--color-text-subtle);
	}
	.type {
		color: var(--color-text-accent);
		font-weight: 600;
	}
	.name {
		flex: 1;
		color: var(--color-text-subtle);
	}
	.count,
	.when {
		color: var(--color-text-subtle);
		font-size: 0.85rem;
	}
	.cards {
		list-style: none;
		padding: 0 0 0.5rem 1.5rem;
		margin: 0;
	}
	.cards li {
		padding: 0.2rem 0;
		font-size: 0.9rem;
	}
	.cardmeta {
		color: var(--color-text-subtle);
		font-size: 0.8rem;
	}
	a {
		color: var(--color-text);
	}
	a:hover {
		color: var(--color-text-accent);
	}
</style>
