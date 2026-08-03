<script lang="ts">
	import { Chart, registerables, type ChartConfiguration } from 'chart.js';
	import { untrack } from 'svelte';
	import { money } from '$lib/format';
	import { EmptyState } from '$lib/components/ui';
	import type { ValueSeries } from '$lib/api';

	Chart.register(...registerables);

	let { series, dimension }: { series: ValueSeries[]; dimension: string } = $props();

	let canvas = $state<HTMLCanvasElement | undefined>();

	// Distinct line color per bucket; cycles for large breakdowns.
	const PALETTE = [
		'#e94560',
		'#4a8df0',
		'#f0c878',
		'#5cb85c',
		'#a64ac9',
		'#e88a3a',
		'#5bc0de',
		'#c0d0f0',
		'#aa7733',
		'#8ac926'
	];

	function buildConfig(series: ValueSeries[], dimension: string): ChartConfiguration {
		const dates = Array.from(new Set(series.flatMap((s) => s.points.map((p) => p.date)))).sort();

		let datasets;
		if (dimension === 'all') {
			// One series (the whole owned collection): a market-value line and a
			// cost-basis line so the gap reads as unrealized gain/loss.
			const s = series[0];
			const mv = new Map((s?.points ?? []).map((p) => [p.date, p.market_value]));
			const cb = new Map((s?.points ?? []).map((p) => [p.date, p.cost_basis]));
			datasets = [
				{
					label: 'Market value',
					data: dates.map((d) => mv.get(d) ?? null),
					borderColor: '#e94560',
					backgroundColor: '#e9456022',
					spanGaps: true,
					tension: 0.2,
					pointRadius: 2,
					fill: true
				},
				{
					label: 'Cost basis',
					data: dates.map((d) => cb.get(d) ?? null),
					borderColor: '#8899aa',
					backgroundColor: 'transparent',
					borderDash: [5, 4],
					spanGaps: true,
					tension: 0.2,
					pointRadius: 2
				}
			];
		} else {
			// One market-value line per bucket (set / binder).
			datasets = series.map((s, i) => {
				const map = new Map(s.points.map((p) => [p.date, p.market_value]));
				return {
					label: s.label ?? s.bucket ?? '—',
					data: dates.map((d) => map.get(d) ?? null),
					borderColor: PALETTE[i % PALETTE.length],
					backgroundColor: PALETTE[i % PALETTE.length] + '33',
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
						grid: { color: '#0f3460' },
						ticks: { color: '#888', maxRotation: 0, autoSkip: true, maxTicksLimit: 8 }
					},
					y: {
						grid: { color: '#0f3460' },
						ticks: { color: '#888', callback: (v) => money(Number(v)) }
					}
				},
				plugins: {
					// A long by-set/by-binder legend gets unwieldy — hide it past 10.
					legend: { display: dimension === 'all' || series.length <= 10, labels: { color: '#ccc' } },
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
		color: #888;
		font-size: 0.85rem;
		margin: 0.4rem 0;
	}
</style>
