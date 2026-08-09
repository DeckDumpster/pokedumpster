<script lang="ts">
	import { onMount } from 'svelte';
	import { api } from '$lib/api';
	import { money, count } from '$lib/format';
	import { Button, EmptyState } from '$lib/components/ui';
	import type { Order } from '$lib/types/Order';

	let orders = $state<Order[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	onMount(async () => {
		try {
			orders = await api.orders();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	});

</script>

<svelte:head><title>Orders — PokeDumpster</title></svelte:head>

<header>
	<h1>Orders</h1>
	<a class="btn" href="/ingest/order">+ Import order</a>
</header>

{#if loading}
	<p class="muted">Loading…</p>
{:else if error}
	<p class="error">Failed to load orders: {error}</p>
{:else if orders.length === 0}
	<EmptyState
		title="No orders yet."
		description="An order records what you bought, from whom and for how much, and keeps its cards marked as ordered until they arrive."
	>
		{#snippet action()}
			<Button href="/ingest/order">Import an order</Button>
		{/snippet}
	</EmptyState>
{:else}
	<table>
		<thead>
			<tr><th>Source</th><th>Seller</th><th>Date</th><th>Cards</th><th>Total</th></tr>
		</thead>
		<tbody>
			{#each orders as order (order.id)}
				<tr>
					<td><a href="/orders/{order.id}">{order.source}</a></td>
					<td>{order.seller_name ?? '—'}</td>
					<td>{order.order_date ?? order.created_at.slice(0, 10)}</td>
					<td>{count(order.card_count)}</td>
					<td>{money(order.total)}</td>
				</tr>
			{/each}
		</tbody>
	</table>
{/if}

<style>
	header {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
	}
	h1 {
		color: var(--color-text-accent);
	}
	.muted {
		color: var(--color-text-subtle);
	}
	.error {
		color: var(--color-text-accent);
	}
	.btn {
		background: var(--color-accent);
		color: var(--color-on-accent);
		text-decoration: none;
		padding: 0.4rem 0.8rem;
		border-radius: 6px;
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
