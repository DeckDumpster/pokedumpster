<script lang="ts">
	import { Chart, registerables, type ChartConfiguration } from 'chart.js';
	import { untrack } from 'svelte';
	import { chartFill, chartPalette, token } from '$lib/styles/tokens';
	import { money } from '$lib/format';
	import { EmptyState } from '$lib/components/ui';
	import type { ValueSeries, ValuePoint } from '$lib/api';

	Chart.register(...registerables);

	let { series, dimension }: { series: ValueSeries[]; dimension: string } = $props();

	let canvas = $state<HTMLCanvasElement | undefined>();

	function buildConfig(series: ValueSeries[], dimension: string): ChartConfiguration {
		// Distinct line color per bucket; cycles for large breakdowns. Resolved
		// here rather than at module scope because the token layer only exists
		// once the document does.
		const palette = chartPalette();
		const grid = token('--color-chart-grid');
		const axis = token('--color-chart-axis');
		const dates = Array.from(new Set(series.flatMap((s) => s.points.map((p) => p.date)))).sort();

		let datasets;
		if (dimension === 'all') {
			// The collection's two priced halves as two lines, plus the cost basis
			// of both so the gap still reads as unrealized gain/loss (pd-bbv7).
			//
			// Two lines rather than one blended total, because the halves are
			// priced from different feeds against different keys and a reader is
			// entitled to see which one moved. The two series are told apart by
			// `bucket`, never by their order: the cards series is the one whose
			// bucket is null, and the sealed series is absent entirely for a
			// collection that has never held sealed product.
			const cards = series.find((s) => s.bucket == null);
			const sealed = series.find((s) => s.bucket === 'sealed');
			const at = (s: ValueSeries | undefined, pick: (p: ValuePoint) => number) =>
				new Map((s?.points ?? []).map((p) => [p.date, pick(p)] as const));
			const cardsMv = at(cards, (p) => p.market_value);
			const sealedMv = at(sealed, (p) => p.market_value);
			const cardsCb = at(cards, (p) => p.cost_basis);
			const sealedCb = at(sealed, (p) => p.cost_basis);
			// Summed at READ time, here, from the points actually drawn — there is
			// no stored combined total, and a date only one half reports is that
			// half's number rather than a hole.
			const costBasis = dates.map((d) => {
				const a = cardsCb.get(d);
				const b = sealedCb.get(d);
				return a == null && b == null ? null : (a ?? 0) + (b ?? 0);
			});
			// Typed, because the array grows conditionally below and the
			// inferred element type would be whatever the first line happens
			// to spell.
			const allDatasets: ChartConfiguration['data']['datasets'] = [
				{
					label: 'Cards',
					data: dates.map((d) => cardsMv.get(d) ?? null),
					borderColor: token('--color-chart-1'),
					backgroundColor: token('--color-surface-selected'),
					spanGaps: true,
					tension: 0.2,
					pointRadius: 2,
					fill: true
				}
			];
			if (sealed) {
				allDatasets.push({
					// The label comes off the series — the server names it, the
					// same way it names a set or a binder bucket.
					label: sealed.label ?? 'Sealed',
					data: dates.map((d) => sealedMv.get(d) ?? null),
					borderColor: token('--color-chart-2'),
					backgroundColor: 'transparent',
					spanGaps: true,
					tension: 0.2,
					pointRadius: 2,
					fill: false
				});
			}
			allDatasets.push({
				label: 'Cost basis',
				data: costBasis,
				borderColor: axis,
				backgroundColor: 'transparent',
				borderDash: [5, 4],
				spanGaps: true,
				tension: 0.2,
				pointRadius: 2
			});
			datasets = allDatasets;
		} else {
			// One market-value line per bucket (set / binder).
			datasets = series.map((s, i) => {
				const map = new Map(s.points.map((p) => [p.date, p.market_value]));
				return {
					label: s.label ?? s.bucket ?? '—',
					data: dates.map((d) => map.get(d) ?? null),
					borderColor: palette[i % palette.length],
					backgroundColor: chartFill(palette[i % palette.length]),
					spanGaps: true,
					tension: 0.2,
					pointRadius: 1
				};
			});
		}

		return {
			type: 'line',
			data: { labels: dates, datasets },
			options: {
				responsive: true,
				maintainAspectRatio: false,
				interaction: { mode: 'index', intersect: false },
				scales: {
					x: {
						grid: { color: grid },
						ticks: { color: axis, maxRotation: 0, autoSkip: true, maxTicksLimit: 8 }
					},
					y: {
						grid: { color: grid },
						ticks: { color: axis, callback: (v) => money(Number(v)) }
					}
				},
				plugins: {
					// A long by-set/by-binder legend gets unwieldy — hide it past 10.
					legend: {
						display: dimension === 'all' || series.length <= 10,
						labels: { color: token('--color-text-muted') }
					},
					tooltip: {
						callbacks: { label: (ctx) => `${ctx.dataset.label}: ${money(Number(ctx.parsed.y))}` }
					}
				}
			}
		};
	}

	// Rebuild only when the plotted content changes (mirrors PriceChart).
	const sig = $derived(JSON.stringify([dimension, series]));

	$effect(() => {
		void sig;
		const el = canvas;
		if (!el) return;
		const chart = new Chart(el, buildConfig(untrack(() => series), untrack(() => dimension)));
		return () => chart.destroy();
	});

	const empty = $derived(series.length === 0 || series.every((s) => s.points.length === 0));
	const oneShot = $derived(!empty && series.every((s) => s.points.length <= 1));
</script>

{#if empty}
	<EmptyState
		size="sm"
		title="No value history yet."
		description="It fills in as the nightly refresh runs — or all at once after a one-time backfill."
	/>
{:else}
	{#if oneShot}
		<p class="muted">Only one snapshot so far — the line grows as the nightly refresh runs.</p>
	{/if}
	<div class="wrap" data-testid="value-history-chart" data-series-count={series.length}>
		<canvas bind:this={canvas}></canvas>
	</div>
{/if}

<style>
	.wrap {
		height: 340px;
		width: 100%;
	}
	.muted {
		color: var(--color-text-subtle);
		font-size: 0.85rem;
		margin: 0.4rem 0;
	}
</style>
