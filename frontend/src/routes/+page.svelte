<!--
	The front door.

	It used to be nine nav tiles and 80% empty viewport — a collection tracker
	whose landing page showed no collection. It now opens with the collection
	itself and keeps the nav below it.

	Where the numbers come from, and why (pd-glae is presentation work — no new
	backend surface was added for it):

	  headline figures  /api/collection/value-history?dimension=all, latest
	                    point of EACH series it returns. This is the app's OWN
	                    canonical answer for "what is the collection worth" —
	                    the same series the /collection value chart draws,
	                    condition-adjusted server-side. It is a snapshot table,
	                    written by the nightly transform, so the date it was
	                    valued on is printed beside it rather than implied to
	                    be now.

	                    That dimension answers with the collection's two priced
	                    halves — the loose cards and the sealed product — and
	                    the headline is their SUM, added here (pd-bbv7). There
	                    is no stored combined total on purpose: a stored total
	                    is a third number that can disagree with the two it is
	                    made of. The split is printed under the figure, because
	                    a total that does not say which half is which is how
	                    half a collection stayed invisible.
	  set completion    /api/sets, the same summaries /browse tiles.
	  recent additions  the newest ingest batches + their cards, which is what
	                    /recent already means by "activity". Deliberately NOT
	                    /api/collection: that is every copy the user owns
	                    (~3MB on a real collection) to render twelve thumbnails.
