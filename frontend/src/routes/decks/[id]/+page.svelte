<script lang="ts">
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { api } from '$lib/api';
	import { groupDeck } from '$lib/deckLines';
	import type { DeckLine } from '$lib/deckLines';
	import CollectionPicker from '$lib/components/CollectionPicker.svelte';
	import { Button, EmptyState } from '$lib/components/ui';
	import type { DeckDetail } from '$lib/types/DeckDetail';
	import type { DeckGroup } from '$lib/types/DeckGroup';

	let detail = $state<DeckDetail | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let picking = $state(false);
	let busy = $state(false);
	let groups = $state<DeckGroup[]>([]);
	let preview = $state<string | null>(null);
	const typeGroups = $derived(detail ? groupDeck(detail.cards) : []);

	// Touch has no hover: the first tap previews, a second tap on the same line follows the link.
	function touchPreview(e: MouseEvent, l: DeckLine) {
		if (!window.matchMedia('(hover: none)').matches) return;
		if (preview !== l.image) {
			e.preventDefault();
			preview = l.image;
		}
	}

	const states = ['idea', 'ready', 'built'];

	async function load() {
		const id = Number(page.params.id);
		if (!id) return;
		try {
			[detail, groups] = await Promise.all([api.deckDetail(id), api.deckGroups()]);
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

	async function changeGroup(value: string) {
		if (!detail) return;
		busy = true;
		error = null;
		try {
			await api.setDeckGroup(detail.deck.id, value === '' ? null : Number(value));
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
				<label>
					Group
					<select
						value={detail.deck.group_id == null ? '' : String(detail.deck.group_id)}
						disabled={busy}
						onchange={(e) => changeGroup(e.currentTarget.value)}
					>
						<option value="">None</option>
						{#each groups as g (g.id)}<option value={String(g.id)}>{g.name}</option>{/each}
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
		<EmptyState
			title="No cards in this deck."
			description="Pick copies you already own and they're assigned to this list, so the rest of the collection knows they're spoken for."
		>
			{#snippet action()}
				<Button onclick={() => (picking = true)}>Add cards</Button>
			{/snippet}
		</EmptyState>
	{:else}
		<div class="deck">
			{#each typeGroups as g (g.supertype)}
				<section>
					<h2>{g.supertype} <span class="count">{g.count}</span></h2>
					<ul>
						{#each g.lines as l (l.key)}
							<li
								onmouseenter={() => (preview = l.image)}
								onmouseleave={() => (preview = null)}
							>
								<span class="qty">{l.quantity}×</span>
								<a
									href={l.href}
									onclick={(e) => touchPreview(e, l)}
									onfocus={() => (preview = l.image)}
									onblur={() => (preview = null)}>{l.name}</a
								>
								<span class="attrs">
									{#if l.types.length}{l.types.join('/')}{/if}
									{#if l.hp}{l.hp} HP{/if}
								</span>
								<button class="link" disabled={busy} onclick={() => remove(l.copyIds[0])}>Remove</button>
							</li>
						{/each}
					</ul>
				</section>
			{/each}
			{#if preview}
				<img class="preview" src={preview} alt="" />
			{/if}
		</div>
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
	.actions {
		display: flex;
		gap: 0.5rem;
	}
	select {
		background: var(--color-surface-page);
		border: 1px solid var(--color-border);
		color: var(--color-text);
		border-radius: 6px;
		padding: 0.15rem;
	}
	button {
		background: var(--color-accent);
		border: none;
		color: var(--color-on-accent);
		padding: 0.4rem 0.8rem;
		border-radius: 6px;
		cursor: pointer;
	}
	button.danger {
		background: var(--color-danger);
	}
	button.link {
		background: none;
		color: var(--color-text-subtle);
		padding: 0;
	}
	button.link:hover {
		color: var(--color-text-accent);
	}
	.deck {
		margin-top: var(--space-4);
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(min(100%, 18rem), 1fr));
		gap: var(--space-4) var(--space-8);
		align-items: start;
	}
	h2 {
		font-size: var(--text-md);
		margin: 0 0 var(--space-1);
		border-bottom: 2px solid var(--color-border);
		padding-bottom: var(--space-1);
	}
	.count {
		color: var(--color-text-subtle);
		font-weight: normal;
	}
	ul {
		list-style: none;
		margin: 0;
		padding: 0;
	}
	li {
		display: flex;
		gap: var(--space-2);
		align-items: baseline;
		padding: var(--space-1) 0;
		border-bottom: 1px solid var(--color-border);
		min-width: 0;
	}
	li a {
		flex: 1;
		min-width: 0;
		overflow-wrap: anywhere;
	}
	.qty,
	.attrs {
		color: var(--color-text-subtle);
		font-size: var(--text-sm);
	}
	.preview {
		position: fixed;
		top: var(--space-16, 5rem);
		right: var(--space-4);
		width: min(14rem, 45vw);
		border-radius: var(--radius-lg);
		pointer-events: none;
		z-index: 10;
	}
	a {
		color: var(--color-text);
	}
	a:hover {
		color: var(--color-text-accent);
	}
</style>
