<script lang="ts">
	import { api, variantLabel } from '$lib/api';
	import type { CardDetail } from '$lib/types/CardDetail';
	import type { Binder } from '$lib/types/Binder';
	import type { Deck } from '$lib/types/Deck';

	// The card-detail body, shared by the /card/[set]/[number] route and the
	// collection-page modal. Self-contained: it fetches its own data.
	let { setCode, number }: { setCode: string; number: string } = $props();

	let detail = $state<CardDetail | null>(null);
	let binders = $state<Binder[]>([]);
	let decks = $state<Deck[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let busy = $state(false);

	const STATUSES = ['owned', 'ordered', 'listed', 'sold', 'removed', 'traded', 'gifted', 'lost'];

	async function load() {
		if (!setCode || !number) return;
		loading = true;
		error = null;
		try {
			[detail, binders, decks] = await Promise.all([
				api.card(setCode, number),
				api.binders(),
				api.decks()
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

	function jsonList(raw: string | null): string[] {
		if (!raw) return [];
		try {
			const parsed: unknown = JSON.parse(raw);
			return Array.isArray(parsed) ? parsed.map(String) : [];
		} catch {
			return [];
		}
	}
	function price(p: number | null): string {
		return p == null ? '—' : `$${p.toFixed(2)}`;
	}
</script>

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
				{card.set_code} · #{card.number}{#if card.rarity} · {card.rarity}{/if}
			</p>
			<dl>
				{#if card.supertype}<dt>Type</dt><dd>{card.supertype}</dd>{/if}
				{#if card.hp != null}<dt>HP</dt><dd>{card.hp}</dd>{/if}
				{#if jsonList(card.types).length}
					<dt>Energy</dt><dd>{jsonList(card.types).join(', ')}</dd>
				{/if}
				{#if card.artist}<dt>Artist</dt><dd>{card.artist}</dd>{/if}
			</dl>
			{#if card.flavor_text}<p class="flavor">{card.flavor_text}</p>{/if}
		</div>
	</div>

	{#if error}<p class="error">{error}</p>{/if}

	<section>
		<h2>Printings</h2>
		<table>
			<thead><tr><th>Variant</th><th>Owned</th><th>Market</th><th></th></tr></thead>
			<tbody>
				{#each detail.printings as p (p.printing_id)}
					<tr class:dim={p.deprecated}>
						<td data-label="Variant">{variantLabel(p.variant)}</td>
						<td data-label="Owned">{p.owned_count}</td>
						<td data-label="Market">{price(p.market_price)}</td>
						<td data-label="">
							<button disabled={busy || p.deprecated} onclick={() => addCopy(p.printing_id)}>
								+ Add
							</button>
						</td>
					</tr>
				{/each}
			</tbody>
		</table>
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
	h1 {
		color: #e94560;
		margin: 0;
	}
	.sub {
		color: #888;
		margin: 0.25rem 0 1rem;
	}
	dl {
		display: grid;
		grid-template-columns: auto 1fr;
		gap: 0.25rem 1rem;
		margin: 0;
	}
	dt {
		color: #888;
		font-size: 0.85rem;
	}
	.flavor {
		font-style: italic;
		color: #aaa;
		margin-top: 1rem;
	}
	section {
		margin-top: 2rem;
	}
	h2 {
		color: #e94560;
		font-size: 1.1rem;
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
	tr.dim {
		opacity: 0.5;
	}
	select {
		background: #1a1a2e;
		border: 1px solid #0f3460;
		color: #e0e0e0;
		border-radius: 6px;
		padding: 0.15rem;
		font: inherit;
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
