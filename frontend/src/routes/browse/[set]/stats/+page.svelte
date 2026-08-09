<script lang="ts">
	import { page } from '$app/state';
	import { api } from '$lib/api';
	import { breadcrumbs } from '$lib/breadcrumbs.svelte';
	import { EmptyState, ProgressBar } from '$lib/components/ui';
	import { money, count } from '$lib/format';
	import type { SetAnalytics } from '$lib/types/SetAnalytics';

	let stats = $state<SetAnalytics | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);

	$effect(() => {
		const set = page.params.set;
		if (!set) return;
		loading = true;
		error = null;
		api
			.setAnalytics(set)
			.then((s) => (stats = s))
			.catch((e) => (error = e instanceof Error ? e.message : String(e)))
			.finally(() => (loading = false));
	});

	// Keep the breadcrumb fresh — use the URL-param set code as a
	// placeholder until analytics resolves with the full set name. No
	// unmount cleanup; the breadcrumbs store is path-keyed and invalidates
	// itself when the URL changes.
	$effect.pre(() => {
		const setCode = page.params.set;
		if (!setCode) return;
		breadcrumbs.set([
			{ label: 'Browse', href: '/browse' },
			{ label: stats?.name ?? setCode, href: `/browse/${setCode}` },
			{ label: 'Stats' }
		]);
	});

	function pct(owned: number, total: number): number {
		return total > 0 ? Math.round((owned / total) * 100) : 0;
	}

	// Rarity glyphs live under static/rarity/ — same convention the
	// /collection table uses (rarityIconSrc + isBasicRarity scaling).
	function rarityIconSrc(rarity: string | null): string | null {
		if (!rarity) return null;
		const slug = rarity.toLowerCase().replace(/[ ._]/g, '-');
		return `/rarity/${slug}.svg`;
	}
	function isBasicRarity(rarity: string | null): boolean {
		return (
			rarity === 'Common' ||
			rarity === 'Uncommon' ||
			rarity === 'Rare' ||
			rarity === 'Illustration Rare' ||
			rarity === 'ACE SPEC Rare' ||
			rarity === 'Rare ACE'
		);
	}

	// Rarity order, canonical spelling and tier group all arrive on the
	// API rows, read off the shared catalog's `rarities` table. This page
	// used to carry its own RARITY_ORDER map and a canonicalRarity()
	// normaliser — a second copy of a typology the catalog already owns,
	// and one that only knew eleven of the fifty-four tiers (pd-xzea).
	// `stats.rarities` is pre-sorted by rank; render it as it comes.

	// Duplicates summary — owned_copies counts every physical card,
	// owned_cards counts unique cards. Dupes = the difference. Cards
	// with 2+ copies are the ones the user might trade away.
	const duplicates = $derived.by(() => {
		if (!stats) return null;
		const dupes = stats.owned_copies - stats.owned_cards;
		const cardsWithDupes = stats.copy_counts.filter((c) => c.copies > 1);
		const mostOwned = stats.copy_counts.reduce((m, c) => Math.max(m, c.copies), 0);
		return {
			total_copies: stats.owned_copies,
			extra_copies: dupes,
			cards_with_dupes: cardsWithDupes.length,
			most_owned: mostOwned
		};
	});

	// Histogram. Cap bar heights to maxCopies so single-extreme columns
	// don't squash everything else; rarity tier paints the bar colour so
	// the rarity boundaries are visible without explicit divider lines.
	const maxCopies = $derived(
		stats ? Math.max(1, ...stats.copy_counts.map((c) => c.copies)) : 1
	);
	// Which BUCKET a column belongs to is data (`rarity_grp`, straight off
	// the catalog); which colour a bucket wears is a `.g-*` rule below,
	// pointing at a --color-rarity-* role. Nothing here picks a colour —
	// this only turns the group into a class name.
	function rarityClass(grp: string | null, copies: number): string {
		if (copies === 0) return 'g-none';
		return `g-${grp ?? 'unranked'}`;
	}
</script>

<svelte:head><title>{stats ? stats.name : 'Set'} stats — PokeDumpster</title></svelte:head>

