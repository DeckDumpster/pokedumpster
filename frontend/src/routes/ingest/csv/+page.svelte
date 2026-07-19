<script lang="ts">
	import { SvelteSet } from 'svelte/reactivity';
	import { api } from '$lib/api';
	import type { ResolutionReport } from '$lib/types/ResolutionReport';
	import type { CommitResult } from '$lib/types/CommitResult';
	import type { CombinedReport } from '$lib/types/CombinedReport';
	import type { CombinedCommitResult } from '$lib/types/CombinedCommitResult';

	let format = $state('manabox');
	let content = $state('');
	let fileName = $state<string | null>(null);

	// Single-card formats populate `report`; Collectr populates `combined`
	// (singles + sealed kept apart — the garden wall).
	let report = $state<ResolutionReport | null>(null);
	let combined = $state<CombinedReport | null>(null);
	let result = $state<CommitResult | null>(null);
	let combinedResult = $state<CombinedCommitResult | null>(null);
	let busy = $state(false);
	let error = $state<string | null>(null);

	const isCollectr = $derived(format === 'collectr');

	// --- Per-row include/exclude (pokedumpster-oq3i.4) -----------------------
	// Selection is keyed by CSV `source_line`: a single line with quantity > 1
	// expands to several matched rows, and selecting/deselecting the line acts
	// on all of them together (they're one physical CSV row).
	const selSingles = new SvelteSet<number>();
	const selSealed = new SvelteSet<number>();

	const singlesMatched = $derived(
		(isCollectr ? combined?.singles.matched : report?.matched) ?? []
	);
	const sealedMatched = $derived(combined?.sealed.matched ?? []);

	// Committed-copy counts (row-level, not line-level).
	const selSinglesCount = $derived(
		singlesMatched.filter((r) => selSingles.has(r.source_line)).length
	);
	const selSealedCount = $derived(
		sealedMatched.filter((r) => selSealed.has(r.source_line)).length
	);
	const totalSelected = $derived(selSinglesCount + selSealedCount);

	// How many currently-selected matched rows duplicate something you own —
	// drives the "Deselect already-owned" convenience button.
	const ownedSinglesSelected = $derived(
		singlesMatched.filter((r) => r.already_owned > 0 && selSingles.has(r.source_line)).length
	);
	const ownedSealedSelected = $derived(
		sealedMatched.filter((r) => r.already_owned > 0 && selSealed.has(r.source_line)).length
	);

	type Keyed = { source_line: number };
	type Owned = Keyed & { already_owned: number };

	function toggle(set: SvelteSet<number>, line: number) {
		if (set.has(line)) set.delete(line);
		else set.add(line);
	}
	function setAll(set: SvelteSet<number>, rows: Keyed[], on: boolean) {
		if (on) for (const r of rows) set.add(r.source_line);
		else set.clear();
	}
	function deselectOwned(set: SvelteSet<number>, rows: Owned[]) {
		for (const r of rows) if (r.already_owned > 0) set.delete(r.source_line);
	}

	// Default: every matched row selected (both panes).
	function initSelection() {
		selSingles.clear();
		selSealed.clear();
		const sm = isCollectr ? combined?.singles.matched : report?.matched;
		for (const r of sm ?? []) selSingles.add(r.source_line);
		for (const r of combined?.sealed.matched ?? []) selSealed.add(r.source_line);
	}

	function clearResults() {
		report = null;
		combined = null;
		result = null;
		combinedResult = null;
		selSingles.clear();
		selSealed.clear();
	}

	async function onFile(e: Event) {
		const file = (e.target as HTMLInputElement).files?.[0];
		if (!file) return;
		fileName = file.name;
		content = await file.text();
		clearResults();
	}

	async function preview() {
		if (!content.trim()) return;
		busy = true;
		error = null;
		clearResults();
		try {
			if (isCollectr) {
				combined = await api.importCollectrPreview(content);
			} else {
				report = await api.importPreview(format, content);
			}
			initSelection();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
			clearResults();
		} finally {
			busy = false;
		}
	}

	async function commit() {
		if (!content.trim() || totalSelected === 0) return;
		busy = true;
		error = null;
		try {
			if (isCollectr) {
				combinedResult = await api.importCollectrCommitSelected(
					content,
					[...selSingles],
					[...selSealed],
					fileName ?? undefined
				);
			} else {
				result = await api.importCommitSelected(
					format,
					content,
					[...selSingles],
					fileName ?? undefined
				);
			}
			report = null;
			combined = null;
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
	Bring in a collection from ManaBox, a TCGplayer export, or Collectr. Other ways to add cards:
	<a href="/ingest/manual">manual entry</a> · <a href="/ingest/order">paste an order</a>.
</p>

<div class="form">
	<label>
		Format
		<select bind:value={format} onchange={clearResults}>
			<option value="manabox">ManaBox</option>
			<option value="tcgplayer">TCGplayer</option>
			<option value="pokedumpster">PokeDumpster (pkmn.gg export)</option>
			<option value="collectr">Collectr (cards + sealed)</option>
		</select>
	</label>

	{#if isCollectr}
		<p class="hint">
			A Collectr export mixes single cards and sealed products. They're imported
			separately — cards into your collection, sealed into your sealed shelf — and
			non-Pokémon rows are skipped.
		</p>
	{/if}

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
			oninput={clearResults}
		></textarea>
	</label>

	<div class="actions">
		<button onclick={preview} disabled={busy || !content.trim()}>Preview</button>
		<button class="commit" onclick={commit} disabled={busy || totalSelected === 0}>
			{totalSelected > 0
				? `Import ${totalSelected} ${totalSelected === 1 ? 'item' : 'items'}`
				: 'Import'}
		</button>
	</div>
</div>

{#if error}<p class="error">{error}</p>{/if}

{#if result}
	<div class="result">
		Imported <strong>{result.added}</strong>
		{result.added === 1 ? 'card' : 'cards'}.
		<a href="/batches/{result.batch_id}">View batch →</a>
	</div>
{/if}

{#if combinedResult}
	<div class="result">
		Imported <strong>{combinedResult.singles.added}</strong>
		{combinedResult.singles.added === 1 ? 'card' : 'cards'}
		and <strong>{combinedResult.sealed.added}</strong>
		sealed {combinedResult.sealed.added === 1 ? 'product' : 'products'}.
		<a href="/batches/{combinedResult.singles.batch_id}">View card batch →</a>
	</div>
{/if}

{#if report}
	<section class="preview">
		<p class="summary">
			<span class="ok">{report.matched.length} matched</span>
			{#if report.unmatched.length}
				· <span class="miss">{report.unmatched.length} unmatched</span>
			{/if}
			· <span class="sel">{selSinglesCount} selected</span>
		</p>

		{#if report.matched.length}
			<h2>Matched rows</h2>
			<div class="seltools">
				<button onclick={() => setAll(selSingles, singlesMatched, true)}>Select all</button>
				<button onclick={() => setAll(selSingles, singlesMatched, false)}>Select none</button>
				{#if ownedSinglesSelected}
					<button class="warn" onclick={() => deselectOwned(selSingles, singlesMatched)}>
						Deselect {ownedSinglesSelected} already-owned
					</button>
				{/if}
			</div>
			<table>
				<thead>
					<tr>
						<th class="chk"></th>
						<th>Line</th><th>Card</th><th>Set</th><th>#</th><th>Variant</th><th>Condition</th>
					</tr>
				</thead>
				<tbody>
					{#each report.matched as row, i (i)}
						<tr class:off={!selSingles.has(row.source_line)}>
							<td class="chk">
								<input
									type="checkbox"
									checked={selSingles.has(row.source_line)}
									onchange={() => toggle(selSingles, row.source_line)}
								/>
							</td>
							<td>{row.source_line}</td>
							<td>
								{row.card_name}
								{#if row.already_owned > 0}
									<span class="badge" title="You already own {row.already_owned} of this printing">
										owned ×{row.already_owned}
									</span>
								{/if}
							</td>
							<td>{row.set_code}</td>
							<td>{row.number}</td>
							<td>{row.variant}</td>
							<td>{row.condition}</td>
						</tr>
					{/each}
				</tbody>
			</table>
		{/if}

		{#if report.unmatched.length}
			<h2>Unmatched rows</h2>
			<p class="subhint">Can't be imported until matched — coming with conflict resolution.</p>
			<table>
				<thead>
					<tr><th class="chk"></th><th>Line</th><th>Set</th><th>#</th><th>Variant</th><th>Reason</th></tr>
				</thead>
				<tbody>
					{#each report.unmatched as row, i (i)}
						<tr class="off">
							<td class="chk"><input type="checkbox" disabled title="Unresolved — can't import" /></td>
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
	</section>
{/if}

{#if combined}
	<section class="preview">
		<!-- Cards and sealed are shown as separate panes — never merged. -->
		<div class="pane">
			<h2 class="pane-head">
				Single cards
				<span class="counts">
					<span class="ok">{combined.singles.matched.length} matched</span>
					{#if combined.singles.unmatched.length}
						· <span class="miss">{combined.singles.unmatched.length} unmatched</span>
					{/if}
					· <span class="sel">{selSinglesCount} selected</span>
				</span>
			</h2>
			{#if combined.singles.matched.length}
				<div class="seltools">
					<button onclick={() => setAll(selSingles, singlesMatched, true)}>Select all</button>
					<button onclick={() => setAll(selSingles, singlesMatched, false)}>Select none</button>
					{#if ownedSinglesSelected}
						<button class="warn" onclick={() => deselectOwned(selSingles, singlesMatched)}>
							Deselect {ownedSinglesSelected} already-owned
						</button>
					{/if}
				</div>
				<table>
					<thead>
						<tr>
							<th class="chk"></th>
							<th>Line</th><th>Card</th><th>Set</th><th>#</th><th>Variant</th><th>Cond.</th>
						</tr>
					</thead>
					<tbody>
						{#each combined.singles.matched as row, i (i)}
							<tr class:off={!selSingles.has(row.source_line)}>
								<td class="chk">
									<input
										type="checkbox"
										checked={selSingles.has(row.source_line)}
										onchange={() => toggle(selSingles, row.source_line)}
									/>
								</td>
								<td>{row.source_line}</td>
								<td>
									{row.card_name}
									{#if row.already_owned > 0}
										<span class="badge" title="You already own {row.already_owned} of this printing">
											owned ×{row.already_owned}
										</span>
									{/if}
								</td>
								<td>{row.set_code}</td>
								<td>{row.number}</td>
								<td>{row.variant}</td>
								<td>{row.condition}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			{/if}
			{#if combined.singles.unmatched.length}
				<h3>Unmatched cards</h3>
				<table>
					<thead>
						<tr><th class="chk"></th><th>Line</th><th>Set</th><th>#</th><th>Variant</th><th>Reason</th></tr>
					</thead>
					<tbody>
						{#each combined.singles.unmatched as row, i (i)}
							<tr class="off">
								<td class="chk"><input type="checkbox" disabled title="Unresolved — can't import" /></td>
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
		</div>

		<div class="pane">
			<h2 class="pane-head">
				Sealed products
				<span class="counts">
					<span class="ok">{combined.sealed.matched.length} matched</span>
					{#if combined.sealed.unmatched.length}
						· <span class="miss">{combined.sealed.unmatched.length} unmatched</span>
					{/if}
					· <span class="sel">{selSealedCount} selected</span>
				</span>
			</h2>
			{#if combined.sealed.matched.length}
				<div class="seltools">
					<button onclick={() => setAll(selSealed, sealedMatched, true)}>Select all</button>
					<button onclick={() => setAll(selSealed, sealedMatched, false)}>Select none</button>
					{#if ownedSealedSelected}
						<button class="warn" onclick={() => deselectOwned(selSealed, sealedMatched)}>
							Deselect {ownedSealedSelected} already-owned
						</button>
					{/if}
				</div>
				<table>
					<thead>
						<tr><th class="chk"></th><th>Line</th><th>Product</th><th>Set</th><th>Qty</th><th>Cond.</th></tr>
					</thead>
					<tbody>
						{#each combined.sealed.matched as row, i (i)}
							<tr class:off={!selSealed.has(row.source_line)}>
								<td class="chk">
									<input
										type="checkbox"
										checked={selSealed.has(row.source_line)}
										onchange={() => toggle(selSealed, row.source_line)}
									/>
								</td>
								<td>{row.source_line}</td>
								<td>
									{row.name}
									{#if row.already_owned > 0}
										<span class="badge" title="You already own {row.already_owned} of this product">
											owned ×{row.already_owned}
										</span>
									{/if}
								</td>
								<td>{row.set_code ?? ''}</td>
								<td>{row.quantity}</td>
								<td>{row.condition}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			{/if}
			{#if combined.sealed.unmatched.length}
				<h3>Unmatched sealed</h3>
				<table>
					<thead>
						<tr><th class="chk"></th><th>Line</th><th>Product</th><th>Set</th><th>Reason</th></tr>
					</thead>
					<tbody>
						{#each combined.sealed.unmatched as row, i (i)}
							<tr class="off">
								<td class="chk"><input type="checkbox" disabled title="Unresolved — can't import" /></td>
								<td>{row.source_line}</td>
								<td>{row.name}</td>
								<td>{row.set_hint}</td>
								<td class="reason">{row.reason}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			{/if}
		</div>

		{#if combined.skipped.length}
			<div class="pane">
				<h2 class="pane-head">
					Skipped <span class="counts muted">{combined.skipped.length} non-Pokémon rows</span>
				</h2>
				<table>
					<thead>
						<tr><th>Line</th><th>Category</th><th>Name</th><th>Reason</th></tr>
					</thead>
					<tbody>
						{#each combined.skipped as row, i (i)}
							<tr>
								<td>{row.source_line}</td>
								<td>{row.category}</td>
								<td>{row.name}</td>
								<td class="reason">{row.reason}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
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
	h3 {
		font-size: 0.72rem;
		text-transform: uppercase;
		color: #e9a045;
		margin: 0.9rem 0 0.3rem;
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
	.hint {
		font-size: 0.82rem;
		color: #8fb7e0;
		background: #16213e;
		border: 1px solid #0f3460;
		border-radius: 6px;
		padding: 0.5rem 0.7rem;
		max-width: 640px;
	}
	.subhint {
		font-size: 0.78rem;
		color: #8a8aa0;
		margin: 0 0 0.3rem;
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
		max-width: 240px;
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
	.seltools {
		display: flex;
		gap: 0.5rem;
		align-items: center;
		margin: 0 0 0.5rem;
	}
	.seltools button {
		padding: 0.28rem 0.6rem;
		font-size: 0.75rem;
		background: #16213e;
		border: 1px solid #0f3460;
		border-radius: 5px;
		color: #cfd6e6;
		cursor: pointer;
	}
	.seltools button:hover {
		border-color: #e94560;
		color: #fff;
	}
	.seltools button.warn {
		border-color: #e9a045;
		color: #e9a045;
	}
	.seltools button.warn:hover {
		background: #e9a045;
		color: #16213e;
	}
	.pane {
		margin: 1.4rem 0;
		padding: 0 0 0.4rem;
		border-top: 2px solid #0f3460;
	}
	.pane-head {
		display: flex;
		align-items: baseline;
		gap: 0.6rem;
		color: #e0e0e0;
		font-size: 0.9rem;
	}
	.counts {
		font-size: 0.8rem;
		text-transform: none;
	}
	.ok {
		color: #6bd968;
	}
	.miss {
		color: #e94560;
	}
	.sel {
		color: #8fb7e0;
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
	th.chk,
	td.chk {
		width: 1.6rem;
		padding-right: 0;
		text-align: center;
	}
	td.chk input {
		cursor: pointer;
	}
	tr.off td:not(.chk) {
		opacity: 0.45;
	}
	.badge {
		display: inline-block;
		margin-left: 0.4rem;
		padding: 0.05rem 0.4rem;
		font-size: 0.68rem;
		border-radius: 999px;
		background: #3a2f16;
		color: #e9a045;
		border: 1px solid #e9a045;
		vertical-align: middle;
	}
	.reason {
		color: #e9a045;
	}
</style>
