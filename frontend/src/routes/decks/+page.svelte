<script lang="ts">
	import { onMount } from 'svelte';
	import { api } from '$lib/api';
	import { count } from '$lib/format';
	import type { Deck } from '$lib/types/Deck';

	let decks = $state<Deck[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let newName = $state('');
	let newOwner = $state('');
	let busy = $state(false);

	async function load() {
		try {
			decks = await api.decks();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	}
	onMount(load);

	async function create() {
		if (!newName.trim()) return;
		busy = true;
		error = null;
		try {
			await api.createDeck({
				name: newName.trim(),
				owner: newOwner.trim() || undefined
			});
			newName = '';
			newOwner = '';
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}
</script>

<svelte:head><title>Decks — PokeDumpster</title></svelte:head>

<h1>Decks</h1>

<form
	class="newform"
	onsubmit={(e) => {
		e.preventDefault();
		create();
	}}
>
	<input type="text" placeholder="New deck name…" bind:value={newName} />
	<input type="text" placeholder="Owner (optional)" bind:value={newOwner} />
	<button type="submit" disabled={busy}>Create</button>
</form>

{#if error}<p class="error">{error}</p>{/if}

{#if loading}
	<p class="muted">Loading…</p>
{:else if decks.length === 0}
	<p class="muted">No decks yet. Create one above.</p>
{:else}
	<div class="grid">
		{#each decks as deck (deck.id)}
			<a class="tile" href="/decks/{deck.id}">
				<div class="name">{deck.name}</div>
				<div class="meta">
					<span class="state state-{deck.state}">{deck.state}</span>
					{#if deck.owner}· {deck.owner}{/if}
				</div>
				<div class="count">{count(deck.card_count)} cards</div>
			</a>
		{/each}
	</div>
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
	.newform {
		display: flex;
		gap: 0.5rem;
		margin: 1rem 0;
		flex-wrap: wrap;
	}
	input {
		padding: 0.5rem;
		background: #1a1a2e;
		border: 1px solid #0f3460;
		border-radius: 6px;
		color: #e0e0e0;
	}
	button {
		background: #e94560;
		border: none;
		color: #fff;
		padding: 0.5rem 1rem;
		border-radius: 6px;
		cursor: pointer;
	}
	button:disabled {
		opacity: 0.5;
	}
	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(180px, 1fr));
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
	}
	.tile:hover {
		border-color: #e94560;
	}
	.name {
		font-weight: 700;
		color: #e94560;
	}
	.meta {
		font-size: 0.8rem;
		color: #888;
		margin: 0.3rem 0;
	}
	.state {
		text-transform: uppercase;
		font-size: 0.7rem;
		padding: 0.1rem 0.4rem;
		border-radius: 4px;
		background: #0f3460;
	}
	.state-built {
		background: #2d6a4f;
		color: #d8f3dc;
	}
	.state-ready {
		background: #5a4a14;
		color: #f0e4b8;
	}
	.count {
		font-size: 0.85rem;
		color: #888;
	}
</style>