{#if loading}
	<p class="muted">Loading…</p>
{:else if error}
	<p class="error">Failed to load set stats: {error}</p>
{:else if stats}
	<header>
		<div>
			<h1>{stats.name}</h1>
			<p class="series">{stats.series}</p>
		</div>
		<a class="binderlink" href="/browse/{stats.set_code}">Binder view →</a>
	</header>

	<div class="cards">
		<section class="card">
			<h2>Completion</h2>
			<div class="metrics">
				{#snippet bar(label: string, owned: number, total: number)}
					<ProgressBar
						size="md"
						tone="complete"
						{label}
						hint="{count(owned)} / {count(total)} · {pct(owned, total)}%"
						value={owned}
						max={total}
					/>
				{/snippet}
				{@render bar('Base set', stats.base_owned_cards, stats.base_total_cards)}
				{@render bar('Numbered set', stats.owned_cards, stats.total_cards)}
				{@render bar('Master set', stats.owned_printings, stats.total_printings)}
			</div>
		</section>

		<section class="card">
			<h2>Value</h2>
			<div class="figs">
				<div class="fig">
					<span class="figval">{money(stats.owned_value_unique)}</span>
					<span class="figlabel">Owned (unique)</span>
				</div>
				<div class="fig">
					<span class="figval">{money(stats.owned_value)}</span>
					<span class="figlabel">With duplicates</span>
				</div>
				<div class="fig">
					<span class="figval">{money(stats.market_value)}</span>
					<span class="figlabel">Full set</span>
				</div>
				<div class="fig">
					<span class="figval">{pct(stats.owned_value_unique, stats.market_value)}%</span>
					<span class="figlabel">of set value</span>
				</div>
			</div>
		</section>
	</div>

	{#if duplicates}
		<section class="card">
			<h2>Duplicates</h2>
			<div class="figs">
				<div class="fig">
					<span class="figval">{count(duplicates.total_copies)}</span>
					<span class="figlabel">Total copies</span>
				</div>
				<div class="fig">
					<span class="figval">{count(duplicates.extra_copies)}</span>
					<span class="figlabel">Extra copies</span>
				</div>
				<div class="fig">
					<span class="figval">{count(duplicates.cards_with_dupes)}</span>
					<span class="figlabel">Cards with dupes</span>
				</div>
				<div class="fig">
					<span class="figval">{count(duplicates.most_owned)}</span>
					<span class="figlabel">Most owned</span>
				</div>
			</div>
		</section>
	{/if}

	{#if stats.copy_counts.length > 0}
		<section class="card">
			<h2>Copies by card #</h2>
			<p class="histohint">
				Bar height = physical copies owned · colour = rarity tier · hover for details.
			</p>
			<div class="histo" style:--max={maxCopies}>
				<!-- Index-keyed, not number-keyed: bundles aggregate cards
				     across sets, so card numbers repeat (e.g. #37 as both
				     Rare and Common). A number key collides and Svelte
				     throws a duplicate-key error that blanks the whole
				     page. The list is rebuilt wholesale per load with no
				     per-item state, so the index is a safe stable key. -->
				{#each stats.copy_counts as c, i (i)}
					<span
						class="histo-col {rarityClass(c.rarity_grp, c.copies)}"
						class:owned={c.copies > 0}
						class:dupe={c.copies > 1}
						style:height="{Math.max(2, (c.copies / maxCopies) * 100)}%"
						title="#{c.number} · {c.rarity ?? 'Unknown'} · {c.copies} {c.copies === 1
							? 'copy'
							: 'copies'}"
					></span>
				{/each}
			</div>
		</section>
	{/if}

	<section class="card">
		<h2>Rarity split</h2>
		{#if stats.rarities.length === 0}
			<EmptyState
				size="sm"
				title="No cards catalogued."
				description="This set has no cards in the shared catalog, so there is nothing to break down."
			/>
		{:else}
			<table>
				<thead>
					<tr>
						<th class="iconcol" aria-label="Rarity glyph"></th>
						<th>Rarity</th>
						<th>Owned</th>
						<th>Total</th>
						<th class="pcol">Progress</th>
					</tr>
				</thead>
				<tbody>
					{#each stats.rarities as r (r.rarity)}
						<tr>
							<td class="iconcol">
								{#if rarityIconSrc(r.rarity)}
									<img
										class="rarityicon"
										class:basic={isBasicRarity(r.rarity)}
										src={rarityIconSrc(r.rarity)}
										alt=""
										onerror={(e) =>
											((e.currentTarget as HTMLImageElement).style.display =
												'none')}
									/>
								{/if}
							</td>
							<td>{r.rarity}</td>
							<td>{count(r.owned_cards)}</td>
							<td>{count(r.total_cards)}</td>
							<td class="pcol">
								<div class="pcell">
									<ProgressBar
										class="rbar"
										tone="complete"
										label={r.rarity}
										labelHidden
										value={r.owned_cards}
										max={r.total_cards}
									/>
									<span class="rpct">{pct(r.owned_cards, r.total_cards)}%</span>
								</div>
							</td>
						</tr>
					{/each}
				</tbody>
			</table>
		{/if}
	</section>
{/if}

<style>
	header {
		display: flex;
		justify-content: space-between;
		align-items: flex-start;
		gap: 1rem;
	}
	h1 {
		color: var(--color-text-accent);
		margin: 0;
	}
	.series {
		color: var(--color-text-subtle);
		font-size: 0.85rem;
		margin: 0.1rem 0 0;
	}
	.binderlink {
		color: var(--color-text);
		font-size: 0.9rem;
		white-space: nowrap;
	}
	.binderlink:hover {
		color: var(--color-text-accent);
	}
	.muted {
		color: var(--color-text-subtle);
	}
	.error {
		color: var(--color-text-accent);
	}
	.cards {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
		gap: 1rem;
		margin: 1rem 0;
	}
	.card {
		background: var(--color-surface-panel);
		border: 1px solid var(--color-border);
		border-radius: 10px;
		padding: 1rem 1.2rem;
		margin-bottom: 1rem;
	}
	h2 {
		font-size: 0.8rem;
		text-transform: uppercase;
		color: var(--color-text-subtle);
		margin: 0 0 0.8rem;
	}
	/* The three completion meters are ProgressBar; the route only says how
	   they stack. Track, fill, thickness and the a11y attributes all come
	   from the primitive — this page hand-rolled its own track/fill pair
	   until pd-ifhu, which is why they were never progressbars to a screen
	   reader. */
	.metrics {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
	}
	.figs {
		display: flex;
		gap: 1.5rem;
	}
	.fig {
		display: flex;
		flex-direction: column;
	}
	.figval {
		font-size: 1.4rem;
		font-weight: 700;
		color: var(--color-text-accent);
	}
	.figlabel {
		font-size: 0.75rem;
		text-transform: uppercase;
		color: var(--color-text-subtle);
	}
	table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.9rem;
	}
	th {
		text-align: left;
		padding: 0.4rem 0.6rem;
		border-bottom: 2px solid var(--color-border);
		color: var(--color-text-subtle);
		font-size: 0.75rem;
		text-transform: uppercase;
	}
	td {
		padding: 0.4rem 0.6rem;
		border-bottom: 1px solid var(--color-border);
	}
	/* Rarity glyph in its own column, right-aligned, so every rarity
	   name in the next column starts on the same x-position. Glyph
	   scaling matches the /collection table for visual consistency. */
	.iconcol {
		width: 1%;
		text-align: right;
		white-space: nowrap;
		padding-right: 0.4rem;
	}
	.rarityicon {
		width: 22px;
		height: 22px;
		display: inline-block;
		vertical-align: middle;
	}
	.rarityicon.basic {
		width: 11px;
		height: 11px;
	}
	.pcol {
		width: 40%;
	}
	/* Flex on a div INSIDE the td — applying it to td breaks table-cell
	   layout and the Progress column floats out of the row alignment. */
	.pcell {
		display: flex;
		align-items: center;
		gap: var(--space-2);
	}
	/* The row's bar takes the slack the percentage doesn't. It carries no
	   caption — the rarity is already two columns to the left — but keeps
	   the tier as its accessible name via `labelHidden`. */
	.pcell :global(.rbar) {
		flex: 1;
	}
	.rpct {
		color: var(--color-text-subtle);
		font-size: var(--text-sm);
		min-width: 2.5rem;
		text-align: right;
	}
	/* Copies histogram. One thin column per card across the row,
	   sized by flex so the whole set fits regardless of card count;
	   each column's height is set inline as a percentage of the row. */
	.histohint {
		color: var(--color-text-subtle);
		font-size: 0.8rem;
		margin: 0 0 0.6rem;
	}
	.histo {
		display: flex;
		align-items: flex-end;
		gap: 1px;
		height: 120px;
		padding: 4px 0;
		background: var(--color-surface-well);
		border-radius: 6px;
	}
	.histo-col {
		flex: 1 1 0;
		min-width: 0;
		border-radius: 1px 1px 0 0;
		transition: filter 0.1s ease-out;
	}
	.histo-col:hover {
		filter: brightness(1.4);
	}
	.histo-col.dupe {
		outline: 1px solid var(--color-border-focus);
		outline-offset: -1px;
	}
	/* One rule per rarity group the catalog declares — the colour decision
	   the route used to make in a TypeScript switch (pd-xzea). The buckets
	   come from `rarities.grp` in shared.sqlite and arrive on the API row;
	   all this does is spend the matching semantic role. A new group in
	   data/rarities.json needs a role in tokens.css and a line here, and
	   until it gets one it draws as unranked rather than as nothing. */
	.histo-col.g-common {
		background: var(--color-rarity-common);
	}
	.histo-col.g-uncommon {
		background: var(--color-rarity-uncommon);
	}
	.histo-col.g-rare {
		background: var(--color-rarity-rare);
	}
	.histo-col.g-promo {
		background: var(--color-rarity-promo);
	}
	.histo-col.g-holo {
		background: var(--color-rarity-holo);
	}
	.histo-col.g-ultra {
		background: var(--color-rarity-ultra);
	}
	.histo-col.g-special {
		background: var(--color-rarity-special);
	}
	.histo-col.g-secret {
		background: var(--color-rarity-secret);
	}
	.histo-col.g-unranked {
		background: var(--color-rarity-unranked);
	}
	/* Owned nothing — the column is the absence, whatever its rarity. */
	.histo-col.g-none {
		background: var(--color-chart-empty);
	}
</style>
