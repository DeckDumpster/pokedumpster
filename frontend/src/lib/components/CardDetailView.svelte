<script lang="ts">
	import { api, variantLabel } from '$lib/api';
	import type { CardDetail } from '$lib/types/CardDetail';
	import type { Binder } from '$lib/types/Binder';
	import type { Deck } from '$lib/types/Deck';
	import type { PriceSeries } from '$lib/types/PriceSeries';
	import PriceChart from './PriceChart.svelte';

	// The card-detail body, shared by the /card/[set]/[number] route and the
	// collection-page modal. Self-contained: it fetches its own data.
	let {
		setCode,
		number,
		onNavigate
	}: {
		setCode: string;
		number: string;
		/** Switch this view to a different (set, number) — wired by the
		 *  collection modal to support evolution-chain links without
		 *  closing/reopening. Falls back to a full-page nav if absent. */
		onNavigate?: (set: string, number: string) => void;
	} = $props();

	let detail = $state<CardDetail | null>(null);
	let binders = $state<Binder[]>([]);
	let decks = $state<Deck[]>([]);
	let priceSeries = $state<PriceSeries[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let busy = $state(false);

	const STATUSES = ['owned', 'ordered', 'listed', 'sold', 'removed', 'traded', 'gifted', 'lost'];

	async function load() {
		if (!setCode || !number) return;
		loading = true;
		error = null;
		try {
			[detail, binders, decks, priceSeries] = await Promise.all([
				api.card(setCode, number),
				api.binders(),
				api.decks(),
				api.cardPrices(setCode, number)
			]);
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void setCode;
		void number;
		load();
	});

	async function withBusy(fn: () => Promise<unknown>) {
		busy = true;
		error = null;
		try {
			await fn();
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}

	const addCopy = (printingId: string) =>
		withBusy(() => api.addCopy({ printing_id: printingId, source: 'manual' }));
	const removeCopy = (printingId: string) =>
		withBusy(() => api.removeCopyByPrinting(printingId));
	const changeVariant = (copyId: number, printingId: string) =>
		withBusy(() => api.changePrinting(copyId, printingId));
	const changeStatus = (copyId: number, status: string) =>
		withBusy(() => api.setCopyStatus(copyId, status));

	function assignValue(copy: { binder_id: number | null; deck_id: number | null }): string {
		if (copy.binder_id != null) return `b:${copy.binder_id}`;
		if (copy.deck_id != null) return `d:${copy.deck_id}`;
		return '';
	}
	function assignCopy(copyId: number, value: string) {
		const body = value.startsWith('b:')
			? { binder_id: Number(value.slice(2)) }
			: value.startsWith('d:')
				? { deck_id: Number(value.slice(2)) }
				: {};
		return withBusy(() => api.moveCopy(copyId, body));
	}

	function parseStrArr(raw: string | null): string[] {
		if (!raw) return [];
		try {
			const v: unknown = JSON.parse(raw);
			return Array.isArray(v) ? v.map(String) : [];
		} catch {
			return [];
		}
	}
	function parseObjArr<T = Record<string, unknown>>(raw: string | null): T[] {
		if (!raw) return [];
		try {
			const v: unknown = JSON.parse(raw);
			return Array.isArray(v) ? (v as T[]) : [];
		} catch {
			return [];
		}
	}
	function price(p: number | null): string {
		return p == null ? '—' : `$${p.toFixed(2)}`;
	}

	// Pokémon energy-type icons, served from /static/energy. "Free" (a
	// zero-energy attack cost rendered on the card art as a clear circle)
	// falls back to colorless; any other unknown type does too rather than
	// 404 → broken-image.
	const KNOWN_ENERGY = new Set([
		'grass',
		'fire',
		'water',
		'lightning',
		'psychic',
		'fighting',
		'darkness',
		'metal',
		'fairy',
		'dragon',
		'colorless'
	]);
	function energyIcon(type: string): string {
		const t = type.toLowerCase();
		return `/energy/${KNOWN_ENERGY.has(t) ? t : 'colorless'}.png`;
	}

	// Rarity-glyph SVG slug; mirrors the collection page's rarityIconSrc.
	// "Special Illustration Rare" → /rarity/special-illustration-rare.svg.
	function rarityIconSrc(rarity: string | null): string | null {
		if (!rarity) return null;
		return `/rarity/${rarity.toLowerCase().replace(/[ ._]/g, '-')}.svg`;
	}

	/** Link a metadata cell (artist, set, rarity, type, variant, …) to a
	 *  pre-filled /collection?q= search. The collection page's `rowMatches`
	 *  does case-insensitive substring across all the facets so the bare
	 *  value works as-is. */
	function facetHref(value: string): string {
		return `/collection?q=${encodeURIComponent(value)}`;
	}

	type AttackData = {
		name?: string;
		cost?: string[];
		damage?: string;
		text?: string;
	};
	type AbilityData = { name?: string; type?: string; text?: string };
	type WrData = { type?: string; value?: string };

	// Evolution-link navigation. Resolves a card name to its newest printing
	// via /api/cards/by-name, then switches the modal (or navigates the
	// route page) to that card.
	async function gotoCard(name: string) {
		try {
			const ref = await api.cardByName(name);
			if (onNavigate) onNavigate(ref.set_code, ref.number);
			else window.location.assign(`/card/${ref.set_code}/${ref.number}`);
		} catch (e) {
			error = `No card named "${name}" in catalog`;
		}
	}
</script>

<svelte:head>
	<title>{detail ? detail.card.name : 'Card'} — PokeDumpster</title>
</svelte:head>

{#if loading && !detail}
	<p class="muted">Loading…</p>
{:else if error && !detail}
	<p class="error">Failed to load card: {error}</p>
{:else if detail}
	{@const card = detail.card}

	<div class="detail">
		<div class="art">
			{#if card.image_large}
				<img src={card.image_large} alt={card.name} />
			{:else}
				<div class="noart">No image</div>
			{/if}
		</div>
		<div class="info">
			<h1>{card.name}</h1>
			<p class="sub">
				<a
					class="facet"
					href={facetHref((card.set_ptcgo_code ?? card.set_code).toUpperCase())}
					title="Filter collection by {(card.set_ptcgo_code ?? card.set_code).toUpperCase()}"
				>
					{#if card.set_symbol_url}
						<img
							class="setsym"
							src={card.set_symbol_url}
							alt={card.set_code}
							title="{card.set_name} ({card.set_code})"
						/>
					{/if}
					<span>{(card.set_ptcgo_code ?? card.set_code).toUpperCase()}</span>
				</a>
				· #{card.number}{#if card.rarity}
					·
					<a class="facet" href={facetHref(card.rarity)} title="Filter collection by {card.rarity}">
						{#if rarityIconSrc(card.rarity)}
							<img class="raritysym" src={rarityIconSrc(card.rarity)} alt="" />
						{/if}
						{card.rarity}
					</a>{/if}
			</p>
			<dl>
				{#if card.supertype}<dt>Type</dt><dd>
						<a class="facet" href={facetHref(card.supertype)}>{card.supertype}</a>{#if parseStrArr(card.subtypes).length}
							·
							{#each parseStrArr(card.subtypes) as st (st)}
								<a class="facet" href={facetHref(st)}>{st}</a>
							{/each}
						{/if}
					</dd>{/if}
				{#if card.hp != null}<dt>HP</dt><dd>{card.hp}</dd>{/if}
				{#if parseStrArr(card.types).length}
					<dt>Element</dt>
					<dd class="enr">
						{#each parseStrArr(card.types) as t (t)}
							<a class="facet" href={facetHref(t)} title="Filter collection by {t}">
								<img class="energy" src={energyIcon(t)} alt={t} title={t} />
							</a>
						{/each}
					</dd>
				{/if}
				{#if card.regulation_mark}<dt>Regulation</dt><dd>{card.regulation_mark}</dd>{/if}
				{#if card.artist}<dt>Artist</dt><dd>
						<a class="facet" href={facetHref(card.artist)}>{card.artist}</a>
					</dd>{/if}

				{#if card.evolves_from}
					<dt>Evolves from</dt>
					<dd>
						<button class="evolink" onclick={() => gotoCard(card.evolves_from!)}>
							{card.evolves_from}
						</button>
					</dd>
				{/if}
				{#if parseStrArr(card.evolves_to).length}
					<dt>Evolves to</dt>
					<dd>
						{#each parseStrArr(card.evolves_to) as name, i (name)}
							{#if i > 0},
							{/if}
							<button class="evolink" onclick={() => gotoCard(name)}>{name}</button>
						{/each}
					</dd>
				{/if}
			</dl>

			{#if parseObjArr<AbilityData>(card.abilities).length > 0}
				<section class="cardSection">
					<h2>Abilities</h2>
					{#each parseObjArr<AbilityData>(card.abilities) as ab, i (i)}
						<div class="abilityBlock">
							<div class="abilityHead">
								{#if ab.type}<span class="abilityType">{ab.type}</span>{/if}
								<span class="abilityName">{ab.name ?? ''}</span>
							</div>
							{#if ab.text}<p class="cardText">{ab.text}</p>{/if}
						</div>
					{/each}
				</section>
			{/if}

			{#if parseObjArr<AttackData>(card.attacks).length > 0}
				<section class="cardSection">
					<h2>Attacks</h2>
					{#each parseObjArr<AttackData>(card.attacks) as att, i (i)}
						<div class="attackBlock">
							<div class="attackHead">
								<span class="attackCost">
									{#each att.cost ?? [] as c, i (i)}
										<img class="energy" src={energyIcon(c)} alt={c} title={c} />
									{/each}
								</span>
								<span class="attackName">{att.name ?? ''}</span>
								{#if att.damage}<span class="attackDamage">{att.damage}</span>{/if}
							</div>
							{#if att.text}<p class="cardText">{att.text}</p>{/if}
						</div>
					{/each}
				</section>
			{/if}

			<!-- Always render the W/R/R block so the layout stays consistent
			     across cards — Resistance and Retreat cells appear even when
			     empty (rendered as '—') rather than dropping out entirely. -->
			<section class="cardSection combat">
				<div class="combatCell">
					<h3>Weakness</h3>
					{#if parseObjArr<WrData>(card.weaknesses).length > 0}
						{#each parseObjArr<WrData>(card.weaknesses) as w (w.type)}
							<span class="wr">
								{#if w.type}<img class="energy" src={energyIcon(w.type)} alt={w.type} title={w.type} />{/if}
								{w.value ?? ''}
							</span>
						{/each}
					{:else}
						<span class="wr-empty">—</span>
					{/if}
				</div>
				<div class="combatCell">
					<h3>Resistance</h3>
					{#if parseObjArr<WrData>(card.resistances).length > 0}
						{#each parseObjArr<WrData>(card.resistances) as r (r.type)}
							<span class="wr">
								{#if r.type}<img class="energy" src={energyIcon(r.type)} alt={r.type} title={r.type} />{/if}
								{r.value ?? ''}
							</span>
						{/each}
					{:else}
						<span class="wr-empty">—</span>
					{/if}
				</div>
				<div class="combatCell">
					<h3>Retreat</h3>
					{#if parseStrArr(card.retreat_cost).length > 0}
						<span class="retreat">
							{#each parseStrArr(card.retreat_cost) as c, i (i)}
								<img class="energy" src={energyIcon(c)} alt={c} title={c} />
							{/each}
						</span>
					{:else}
						<span class="wr-empty">—</span>
					{/if}
				</div>
			</section>

			{#if card.flavor_text}<p class="flavor">{card.flavor_text}</p>{/if}
		</div>
	</div>

	{#if error}<p class="error">{error}</p>{/if}

	<section>
		<h2>Printings</h2>
		<ul class="printings">
			{#each detail.printings.filter((p) => !p.deprecated || p.owned_count > 0) as p (p.printing_id)}
				<li class:dim={p.deprecated}>
					<a class="facet variant" href={facetHref(variantLabel(p.variant))}>
						{variantLabel(p.variant)}
					</a>
					<span class="market">{price(p.market_price)}</span>
					{#if p.tcgplayer_product_id != null}
						<a
							class="tcgp"
							href="https://www.tcgplayer.com/product/{p.tcgplayer_product_id}"
							target="_blank"
							rel="noopener"
							title="Open on TCGplayer"
						>TCG↗</a>
					{:else}
						<span class="tcgp-spacer"></span>
					{/if}
					<div class="stepper">
						<button
							class="step"
							disabled={busy || p.owned_count <= 0}
							onclick={() => removeCopy(p.printing_id)}
							aria-label="Remove one {variantLabel(p.variant)}"
						>−</button>
						<span class="count" class:has={p.owned_count > 0}>{p.owned_count}</span>
						<button
							class="step"
							disabled={busy || p.deprecated}
							onclick={() => addCopy(p.printing_id)}
							aria-label="Add one {variantLabel(p.variant)}"
						>+</button>
					</div>
				</li>
			{/each}
		</ul>
	</section>

	<section>
		<h2>Price history</h2>
		<PriceChart series={priceSeries} />
	</section>

	<section>
		<h2>Your copies ({detail.copies.length})</h2>
		{#if detail.copies.length === 0}
			<p class="muted">You don't own this card yet.</p>
		{:else}
			<table>
				<thead>
					<tr><th>Variant</th><th>Condition</th><th>Status</th><th>Location</th><th>Paid</th></tr>
				</thead>
				<tbody>
					{#each detail.copies as copy (copy.id)}
						<tr>
							<td data-label="Variant">
								<select
									value={copy.printing_id}
									disabled={busy}
									onchange={(e) => changeVariant(copy.id, e.currentTarget.value)}
								>
									{#each detail.printings as p (p.printing_id)}
										<option value={p.printing_id}>{variantLabel(p.variant)}</option>
									{/each}
								</select>
							</td>
							<td data-label="Condition">{copy.condition}</td>
							<td data-label="Status">
								<select
									value={copy.status}
									disabled={busy}
									onchange={(e) => changeStatus(copy.id, e.currentTarget.value)}
								>
									{#each STATUSES as s (s)}<option value={s}>{s}</option>{/each}
								</select>
							</td>
							<td data-label="Location">
								<select
									value={assignValue(copy)}
									disabled={busy}
									onchange={(e) => assignCopy(copy.id, e.currentTarget.value)}
								>
									<option value="">Unassigned</option>
									{#each binders as b (b.id)}<option value="b:{b.id}">Binder: {b.name}</option>{/each}
									{#each decks as d (d.id)}<option value="d:{d.id}">Deck: {d.name}</option>{/each}
								</select>
							</td>
							<td data-label="Paid">{price(copy.purchase_price)}</td>
						</tr>
					{/each}
				</tbody>
			</table>
		{/if}
	</section>
{/if}

<style>
	.muted {
		color: #888;
	}
	.error {
		color: #e94560;
	}
	.detail {
		display: flex;
		gap: 1.5rem;
		flex-wrap: wrap;
		/* Centers each flex row, so when .info wraps below .art the lone
		   card image sits in the middle of the viewport (mirrors how DD
		   centers its card-image-section). */
		justify-content: center;
	}
	.art img {
		width: 320px;
		max-width: 80vw;
		border-radius: 12px;
	}
	.noart {
		width: 320px;
		height: 446px;
		display: flex;
		align-items: center;
		justify-content: center;
		background: #16213e;
		border-radius: 12px;
		color: #888;
	}
	.info {
		flex: 1;
		min-width: 260px;
	}
	/* Inside .info: abilities, attacks, weakness/resistance, flavor sit
	   next to the card art on wide viewports (and wrap below it on narrow
	   via the parent's flex-wrap). Tighter top margin than top-level
	   sections so the right column doesn't gap out. */
	.cardSection {
		margin-top: 1.2rem;
	}
	h1 {
		color: #e94560;
		margin: 0;
	}
	.sub {
		color: #888;
		margin: 0.25rem 0 1rem;
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		flex-wrap: wrap;
	}
	.sub .setsym {
		width: 22px;
		height: 22px;
		object-fit: contain;
		vertical-align: middle;
	}
	.sub .raritysym {
		width: 16px;
		height: 16px;
		object-fit: contain;
		vertical-align: middle;
	}
	dl {
		display: grid;
		grid-template-columns: auto 1fr;
		gap: 0.3rem 1rem;
		margin: 0;
	}
	dt {
		color: #888;
		font-size: 0.85rem;
	}
	dd {
		margin: 0;
	}
	.enr {
		display: inline-flex;
		gap: 0.25rem;
		align-items: center;
	}
	.energy {
		width: 18px;
		height: 18px;
		vertical-align: middle;
	}
	/* Clickable facet — set glyph, rarity, type, artist, variant. Looks
	   like body text by default; hover hints the link affordance. */
	.facet {
		color: inherit;
		text-decoration: none;
		cursor: pointer;
		border-radius: 4px;
		padding: 1px 3px;
	}
	.facet:hover {
		background: rgba(233, 69, 96, 0.18);
		color: #e0e0e0;
	}
	.facet:hover .energy,
	.facet:hover .setsym,
	.facet:hover .raritysym {
		filter: brightness(1.2);
	}
	.evolink {
		background: none;
		border: none;
		color: #e94560;
		cursor: pointer;
		font: inherit;
		padding: 0;
		text-decoration: underline dotted;
	}
	.evolink:hover {
		color: #ff6b85;
	}
	.flavor {
		font-style: italic;
		color: #aaa;
		margin: 1.5rem 0 0;
	}
	section {
		margin-top: 2rem;
	}
	h2 {
		color: #e94560;
		font-size: 1.1rem;
		margin: 0 0 0.4rem;
	}
	h3 {
		color: #888;
		font-size: 0.75rem;
		text-transform: uppercase;
		margin: 0 0 0.3rem;
	}

	/* Abilities + attacks: compact card-style blocks. */
	.abilityBlock,
	.attackBlock {
		border-left: 3px solid #0f3460;
		padding: 0.4rem 0.7rem;
		margin-bottom: 0.6rem;
	}
	.abilityHead,
	.attackHead {
		display: flex;
		gap: 0.5rem;
		align-items: center;
		font-weight: 600;
	}
	.abilityType {
		background: #5c3a1a;
		color: #f0c878;
		font-size: 0.7rem;
		text-transform: uppercase;
		padding: 1px 5px;
		border-radius: 3px;
	}
	.abilityName,
	.attackName {
		color: #e0e0e0;
	}
	.attackCost {
		display: inline-flex;
		gap: 2px;
	}
	.attackDamage {
		margin-left: auto;
		color: #e94560;
		font-weight: 700;
		font-variant-numeric: tabular-nums;
	}
	.cardText {
		margin: 0.25rem 0 0;
		color: #ccc;
		font-size: 0.88rem;
		line-height: 1.4;
	}

	/* Weakness / Resistance / Retreat: three small cells side by side. */
	.combat {
		display: flex;
		gap: 1.5rem;
		flex-wrap: wrap;
	}
	.combatCell {
		min-width: 90px;
	}
	.wr,
	.retreat {
		display: inline-flex;
		gap: 0.2rem;
		align-items: center;
		font-size: 0.95rem;
		color: #ddd;
	}
	.wr-empty {
		color: #555;
		font-size: 0.95rem;
	}

	/* Printings list — one row per variant with [- N +] stepper, mirroring
	   the browse-page VariantModal so the same muscle memory works here. */
	.printings {
		list-style: none;
		padding: 0;
		margin: 0;
		max-width: 480px;
	}
	.printings li {
		display: grid;
		grid-template-columns: 1fr auto auto auto;
		align-items: center;
		gap: 0.75rem;
		padding: 0.45rem 0;
		border-bottom: 1px solid #0f3460;
	}
	.printings li.dim {
		opacity: 0.5;
	}
	.printings .variant {
		color: #e0e0e0;
	}
	.printings .market {
		color: #888;
		font-size: 0.85rem;
		font-variant-numeric: tabular-nums;
	}
	.tcgp-spacer {
		display: inline-block;
		width: 2.6rem;
	}
	.stepper {
		display: flex;
		align-items: center;
	}
	.step {
		background: #0f3460;
		border: none;
		color: #e0e0e0;
		width: 30px;
		height: 30px;
		cursor: pointer;
		font-size: 1.1rem;
		line-height: 1;
		padding: 0;
		border-radius: 0;
	}
	.step:first-child {
		border-radius: 6px 0 0 6px;
	}
	.step:last-child {
		border-radius: 0 6px 6px 0;
	}
	.step:hover:not(:disabled) {
		background: #e94560;
	}
	.step:disabled {
		opacity: 0.35;
		cursor: default;
	}
	.count {
		min-width: 34px;
		height: 30px;
		line-height: 30px;
		text-align: center;
		font-size: 0.9rem;
		color: #888;
		background: #1a1a2e;
	}
	.count.has {
		color: #9fe7a0;
	}

	table {
		width: 100%;
		max-width: 640px;
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
	select {
		background: #1a1a2e;
		border: 1px solid #0f3460;
		color: #e0e0e0;
		border-radius: 6px;
		padding: 0.15rem;
		font: inherit;
	}
	.tcgp {
		font-size: 0.8rem;
		color: #4a8df0;
		text-decoration: none;
	}
	.tcgp:hover {
		color: #e94560;
	}
	button {
		background: #e94560;
		border: none;
		color: #fff;
		padding: 0.25rem 0.6rem;
		border-radius: 6px;
		cursor: pointer;
		font-size: 0.8rem;
	}
	button:disabled {
		opacity: 0.5;
		cursor: default;
	}

	/* On a phone the data tables reflow to stacked label:value blocks so
	   they fit the modal instead of forcing it wider. */
	@media (max-width: 540px) {
		/* Stack art over info on phones (mirrors DD's
		   card-detail-layout). Setting flex-direction: column means the
		   row-wise min-width of .info never wedges info beside art —
		   info always flows below, full-width. */
		.detail {
			flex-direction: column;
			align-items: center;
		}
		.info {
			min-width: 0;
			width: 100%;
		}
		.art img {
			width: 320px;
			max-width: 100%;
		}
		/* W/R/R combat row was 3 × 90px min + 2 × 1.5rem gap ≈ 318px
		   which clipped on 320-340px viewports. */
		.combat {
			gap: 0.6rem;
		}
		.combatCell {
			min-width: 0;
		}
		table {
			max-width: 100%;
		}
		thead {
			display: none;
		}
		tbody,
		tr,
		td {
			display: block;
		}
		tr {
			border: 1px solid #0f3460;
			border-radius: 8px;
			margin-bottom: 0.6rem;
			padding: 0.1rem 0.6rem;
		}
		td {
			display: flex;
			justify-content: space-between;
			align-items: center;
			gap: 1rem;
			padding: 0.35rem 0;
			border-bottom: none;
		}
		td::before {
			content: attr(data-label);
			color: #888;
			font-size: 0.72rem;
			text-transform: uppercase;
			flex-shrink: 0;
		}
		td select {
			flex: 1;
			min-width: 0;
		}
	}
</style>
