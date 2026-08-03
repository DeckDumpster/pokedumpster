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
		background: var(--color-scrim);
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
		background: var(--color-surface-overlay);
		border: 1px solid var(--color-border);
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
		color: var(--color-text-accent);
	}
	.x {
		background: none;
		border: none;
		color: var(--color-text-subtle);
		font-size: 1.4rem;
		line-height: 1;
		cursor: pointer;
	}
	.x:hover {
		color: var(--color-text);
	}
	.dims {
		display: flex;
		gap: 0.4rem;
		margin-bottom: 0.8rem;
	}
	.dims button {
		padding: 0.35rem 0.8rem;
		background: var(--color-surface-page);
		border: 1px solid var(--color-border);
		border-radius: 6px;
		color: var(--color-text-muted);
		cursor: pointer;
		font-size: 0.85rem;
	}
	.dims button:hover {
		color: var(--color-text);
	}
	.dims button.active {
		background: var(--color-info-surface);
		color: var(--color-text);
		border-color: var(--color-border-accent);
	}
	.muted {
		color: var(--color-text-subtle);
		font-size: 0.9rem;
	}
	.error {
		color: var(--color-text-accent);
	}
</style>
