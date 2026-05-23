<script lang="ts">
	import { Chart, registerables, type ChartConfiguration } from 'chart.js';
	import { variantLabel } from '$lib/api';
	import type { PriceSeries } from '$lib/types/PriceSeries';

	Chart.register(...registerables);

	let { series }: { series: PriceSeries[] } = $props();

	let canvas = $state<HTMLCanvasElement | undefined>();

	// Distinct line color per series; cycles if there are more printings than
	// palette entries (Pokémon cards almost always have <= 4 printings).
	const PALETTE = ['#e94560', '#4a8df0', '#f0c878', '#5cb85c', '#a64ac9', '#ccc'];

	function buildConfig(series: PriceSeries[]): ChartConfiguration {
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
						borderColor: PALETTE[i % PALETTE.length],
						backgroundColor: PALETTE[i % PALETTE.length] + '33',
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
					x: { grid: { color: '#0f3460' }, ticks: { color: '#888', maxRotation: 0 } },
					y: {
						grid: { color: '#0f3460' },
						ticks: {
							color: '#888',
							callback: (v) => '$' + Number(v).toFixed(2)
						}
					}
				},
				plugins: {
					legend: { labels: { color: '#ccc' } },
					tooltip: {
						callbacks: {
							label: (ctx) =>
								`${ctx.dataset.label}: $${Number(ctx.parsed.y).toFixed(2)}`
						}
					}
				}
			}
		};
	}

	$effect(() => {
		if (!canvas) return;
		const chart = new Chart(canvas, buildConfig(series));
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
	<p class="muted">No price history yet.</p>
{:else if oneShot}
	<p class="muted">Only one price snapshot so far — the chart will grow as the daily refresh runs.</p>
	<div class="wrap"><canvas bind:this={canvas}></canvas></div>
{:else}
	<div class="wrap"><canvas bind:this={canvas}></canvas></div>
{/if}

<style>
	.wrap {
		height: 280px;
		max-width: 640px;
	}
	.muted {
		color: #888;
		font-size: 0.85rem;
		margin: 0 0 0.4rem;
	}
</style>
