<script lang="ts">
	import CardDetailView from './CardDetailView.svelte';

	let {
		setCode,
		number,
		onClose,
		onNavigate
	}: {
		setCode: string;
		number: string;
		onClose: () => void;
		onNavigate?: (set: string, number: string) => void;
	} = $props();
</script>

<svelte:window
	onkeydown={(e) => {
		if (e.key === 'Escape') onClose();
	}}
/>

<div class="backdrop" role="presentation" onclick={onClose}></div>
<div class="modal" role="dialog" aria-modal="true" aria-label="Card detail">
	<!-- Sticky so the close control stays reachable however far the card
	     detail is scrolled — important on the mobile bottom sheet. -->
	<div class="closebar">
		<button class="x" onclick={onClose} aria-label="Close">×</button>
	</div>
	<div class="body">
		<CardDetailView {setCode} {number} {onNavigate} />
	</div>
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
	}
	.closebar {
		position: sticky;
		top: 0;
		z-index: 2;
		display: flex;
		justify-content: flex-end;
		background: #1a1a2e;
		border-bottom: 1px solid #0f3460;
		padding: 0.4rem 0.6rem;
	}
	.x {
		background: none;
		border: none;
		color: #888;
		font-size: 1.7rem;
		line-height: 1;
		cursor: pointer;
		padding: 0 0.4rem;
	}
	.x:hover {
		color: #e94560;
	}
	.body {
		padding: 1.25rem 1.5rem 1.5rem;
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
		.x {
			font-size: 2rem;
			padding: 0.2rem 0.6rem;
		}
		.body {
			padding: 1rem 0.85rem 1.25rem;
		}
	}
</style>
