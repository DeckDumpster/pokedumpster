<script lang="ts">
	import { SvelteSet } from 'svelte/reactivity';
	import { api } from '$lib/api';
	import MatchPicker from '$lib/components/MatchPicker.svelte';
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

	// Which unmatched row's inline picker is open, keyed `${pane}:${index}`.
	// Panes: 'r' = single-format report, 'cs' = Collectr singles, 'cx' = sealed.
	let matchOpen = $state<string | null>(null);
	// After a commit with misses: offer to park them for later. (oq3i.5)
	let parkPrompt = $state<number | null>(null);
	let parkResult = $state<number | null>(null);
	// Open dead-letter count, for the nav badge.
	let openCount = $state(0);

	const isCollectr = $derived(format === 'collectr');

	function refreshOpenCount() {
		api.unresolvedList()
			.then((r) => (openCount = r.length))
			.catch(() => {});
	}
	$effect(() => {
		refreshOpenCount();
	});

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
		parkPrompt = null;
		parkResult = null;
		matchOpen = null;
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
			// Misses still showing after any inline matches — offered for parking.
			const misses = isCollectr
				? (combined?.singles.unmatched.length ?? 0) + (combined?.sealed.unmatched.length ?? 0)
				: (report?.unmatched.length ?? 0);
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
			parkPrompt = misses > 0 ? misses : null;
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}

	// --- Inline manual match of an unmatched preview row (oq3i.5) -------------
	// A quick match: adds one copy immediately and drops the row from the
	// preview. For full metadata fidelity (price/date/tags/quantity), import and
	// then park the misses — the /ingest/unresolved queue replays those.
	function matchKey(pane: string, i: number): string {
		return `${pane}:${i}`;
	}

	async function matchSingle(pane: 'r' | 'cs', i: number, printingId: string) {
		busy = true;
		error = null;
		try {
			await api.addCopy({ printing_id: printingId, source: 'manual_id' });
			if (pane === 'r' && report) {
				report.unmatched = report.unmatched.filter((_, idx) => idx !== i);
			} else if (pane === 'cs' && combined) {
				combined.singles.unmatched = combined.singles.unmatched.filter((_, idx) => idx !== i);
			}
			matchOpen = null;
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}

	async function matchSealed(i: number, productId: number) {
		busy = true;
		error = null;
		try {
			await api.addSealed({ product_id: productId, source: 'manual_id' });
			if (combined) {
				combined.sealed.unmatched = combined.sealed.unmatched.filter((_, idx) => idx !== i);
			}
			matchOpen = null;
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}

	async function parkMisses() {
		busy = true;
		error = null;
		try {
			if (isCollectr) {
				const r = await api.importCollectrCommitSelected(content, [], [], fileName ?? undefined, true);
				parkResult = r.singles.skipped + r.sealed.skipped;
			} else {
				const r = await api.importCommitSelected(format, content, [], fileName ?? undefined, true);
				parkResult = r.skipped;
			}
			parkPrompt = null;
			refreshOpenCount();
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

{#if openCount > 0}
	<p class="queuelink">
		<a href="/ingest/unresolved">
			⚠ {openCount} unresolved import {openCount === 1 ? 'row' : 'rows'} to review →
		</a>
	</p>
{/if}

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

{#if parkPrompt}
	<div class="parkprompt">
		<span>
			<strong>{parkPrompt}</strong>
			{parkPrompt === 1 ? 'row' : 'rows'} couldn't be matched — park {parkPrompt === 1
				? 'it'
				: 'them'} to resolve later?
		</span>
		<button onclick={parkMisses} disabled={busy}>Park to resolve later</button>
		<button class="ghost" onclick={() => (parkPrompt = null)} disabled={busy}>Not now</button>
	</div>
{/if}

{#if parkResult != null}
	<div class="result">
		Parked <strong>{parkResult}</strong>
		{parkResult === 1 ? 'row' : 'rows'}.
		<a href="/ingest/unresolved">Resolve them →</a>
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
			<p class="subhint">Match each to a catalog card, or import and park them to resolve later.</p>
			<table>
				<thead>
					<tr><th>Match</th><th>Line</th><th>Set</th><th>#</th><th>Variant</th><th>Reason</th></tr>
				</thead>
				<tbody>
					{#each report.unmatched as row, i (i)}
						<tr>
							<td>
								<button
									class="matchbtn"
									onclick={() =>
										(matchOpen = matchOpen === matchKey('r', i) ? null : matchKey('r', i))}
								>
									{matchOpen === matchKey('r', i) ? 'Close' : 'Match…'}
								</button>
							</td>
							<td>{row.source_line}</td>
							<td>{row.set_hint}</td>
							<td>{row.number}</td>
							<td>{row.variant}</td>
							<td class="reason">{row.reason}</td>
						</tr>
						{#if matchOpen === matchKey('r', i)}
							<tr class="pickrow">
								<td colspan="6">
									<MatchPicker
										kind="single"
										busy={busy}
										onPickSingle={(pid) => matchSingle('r', i, pid)}
										onCancel={() => (matchOpen = null)}
									/>
								</td>
							</tr>
						{/if}
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
						<tr><th>Match</th><th>Line</th><th>Set</th><th>#</th><th>Variant</th><th>Reason</th></tr>
					</thead>
					<tbody>
						{#each combined.singles.unmatched as row, i (i)}
							<tr>
								<td>
									<button
										class="matchbtn"
										onclick={() =>
											(matchOpen = matchOpen === matchKey('cs', i) ? null : matchKey('cs', i))}
									>
										{matchOpen === matchKey('cs', i) ? 'Close' : 'Match…'}
									</button>
								</td>
								<td>{row.source_line}</td>
								<td>{row.set_hint}</td>
								<td>{row.number}</td>
								<td>{row.variant}</td>
								<td class="reason">{row.reason}</td>
							</tr>
							{#if matchOpen === matchKey('cs', i)}
								<tr class="pickrow">
									<td colspan="6">
										<MatchPicker
											kind="single"
											busy={busy}
											onPickSingle={(pid) => matchSingle('cs', i, pid)}
											onCancel={() => (matchOpen = null)}
										/>
									</td>
								</tr>
							{/if}
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
						<tr><th>Match</th><th>Line</th><th>Product</th><th>Set</th><th>Reason</th></tr>
					</thead>
					<tbody>
						{#each combined.sealed.unmatched as row, i (i)}
							<tr>
								<td>
									<button
										class="matchbtn"
										onclick={() =>
											(matchOpen = matchOpen === matchKey('cx', i) ? null : matchKey('cx', i))}
									>
										{matchOpen === matchKey('cx', i) ? 'Close' : 'Match…'}
									</button>
								</td>
								<td>{row.source_line}</td>
								<td>{row.name}</td>
								<td>{row.set_hint}</td>
								<td class="reason">{row.reason}</td>
							</tr>
							{#if matchOpen === matchKey('cx', i)}
								<tr class="pickrow">
									<td colspan="5">
										<MatchPicker
											kind="sealed"
											initialQuery={row.name}
											busy={busy}
											onPickSealed={(prod) => matchSealed(i, prod)}
											onCancel={() => (matchOpen = null)}
										/>
									</td>
								</tr>
							{/if}
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
		color: var(--color-text-accent);
	}
	h2 {
		font-size: 0.8rem;
		text-transform: uppercase;
		color: var(--color-text-subtle);
		margin: 1.2rem 0 0.4rem;
	}
	h3 {
		font-size: 0.72rem;
		text-transform: uppercase;
		color: var(--color-warning-text);
		margin: 0.9rem 0 0.3rem;
	}
	.muted {
		color: var(--color-text-subtle);
	}
	.muted a {
		color: var(--color-text);
	}
	.muted a:hover {
		color: var(--color-text-accent);
	}
	.hint {
		font-size: 0.82rem;
		color: var(--color-info-text);
		background: var(--color-surface-panel);
		border: 1px solid var(--color-border);
		border-radius: 6px;
		padding: 0.5rem 0.7rem;
		max-width: 640px;
	}
	.subhint {
		font-size: 0.78rem;
		color: var(--color-text-subtle);
		margin: 0 0 0.3rem;
	}
	.error {
		color: var(--color-text-accent);
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
		color: var(--color-text-subtle);
	}
	select,
	textarea {
		background: var(--color-surface-page);
		border: 1px solid var(--color-border);
		border-radius: 6px;
		color: var(--color-text);
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
		background: var(--color-info-surface);
		border: none;
		border-radius: 6px;
		color: var(--color-text);
		cursor: pointer;
	}
	.actions button:hover:not(:disabled) {
		background: var(--color-accent);
	}
	.actions button:disabled {
		opacity: 0.5;
		cursor: default;
	}
	.commit {
		background: var(--color-accent) !important;
	}
	.commit:disabled {
		background: var(--color-info-surface) !important;
	}
	.result {
		margin: 1rem 0;
		padding: 0.7rem 1rem;
		background: var(--color-surface-panel);
		border: 1px solid var(--color-border);
		border-radius: 8px;
	}
	.result a,
	.result strong {
		color: var(--color-text-accent);
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
		background: var(--color-surface-panel);
		border: 1px solid var(--color-border);
		border-radius: 5px;
		color: var(--color-text-muted);
		cursor: pointer;
	}
	.seltools button:hover {
		border-color: var(--color-border-accent);
		color: var(--color-text-strong);
	}
	.seltools button.warn {
		border-color: var(--color-warning);
		color: var(--color-warning-text);
	}
	.seltools button.warn:hover {
		background: var(--color-warning);
		color: var(--color-on-warning);
	}
	.pane {
		margin: 1.4rem 0;
		padding: 0 0 0.4rem;
		border-top: 2px solid var(--color-border);
	}
	.pane-head {
		display: flex;
		align-items: baseline;
		gap: 0.6rem;
		color: var(--color-text);
		font-size: 0.9rem;
	}
	.counts {
		font-size: 0.8rem;
		text-transform: none;
	}
	.ok {
		color: var(--color-success-text);
	}
	.miss {
		color: var(--color-text-accent);
	}
	.sel {
		color: var(--color-info-text);
	}
	table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.85rem;
	}
	th {
		text-align: left;
		padding: 0.35rem 0.6rem;
		border-bottom: 2px solid var(--color-border);
		color: var(--color-text-subtle);
		font-size: 0.72rem;
		text-transform: uppercase;
	}
	td {
		padding: 0.35rem 0.6rem;
		border-bottom: 1px solid var(--color-border);
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
		background: var(--color-warning-surface);
		color: var(--color-warning-text);
		border: 1px solid var(--color-warning);
		vertical-align: middle;
	}
	.reason {
		color: var(--color-warning-text);
	}
	.queuelink a {
		display: inline-block;
		padding: 0.4rem 0.8rem;
		background: var(--color-warning-surface);
		border: 1px solid var(--color-warning);
		border-radius: 6px;
		color: var(--color-warning-text);
		text-decoration: none;
		font-size: 0.88rem;
	}
	.queuelink a:hover {
		background: var(--color-warning);
		color: var(--color-on-warning);
	}
	.parkprompt {
		display: flex;
		align-items: center;
		gap: 0.7rem;
		flex-wrap: wrap;
		margin: 1rem 0;
		padding: 0.7rem 1rem;
		background: var(--color-surface-panel);
		border: 1px solid var(--color-warning);
		border-radius: 8px;
		font-size: 0.9rem;
	}
	.parkprompt strong {
		color: var(--color-warning-text);
	}
	.parkprompt button {
		padding: 0.4rem 0.9rem;
		background: var(--color-warning);
		border: none;
		border-radius: 6px;
		color: var(--color-on-warning);
		cursor: pointer;
		font-weight: 600;
	}
	.parkprompt button.ghost {
		background: transparent;
		border: 1px solid var(--color-border);
		color: var(--color-text-muted);
		font-weight: 400;
	}
	.parkprompt button:disabled {
		opacity: 0.5;
		cursor: default;
	}
	.matchbtn {
		padding: 0.22rem 0.6rem;
		font-size: 0.75rem;
		background: var(--color-surface-panel);
		border: 1px solid var(--color-border);
		border-radius: 5px;
		color: var(--color-text-muted);
		cursor: pointer;
		white-space: nowrap;
	}
	.matchbtn:hover {
		border-color: var(--color-border-accent);
		color: var(--color-text-strong);
	}
	.pickrow td {
		background: var(--color-surface-sunken);
		padding: 0.2rem 0.4rem;
	}
</style>
