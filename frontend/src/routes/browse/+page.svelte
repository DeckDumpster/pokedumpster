<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { api } from '$lib/api';
	import { count } from '$lib/format';
	import { Button, EmptyState } from '$lib/components/ui';
	import type { SetSummary } from '$lib/types/SetSummary';

	let containers = $state<SetSummary[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let search = $state('');

	// Series headers can be opened/closed individually. The default state
	// is computed once per fetch: any series with at least one owned card
	// opens; everything else collapses. Subsequent user toggles override.
	let collapsed = $state<Record<string, boolean>>({});

	onMount(async () => {
		try {
			containers = await api.sets();
			// Default: open anything you own at least one card in;
			// collapse the rest so the page is scannable.
			const next: Record<string, boolean> = {};
			for (const c of containers) {
				const owned = (c.owned_cards ?? 0) > 0;
				const key = seriesKey(c);
				if (next[key] === undefined) next[key] = !owned;
				else next[key] = next[key] && !owned;
			}
			collapsed = next;
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	});

	/** Bundle entries go under a fixed "Bundles" pseudo-series so they
	 *  group together regardless of the synthetic `series` field on the
	 *  backend SetSummary (which is the bundle's marketing name, not a
	 *  Pokémon TCG era). */
	function seriesKey(c: SetSummary): string {
		return c.kind === 'bundle' ? 'Bundles' : c.series;
	}

	// One group per series, ordered chronologically newest-first (matches
	// the backend's release_date DESC). Bundles always at the top.
	type Group = {
		series: string;
		sets: SetSummary[];
		total_cards: number;
		owned_cards: number;
	};
	const groups = $derived.by<Group[]>(() => {
		const q = search.trim().toLowerCase();
		const matchSet = (c: SetSummary) =>
			!q || c.name.toLowerCase().includes(q) || c.series.toLowerCase().includes(q);
		const visible = containers.filter(matchSet);

		const byKey: Map<string, Group> = new Map();
		// Use the first appearance order from `containers` (already release_date
		// DESC from the API) so groups naturally show newest series first.
		for (const c of containers) {
			const k = seriesKey(c);
			if (!byKey.has(k))
				byKey.set(k, { series: k, sets: [], total_cards: 0, owned_cards: 0 });
		}
		// Populate from the filtered view so search narrows what's listed.
		for (const c of visible) {
			const g = byKey.get(seriesKey(c))!;
			g.sets.push(c);
			g.total_cards += c.total_cards;
			g.owned_cards += c.owned_cards;
		}
		// Bundles section first if present; then the rest in their original order.
		const all = Array.from(byKey.values()).filter((g) => g.sets.length > 0);
		const bundles = all.filter((g) => g.series === 'Bundles');
		const rest = all.filter((g) => g.series !== 'Bundles');
		return [...bundles, ...rest];
	});

	const totalShown = $derived(groups.reduce((n, g) => n + g.sets.length, 0));

	function pct(s: SetSummary): number {
		return s.total_cards > 0 ? Math.round((s.owned_cards / s.total_cards) * 100) : 0;
	}
	function basePct(s: SetSummary): number {
		if (s.base_total_cards == null || s.base_owned_cards == null || s.base_total_cards === 0)
			return 0;
		return Math.round((s.base_owned_cards / s.base_total_cards) * 100);
	}
	function groupPct(g: Group): number {
		return g.total_cards > 0 ? Math.round((g.owned_cards / g.total_cards) * 100) : 0;
	}
	function anchorId(series: string): string {
		return 'series-' + series.toLowerCase().replace(/[^a-z0-9]+/g, '-');
	}

	function toggle(series: string) {
		collapsed[series] = !collapsed[series];
	}
	function setAll(v: boolean) {
		const next: Record<string, boolean> = {};
		for (const g of groups) next[g.series] = v;
		collapsed = next;
	}
	/** Sidebar click: force-open the series and scroll to its header. */
	async function jumpTo(series: string) {
		collapsed[series] = false;
		await tick();
		const el = document.getElementById(anchorId(series));
		if (el) el.scrollIntoView({ behavior: 'smooth', block: 'start' });
	}
</script>

<svelte:head><title>Browse sets — PokeDumpster</title></svelte:head>

<header class="page">
	<div>
		<h1>Browse sets</h1>
		<p class="muted">Pick a set or bundle to open its binder view.</p>
	</div>
</header>

{#if loading}
	<p class="muted">Loading…</p>
{:else if error}
	<p class="error">Failed to load sets: {error}</p>
{:else}
	<div class="controls">
		<input
			class="search"
			type="text"
			placeholder="Search sets, bundles, or series…"
			bind:value={search}
		/>
		<span class="counts">{totalShown} of {containers.length}</span>
		<span class="spacer"></span>
		<button class="ghost" onclick={() => setAll(false)}>Expand all</button>
		<button class="ghost" onclick={() => setAll(true)}>Collapse all</button>
	</div>

	{#if totalShown === 0}
		{#if search.trim()}
			<EmptyState
				title="No sets match “{search.trim()}”."
				description="The search reads set names, bundle names and series — try the era (“Scarlet & Violet”) or a shorter fragment."
			>
				{#snippet action()}
					<Button variant="ghost" onclick={() => (search = '')}>Clear search</Button>
				{/snippet}
			</EmptyState>
		{:else}
			<EmptyState
				title="No sets in the catalog."
				description="The shared catalog hasn't been built yet — until it is, there are no binder pages to browse."
			/>
		{/if}
	{:else}
		<div class="layout">
			<!-- Sticky series nav. Click a row to scroll-and-expand the matching
			     section. Owned dot makes it obvious which series have your cards. -->
			<aside class="nav">
				<div class="nav-label">Jump to</div>
				<ul>
					{#each groups as g (g.series)}
						<li>
							<button
								class="navlink"
								class:owned={g.owned_cards > 0}
								onclick={() => jumpTo(g.series)}
							>
								<span class="dot" aria-hidden="true"></span>
								<span class="name">{g.series}</span>
								<span class="setn">{g.sets.length}</span>
							</button>
						</li>
					{/each}
				</ul>
			</aside>

			<div class="main">
				{#each groups as g (g.series)}
					<section class="seriesgroup" id={anchorId(g.series)}>
						<button
							class="grouphdr"
							onclick={() => toggle(g.series)}
							aria-expanded={!collapsed[g.series]}
						>
							<span class="caret">{collapsed[g.series] ? '▸' : '▾'}</span>
							<span class="ghname">{g.series}</span>
							<span class="ghmeta">
								{g.sets.length} {g.sets.length === 1 ? 'set' : 'sets'}
								{#if g.total_cards > 0 && g.series !== 'Bundles'}
									· {count(g.owned_cards)} / {count(g.total_cards)} cards ({groupPct(g)}%)
								{/if}
							</span>
						</button>
						{#if !collapsed[g.series]}
							<div class="grid">
								{#each g.sets as set (set.set_code)}
									<a
										class="tile"
										class:bundle={set.kind === 'bundle'}
										href="/browse/{set.set_code}"
									>
										{#if set.symbol_url}
											<img class="symbol" src={set.symbol_url} alt="" />
										{/if}
										<div class="title">{set.name}</div>
										<div class="series">
											{set.series}
											{#if set.synthesized}
												<span class="provisional" title="Built from TCGCSV — pokemontcg.io has not published this set yet, so its card list and art are provisional."
													>TCGCSV</span
												>
											{/if}
										</div>
										{#if set.base_total_cards != null && set.base_owned_cards != null}
											<div class="count">
												Base {count(set.base_owned_cards)} / {count(set.base_total_cards)}
											</div>
											<div class="bar base"><span style:width="{basePct(set)}%"></span></div>
										{/if}
										<div class="count">Master {count(set.owned_cards)} / {count(set.total_cards)}</div>
										<div class="bar"><span style:width="{pct(set)}%"></span></div>
									</a>
								{/each}
							</div>
						{/if}
					</section>
				{/each}
			</div>
		</div>
	{/if}
{/if}

<style>
	h1 {
		color: var(--color-text-accent);
		margin-bottom: 0.25rem;
	}
	.page {
		display: flex;
		gap: 1rem;
		align-items: baseline;
		justify-content: space-between;
	}
	.muted {
		color: var(--color-text-subtle);
	}
	.error {
		color: var(--color-text-accent);
	}
	.controls {
		display: flex;
		gap: 0.5rem;
		align-items: center;
		flex-wrap: wrap;
		margin: 0.75rem 0;
	}
	.spacer {
		flex: 1;
	}
	.counts {
		color: var(--color-text-subtle);
		font-size: 0.85rem;
	}
	.search {
		flex: 1;
		min-width: 220px;
		max-width: 360px;
		padding: 0.5rem;
		background: var(--color-surface-page);
		border: 1px solid var(--color-border);
		border-radius: 6px;
		color: var(--color-text);
	}
	.ghost {
		background: var(--color-surface-panel);
		border: 1px solid var(--color-border);
		color: var(--color-text-muted);
		padding: 0.4rem 0.7rem;
		border-radius: 6px;
		font: inherit;
		font-size: 0.85rem;
		cursor: pointer;
	}
	.ghost:hover {
		border-color: var(--color-border-accent);
		color: var(--color-text-accent);
	}

	/* Two-column layout: sticky sidebar + main grid area. Sidebar
	   collapses to a horizontal chip row on narrow viewports. */
	.layout {
		display: grid;
		grid-template-columns: 200px 1fr;
		gap: 1.25rem;
		align-items: start;
	}
	.nav {
		position: sticky;
		top: 0.5rem;
		max-height: calc(100vh - 1rem);
		overflow-y: auto;
		background: var(--color-surface-panel);
		border: 1px solid var(--color-border);
		border-radius: 8px;
		padding: 0.5rem;
	}
	.nav-label {
		text-transform: uppercase;
		font-size: 0.7rem;
		letter-spacing: 0.06em;
		color: var(--color-text-subtle);
		padding: 0.2rem 0.4rem 0.4rem;
	}
	.nav ul {
		list-style: none;
		padding: 0;
		margin: 0;
		display: flex;
		flex-direction: column;
		gap: 1px;
	}
	.navlink {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		width: 100%;
		background: transparent;
		border: none;
		color: var(--color-text-subtle);
		padding: 0.35rem 0.4rem;
		border-radius: 4px;
		cursor: pointer;
		font: inherit;
		font-size: 0.85rem;
		text-align: left;
	}
	.navlink:hover {
		background: var(--color-info-surface);
		color: var(--color-text);
	}
	.navlink.owned {
		color: var(--color-text);
	}
	.dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--color-surface-raised);
		flex-shrink: 0;
	}
	.navlink.owned .dot {
		background: var(--color-progress-fill);
	}
	.navlink .name {
		flex: 1;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.navlink .setn {
		color: var(--color-text-decorative);
		font-variant-numeric: tabular-nums;
		font-size: 0.78rem;
	}

	.main {
		min-width: 0;
	}
	.seriesgroup {
		margin-bottom: 1rem;
	}
	.grouphdr {
		display: flex;
		align-items: baseline;
		gap: 0.6rem;
		width: 100%;
		background: var(--color-info-surface);
		border: none;
		color: var(--color-text);
		padding: 0.55rem 0.8rem;
		border-radius: 6px;
		font: inherit;
		font-size: 0.95rem;
		cursor: pointer;
		text-align: left;
	}
	.grouphdr:hover {
		background: var(--color-info-surface-hover);
	}
	.caret {
		width: 1ch;
		color: var(--color-text-muted);
		font-size: 0.85rem;
	}
	.ghname {
		font-weight: 700;
		color: var(--color-text-accent);
	}
	.ghmeta {
		color: var(--color-text-subtle);
		font-size: 0.82rem;
	}

	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(190px, 1fr));
		gap: 1rem;
		margin-top: 0.75rem;
	}
	.tile {
		display: block;
		background: var(--color-surface-panel);
		border: 2px solid var(--color-border);
		border-radius: 10px;
		padding: 1rem;
		text-decoration: none;
		color: var(--color-text);
		transition: border-color 0.15s;
	}
	.tile:hover {
		border-color: var(--color-border-accent);
	}
	.tile.bundle {
		border-color: var(--color-warning-border);
	}
	.tile.bundle:hover {
		border-color: var(--color-warning);
	}
	.tile.bundle .title {
		color: var(--color-warning-text);
	}
	.symbol {
		height: 28px;
		margin-bottom: 0.4rem;
	}
	.title {
		font-weight: 700;
		color: var(--color-text-accent);
	}
	.series {
		font-size: 0.8rem;
		color: var(--color-text-subtle);
		margin: 0.1rem 0 0.5rem;
	}
	.provisional {
		display: inline-block;
		padding: 0 0.3rem;
		border: 1px solid var(--color-border);
		border-radius: 3px;
		font-size: 0.7rem;
		color: var(--color-warning-text);
		vertical-align: 1px;
	}
	.count {
		font-size: 0.85rem;
	}
	.bar {
		height: 6px;
		background: var(--color-progress-track);
		border-radius: 3px;
		margin-top: 0.2rem;
		overflow: hidden;
	}
	.bar span {
		display: block;
		height: 100%;
		background: var(--color-accent);
	}
	.bar.base span {
		background: var(--color-progress-fill);
	}
	.count + .bar.base {
		margin-bottom: 0.35rem;
	}

	/* Mobile: sidebar becomes a horizontal scrollable chip row above the main. */
	@media (max-width: 720px) {
		.layout {
			grid-template-columns: 1fr;
		}
		.nav {
			position: static;
			max-height: none;
		}
		.nav ul {
			flex-direction: row;
			overflow-x: auto;
			gap: 0.4rem;
		}
		.navlink {
			flex: 0 0 auto;
			padding: 0.3rem 0.55rem;
			border: 1px solid var(--color-border);
			border-radius: 999px;
		}
		.navlink .setn {
			display: none;
		}
	}
</style>
