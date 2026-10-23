<script lang="ts">
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { api } from '$lib/api';
	import { count } from '$lib/format';
	import { Button, EmptyState } from '$lib/components/ui';
	import type { DeckGroupDetail } from '$lib/types/DeckGroupDetail';

	let detail = $state<DeckGroupDetail | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let busy = $state(false);
	let renaming = $state(false);
	let newName = $state('');
	let deleting = $state(false);

	async function load() {
		const id = Number(page.params.id);
		if (!id) return;
		try {
			detail = await api.deckGroup(id);
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

	async function act(fn: () => Promise<unknown>) {
		busy = true;
		error = null;
		try {
			await fn();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}

	function startRename() {
		if (!detail) return;
		newName = detail.group.name;
		renaming = true;
	}

	async function rename() {
		if (!detail || !newName.trim()) return;
		await act(async () => {
			await api.updateDeckGroup(detail!.group.id, { name: newName.trim() });
			renaming = false;
			await load();
		});
	}

	async function releaseAll() {
		if (!detail) return;
		if (
			!confirm(
				`Release every deck in "${detail.group.name}"? ${count(detail.group.card_count)} cards go back to your collection; the decks stay.`
			)
		)
			return;
		await act(async () => {
			await api.releaseDeckGroup(detail!.group.id);
			await load();
		});
	}

	async function remove(mode: 'ungroup' | 'decks') {
		if (!detail) return;
		await act(async () => {
			await api.deleteDeckGroup(detail!.group.id, mode);
			goto('/decks');
		});
	}
</script>

<svelte:head><title>{detail ? detail.group.name : 'Deck group'} — PokeDumpster</title></svelte:head>

{#if loading}
	<p class="muted">Loading…</p>
{:else if error && !detail}
	<p class="error">Failed to load deck group: {error}</p>
{:else if detail}
	<header>
		<div>
			{#if renaming}
				<form
					class="rename"
					onsubmit={(e) => {
						e.preventDefault();
						rename();
					}}
				>
					<input type="text" bind:value={newName} aria-label="Group name" />
					<Button type="submit" disabled={busy}>Save</Button>
					<Button variant="ghost" type="button" onclick={() => (renaming = false)}>Cancel</Button>
				</form>
			{:else}
				<h1>{detail.group.name}</h1>
			{/if}
			<p class="sub">
				{#if detail.group.format}{detail.group.format} · {/if}
				{count(detail.group.deck_count)} decks · {count(detail.group.card_count)} cards
			</p>
			{#if detail.group.description}<p class="sub">{detail.group.description}</p>{/if}
		</div>
		<div class="actions">
			<Button variant="ghost" onclick={startRename} disabled={busy}>Rename</Button>
			<Button variant="ghost" onclick={releaseAll} disabled={busy || detail.group.card_count === 0}>
				Release all decks
			</Button>
			<Button variant="danger" onclick={() => (deleting = true)} disabled={busy}>Delete group</Button>
		</div>
	</header>

	{#if error}<p class="error">{error}</p>{/if}

	{#if deleting}
		<div class="confirm" role="alertdialog" aria-label="Delete group">
			<p>
				Delete "{detail.group.name}"? Its {count(detail.group.deck_count)} decks can stay (ungrouped)
				or be deleted with it. Cards always go back to your collection, never deleted.
			</p>
			<div class="actions">
				<Button onclick={() => remove('ungroup')} disabled={busy}>Delete group, keep decks</Button>
				<Button variant="danger" onclick={() => remove('decks')} disabled={busy}>
					Delete group and its decks
				</Button>
				<Button variant="ghost" onclick={() => (deleting = false)}>Cancel</Button>
			</div>
		</div>
	{/if}

	{#if detail.decks.length === 0}
		<EmptyState
			title="No decks in this group."
			description="Open a deck and choose this group from its Group menu to move it in."
		/>
	{:else}
		<div class="grid">
			{#each detail.decks as deck (deck.id)}
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
{/if}

<style>
	header {
		display: flex;
		justify-content: space-between;
		align-items: flex-start;
		flex-wrap: wrap;
		gap: var(--space-4);
		margin-bottom: var(--space-4);
	}
	h1 {
		color: var(--color-text-accent);
		margin: 0;
	}
	.sub {
		color: var(--color-text-subtle);
		font-size: var(--text-md);
		margin: var(--space-1) 0 0;
	}
	.muted {
		color: var(--color-text-subtle);
	}
	.error {
		color: var(--color-text-accent);
	}
	.actions {
		display: flex;
		gap: var(--space-2);
		flex-wrap: wrap;
	}
	.rename {
		display: flex;
		gap: var(--space-2);
		flex-wrap: wrap;
	}
	input {
		padding: var(--space-2);
		background: var(--color-surface-page);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-md);
		color: var(--color-text);
	}
	.confirm {
		background: var(--color-surface-panel);
		border: 2px solid var(--color-border-accent);
		border-radius: var(--radius-lg);
		padding: var(--space-4);
		margin-bottom: var(--space-4);
	}
	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(11rem, 1fr));
		gap: var(--space-4);
	}
	.tile {
		display: block;
		background: var(--color-surface-panel);
		border: 2px solid var(--color-border);
		border-radius: var(--radius-lg);
		padding: var(--space-4);
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
		font-size: var(--text-sm);
		color: var(--color-text-subtle);
		margin: var(--space-1) 0;
	}
	.state {
		text-transform: uppercase;
		font-size: var(--text-xs);
		padding: var(--space-0-5) var(--space-1);
		border-radius: var(--radius-sm);
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
		font-size: var(--text-md);
		color: var(--color-text-subtle);
	}
</style>
