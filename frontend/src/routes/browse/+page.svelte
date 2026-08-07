<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { api } from '$lib/api';
	import { count } from '$lib/format';
	import {
		Badge,
		Button,
		EmptyState,
		Field,
		Panel,
		ProgressBar,
		Toolbar
	} from '$lib/components/ui';
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
			if (!byKey.has(k)) byKey.set(k, { series: k, sets: [], total_cards: 0, owned_cards: 0 });
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

	function groupPct(g: Group): number {
		return g.total_cards > 0 ? Math.round((g.owned_cards / g.total_cards) * 100) : 0;
	}
	function anchorId(series: string): string {
		return 'series-' + series.toLowerCase().replace(/[^a-z0-9]+/g, '-');
	}
	/** The short identity stamp shown when a set has no symbol art. The PTCGO
	 *  code is the one the physical card carries; `set_code` is the catalogue
	 *  key and only stands in when there is no printed code. */
	function shortCode(s: SetSummary): string {
		return (s.ptcgo_code ?? s.set_code).toUpperCase();
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
	<div class="browsepage">
		<Toolbar class="controls" gap="sm">
			<Field
				class="search"
				type="text"
				placeholder="Search sets, bundles, or series…"
				aria-label="Search sets, bundles, or series"
				bind:value={search}
			/>
			<span class="counts">{totalShown} of {containers.length}</span>
			<span class="spacer"></span>
			<Button variant="ghost" size="sm" onclick={() => setAll(false)}>Expand all</Button>
			<Button variant="ghost" size="sm" onclick={() => setAll(true)}>Collapse all</Button>
		</Toolbar>

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
				<aside class="navcol">
					<Panel class="nav" padding="sm">
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
					</Panel>
				</aside>

				<div class="main">
					{#each groups as g (g.series)}
						<section class="seriesgroup" id={anchorId(g.series)}>
							<button
								class="grouphdr"
								onclick={() => toggle(g.series)}
								aria-expanded={!collapsed[g.series]}
							>
								<span class="caret" aria-hidden="true">{collapsed[g.series] ? '▸' : '▾'}</span>
								<span class="ghname">{g.series}</span>
								<span class="ghmeta">
									{g.sets.length}
									{g.sets.length === 1 ? 'set' : 'sets'}
									{#if g.total_cards > 0 && g.series !== 'Bundles'}
										· {count(g.owned_cards)} / {count(g.total_cards)} cards ({groupPct(g)}%)
									{/if}
								</span>
							</button>
							{#if !collapsed[g.series]}
								<div class="grid">
									{#each g.sets as set (set.set_code)}
										<Panel class="tile" href="/browse/{set.set_code}">
											<div class="ident">
												<!-- The symbol well. Upstream symbol art is a mixed bag —
												     some sets ship a white-on-black raster, some a dark
												     glyph, some nothing at all — so it lands in a fixed,
												     token-themed frame rather than setting the tile's
												     rhythm itself. No art: the set's printed code, which
												     is the same identity in text. -->
												<span class="symbol">
													{#if set.symbol_url}
														<img src={set.symbol_url} alt="" loading="lazy" />
													{:else}
														<span class="code">{shortCode(set)}</span>
													{/if}
												</span>
												<span class="names">
													<span class="title">{set.name}</span>
													<span class="series">{set.series}</span>
												</span>
											</div>
											{#if set.kind === 'bundle' || set.synthesized}
												<div class="tags">
													{#if set.kind === 'bundle'}
														<Badge tone="neutral" variant="outline" shape="tag" size="sm">
															Bundle
														</Badge>
													{/if}
													{#if set.synthesized}
														<Badge
															tone="warning"
															variant="soft"
															shape="tag"
															size="sm"
															title="Built from TCGCSV — pokemontcg.io has not published this set yet, so its card list and art are provisional."
														>
															TCGCSV
														</Badge>
													{/if}
												</div>
											{/if}
											<div class="meters">
												{#if set.base_total_cards != null && set.base_owned_cards != null}
													<ProgressBar
														tone="complete"
														label="Base {count(set.base_owned_cards)} / {count(
															set.base_total_cards
														)}"
														value={set.base_owned_cards}
														max={set.base_total_cards}
													/>
												{/if}
												<ProgressBar
													tone="complete"
													label="Master {count(set.owned_cards)} / {count(set.total_cards)}"
													value={set.owned_cards}
													max={set.total_cards}
												/>
											</div>
										</Panel>
									{/each}
								</div>
							{/if}
						</section>
					{/each}
				</div>
			</div>
		{/if}
	</div>
{/if}

<style>
	/*
		ONE MEANING PER COLOUR — the mapping this page is now built on. It was
		the noisiest surface in the app because four hues were in view at once
		and none of them meant a single thing: crimson titles beside orange
		titles, a saturated blue section bar, red/green/blue progress bars.

		  crimson (accent)     the app's own voice: the page title, and the
		                       edge a tile takes on hover/focus. Never data.
		  green (progress)     ownership and completion: every meter fill, and
		                       the jump-nav dot that says "you own something in
		                       this series". The only hue that encodes a number.
		  amber (warning)      provisional data: the TCGCSV chip on a set
		                       pokemontcg.io has not published yet.
		  neutral (ink/slate)  structure and content: surfaces, rules, the
		                       group header, meter tracks, names, counts, and
		                       the "Bundle" kind tag. Carries no meaning.

		What that cost, deliberately: the master meter is no longer crimson
		(a number is not the brand), bundles are no longer orange (a kind is a
		word, not a hue), and the full-width blue section bar is now a rule
		under a heading (structure is not a signal). Nothing was removed — the
		bundle hue became a legible tag, so a reader no longer has to have
		learnt what orange meant.

		Only layout and geometry are left in this block. Surfaces, fills,
		rules, text colour, radius, spacing and elevation arrive through the
		semantic token layer or through a primitive that owns them.

		WHERE A PRIMITIVE IS PLACED. Svelte scopes a rule to the elements this
		file declares, so a class handed to a component compiles to a selector
		matching nothing. Placement is written as `:global()` nested under a
		scoped ancestor — `.browsepage` exists to be that ancestor for the
		page's top-level rows. Never a bare `:global()`, which would leak the
		rule to every route.
	*/
	h1 {
		color: var(--color-text-accent);
		margin-bottom: var(--space-1);
	}
	.page {
		display: flex;
		gap: var(--space-4);
		align-items: baseline;
		justify-content: space-between;
	}
	.muted {
		color: var(--color-text-subtle);
	}
	.error {
		color: var(--color-danger-text);
	}
	.browsepage :global(.controls) {
		margin: var(--space-3) var(--space-0);
	}
	.browsepage :global(.search) {
		flex: 1;
		min-width: 220px;
		max-width: 360px;
	}
	.spacer {
		flex: 1;
	}
	.counts {
		color: var(--color-text-subtle);
		font-size: var(--text-md);
	}

	/* Two-column layout: sticky sidebar + main grid area. Sidebar
	   collapses to a horizontal chip row on narrow viewports. */
	.layout {
		display: grid;
		grid-template-columns: 200px 1fr;
		gap: var(--space-5);
		align-items: start;
	}
	.navcol {
		position: sticky;
		top: var(--space-2);
	}
	.navcol :global(.nav) {
		max-height: calc(100vh - var(--space-4));
		overflow-y: auto;
	}
	.nav-label {
		text-transform: uppercase;
		font-size: var(--text-xs);
		letter-spacing: 0.06em;
		color: var(--color-text-subtle);
		padding: var(--space-1) var(--space-1) var(--space-2);
	}
	.navcol ul {
		list-style: none;
		padding: var(--space-0);
		margin: var(--space-0);
		display: flex;
		flex-direction: column;
		gap: var(--space-px);
	}
	.navlink {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		width: 100%;
		background: none;
		border: none;
		color: var(--color-text-subtle);
		padding: var(--space-1);
		border-radius: var(--radius-sm);
		cursor: pointer;
		font: inherit;
		font-size: var(--text-md);
		text-align: left;
	}
	.navlink:hover {
		background: var(--color-surface-hover);
		color: var(--color-text-strong);
	}
	.navlink:focus-visible {
		outline: none;
		box-shadow: var(--shadow-focus);
	}
	.navlink.owned {
		color: var(--color-text);
	}
	.dot {
		width: var(--space-2);
		height: var(--space-2);
		border-radius: var(--radius-round);
		background: var(--color-neutral);
		flex-shrink: 0;
	}
	.navlink.owned .dot {
		background: var(--color-success);
	}
	.navlink .name {
		flex: 1;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.navlink .setn {
		color: var(--color-text-subtle);
		font-variant-numeric: tabular-nums;
		font-size: var(--text-sm);
	}

	.main {
		min-width: 0;
	}
	.seriesgroup {
		margin-bottom: var(--space-6);
	}
	/* The section divider, not a painted band: a rule under a heading. */
	.grouphdr {
		display: flex;
		align-items: baseline;
		gap: var(--space-2);
		width: 100%;
		background: none;
		border: none;
		border-bottom: 1px solid var(--color-border);
		border-radius: var(--radius-sm) var(--radius-sm) 0 0;
		color: var(--color-text);
		padding: var(--space-2);
		font: inherit;
		font-size: var(--text-lg);
		cursor: pointer;
		text-align: left;
	}
	.grouphdr:hover {
		background: var(--color-surface-hover);
	}
	.grouphdr:focus-visible {
		outline: none;
		box-shadow: var(--shadow-focus);
	}
	.caret {
		width: 1ch;
		color: var(--color-text-decorative);
		font-size: var(--text-sm);
	}
	.ghname {
		font-weight: var(--weight-bold);
		color: var(--color-text-strong);
	}
	.ghmeta {
		color: var(--color-text-subtle);
		font-size: var(--text-sm);
		font-variant-numeric: tabular-nums;
	}

	/* GUTTER RHYTHM AND THE RAGGED LAST ROW. Columns are one track repeated,
	   so a tile is the same width in every row of every group — a short final
	   row stops, it does not stretch to fill. `grid-auto-rows: 1fr` gives
	   every row the same height, and the meters block is pinned to the bottom
	   of its tile, so the last bar lands on one line across the whole group
	   whether a set shows one meter or two. Rows are gapped one step wider
	   than columns: the eye reads a row as a row. */
	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(200px, 1fr));
		grid-auto-rows: 1fr;
		column-gap: var(--space-4);
		row-gap: var(--space-5);
		margin-top: var(--space-4);
	}
	/* No `height: 100%` here: a grid item already stretches to its row, and
	   the app sets no global `box-sizing`, so a percentage height would add
	   the tile's padding on top of the row and overflow the group. */
	.grid :global(.tile) {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
	}
	.ident {
		display: flex;
		align-items: flex-start;
		gap: var(--space-2);
		min-width: 0;
	}
	/* Upstream ships the set symbol as a white-on-near-black raster of the
	   set's code — SSP, MEW, TTBB-2024 — at whatever aspect it likes. Dropped
	   straight onto the panel it read as a chip pasted in from another app.
	   The well is the fix: the darkest surface in the ramp, so the raster's
	   own ground melts into it, one square size whatever the art's aspect, and
	   a rule that is the same rule every other box on this page draws. */
	.symbol {
		box-sizing: border-box;
		display: grid;
		place-items: center;
		flex: 0 0 auto;
		width: var(--space-10);
		height: var(--space-10);
		padding: var(--space-1);
		background: var(--color-surface-inset);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-sm);
		overflow: hidden;
	}
	.symbol img {
		max-width: 100%;
		max-height: 100%;
		object-fit: contain;
	}
	.symbol .code {
		font-size: var(--text-xs);
		font-weight: var(--weight-semibold);
		color: var(--color-text-subtle);
		letter-spacing: 0.04em;
	}
	.names {
		display: flex;
		flex-direction: column;
		gap: var(--space-0-5);
		min-width: 0;
	}
	/* Two lines, then an ellipsis — a long set name may not decide how tall
	   every tile in the group is. */
	.title {
		font-weight: var(--weight-semibold);
		color: var(--color-text-strong);
		line-height: var(--leading-tight);
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
		overflow: hidden;
	}
	.series {
		font-size: var(--text-sm);
		color: var(--color-text-subtle);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.tags {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-1);
	}
	.meters {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		margin-top: auto;
	}

	/* Mobile: sidebar becomes a horizontal scrollable chip row above the main. */
	@media (max-width: 720px) {
		.layout {
			grid-template-columns: 1fr;
		}
		.navcol {
			position: static;
		}
		.navcol :global(.nav) {
			max-height: none;
		}
		.navcol ul {
			flex-direction: row;
			overflow-x: auto;
			gap: var(--space-1);
		}
		.navlink {
			flex: 0 0 auto;
			padding: var(--space-1) var(--space-2);
			border: 1px solid var(--color-border);
			border-radius: var(--radius-pill);
		}
		.navlink .setn {
			display: none;
		}
	}
</style>
