<script lang="ts">
	import { page } from '$app/state';
	import { api, variantLabel } from '$lib/api';
	import type { CardDetail } from '$lib/types/CardDetail';

	let detail = $state<CardDetail | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let busy = $state(false);

	async function load() {
		const set = page.params.set;
		const number = page.params.number;
		if (!set || !number) return;
		loading = true;
		error = null;
		try {
			detail = await api.card(set, number);
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	}

	// Re-fetch whenever the route params change (client-side navigation
	// between cards reuses this component).
	$effect(() => {
		void page.params.set;
		void page.params.number;
		load();
	});

	async function addCopy(printingId: string) {
		busy = true;
		error = null;
		try {
			await api.addCopy({ printing_id: printingId, source: 'manual' });
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}

	function variantOf(printingId: string): string {
		const p = detail?.printings.find((x) => x.printing_id === printingId);
		return p ? variantLabel(p.variant) : printingId;
	}

	/** Parse a JSON-array string column (`types`, `subtypes`) into a list. */
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

<svelte:head>
	<title>{detail ? detail.card.name : 'Card'} — PokeDumpster</title>
</svelte:head>

{#if loading}
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
				{card.set_code} · #{card.number}
				{#if card.rarity}· {card.rarity}{/if}
			</p>
			<dl>
				{#if card.supertype}<dt>Type</dt><dd>{card.supertype}</dd>{/if}
				{#if card.hp != null}<dt>HP</dt><dd>{card.hp}</dd>{/if}
				{#if jsonList(card.types).length}
					<dt>Energy</dt><dd>{jsonList(card.types).join(', ')}</dd>
				{/if}
				{#if jsonList(card.subtypes).length}
					<dt>Subtypes</dt><dd>{jsonList(card.subtypes).join(', ')}</dd>
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
			<thead>
				<tr><th>Variant</th><th>Owned</th><th>Market</th><th></th></tr>
			</thead>
			<tbody>
				{#each detail.printings as p (p.printing_id)}
					<tr class:dim={p.deprecated}>
						<td>{variantLabel(p.variant)}{#if p.deprecated} (deprecated){/if}</td>
						<td>{p.owned_count}</td>
						<td>{price(p.market_price)}</td>
						<td>
							<button disabled={busy} onclick={() => addCopy(p.printing_id)}>+ Add</button>
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
					<tr><th>Variant</th><th>Condition</th><th>Status</th><th>Paid</th><th>Acquired</th></tr>
				</thead>
				<tbody>
					{#each detail.copies as copy (copy.id)}
						<tr>
							<td>{variantOf(copy.printing_id)}</td>
							<td>{copy.condition}</td>
							<td>{copy.status}</td>
							<td>{price(copy.purchase_price)}</td>
							<td>{copy.acquired_at.slice(0, 10)}</td>
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
		max-width: 560px;
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
</style>
