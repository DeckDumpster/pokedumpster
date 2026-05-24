<script lang="ts">
	import { api } from '$lib/api';
	import { variantLabel } from '$lib/variants.svelte';
	import type { CardDetail } from '$lib/types/CardDetail';

	let setCode = $state('');
	let number = $state('');
	let condition = $state('Near Mint');
	let card = $state<CardDetail | null>(null);
	let error = $state<string | null>(null);
	let busy = $state(false);
	let log = $state<string[]>([]);

	const conditions = [
		'Near Mint',
		'Lightly Played',
		'Moderately Played',
		'Heavily Played',
		'Damaged'
	];

	async function lookup() {
		error = null;
		card = null;
		const s = setCode.trim();
		const n = number.trim();
		if (!s || !n) {
			error = 'Enter a set code and collector number.';
			return;
		}
		busy = true;
		try {
			card = await api.card(s, n);
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}

	async function add(printingId: string, variant: string) {
		if (!card) return;
		busy = true;
		error = null;
		const name = card.card.name;
		try {
			await api.addCopy({ printing_id: printingId, source: 'manual_id', condition });
			log = [`Added ${name} · ${variantLabel(variant)} · ${condition}`, ...log];
			// Refresh owned counts.
			card = await api.card(card.card.set_code, card.card.number);
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}
</script>

<svelte:head><title>Manual entry — PokeDumpster</title></svelte:head>

<h1>Manual entry</h1>
<p class="muted">Look up a card by set code and collector number, then add the copies you own.</p>

<form
	class="lookup"
	onsubmit={(e) => {
		e.preventDefault();
		lookup();
	}}
>
	<label>
		Set code
		<input type="text" placeholder="sv3pt5" bind:value={setCode} />
	</label>
	<label>
		Number
		<input type="text" placeholder="6" bind:value={number} />
	</label>
	<label>
		Condition
		<select bind:value={condition}>
			{#each conditions as c (c)}<option value={c}>{c}</option>{/each}
		</select>
	</label>
	<button type="submit" disabled={busy}>Look up</button>
</form>

{#if error}<p class="error">{error}</p>{/if}

{#if card}
	{@const c = card.card}
	<section class="resolved">
		<div class="cardhead">
			{#if c.image_small}<img src={c.image_small} alt={c.name} />{/if}
			<div>
				<h2><a href="/card/{c.set_code}/{c.number}">{c.name}</a></h2>
				<p class="muted">{c.set_code} · #{c.number}{#if c.rarity} · {c.rarity}{/if}</p>
			</div>
		</div>
		<table>
			<thead><tr><th>Variant</th><th>Owned</th><th></th></tr></thead>
			<tbody>
				{#each card.printings as p (p.printing_id)}
					<tr class:dim={p.deprecated}>
						<td>{variantLabel(p.variant)}</td>
						<td>{p.owned_count}</td>
						<td>
							<button disabled={busy} onclick={() => add(p.printing_id, p.variant)}>
								+ Add
							</button>
						</td>
					</tr>
				{/each}
			</tbody>
		</table>
	</section>
{/if}

{#if log.length}
	<section>
		<h2>This session ({log.length})</h2>
		<ul class="log">
			{#each log as entry, i (i)}<li>{entry}</li>{/each}
		</ul>
	</section>
{/if}

<style>
	h1 {
		color: #e94560;
	}
	.muted {
		color: #888;
	}
	.error {
		color: #e94560;
	}
	.lookup {
		display: flex;
		gap: 1rem;
		align-items: flex-end;
		flex-wrap: wrap;
		margin: 1rem 0;
	}
	label {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		font-size: 0.8rem;
		color: #888;
	}
	input,
	select {
		padding: 0.45rem;
		background: #1a1a2e;
		border: 1px solid #0f3460;
		border-radius: 6px;
		color: #e0e0e0;
		font-size: 0.9rem;
	}
	button {
		background: #e94560;
		border: none;
		color: #fff;
		padding: 0.45rem 0.9rem;
		border-radius: 6px;
		cursor: pointer;
		font-size: 0.85rem;
	}
	button:disabled {
		opacity: 0.5;
		cursor: default;
	}
	.cardhead {
		display: flex;
		gap: 1rem;
		align-items: center;
	}
	.cardhead h2 {
		margin: 0;
		font-size: 1.1rem;
	}
	.cardhead a {
		color: #e94560;
		text-decoration: none;
	}
	.resolved {
		margin-top: 1rem;
	}
	table {
		width: 100%;
		max-width: 420px;
		border-collapse: collapse;
		font-size: 0.9rem;
		margin-top: 0.75rem;
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
	section {
		margin-top: 2rem;
	}
	h2 {
		color: #e94560;
		font-size: 1.1rem;
	}
	.log {
		list-style: none;
		padding: 0;
		font-size: 0.9rem;
	}
	.log li {
		padding: 0.3rem 0;
		border-bottom: 1px solid #0f3460;
		color: #9fe7a0;
	}
</style>
