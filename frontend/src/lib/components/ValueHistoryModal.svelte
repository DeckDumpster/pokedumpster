<script lang="ts">
	import { onMount } from 'svelte';
	import { api, type ValueSeries } from '$lib/api';
	import ValueHistoryChart from './ValueHistoryChart.svelte';

	let { onClose }: { onClose: () => void } = $props();

	type Dim = 'all' | 'set' | 'binder';
	let dimension = $state<Dim>('all');
	let series = $state<ValueSeries[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	async function load(d: Dim) {
		loading = true;
		error = null;
		try {
			series = await api.valueHistory(d);
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
			series = [];
		} finally {
			loading = false;
		}
	}

	onMount(() => load('all'));

	function setDim(d: Dim) {
		if (d === dimension) return;
		dimension = d;
		load(d);
	}

	function onKey(e: KeyboardEvent) {
		if (e.key === 'Escape') onClose();
	}
</script>

<svelte:window on:keydown={onKey} />

<div class="backdrop" role="presentation" onclick={onClose}></div>
<div class="modal" role="dialog" aria-modal="true" aria-label="Collection value over time">
	<header>
		<h2>Collection value over time</h2>
		<button class="x" onclick={onClose} aria-label="Close">×</button>
	</header>

	<div class="dims" role="tablist" aria-label="Breakdown">
		<button role="tab" aria-selected={dimension === 'all'} class:active={dimension === 'all'} onclick={() => setDim('all')}>Total</button>
		<button role="tab" aria-selected={dimension === 'set'} class:active={dimension === 'set'} onclick={() => setDim('set')}>By set</button>
		<button role="tab" aria-selected={dimension === 'binder'} class:active={dimension === 'binder'} onclick={() => setDim('binder')}>By binder</button>
	</div>

	{#if error}
		<p class="error">{error}</p>
	{:else if loading}
		<p class="muted">Loading…</p>
	{:else}
		<ValueHistoryChart {series} {dimension} />
	{/if}
</div>

<style>
	.backdrop {
		position: fixed;
		inset: 0;
		background: rgba(0, 0, 0, 0.55);
		z-index: 100;
	}
	.modal {
		position: fixed;
		top: 50%;
		left: 50%;
		transform: translate(-50%, -50%);
		width: min(760px, 92vw);
		max-height: 88vh;
		overflow: auto;
		background: #16213e;
		border: 1px solid #0f3460;
		border-radius: 12px;
		padding: 1rem 1.2rem 1.4rem;
		z-index: 101;
	}
	header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		margin-bottom: 0.6rem;
	}
	h2 {
		margin: 0;
		font-size: 1.05rem;
		color: #e94560;
	}
	.x {
		background: none;
		border: none;
		color: #888;
		font-size: 1.4rem;
		line-height: 1;
		cursor: pointer;
	}
	.x:hover {
		color: #e0e0e0;
	}
	.dims {
		display: flex;
		gap: 0.4rem;
		margin-bottom: 0.8rem;
	}
	.dims button {
		padding: 0.35rem 0.8rem;
		background: #1a1a2e;
		border: 1px solid #0f3460;
		border-radius: 6px;
		color: #bbb;
		cursor: pointer;
		font-size: 0.85rem;
	}
	.dims button:hover {
		color: #e0e0e0;
	}
	.dims button.active {
		background: #0f3460;
		color: #e0e0e0;
		border-color: #e94560;
	}
	.muted {
		color: #888;
		font-size: 0.9rem;
	}
	.error {
		color: #e94560;
	}
</style>