-->
<script lang="ts">
	import { onMount } from 'svelte';
	import Pokeball from '$lib/components/Pokeball.svelte';
	import { api } from '$lib/api';
	import { money, count } from '$lib/format';
	import { Badge, Button, EmptyState, Panel, ProgressBar, SectionHeader } from '$lib/components/ui';
	import type { CollectionRow } from '$lib/types/CollectionRow';
	import type { SetSummary } from '$lib/types/SetSummary';
	import type { ValuePoint } from '$lib/types/ValuePoint';

	const sections = [
		{ href: '/collection', label: 'Collection', desc: 'Browse every card you own.' },
		{ href: '/browse', label: 'Browse', desc: 'Open sets as virtual binder pages.' },
		{ href: '/binders', label: 'Binders', desc: 'Custom binders and their pages.' },
		{ href: '/decks', label: 'Decks', desc: 'Saved decklists.' },
		{ href: '/sealed', label: 'Sealed', desc: 'Sealed product (boxes, packs, ETBs).' },
		{ href: '/wishlist', label: 'Wishlist', desc: 'Cards you want.' },
		{ href: '/orders', label: 'Orders', desc: 'Purchases and their attached cards.' },
		{ href: '/recent', label: 'Recent', desc: 'Latest collection activity.' },
		{ href: '/ingest/csv', label: 'Import', desc: 'Bulk-import via CSV.' }
	];

	/** Batches to pull cards from, and the number of thumbnails to keep. 12
	 *  fills exactly two rows of the six-column strip below — a ragged final
	 *  row is half of what this page was filed for. */
	const RECENT_BATCHES = 4;
	const RECENT_CARDS = 12;
	/** Set-completion rows in the sidebar — enough to stand beside the two-row
	 *  thumbnail strip rather than leave the column half-empty. */
	const HIGHLIGHT_SETS = 8;

	/** The latest point of each half, both on the SAME date — see below. */
	let latest = $state<ValuePoint | null>(null);
	let sealedLatest = $state<ValuePoint | null>(null);
	let valuedOn = $state<string | null>(null);
	let sets = $state<SetSummary[]>([]);
	let recent = $state<CollectionRow[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	onMount(async () => {
		try {
			const [series, allSets, batches] = await Promise.all([
				api.valueHistory('all'),
				api.sets(),
				api.batches(RECENT_BATCHES)
			]);
			// The `all` dimension answers with the loose cards (bucket null) and,
			// when the tenant owns any, the sealed product. Told apart by
			// `bucket`, never by order.
			//
			// Both halves are read at ONE date — the newest either reports —
			// rather than each at its own latest point. Two halves valued on
			// two days would add up to a number that was never true on either.
			// A half with no row on that date contributes nothing and shows as
			// absent in the split below.
			const cards = series.find((s) => s.bucket == null);
			const sealed = series.find((s) => s.bucket === 'sealed');
			const lastDates = [cards?.points.at(-1)?.date, sealed?.points.at(-1)?.date].filter(
				(d): d is string => d != null
			);
			valuedOn = lastDates.sort().at(-1) ?? null;
			latest = cards?.points.find((p) => p.date === valuedOn) ?? null;
			sealedLatest = sealed?.points.find((p) => p.date === valuedOn) ?? null;
			sets = allSets;
			// Batches arrive newest-first, so flattening preserves recency.
			const details = await Promise.all(batches.map((b) => api.batchDetail(b.id)));
			recent = details.flatMap((d) => d.cards).slice(0, RECENT_CARDS);
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	});

	// Open dead-letter count — shows the "Unresolved" card only when there's a
	// backlog to work through. (pokedumpster-oq3i.5)
	let openCount = $state(0);
	$effect(() => {
		api.unresolvedList()
			.then((r) => (openCount = r.length))
			.catch(() => {});
	});

	/** Bundles are logical containers, not sets you can complete. */
	const realSets = $derived(sets.filter((s) => s.kind !== 'bundle'));
	const setsStarted = $derived(realSets.filter((s) => s.owned_cards > 0).length);

	/**
	 * Completion per set, measured on the base set where the catalog knows a
	 * printed total (so secret rares don't make a finished set read 84%), and
	 * on everything catalogued where it doesn't — the same rule /browse's
	 * tiles use.
	 */
	const highlights = $derived.by(() => {
		const rows = realSets
			.map((s) => {
				const base = s.base_total_cards;
				const total = base ?? s.total_cards;
				const owned = base != null ? (s.base_owned_cards ?? 0) : s.owned_cards;
				return { set: s, owned, total, pct: total > 0 ? owned / total : 0 };
			})
			.filter((r) => r.owned > 0);
		rows.sort((a, b) => b.pct - a.pct || b.owned - a.owned);
		return rows.slice(0, HIGHLIGHT_SETS);
	});

	/**
	 * The split, under the figures it explains. Built here rather than in the
	 * markup so a collection with no sealed product renders EXACTLY the string
	 * it always did — this page is screenshotted, and a conditional block in
	 * the template would move pixels for every tenant who owns no sealed
	 * product.
	 */
	const ownedMeta = $derived(
		(sealedLatest ? `${count(sealedLatest.card_count)} sealed · ` : '') +
			`${count(setsStarted)} of ${count(realSets.length)} sets started`
	);
	const valueMeta = $derived(
		sealedLatest
			? `cards ${money(latest?.market_value ?? 0)} · sealed ${money(sealedLatest.market_value)}`
			: 'condition-adjusted'
	);

	/** Both halves, summed at read time. The headline is the whole collection. */
	const marketValue = $derived((latest?.market_value ?? 0) + (sealedLatest?.market_value ?? 0));
	const costBasis = $derived((latest?.cost_basis ?? 0) + (sealedLatest?.cost_basis ?? 0));
	const unrealized = $derived(valuedOn == null ? null : marketValue - costBasis);
	/** Nothing snapshotted, or nothing owned — a genuinely empty collection. */
	const started = $derived(
		valuedOn != null && (latest?.card_count ?? 0) + (sealedLatest?.card_count ?? 0) > 0
	);
</script>

<svelte:head><title>PokeDumpster</title></svelte:head>

<header class="hero">
	<span class="logo"><Pokeball size={56} /></span>
	<div>
		<h1>PokeDumpster</h1>
		<p class="tagline">A Pokémon TCG collection tracker.</p>
	</div>
</header>

{#if error}
	<Panel variant="sunken" padding="sm">
		<p class="error">Couldn’t load collection stats — {error}</p>
	</Panel>
{/if}

<SectionHeader title="Your collection" meta={valuedOn ? `valued ${valuedOn}` : undefined} divider>
	{#snippet actions()}
		<Button variant="link" href="/collection">Open collection</Button>
	{/snippet}
</SectionHeader>

{#if started}
	<div class="stats">
		<Panel padding="md">
			<div class="stat">
				<span class="stat-label">Cards owned</span>
				<strong class="stat-figure">{count(latest?.card_count ?? 0)}</strong>
				<span class="stat-meta">{ownedMeta}</span>
			</div>
		</Panel>
		<Panel padding="md">
			<div class="stat">
				<span class="stat-label">Market value</span>
				<strong class="stat-figure">{money(marketValue)}</strong>
				<span class="stat-meta">{valueMeta}</span>
			</div>
		</Panel>
		<Panel padding="md">
			<div class="stat">
				<span class="stat-label">Cost basis</span>
				<strong class="stat-figure">{money(costBasis)}</strong>
				<span class="stat-meta">what you paid</span>
			</div>
		</Panel>
		<Panel padding="md">
			<div class="stat">
				<span class="stat-label">Unrealised</span>
				<span class="stat-figure">
					{#if unrealized != null}
						<Badge tone={unrealized < 0 ? 'danger' : 'success'}>
							{unrealized < 0 ? '−' : '+'}{money(Math.abs(unrealized))}
						</Badge>
					{/if}
				</span>
				<span class="stat-meta">market less cost</span>
			</div>
		</Panel>
	</div>
{:else if !loading}
	<Panel padding="md">
		<EmptyState
			title="No cards registered yet"
			description="Import a CSV export from another tracker, or open a set as a binder page and click the cards you own."
		>
			{#snippet action()}
				<Button href="/ingest/csv">Import a CSV</Button>
			{/snippet}
		</EmptyState>
	</Panel>
{/if}

<div class="split">
	<section>
		<SectionHeader title="Recent additions" divider>
			{#snippet actions()}
				<Button variant="link" href="/recent">All activity</Button>
			{/snippet}
		</SectionHeader>
		<Panel padding="md">
			{#if recent.length > 0}
				<ul class="thumbs">
					{#each recent as card (card.id)}
						<li>
							<a href="/card/{card.set_code}/{card.number}">
								{#if card.image_small}
									<img src={card.image_small} alt="" loading="lazy" />
								{:else}
									<span class="noart" aria-hidden="true"></span>
								{/if}
								<span class="thumb-name">{card.name}</span>
								<span class="thumb-meta">{card.set_name} · {card.number}</span>
							</a>
						</li>
					{/each}
				</ul>
			{:else if !loading}
				<EmptyState
					size="sm"
					title="Nothing added yet"
					description="Cards you register show up here, newest first."
				/>
			{/if}
		</Panel>
	</section>

	<section>
		<SectionHeader title="Set completion" divider>
			{#snippet actions()}
				<Button variant="link" href="/browse">Browse sets</Button>
			{/snippet}
		</SectionHeader>
		<Panel padding="md">
			{#if highlights.length > 0}
				<ul class="sets">
					{#each highlights as h (h.set.set_code)}
						<li>
							<div class="set-head">
								<a href="/browse/{h.set.set_code}">{h.set.name}</a>
								{#if h.pct >= 1}
									<Badge tone="success" size="sm">Complete</Badge>
								{/if}
							</div>
							<!-- `complete`, the green ownership fill /browse already paints
							     its base-set bar with: one meaning per colour, and crimson
							     is the master-set bar next door. -->
							<ProgressBar
								value={h.owned}
								max={h.total}
								tone="complete"
								label="{count(h.owned)} / {count(h.total)}"
							/>
						</li>
					{/each}
				</ul>
			{:else if !loading}
				<EmptyState
					size="sm"
					title="No set started"
					description="Open a set as a binder page to start filling it in."
				/>
			{/if}
		</Panel>
	</section>
</div>

<SectionHeader title="Sections" divider />

<nav class="capabilities" aria-label="Sections">
	{#each sections as s (s.href)}
		<Panel href={s.href} padding="md">
			<span class="cap">
				<span class="cap-label">{s.label}</span>
				<span class="cap-desc">{s.desc}</span>
			</span>
		</Panel>
	{/each}
	{#if openCount > 0}
		<Panel href="/ingest/unresolved" padding="md">
			<span class="cap">
				<span class="cap-label">
					Unresolved <Badge tone="warning" variant="solid" size="sm">{openCount}</Badge>
				</span>
				<span class="cap-desc">Import rows waiting to be matched.</span>
			</span>
		</Panel>
	{/if}
</nav>

<style>
	.hero {
		display: flex;
		align-items: center;
		gap: var(--space-4);
		margin-bottom: var(--space-6);
	}
	.hero h1 {
		margin: var(--space-0);
		font-size: var(--text-3xl);
		line-height: var(--leading-tight);
		color: var(--color-text-accent);
	}
	.tagline {
		margin: var(--space-1) var(--space-0) var(--space-0);
		color: var(--color-text-subtle);
		font-size: var(--text-lg);
	}
	.logo {
		display: block;
		flex-shrink: 0;
	}

	.error {
		margin: var(--space-0);
		font-size: var(--text-md);
		color: var(--color-danger-text);
	}

	/* --- Headline figures --------------------------------------------------
	   Four equal columns, never auto-fit: half the complaint about this page
	   was that its tiles took whatever width the track algorithm handed them. */
	.stats {
		display: grid;
		grid-template-columns: repeat(4, minmax(0, 1fr));
		gap: var(--space-3);
	}
	.stat {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
	}
	.stat-label {
		font-size: var(--text-sm);
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--color-text-subtle);
	}
	/* Fixed line box so the Badge in the "Unrealised" tile sits on the same
	   baseline as the three plain figures beside it. */
	.stat-figure {
		display: flex;
		align-items: center;
		min-height: calc(var(--text-2xl) * var(--leading-tight));
		font-size: var(--text-2xl);
		font-weight: var(--weight-semibold);
		line-height: var(--leading-tight);
		color: var(--color-text-strong);
	}
	.stat-meta {
		font-size: var(--text-md);
		color: var(--color-text-subtle);
	}

	/* --- The two content columns ------------------------------------------ */
	.split {
		display: grid;
		grid-template-columns: 2fr 1fr;
		gap: var(--space-6);
		align-items: start;
	}

	/* --- Recent additions -------------------------------------------------- */
	.thumbs {
		display: grid;
		grid-template-columns: repeat(6, minmax(0, 1fr));
		gap: var(--space-3);
		margin: var(--space-0);
		padding: var(--space-0);
		list-style: none;
	}
	.thumbs a {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		text-decoration: none;
		color: var(--color-text);
	}
	.thumbs img,
	.noart {
		display: block;
		width: 100%;
		aspect-ratio: 245 / 342;
		border-radius: var(--radius-md);
		background: var(--color-surface-sunken);
		object-fit: cover;
		transition: transform var(--dur-base) var(--ease-standard);
	}
	.thumbs a:hover img,
	.thumbs a:hover .noart {
		transform: translateY(-2px);
	}
	.thumbs a:focus-visible {
		outline: none;
		border-radius: var(--radius-md);
		box-shadow: var(--shadow-focus);
	}
	/* One line each, always — a wrapping card name is what made the old tiles
	   different heights from one another. */
	.thumb-name,
	.thumb-meta {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.thumb-name {
		font-size: var(--text-md);
		font-weight: var(--weight-medium);
	}
	.thumb-meta {
		font-size: var(--text-xs);
		color: var(--color-text-subtle);
	}

	/* --- Set completion ---------------------------------------------------- */
	.sets {
		display: flex;
		flex-direction: column;
		gap: var(--space-4);
		margin: var(--space-0);
		padding: var(--space-0);
		list-style: none;
	}
	.set-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-2);
		margin-bottom: var(--space-1);
		min-width: 0;
	}
	.set-head a {
		overflow: hidden;
		font-size: var(--text-lg);
		color: var(--color-link);
		text-decoration: none;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.set-head a:hover {
		color: var(--color-link-hover);
	}

	/* --- Sections ---------------------------------------------------------- */
	.capabilities {
		display: grid;
		grid-template-columns: repeat(5, minmax(0, 1fr));
		grid-auto-rows: 1fr;
		gap: var(--space-3);
	}
	.cap {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
	}
	.cap-label {
		font-weight: var(--weight-semibold);
		color: var(--color-text-accent);
	}
	.cap-desc {
		font-size: var(--text-md);
		color: var(--color-text-subtle);
	}

	/* --- Narrower viewports ------------------------------------------------
	   Explicit column counts at every step, for the same reason as above. */
	@media (max-width: 1100px) {
		.stats {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
		.split {
			grid-template-columns: 1fr;
			gap: var(--space-4);
		}
		/* The strip keeps its six columns here — it has the full width to
		   itself once the split collapses, so widening the cells would only
		   add half a screen of scroll. */
		.capabilities {
			grid-template-columns: repeat(3, minmax(0, 1fr));
		}
	}
	@media (max-width: 640px) {
		.thumbs {
			grid-template-columns: repeat(4, minmax(0, 1fr));
		}
		.capabilities {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
	}
</style>
