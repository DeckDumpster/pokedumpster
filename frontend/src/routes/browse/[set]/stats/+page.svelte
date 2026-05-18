<script lang="ts">
	import { page } from '$app/state';
	import { api } from '$lib/api';
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

	function pct(owned: number, total: number): number {
		return total > 0 ? Math.round((owned / total) * 100) : 0;
	}
	function money(n: number): string {
		return `$${n.toFixed(2)}`;
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
						<span class="metricval">{owned} / {total} · {pct(owned, total)}%</span>
					</div>
					<div class="bar"><span style:width="{pct(owned, total)}%"></span></div>
				</div>
			{/snippet}
			{@render bar('Numbered set', stats.owned_cards, stats.total_cards)}
			{@render bar('Master set', stats.owned_printings, stats.total_printings)}
		</section>

		<section class="card">
			<h2>Value</h2>
			<div class="figs">
				<div class="fig">
					<span class="figval">{money(stats.owned_value)}</span>
					<span class="figlabel">Owned</span>
				</div>
				<div class="fig">
					<span class="figval">{money(stats.market_value)}</span>
					<span class="figlabel">Full set</span>
				</div>
				<div class="fig">
					<span class="figval">{pct(stats.owned_value, stats.market_value)}%</span>
					<span class="figlabel">of set value</span>
				</div>
			</div>
		</section>
	</div>

	<section class="card">
		<h2>Rarity split</h2>
		{#if stats.rarities.length === 0}
			<p class="muted">No cards catalogued.</p>
		{:else}
			<table>
				<thead>
					<tr><th>Rarity</th><th>Owned</th><th>Total</th><th class="pcol">Progress</th></tr>
				</thead>
				<tbody>
					{#each stats.rarities as r (r.rarity)}
						<tr>
							<td>{r.rarity}</td>
							<td>{r.owned_cards}</td>
							<td>{r.total_cards}</td>
							<td class="pcol">
								<div class="bar small">
									<span style:width="{pct(r.owned_cards, r.total_cards)}%"></span>
								</div>
								<span class="rpct">{pct(r.owned_cards, r.total_cards)}%</span>
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
	.pcol {
		width: 40%;
	}
	td.pcol {
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
</style>
