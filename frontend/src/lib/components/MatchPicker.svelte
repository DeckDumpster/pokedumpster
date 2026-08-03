<script lang="ts">
	// Inline manual-match picker for an unresolved import row (pokedumpster-oq3i.5).
	// Reuses the existing search endpoints — no new search code:
	//   singles → /api/collection/search?include_unowned=1  (SearchRow per printing)
	//   sealed  → /api/sealed/products                       (SealedProduct)
	// Shared by the CSV preview's Unmatched tables and the /ingest/unresolved page.
	import { untrack } from 'svelte';
	import { api } from '$lib/api';
	import { variantTag, variantColor, variantLabel } from '$lib/variants.svelte';
	import { EmptyState } from '$lib/components/ui';
	import type { SearchRow } from '$lib/types/SearchRow';
	import type { SealedProduct } from '$lib/types/SealedProduct';

	let {
		kind,
		initialQuery = '',
		busy = false,
		onPickSingle,
		onPickSealed,
		onCancel
	}: {
		kind: 'single' | 'sealed';
		initialQuery?: string;
		busy?: boolean;
		onPickSingle?: (printingId: string) => void;
		onPickSealed?: (productId: number) => void;
		onCancel?: () => void;
	} = $props();

	// Each unresolved row mounts its own keyed MatchPicker, so capturing the
	// pre-filled query once at construction is exactly the intent.
	let q = $state(untrack(() => initialQuery));
	let singles = $state<SearchRow[]>([]);
	let sealed = $state<SealedProduct[]>([]);
	let loading = $state(false);
	let error = $state<string | null>(null);
	let timer: ReturnType<typeof setTimeout> | null = null;

	async function run() {
		const query = q.trim();
		loading = true;
		error = null;
		try {
			if (kind === 'single') {
				singles = query ? await api.collectionSearch(query, undefined, undefined, true) : [];
			} else {
				sealed = query ? await api.sealedProducts(query) : [];
			}
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	}

	// Debounce keystrokes; the first run fires from the pre-filled query.
	function onInput() {
		if (timer) clearTimeout(timer);
		timer = setTimeout(run, 250);
	}

	$effect(() => {
		run();
		return () => {
			if (timer) clearTimeout(timer);
		};
	});
</script>

<div class="picker">
	<div class="bar">
		<input
			type="text"
			placeholder={kind === 'single' ? 'Search cards…' : 'Search sealed products…'}
			bind:value={q}
			oninput={onInput}
			disabled={busy}
		/>
		{#if onCancel}
			<button class="cancel" onclick={onCancel} disabled={busy}>Cancel</button>
		{/if}
	</div>

	{#if error}
		<p class="error">{error}</p>
	{:else if loading}
		<p class="muted">Searching…</p>
	{:else if kind === 'single'}
		{#if singles.length === 0}
			<EmptyState
				size="sm"
				title={q.trim() ? 'No matching printings.' : 'Type to search.'}
				description={q.trim()
					? 'Search the card name; add its number to narrow a reprint (“Pikachu 25”).'
					: 'The whole catalog is searchable here, not just what you own.'}
			/>
		{:else}
			<ul class="results">
				{#each singles as row (row.printing_id)}
					<li>
						<button class="pick" onclick={() => onPickSingle?.(row.printing_id)} disabled={busy}>
							<span class="name">{row.name}</span>
							<span
								class="chip"
								style="border-color: {variantColor(row.variant)}; color: {variantColor(
									row.variant
								)}"
								title={variantLabel(row.variant)}
							>
								{variantTag(row.variant)}
							</span>
							<span class="meta">
								{row.set_ptcgo_code ?? row.set_code} · #{row.number}
								{#if row.owned}· owned ×{row.owned_count}{/if}
							</span>
						</button>
					</li>
				{/each}
			</ul>
		{/if}
	{:else if sealed.length === 0}
		<EmptyState
			size="sm"
			title={q.trim() ? 'No matching products.' : 'Type to search.'}
			description={q.trim()
				? 'Search the product name as TCGplayer lists it — “Obsidian Flames Booster Box”.'
				: 'Every sealed product in the catalog is searchable here.'}
		/>
	{:else}
		<ul class="results">
			{#each sealed as p (p.product_id)}
				<li>
					<button class="pick" onclick={() => onPickSealed?.(p.product_id)} disabled={busy}>
						<span class="name">{p.name}</span>
						<span class="meta">{p.set_code ?? '—'} · {p.category}</span>
					</button>
				</li>
			{/each}
		</ul>
	{/if}
</div>

<style>
	.picker {
		background: var(--color-surface-sunken);
		border: 1px solid var(--color-border);
		border-radius: 8px;
		padding: 0.6rem;
		margin: 0.3rem 0;
	}
	.bar {
		display: flex;
		gap: 0.5rem;
	}
	.bar input {
		flex: 1;
		background: var(--color-surface-page);
		border: 1px solid var(--color-border);
		border-radius: 6px;
		color: var(--color-text);
		padding: 0.4rem 0.5rem;
		font: inherit;
	}
	.cancel {
		background: var(--color-surface-panel);
		border: 1px solid var(--color-border);
		border-radius: 6px;
		color: var(--color-text-muted);
		padding: 0.3rem 0.7rem;
		cursor: pointer;
	}
	.cancel:hover:not(:disabled) {
		border-color: var(--color-border-accent);
	}
	.muted {
		color: var(--color-text-subtle);
		margin: 0.5rem 0 0.2rem;
		font-size: 0.85rem;
	}
	.error {
		color: var(--color-text-accent);
		margin: 0.5rem 0 0.2rem;
		font-size: 0.85rem;
	}
	.results {
		list-style: none;
		margin: 0.4rem 0 0;
		padding: 0;
		max-height: 260px;
		overflow-y: auto;
	}
	.pick {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		width: 100%;
		text-align: left;
		background: none;
		border: none;
		border-bottom: 1px solid var(--color-border);
		color: var(--color-text);
		padding: 0.4rem 0.3rem;
		cursor: pointer;
		font: inherit;
	}
	.pick:hover:not(:disabled) {
		background: var(--color-surface-selected);
	}
	.pick:disabled {
		opacity: 0.5;
		cursor: default;
	}
	.name {
		flex: 1;
		min-width: 0;
	}
	.chip {
		flex-shrink: 0;
		font-size: 0.66rem;
		font-weight: 600;
		padding: 0.02rem 0.35rem;
		border: 1px solid;
		border-radius: 999px;
		background: var(--color-surface-shade);
	}
	.meta {
		flex-shrink: 0;
		font-size: 0.78rem;
		color: var(--color-text-subtle);
	}
</style>
