<script lang="ts">
	import { api } from '$lib/api';
	import type { ResolutionReport } from '$lib/types/ResolutionReport';
	import type { CommitResult } from '$lib/types/CommitResult';

	let format = $state('manabox');
	let content = $state('');
	let fileName = $state<string | null>(null);

	let report = $state<ResolutionReport | null>(null);
	let result = $state<CommitResult | null>(null);
	let busy = $state(false);
	let error = $state<string | null>(null);

	async function onFile(e: Event) {
		const file = (e.target as HTMLInputElement).files?.[0];
		if (!file) return;
		fileName = file.name;
		content = await file.text();
		report = null;
		result = null;
	}

	async function preview() {
		if (!content.trim()) return;
		busy = true;
		error = null;
		result = null;
		try {
			report = await api.importPreview(format, content);
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
			report = null;
		} finally {
			busy = false;
		}
	}

	async function commit() {
		if (!content.trim()) return;
		busy = true;
		error = null;
		try {
			result = await api.importCommit(format, content, fileName ?? undefined);
			report = null;
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}
</script>

<svelte:head><title>Import CSV — PokeDumpster</title></svelte:head>

<h1>Import CSV</h1>
<p class="muted">
	Bring in a collection from ManaBox or a TCGplayer export. Other ways to add cards:
	<a href="/ingest/manual">manual entry</a> · <a href="/ingest/order">paste an order</a>.
</p>

<div class="form">
	<label>
		Format
		<select bind:value={format}>
			<option value="manabox">ManaBox</option>
			<option value="tcgplayer">TCGplayer</option>
			<option value="pokedumpster">PokeDumpster (pkmn.gg export)</option>
		</select>
	</label>

	<label>
		CSV file
		<input type="file" accept=".csv,text/csv" onchange={onFile} />
	</label>

	<label>
		…or paste CSV text
		<textarea
			rows="8"
			placeholder="Set code,Set name,Collector number,Foil,…"
			bind:value={content}
			oninput={() => {
				report = null;
				result = null;
			}}
		></textarea>
	</label>

	<div class="actions">
		<button onclick={preview} disabled={busy || !content.trim()}>Preview</button>
		<button
			class="commit"
			onclick={commit}
			disabled={busy || !report || report.matched.length === 0}
		>
			{report ? `Import ${report.matched.length} cards` : 'Import'}
		</button>
	</div>
</div>

{#if error}<p class="error">{error}</p>{/if}

{#if result}
	<div class="result">
		Imported <strong>{result.added}</strong>
		{result.added === 1 ? 'card' : 'cards'}{result.skipped > 0
			? ` · ${result.skipped} row${result.skipped === 1 ? '' : 's'} skipped`
			: ''}.
		<a href="/batches/{result.batch_id}">View batch →</a>
	</div>
{/if}

{#if report}
	<section class="preview">
		<p class="summary">
			<span class="ok">{report.matched.length} matched</span>
			{#if report.unmatched.length}
				· <span class="miss">{report.unmatched.length} unmatched</span>
			{/if}
		</p>

		{#if report.unmatched.length}
			<h2>Unmatched rows</h2>
			<table>
				<thead>
					<tr><th>Line</th><th>Set</th><th>#</th><th>Variant</th><th>Reason</th></tr>
				</thead>
				<tbody>
					{#each report.unmatched as row (row.source_line + row.set_hint + row.number + row.variant)}
						<tr>
							<td>{row.source_line}</td>
							<td>{row.set_hint}</td>
							<td>{row.number}</td>
							<td>{row.variant}</td>
							<td class="reason">{row.reason}</td>
						</tr>
					{/each}
				</tbody>
			</table>
		{/if}

		{#if report.matched.length}
			<h2>Matched rows</h2>
			<table>
				<thead>
					<tr><th>Line</th><th>Card</th><th>Set</th><th>#</th><th>Variant</th><th>Condition</th></tr>
				</thead>
				<tbody>
					{#each report.matched as row (row.source_line + row.printing_id)}
						<tr>
							<td>{row.source_line}</td>
							<td>{row.card_name}</td>
							<td>{row.set_code}</td>
							<td>{row.number}</td>
							<td>{row.variant}</td>
							<td>{row.condition}</td>
						</tr>
					{/each}
				</tbody>
			</table>
		{/if}
	</section>
{/if}

<style>
	h1 {
		color: #e94560;
	}
	h2 {
		font-size: 0.8rem;
		text-transform: uppercase;
		color: #888;
		margin: 1.2rem 0 0.4rem;
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
	.error {
		color: #e94560;
	}
	.form {
		display: flex;
		flex-direction: column;
		gap: 0.9rem;
		max-width: 640px;
	}
	label {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		font-size: 0.85rem;
		color: #888;
	}
	select,
	textarea {
		background: #1a1a2e;
		border: 1px solid #0f3460;
		border-radius: 6px;
		color: #e0e0e0;
		padding: 0.5rem;
		font: inherit;
	}
	select {
		max-width: 200px;
	}
	textarea {
		resize: vertical;
	}
	.actions {
		display: flex;
		gap: 0.6rem;
	}
	.actions button {
		padding: 0.5rem 1rem;
		background: #0f3460;
		border: none;
		border-radius: 6px;
		color: #e0e0e0;
		cursor: pointer;
	}
	.actions button:hover:not(:disabled) {
		background: #e94560;
	}
	.actions button:disabled {
		opacity: 0.5;
		cursor: default;
	}
	.commit {
		background: #e94560 !important;
	}
	.commit:disabled {
		background: #0f3460 !important;
	}
	.result {
		margin: 1rem 0;
		padding: 0.7rem 1rem;
		background: #16213e;
		border: 1px solid #0f3460;
		border-radius: 8px;
	}
	.result a,
	.result strong {
		color: #e94560;
	}
	.summary {
		font-size: 0.95rem;
	}
	.ok {
		color: #6bd968;
	}
	.miss {
		color: #e94560;
	}
	table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.85rem;
	}
	th {
		text-align: left;
		padding: 0.35rem 0.6rem;
		border-bottom: 2px solid #0f3460;
		color: #888;
		font-size: 0.72rem;
		text-transform: uppercase;
	}
	td {
		padding: 0.35rem 0.6rem;
		border-bottom: 1px solid #0f3460;
	}
	.reason {
		color: #e9a045;
	}
</style>
