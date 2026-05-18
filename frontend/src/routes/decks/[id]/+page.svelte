<script lang="ts">
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { api, variantLabel } from '$lib/api';
	import CollectionPicker from '$lib/components/CollectionPicker.svelte';
	import type { DeckDetail } from '$lib/types/DeckDetail';

	let detail = $state<DeckDetail | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let picking = $state(false);
	let busy = $state(false);

	const states = ['idea', 'ready', 'built'];

	async function load() {
		const id = Number(page.params.id);
		if (!id) return;
		try {
			detail = await api.deckDetail(id);
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

	async function changeState(state: string) {
		if (!detail) return;
		busy = true;
		error = null;
		try {
			await api.updateDeck(detail.deck.id, { state });
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}

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

	async function removeDeck() {
		if (!detail) return;
		if (!confirm(`Delete deck "${detail.deck.name}"? Its cards stay in your collection.`)) return;
		try {
			await api.deleteDeck(detail.deck.id);
			goto('/decks');
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
	}
</script>

<svelte:head><title>{detail ? detail.deck.name : 'Deck'} — PokeDumpster</title></svelte:head>

{#if loading}
	<p class="muted">Loading…</p>
{:else if error && !detail}
	<p class="error">Failed to load deck: {error}</p>
{:else if detail}
	<header>
		<div>
			<h1>{detail.deck.name}</h1>
			<p class="sub">
				{#if detail.deck.owner}{detail.deck.owner} · {/if}
				{#if detail.deck.format}{detail.deck.format} · {/if}
				<label>
					Lifecycle
					<select
						value={detail.deck.state}
						disabled={busy}
						onchange={(e) => changeState(e.currentTarget.value)}
					>
						{#each states as s (s)}<option value={s}>{s}</option>{/each}
					</select>
				</label>
			</p>
		</div>
		<div class="actions">
			<button onclick={() => (picking = true)}>+ Add cards</button>
			<button class="danger" onclick={removeDeck}>Delete deck</button>
		</div>
	</header>

	{#if error}<p class="error">{error}</p>{/if}

	{#if detail.cards.length === 0}
		<p class="muted">No cards in this deck. Add some with “Add cards”.</p>
	{:else}
		<table>
			<thead>
				<tr><th>Name</th><th>Set</th><th>#</th><th>Variant</th><th>Condition</th><th></th></tr>
			</thead>
			<tbody>
				{#each detail.cards as card (card.id)}
					<tr>
						<td><a href="/card/{card.set_code}/{card.number}">{card.name}</a></td>
						<td>{card.set_code}</td>
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
		target={{ kind: 'deck', id: detail.deck.id, name: detail.deck.name }}
		onClose={() => (picking = false)}
		onAssigned={load}
	/>
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
		color: #e94560;
		margin: 0;
	}
	.sub {
		color: #888;
		font-size: 0.85rem;
		margin: 0.25rem 0 0;
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
	select {
		background: #1a1a2e;
		border: 1px solid #0f3460;
		color: #e0e0e0;
		border-radius: 6px;
		padding: 0.15rem;
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
