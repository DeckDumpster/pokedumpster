<script lang="ts">
	import { onMount } from 'svelte';
	import { api } from '$lib/api';
	import { variantLabel } from '$lib/variants.svelte';
	import type { CollectionRow } from '$lib/types/CollectionRow';

	let {
		target,
		onClose,
		onAssigned
	}: {
		target: { kind: 'binder' | 'deck'; id: number; name: string };
		onClose: () => void;
		onAssigned: () => void;
	} = $props();

	let rows = $state<CollectionRow[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let search = $state('');
	let selected = $state(new Set<number>());
	let busy = $state(false);

	onMount(async () => {
		try {
			rows = await api.collection();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	});

	const filtered = $derived(
		rows.filter((r) => {
			const q = search.trim().toLowerCase();
			return !q || r.name.toLowerCase().includes(q);
		})
	);

	function toggle(id: number) {
		const next = new Set(selected);
		if (next.has(id)) next.delete(id);
		else next.add(id);
		selected = next;
	}

	function whereOf(r: CollectionRow): string {
		if (r.binder_id === target.id && target.kind === 'binder') return 'here';
		if (r.deck_id === target.id && target.kind === 'deck') return 'here';
		if (r.binder_id != null) return 'in a binder';
		if (r.deck_id != null) return 'in a deck';
		return '';
	}

	async function assign() {
		busy = true;
		error = null;
		const body = target.kind === 'binder' ? { binder_id: target.id } : { deck_id: target.id };
		try {
			for (const id of selected) {
				await api.moveCopy(id, body);
			}
			onAssigned();
			onClose();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}
</script>

<svelte:window
	onkeydown={(e) => {
		if (e.key === 'Escape') onClose();
	}}
/>

<div class="backdrop"></div>
<div class="modal" role="dialog" aria-modal="true" aria-label="Add cards to {target.name}">
	<header>
		<h3>Add cards to {target.name}</h3>
		<button class="x" onclick={onClose} aria-label="Close">×</button>
	</header>

	{#if loading}
		<p class="muted">Loading collection…</p>
	{:else if error}
		<p class="error">{error}</p>
	{:else}
		<input class="search" type="text" placeholder="Search your collection…" bind:value={search} />
		<div class="list">
			{#each filtered as row (row.id)}
				<label class="row" class:sel={selected.has(row.id)}>
					<input type="checkbox" checked={selected.has(row.id)} onchange={() => toggle(row.id)} />
					<span class="name">{row.name}</span>
					<span class="meta">{row.set_code} · {variantLabel(row.variant)}</span>
					{#if whereOf(row)}<span class="where">{whereOf(row)}</span>{/if}
				</label>
			{/each}
			{#if filtered.length === 0}<p class="muted">No matching cards.</p>{/if}
		</div>
		<footer>
			<span class="muted">{selected.size} selected</span>
			<button class="primary" disabled={busy || selected.size === 0} onclick={assign}>
				Add {selected.size} to {target.name}
			</button>
		</footer>
	{/if}
</div>

<style>
	.backdrop {
		position: fixed;
		inset: 0;
		background: var(--color-scrim);
		z-index: 100;
	}
	.modal {
		position: fixed;
		top: 50%;
		left: 50%;
		transform: translate(-50%, -50%);
		z-index: 101;
		width: 460px;
		max-width: 92vw;
		max-height: 85vh;
		display: flex;
		flex-direction: column;
		background: var(--color-surface-panel);
		border: 2px solid var(--color-border);
		border-radius: 12px;
		padding: 1.25rem;
	}
	header {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
	}
	h3 {
		margin: 0;
		color: var(--color-text-accent);
	}
	.x {
		background: none;
		border: none;
		color: var(--color-text-subtle);
		font-size: 1.4rem;
		cursor: pointer;
	}
	.muted {
		color: var(--color-text-subtle);
	}
	.error {
		color: var(--color-text-accent);
	}
	.search {
		width: 100%;
		padding: 0.5rem;
		margin: 0.75rem 0;
		background: var(--color-surface-page);
		border: 1px solid var(--color-border);
		border-radius: 6px;
		color: var(--color-text);
	}
	.list {
		flex: 1;
		overflow-y: auto;
		min-height: 120px;
	}
	.row {
		display: flex;
		gap: 0.5rem;
		align-items: center;
		padding: 0.35rem 0.4rem;
		border-bottom: 1px solid var(--color-border);
		cursor: pointer;
	}
	.row.sel {
		background: var(--color-surface-selected);
	}
	.name {
		flex: 1;
	}
	.meta {
		font-size: 0.8rem;
		color: var(--color-text-subtle);
	}
	.where {
		font-size: 0.7rem;
		color: var(--color-text-accent);
	}
	footer {
		display: flex;
		justify-content: space-between;
		align-items: center;
		margin-top: 0.75rem;
	}
	button.primary {
		background: var(--color-accent);
		border: none;
		color: var(--color-on-accent);
		padding: 0.45rem 0.9rem;
		border-radius: 6px;
		cursor: pointer;
	}
	button.primary:disabled {
		opacity: 0.4;
		cursor: default;
	}
</style>
