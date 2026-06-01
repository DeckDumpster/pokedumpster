<script lang="ts">
	import { page } from '$app/state';
	import { api } from '$lib/api';
	import { breadcrumbs } from '$lib/breadcrumbs.svelte';
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

	// Rarity-split table order. Listed rarities come first in this
	// sequence; anything else falls in afterwards, alphabetised.
	const RARITY_ORDER: Record<string, number> = {
		Common: 0,
		Uncommon: 1,
		Rare: 2,
		'Rare Holo': 3,
		'Double Rare': 4,
		'Illustration Rare': 5,
		'Special Illustration Rare': 6,
		'Ultra Rare': 7,
		'Hyper Rare': 8,
		'Mega Attack Rare': 9,
		'Mega Hyper Rare': 10
	};
	// Upstream rarity strings are inconsistent — some are title-case
	// ("Special Illustration Rare"), others are SCREAMING_SNAKE
	// ("MEGA_ATTACK_RARE"). Normalize both into the same canonical form
	// before ranking.
	function canonicalRarity(r: string): string {
		return r
			.toLowerCase()
			.replace(/_/g, ' ')
			.split(' ')
			.filter((w) => w.length > 0)
			.map((w) => w.charAt(0).toUpperCase() + w.slice(1))
			.join(' ');
	}
	const sortedRarities = $derived.by(() => {
		if (!stats) return [];
		const rank = (r: string): number => RARITY_ORDER[canonicalRarity(r)] ?? 100;
		return [...stats.rarities].sort((a, b) => {
			const ra = rank(a.rarity);
			const rb = rank(b.rarity);
			return ra !== rb ? ra - rb : a.rarity.localeCompare(b.rarity);
		});
	});

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
	function rarityColor(rarity: string | null): string {
		if (!rarity) return '#3a3a52';
		switch (canonicalRarity(rarity)) {
			case 'Common':
				return '#6a7280';
			case 'Uncommon':
				return '#5cb85c';
			case 'Rare':
				return '#4a8df0';
			case 'Rare Holo':
			case 'Double Rare':
				return '#9c5fb5';
			case 'Illustration Rare':
				return '#f0c878';
			case 'Special Illustration Rare':
			case 'Ultra Rare':
				return '#e94560';
			case 'Hyper Rare':
			case 'Mega Attack Rare':
			case 'Mega Hyper Rare':
				return '#ffd24a';
			default:
				return '#b88cc0';
		}
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
			{#snippet bar(label: string, owned: number, total: number)}
				<div class="metric">
					<div class="metriclabel">
						<span>{label}</span>
						<span class="metricval">{count(owned)} / {count(total)} · {pct(owned, total)}%</span>
					</div>
					<div class="bar"><span style:width="{pct(owned, total)}%"></span></div>
				</div>
			{/snippet}
			{@render bar('Base set', stats.base_owned_cards, stats.base_total_cards)}
			{@render bar('Numbered set', stats.owned_cards, stats.total_cards)}
			{@render bar('Master set', stats.owned_printings, stats.total_printings)}
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
						class="histo-col"
						class:owned={c.copies > 0}
						class:dupe={c.copies > 1}
						style:height="{Math.max(2, (c.copies / maxCopies) * 100)}%"
						style:background={c.copies > 0 ? rarityColor(c.rarity) : '#1f2640'}
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
			<p class="muted">No cards catalogued.</p>
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
					{#each sortedRarities as r (r.rarity)}
						<tr>
							<td class="iconcol">
								{#if rarityIconSrc(r.rarity)}
									<img
										class="rarityicon"
										class:basic={isBasicRarity(canonicalRarity(r.rarity))}
										src={rarityIconSrc(canonicalRarity(r.rarity))}
										alt=""
										onerror={(e) =>
											((e.currentTarget as HTMLImageElement).style.display =
												'none')}
									/>
								{/if}
							</td>
							<td>{canonicalRarity(r.rarity)}</td>
							<td>{count(r.owned_cards)}</td>
							<td>{count(r.total_cards)}</td>
							<td class="pcol">
								<div class="pcell">
									<div class="bar small">
										<span style:width="{pct(r.owned_cards, r.total_cards)}%"></span>
									</div>
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
		color: #e94560;
		margin: 0;
	}
	.series {
		color: #888;
		font-size: 0.85rem;
		margin: 0.1rem 0 0;
	}
	.binderlink {
		color: #e0e0e0;
		font-size: 0.9rem;
		white-space: nowrap;
	}
	.binderlink:hover {
		color: #e94560;
	}
	.muted {
		color: #888;
	}
	.error {
		color: #e94560;
	}
	.cards {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
		gap: 1rem;
		margin: 1rem 0;
	}
	.card {
		background: #16213e;
		border: 1px solid #0f3460;
		border-radius: 10px;
		padding: 1rem 1.2rem;
		margin-bottom: 1rem;
	}
	h2 {
		font-size: 0.8rem;
		text-transform: uppercase;
		color: #888;
		margin: 0 0 0.8rem;
	}
	.metric {
		margin-bottom: 0.8rem;
	}
	.metriclabel {
		display: flex;
		justify-content: space-between;
		font-size: 0.85rem;
		margin-bottom: 0.25rem;
	}
	.metricval {
		color: #888;
	}
	.bar {
		height: 8px;
		background: #0f3460;
		border-radius: 4px;
		overflow: hidden;
	}
	.bar.small {
		height: 6px;
		flex: 1;
	}
	.bar span {
		display: block;
		height: 100%;
		background: #e94560;
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
		color: #e94560;
	}
	.figlabel {
		font-size: 0.75rem;
		text-transform: uppercase;
		color: #888;
	}
	table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.9rem;
	}
	th {
		text-align: left;
		padding: 0.4rem 0.6rem;
		border-bottom: 2px solid #0f3460;
		color: #888;
		font-size: 0.75rem;
		text-transform: uppercase;
	}
	td {
		padding: 0.4rem 0.6rem;
		border-bottom: 1px solid #0f3460;
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
		gap: 0.5rem;
	}
	.rpct {
		color: #888;
		font-size: 0.8rem;
		min-width: 2.5rem;
		text-align: right;
	}
	/* Copies histogram. One thin column per card across the row,
	   sized by flex so the whole set fits regardless of card count;
	   each column's height is set inline as a percentage of the row. */
	.histohint {
		color: #888;
		font-size: 0.8rem;
		margin: 0 0 0.6rem;
	}
	.histo {
		display: flex;
		align-items: flex-end;
		gap: 1px;
		height: 120px;
		padding: 4px 0;
		background: #0c1426;
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
		outline: 1px solid rgba(255, 255, 255, 0.25);
		outline-offset: -1px;
	}
</style>
