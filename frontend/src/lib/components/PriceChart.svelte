<script lang="ts">
	import { Chart, registerables, type ChartConfiguration } from 'chart.js';
	import { untrack } from 'svelte';
	import { variantLabel } from '$lib/variants.svelte';
	import { chartFill, chartPalette, token } from '$lib/styles/tokens';
	import { money } from '$lib/format';
	import { EmptyState } from '$lib/components/ui';
	import type { PriceSeries } from '$lib/types/PriceSeries';

	Chart.register(...registerables);

	let { series }: { series: PriceSeries[] } = $props();

	let canvas = $state<HTMLCanvasElement | undefined>();
	// Number of times the Chart.js chart has been (re)built. Exposed as
	// data-builds so a test can assert an unrelated edit (e.g. changing a
	// copy's condition) does NOT rebuild the chart.
	let builds = $state(0);

	function buildConfig(series: PriceSeries[]): ChartConfiguration {
		// Distinct line color per series; cycles if there are more printings than
		// palette entries (Pokémon cards almost always have <= 4 printings).
		// Resolved here rather than at module scope because the token layer only
		// exists once the document does.
		const palette = chartPalette();
		const grid = token('--color-chart-grid');
		const axis = token('--color-chart-axis');
		const dates = Array.from(
			new Set(series.flatMap((s) => s.points.map((p) => p.date)))
		).sort();
		return {
			type: 'line',
			data: {
				labels: dates,
				datasets: series.map((s, i) => {
					const map = new Map(s.points.map((p) => [p.date, p.price]));
					return {
						label: variantLabel(s.variant),
						data: dates.map((d) => map.get(d) ?? null),
						borderColor: palette[i % palette.length],
						backgroundColor: chartFill(palette[i % palette.length]),
						spanGaps: true,
						tension: 0.2,
						pointRadius: 3
					};
				})
			},
			options: {
				responsive: true,
				maintainAspectRatio: false,
				scales: {
					x: { grid: { color: grid }, ticks: { color: axis, maxRotation: 0 } },
					y: {
						grid: { color: grid },
						ticks: {
							color: axis,
							callback: (v) => money(Number(v))
						}
					}
				},
				plugins: {
					legend: { labels: { color: token('--color-text-muted') } },
					tooltip: {
						callbacks: {
							label: (ctx) =>
								`${ctx.dataset.label}: ${money(Number(ctx.parsed.y))}`
						}
					}
				}
			}
		};
	}

	// Content signature of the plotted data. The chart is rebuilt only when
	// this changes — so a parent reload that produces an identical dataset
	// (e.g. editing a copy's condition re-fetches the catalog price history,
	// which is unchanged) doesn't destroy + recreate the chart, which would
	// replay Chart.js's entry animation and read as a jarring "refresh"
	// (pokedumpster-i5d).
	const sig = $derived(JSON.stringify(series.map((s) => [s.printing_id, s.variant, s.points])));

	$effect(() => {
		// Depend on the canvas + the content signature only; read `series`
		// itself untracked so a new-but-identical array reference is a no-op.
		void sig;
		const el = canvas;
		if (!el) return;
		const chart = new Chart(el, buildConfig(untrack(() => series)));
		builds = untrack(() => builds) + 1;
		return () => chart.destroy();
	});

	const empty = $derived(
		series.length === 0 || series.every((s) => s.points.length === 0)
	);
	const oneShot = $derived(
		!empty && series.every((s) => s.points.length <= 1)
	);
</script>

{#if empty}
	<EmptyState
		size="sm"
		title="No price history yet."
		description="Prices are recorded by the daily refresh — the chart appears once this printing has been seen at least once."
	/>
{:else if oneShot}
	<p class="muted">Only one price snapshot so far — the chart will grow as the daily refresh runs.</p>
	<div class="wrap" data-testid="price-chart" data-series-count={series.length} data-builds={builds}>
		<canvas bind:this={canvas}></canvas>
	</div>
{:else}
	<div class="wrap" data-testid="price-chart" data-series-count={series.length} data-builds={builds}>
		<canvas bind:this={canvas}></canvas>
	</div>
{/if}

<style>
	.wrap {
		height: 280px;
		max-width: 640px;
	}
	.muted {
		color: var(--color-text-subtle);
		font-size: 0.85rem;
		margin: 0 0 0.4rem;
	}
</style>
