<script lang="ts">
	// The import dead-letter queue (pokedumpster-oq3i.5). Open rows grouped by
	// the import that parked them; each row shows its original hint + reason and
	// an inline manual picker (reusing the search endpoints) plus Dismiss.
	import { api } from '$lib/api';
	import MatchPicker from '$lib/components/MatchPicker.svelte';
	import type { UnresolvedRow } from '$lib/types/UnresolvedRow';

	let rows = $state<UnresolvedRow[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let openPicker = $state<number | null>(null);
	let busyId = $state<number | null>(null);
	let flash = $state<string | null>(null);

	async function load() {
		loading = true;
		error = null;
		try {
			rows = await api.unresolvedList();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		load();
	});

	// Group open rows by the import (batch_id) that parked them, preserving the
	// newest-first order the API returns.
	type Group = { batchId: number | null; parkedAt: string; rows: UnresolvedRow[] };
	const groups = $derived.by((): Group[] => {
		const byBatch = new Map<string, Group>();
		for (const r of rows) {
			const key = r.batch_id == null ? 'none' : String(r.batch_id);
			let g = byBatch.get(key);
			if (!g) {
				g = { batchId: r.batch_id, parkedAt: r.parked_at, rows: [] };
				byBatch.set(key, g);
			}
			g.rows.push(r);
		}
		return [...byBatch.values()];
	});

	function queryFor(r: UnresolvedRow): string {
		if (r.kind === 'sealed') return r.name ?? r.set_hint ?? '';
		return r.name ?? r.number ?? r.set_hint ?? '';
	}

	async function resolveSingle(r: UnresolvedRow, printingId: string) {
		busyId = r.id;
		error = null;
		try {
			await api.unresolvedResolveSingle(r.id, printingId);
			flash = `Matched "${r.name ?? r.number ?? 'row'}" — copy added.`;
			openPicker = null;
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busyId = null;
		}
	}

	async function resolveSealed(r: UnresolvedRow, productId: number) {
		busyId = r.id;
		error = null;
		try {
			await api.unresolvedResolveSealed(r.id, productId);
			flash = `Matched "${r.name ?? 'product'}" — sealed added.`;
			openPicker = null;
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busyId = null;
		}
	}

	async function dismiss(r: UnresolvedRow) {
		busyId = r.id;
		error = null;
		try {
			await api.unresolvedDismiss(r.id);
			if (openPicker === r.id) openPicker = null;
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busyId = null;
		}
	}
</script>

<svelte:head><title>Unresolved imports — PokeDumpster</title></svelte:head>

<h1>Unresolved imports</h1>
<p class="muted">
	Import rows that didn't match a catalog item, parked here to resolve later. Match each
	to the right card or product, or dismiss it. Back to <a href="/ingest/csv">Import CSV</a>.
</p>

{#if flash}<p class="flash">{flash}</p>{/if}
{#if error}<p class="error">{error}</p>{/if}

{#if loading}
	<p class="muted">Loading…</p>
{:else if rows.length === 0}
	<p class="empty">Nothing unresolved — the queue is empty. 🎉</p>
{:else}
	{#each groups as g (g.batchId ?? 'none')}
		<section class="group">
			<h2>
				{#if g.batchId != null}
					Import <a href="/batches/{g.batchId}">batch #{g.batchId}</a>
				{:else}
					Manually parked
				{/if}
				<span class="when">· {new Date(g.parkedAt).toLocaleString()}</span>
				<span class="count">{g.rows.length} open</span>
			</h2>
			<ul class="rows">
				{#each g.rows as r (r.id)}
					<li class="row">
						<div class="head">
							<span class="kindtag" class:sealed={r.kind === 'sealed'}>{r.kind}</span>
							<span class="hint">
								<strong>{r.name ?? '(no name)'}</strong>
								{#if r.number}#{r.number}{/if}
								{#if r.set_hint}<span class="dim">· {r.set_hint}</span>{/if}
								{#if r.variant}<span class="dim">· {r.variant}</span>{/if}
								{#if r.quantity != null && r.quantity > 1}<span class="dim">· ×{r.quantity}</span>{/if}
							</span>
							<span class="reason">{r.reason}</span>
							<span class="actions">
								<button
									class="match"
									onclick={() => (openPicker = openPicker === r.id ? null : r.id)}
									disabled={busyId === r.id}
								>
									{openPicker === r.id ? 'Close' : 'Match…'}
								</button>
								<button class="dismiss" onclick={() => dismiss(r)} disabled={busyId === r.id}>
									Dismiss
								</button>
							</span>
						</div>
						{#if openPicker === r.id}
							<MatchPicker
								kind={r.kind === 'sealed' ? 'sealed' : 'single'}
								initialQuery={queryFor(r)}
								busy={busyId === r.id}
								onPickSingle={(pid) => resolveSingle(r, pid)}
								onPickSealed={(prod) => resolveSealed(r, prod)}
								onCancel={() => (openPicker = null)}
							/>
						{/if}
					</li>
				{/each}
			</ul>
		</section>
	{/each}
{/if}

<style>
	h1 {
		color: #e94560;
	}
	.muted {
		color: #888;
	}
	.muted a {
		color: #e0e0e0;
	}
	.muted a:hover {
		color: #e94560;
	}
	.flash {
		color: #6bd968;
	}
	.error {
		color: #e94560;
	}
	.empty {
		color: #8fb7e0;
		background: #16213e;
		border: 1px solid #0f3460;
		border-radius: 8px;
		padding: 1rem;
	}
	.group {
		margin: 1.4rem 0;
	}
	h2 {
		font-size: 0.8rem;
		text-transform: uppercase;
		color: #888;
		border-bottom: 2px solid #0f3460;
		padding-bottom: 0.3rem;
	}
	h2 a {
		color: #e94560;
	}
	.when {
		text-transform: none;
		font-weight: 400;
		color: #6a6a80;
	}
	.count {
		float: right;
		color: #e9a045;
	}
	.rows {
		list-style: none;
		padding: 0;
		margin: 0.5rem 0 0;
	}
	.row {
		border-bottom: 1px solid #0f3460;
		padding: 0.5rem 0;
	}
	.head {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		flex-wrap: wrap;
	}
	.kindtag {
		font-size: 0.66rem;
		text-transform: uppercase;
		padding: 0.05rem 0.4rem;
		border-radius: 4px;
		background: #0f3460;
		color: #8fb7e0;
	}
	.kindtag.sealed {
		background: #3a2f16;
		color: #e9a045;
	}
	.hint {
		flex: 1;
		min-width: 12rem;
	}
	.dim {
		color: #888;
	}
	.reason {
		flex: 2;
		min-width: 14rem;
		color: #e9a045;
		font-size: 0.82rem;
	}
	.actions {
		display: flex;
		gap: 0.4rem;
	}
	.match,
	.dismiss {
		padding: 0.3rem 0.7rem;
		border-radius: 6px;
		border: 1px solid #0f3460;
		background: #16213e;
		color: #e0e0e0;
		cursor: pointer;
	}
	.match:hover:not(:disabled) {
		border-color: #e94560;
		color: #fff;
	}
	.dismiss {
		color: #b8859a;
	}
	.dismiss:hover:not(:disabled) {
		border-color: #e94560;
		background: #5a1e1e;
		color: #ffd9d9;
	}
	.match:disabled,
	.dismiss:disabled {
		opacity: 0.5;
		cursor: default;
	}
</style>
