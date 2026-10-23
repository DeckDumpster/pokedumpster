<script lang="ts">
	import { onMount } from 'svelte';
	import { api } from '$lib/api';
	import { count } from '$lib/format';
	import { EmptyState } from '$lib/components/ui';
	import type { Deck } from '$lib/types/Deck';
	import type { DeckGroup } from '$lib/types/DeckGroup';

	let decks = $state<Deck[]>([]);
	let groups = $state<DeckGroup[]>([]);
	let newGroup = $state('');
	let loading = $state(true);
	let error = $state<string | null>(null);
	let newName = $state('');
	let newOwner = $state('');
	let busy = $state(false);

	async function load() {
		try {
			[decks, groups] = await Promise.all([api.decks(), api.deckGroups()]);
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

	async function createGroup() {
		if (!newGroup.trim()) return;
		busy = true;
		error = null;
		try {
			await api.createDeckGroup({ name: newGroup.trim() });
			newGroup = '';
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}

	const ungrouped = $derived(decks.filter((d) => d.group_id == null));
	const inGroup = (id: number) => decks.filter((d) => d.group_id === id);
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

<form
	class="groupform"
	onsubmit={(e) => {
		e.preventDefault();
		createGroup();
	}}
>
	<input type="text" placeholder="New deck group name…" bind:value={newGroup} />
	<button type="submit" disabled={busy}>Create group</button>
</form>

{#if error}<p class="error">{error}</p>{/if}

{#if loading}
	<p class="muted">Loading…</p>
{:else if decks.length === 0 && groups.length === 0}
	<EmptyState
		title="No decks yet."
		description="A deck holds the copies you've committed to a list, so they stop counting as loose collection. Name one above to create your first."
	/>
{:else}
	{#each groups as group (group.id)}
		<details class="group" open>
			<summary>
				<a class="groupname" href="/deck-groups/{group.id}">{group.name}</a>
				<span class="groupmeta">
					{#if group.format}{group.format} ·{/if}
					{count(group.deck_count)} decks · {count(group.card_count)} cards
				</span>
			</summary>
			{#if inGroup(group.id).length === 0}
				<p class="muted">No decks in this group yet.</p>
			{:else}
				<div class="grid">
					{#each inGroup(group.id) as deck (deck.id)}
						{@render tile(deck)}
					{/each}
				</div>
			{/if}
		</details>
	{/each}
	{#if groups.length > 0 && ungrouped.length > 0}<h2 class="ungrouped">Ungrouped</h2>{/if}
	<div class="grid">
		{#each ungrouped as deck (deck.id)}
			{@render tile(deck)}
		{/each}
	</div>
{/if}

{#snippet tile(deck: Deck)}
	<a class="tile" href="/decks/{deck.id}">
		<div class="name">{deck.name}</div>
		<div class="meta">
			<span class="state state-{deck.state}">{deck.state}</span>
			{#if deck.owner}· {deck.owner}{/if}
		</div>
		<div class="count">{count(deck.card_count)} cards</div>
	</a>
{/snippet}

<style>
	h1 {
		color: var(--color-text-accent);
	}
	.muted {
		color: var(--color-text-subtle);
	}
	.error {
		color: var(--color-text-accent);
	}
	.newform {
		display: flex;
		gap: 0.5rem;
		margin: 1rem 0;
		flex-wrap: wrap;
	}
	input {
		padding: 0.5rem;
		background: var(--color-surface-page);
		border: 1px solid var(--color-border);
		border-radius: 6px;
		color: var(--color-text);
	}
	button {
		background: var(--color-accent);
		border: none;
		color: var(--color-on-accent);
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
		background: var(--color-surface-panel);
		border: 2px solid var(--color-border);
		border-radius: 10px;
		padding: 1rem;
		text-decoration: none;
		color: var(--color-text);
	}
	.tile:hover {
		border-color: var(--color-border-accent);
	}
	.name {
		font-weight: 700;
		color: var(--color-text-accent);
	}
	.meta {
		font-size: 0.8rem;
		color: var(--color-text-subtle);
		margin: 0.3rem 0;
	}
	.state {
		text-transform: uppercase;
		font-size: 0.7rem;
		padding: 0.1rem 0.4rem;
		border-radius: 4px;
		background: var(--color-info-surface);
	}
	.state-built {
		background: var(--color-success-surface);
		color: var(--color-success-text);
	}
	.state-ready {
		background: var(--color-warning-surface);
		color: var(--color-warning-text);
	}
	.count {
		font-size: 0.85rem;
		color: var(--color-text-subtle);
	}
	.groupform {
		display: flex;
		gap: var(--space-2);
		margin-bottom: var(--space-4);
		flex-wrap: wrap;
	}
	.group {
		margin-bottom: var(--space-4);
	}
	.group summary {
		display: flex;
		gap: var(--space-3);
		align-items: baseline;
		flex-wrap: wrap;
		cursor: pointer;
		margin-bottom: var(--space-2);
	}
	.groupname {
		font-weight: 700;
		font-size: var(--text-xl);
		color: var(--color-text-accent);
	}
	.groupmeta {
		color: var(--color-text-subtle);
		font-size: var(--text-md);
	}
	.ungrouped {
		color: var(--color-text-subtle);
		font-size: var(--text-lg);
	}
</style>
