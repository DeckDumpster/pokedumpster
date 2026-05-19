<script lang="ts">
	import CardDetailView from './CardDetailView.svelte';

	let {
		setCode,
		number,
		onClose
	}: { setCode: string; number: string; onClose: () => void } = $props();
</script>

<svelte:window
	onkeydown={(e) => {
		if (e.key === 'Escape') onClose();
	}}
/>

<div class="backdrop" role="presentation" onclick={onClose}></div>
<div class="modal" role="dialog" aria-modal="true" aria-label="Card detail">
	<button class="x" onclick={onClose} aria-label="Close">×</button>
	<CardDetailView {setCode} {number} />
</div>

<style>
	.backdrop {
		position: fixed;
		inset: 0;
		background: rgba(0, 0, 0, 0.6);
		z-index: 100;
	}
	.modal {
		position: fixed;
		top: 50%;
		left: 50%;
		transform: translate(-50%, -50%);
		z-index: 101;
		width: 760px;
		max-width: 94vw;
		max-height: 88vh;
		overflow-y: auto;
		background: #1a1a2e;
		border: 2px solid #0f3460;
		border-radius: 12px;
		padding: 1.5rem;
	}
	.x {
		position: absolute;
		top: 0.6rem;
		right: 0.8rem;
		background: none;
		border: none;
		color: #888;
		font-size: 1.6rem;
		line-height: 1;
		cursor: pointer;
	}
	.x:hover {
		color: #e94560;
	}

	/* Bottom-sheet on narrow screens, matching VariantModal. */
	@media (max-width: 540px) {
		.modal {
			top: auto;
			bottom: 0;
			left: 0;
			transform: none;
			width: 100%;
			max-width: 100%;
			max-height: 90vh;
			border-radius: 14px 14px 0 0;
		}
	}
</style>
