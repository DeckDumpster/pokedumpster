<script lang="ts">
	import { page } from '$app/state';
	import { api } from '$lib/api';
	import { variantLabel } from '$lib/variants.svelte';
	import { money } from '$lib/format';
	import type { OrderDetail } from '$lib/types/OrderDetail';

	let detail = $state<OrderDetail | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let busy = $state(false);

	async function load() {
		const id = Number(page.params.id);
		if (!id) return;
		try {
			detail = await api.orderDetail(id);
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void page.params.id;
		load();
	});

	const pendingCount = $derived(detail ? detail.cards.filter((c) => c.status === 'ordered').length : 0);

	async function receive() {
		if (!detail) return;
		busy = true;
		error = null;
		try {
			await api.receiveOrder(detail.order.id);
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}

</script>

<svelte:head><title>Order — PokeDumpster</title></svelte:head>

{#if loading}
	<p class="muted">Loading…</p>
{:else if error && !detail}
	<p class="error">Failed to load order: {error}</p>
{:else if detail}
	{@const o = detail.order}
	<header>
		<div>
			<h1>{o.source}{#if o.seller_name} · {o.seller_name}{/if}</h1>
			<p class="sub">
				{#if o.order_number}#{o.order_number} · {/if}
				{o.order_date ?? o.created_at.slice(0, 10)} · {detail.cards.length} cards · {money(o.total)}
			</p>
		</div>
		{#if pendingCount > 0}
			<button disabled={busy} onclick={receive}>Receive {pendingCount} card(s)</button>
		{:else}
			<span class="received">All received</span>
		{/if}
	</header>

	{#if error}<p class="error">{error}</p>{/if}

	<table>
		<thead>
			<tr><th>Name</th><th>Set</th><th>#</th><th>Variant</th><th>Paid</th><th>Status</th></tr>
		</thead>
		<tbody>
			{#each detail.cards as card (card.id)}
				<tr>
					<td><a href="/card/{card.set_code}/{card.number}">{card.name}</a></td>
					<td><a href="/browse/{card.set_code}">{card.set_name}</a></td>
					<td>{card.number}</td>
					<td>{variantLabel(card.variant)}</td>
					<td>{money(card.purchase_price)}</td>
					<td>{card.status}</td>
				</tr>
			{/each}
		</tbody>
	</table>
{/if}

<style>
	header {
		display: flex;
		justify-content: space-between;
		align-items: flex-start;
		flex-wrap: wrap;
		gap: 1rem;
	}
	h1 {
		color: var(--color-text-accent);
		margin: 0;
	}
	.sub {
		color: var(--color-text-subtle);
		font-size: 0.85rem;
		margin: 0.25rem 0 0;
	}
	.muted {
		color: var(--color-text-subtle);
	}
	.error {
		color: var(--color-text-accent);
	}
	.received {
		color: var(--color-success-text);
		font-size: 0.9rem;
	}
	button {
		background: var(--color-accent);
		border: none;
		color: var(--color-on-accent);
		padding: 0.4rem 0.8rem;
		border-radius: 6px;
		cursor: pointer;
	}
	button:disabled {
		opacity: 0.5;
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
