<script lang="ts">
	import { onMount } from 'svelte';
	import { api } from '$lib/api';
	import type { Binder } from '$lib/types/Binder';

	let binders = $state<Binder[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let newName = $state('');
	let busy = $state(false);

	async function load() {
		try {
			binders = await api.binders();
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
			await api.createBinder({ name: newName.trim() });
			newName = '';
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}
</script>

<svelte:head><title>Binders — PokeDumpster</title></svelte:head>

<h1>Binders</h1>

<form
	class="newform"
	onsubmit={(e) => {
		e.preventDefault();
		create();
	}}
>
	<input type="text" placeholder="New binder name…" bind:value={newName} />
	<button type="submit" disabled={busy}>Create</button>
</form>

{#if error}<p class="error">{error}</p>{/if}

{#if loading}
	<p class="muted">Loading…</p>
{:else if binders.length === 0}
	<p class="muted">No binders yet. Create one above.</p>
{:else}
	<div class="grid">
		{#each binders as binder (binder.id)}
			<a class="tile" href="/binders/{binder.id}">
				<div class="name">{binder.name}</div>
				<div class="count">{binder.card_count} cards</div>
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
		grid-template-columns: repeat(auto-fill, minmax(170px, 1fr));
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
	.count {
		font-size: 0.85rem;
		color: #888;
		margin-top: 0.3rem;
	}
</style>
