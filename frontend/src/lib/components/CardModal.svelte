<script lang="ts">
	import CardDetailView from './CardDetailView.svelte';

	let {
		setCode,
		number,
		onClose,
		onMutate
	}: {
		setCode: string;
		number: string;
		onClose: () => void;
		/** Forwarded to CardDetailView — fired when a copy is mutated. */
		onMutate?: () => void;
	} = $props();
</script>

<svelte:window
	onkeydown={(e) => {
		if (e.key === 'Escape') onClose();
	}}
/>

<div class="backdrop" role="presentation" onclick={onClose}></div>
<div class="modal" role="dialog" aria-modal="true" aria-label="Card detail">
	<!-- Floating controls over the modal's top-right corner. Stays above
	     the scrolling content so they're reachable as the user scrolls.
	     The full-page link is the canonical view — every mutation here
	     hits the same backend, so opening the page reloads fresh state
	     and reconciles any drift between this modal and a page open in
	     another tab. -->
	<div class="controls">
		<a
			class="fulllink"
			href="/card/{setCode}/{number}"
			title="Open the full card page (canonical view)"
			aria-label="Open full card page">⤢</a
		>
		<button class="x" onclick={onClose} aria-label="Close">×</button>
	</div>
	<div class="body">
		<CardDetailView {setCode} {number} {onMutate} />
	</div>
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
		background: var(--color-surface-page);
		border: 2px solid var(--color-border);
		border-radius: 12px;
	}
	.controls {
		position: absolute;
		top: 8px;
		right: 8px;
		z-index: 3;
		display: flex;
		gap: 6px;
	}
	.x,
	.fulllink {
		width: 32px;
		height: 32px;
		display: flex;
		align-items: center;
		justify-content: center;
		background: var(--color-scrim);
		border: 1px solid var(--color-border);
		border-radius: 50%;
		color: var(--color-text);
		font-size: 1.4rem;
		line-height: 1;
		cursor: pointer;
		padding: 0;
		text-decoration: none;
	}
	.fulllink {
		font-size: 1rem;
	}
	.x:hover,
	.fulllink:hover {
		background: var(--color-accent);
		color: var(--color-on-accent);
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
