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
	<!-- Floating close button over the modal's top-right corner — gives the
	     card the full width without the dedicated closebar row. Always above
	     the scrolling content so it stays reachable as the user scrolls. -->
	<button class="x" onclick={onClose} aria-label="Close">×</button>
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
		/* Pin near the top so shorter cards don't sit in dead-center
		   whitespace; longer cards fill the remaining 88vh and scroll. */
		top: 4vh;
		left: 50%;
		transform: translateX(-50%);
		z-index: 101;
		width: 760px;
		/* border-box so the 2px borders count inside the 92vw budget —
		   prevents the right edge spilling past the viewport on narrow
		   screens. */
		box-sizing: border-box;
		max-width: 92vw;
		max-height: 92vh;
		/* Scroll lives on .body so the absolute-positioned X stays pinned
		   to the modal's top-right regardless of how far the content has
		   been scrolled. */
		display: flex;
		flex-direction: column;
		overflow: hidden;
		background: #1a1a2e;
		border: 2px solid #0f3460;
		border-radius: 12px;
	}
	.x {
		position: absolute;
		top: 8px;
		right: 8px;
		z-index: 3;
		width: 32px;
		height: 32px;
		display: flex;
		align-items: center;
		justify-content: center;
		background: rgba(10, 10, 30, 0.75);
		border: 1px solid #0f3460;
		border-radius: 50%;
		color: #e0e0e0;
		font-size: 1.4rem;
		line-height: 1;
		cursor: pointer;
		padding: 0;
	}
	.x:hover {
		background: #e94560;
		color: #fff;
	}
	.body {
		flex: 1;
		overflow-y: auto;
		overflow-x: hidden;
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
			max-height: 92vh;
			border-radius: 14px 14px 0 0;
		}
		.body {
			padding: 1rem 0.85rem 1.25rem;
		}
	}
</style>
