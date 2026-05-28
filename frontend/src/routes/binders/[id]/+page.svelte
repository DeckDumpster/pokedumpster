<script lang="ts">
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { api } from '$lib/api';
	import { variantLabel } from '$lib/variants.svelte';
	import CollectionPicker from '$lib/components/CollectionPicker.svelte';
	import type { BinderDetail } from '$lib/types/BinderDetail';

	let detail = $state<BinderDetail | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let picking = $state(false);
	let busy = $state(false);

	async function load() {
		const id = Number(page.params.id);
		if (!id) return;
		try {
			detail = await api.binderDetail(id);
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

	async function remove(copyId: number) {
		busy = true;
		error = null;
		try {
			await api.moveCopy(copyId, {}); // un-assign
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}

	async function removeBinder() {
		if (!detail) return;
		if (!confirm(`Delete binder "${detail.binder.name}"? Its cards stay in your collection.`))
			return;
		try {
			await api.deleteBinder(detail.binder.id);
			goto('/binders');
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
	}
</script>

<svelte:head><title>{detail ? detail.binder.name : 'Binder'} — PokeDumpster</title></svelte:head>

{#if loading}
	<p class="muted">Loading…</p>
{:else if error && !detail}
	<p class="error">Failed to load binder: {error}</p>
{:else if detail}
	<header>
		<h1>{detail.binder.name}</h1>
		<div class="actions">
			<button onclick={() => (picking = true)}>+ Add cards</button>
			<button class="danger" onclick={removeBinder}>Delete binder</button>
		</div>
	</header>

	{#if error}<p class="error">{error}</p>{/if}

	{#if detail.cards.length === 0}
		<p class="muted">No cards in this binder. Add some with “Add cards”.</p>
	{:else}
		<table>
			<thead>
				<tr><th>Name</th><th>Set</th><th>#</th><th>Variant</th><th>Condition</th><th></th></tr>
			</thead>
			<tbody>
				{#each detail.cards as card (card.id)}
					<tr>
						<td><a href="/card/{card.set_code}/{card.number}">{card.name}</a></td>
						<td><a href="/browse/{card.set_code}">{card.set_name}</a></td>
						<td>{card.number}</td>
						<td>{variantLabel(card.variant)}</td>
						<td>{card.condition}</td>
						<td><button class="link" disabled={busy} onclick={() => remove(card.id)}>Remove</button></td>
					</tr>
				{/each}
			</tbody>
		</table>
	{/if}
{/if}

{#if picking && detail}
	<CollectionPicker
		target={{ kind: 'binder', id: detail.binder.id, name: detail.binder.name }}
		onClose={() => (picking = false)}
		onAssigned={load}
	/>
{/if}

<style>
	header {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
		flex-wrap: wrap;
		gap: 1rem;
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
	.actions {
		display: flex;
		gap: 0.5rem;
	}
	button {
		background: #e94560;
		border: none;
		color: #fff;
		padding: 0.4rem 0.8rem;
		border-radius: 6px;
		cursor: pointer;
	}
	button.danger {
		background: #c0392b;
	}
	button.link {
		background: none;
		color: #888;
		padding: 0;
	}
	button.link:hover {
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
