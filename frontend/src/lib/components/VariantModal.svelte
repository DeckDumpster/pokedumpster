<script lang="ts">
	import { variantLabel } from '$lib/api';
	import type { BinderSlot } from '$lib/types/BinderSlot';

	let {
		slot,
		setCode,
		onAdd,
		onClose
	}: {
		slot: BinderSlot;
		setCode: string;
		onAdd: (printingId: string, variant: string) => void;
		onClose: () => void;
	} = $props();
</script>

<svelte:window
	onkeydown={(e) => {
		if (e.key === 'Escape') onClose();
	}}
/>

<div class="backdrop"></div>
<div class="modal" role="dialog" aria-modal="true" aria-label="{slot.name} printings">
	<header>
		<h3>#{slot.number} · {slot.name}</h3>
		<button class="x" onclick={onClose} aria-label="Close">×</button>
	</header>

	<ul>
		{#each slot.printings as p (p.printing_id)}
			<li class:dim={p.deprecated}>
				<span class="variant">{variantLabel(p.variant)}</span>
				<span class="owned" class:has={p.owned_count > 0}>{p.owned_count} owned</span>
				<span class="price">{p.market_price != null ? `$${p.market_price.toFixed(2)}` : ''}</span>
				<button
					class="add"
					disabled={p.deprecated}
					onclick={() => onAdd(p.printing_id, p.variant)}
				>
					+ Add
				</button>
			</li>
		{/each}
	</ul>

	<a class="full" href="/card/{setCode}/{slot.number}">Full card details →</a>
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
		width: 420px;
		max-width: 92vw;
		max-height: 85vh;
		overflow-y: auto;
		background: #16213e;
		border: 2px solid #0f3460;
		border-radius: 12px;
		padding: 1.25rem;
	}
	header {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
	}
	h3 {
		margin: 0;
		color: #e94560;
	}
	.x {
		background: none;
		border: none;
		color: #888;
		font-size: 1.4rem;
		cursor: pointer;
		line-height: 1;
	}
	ul {
		list-style: none;
		padding: 0;
		margin: 1rem 0 0.5rem;
	}
	li {
		display: grid;
		grid-template-columns: 1fr auto auto auto;
		gap: 0.6rem;
		align-items: center;
		padding: 0.45rem 0;
		border-bottom: 1px solid #0f3460;
	}
	li.dim {
		opacity: 0.5;
	}
	.owned {
		font-size: 0.8rem;
		color: #888;
	}
	.owned.has {
		color: #9fe7a0;
	}
	.price {
		font-size: 0.8rem;
		color: #888;
	}
	.add {
		background: #e94560;
		border: none;
		color: #fff;
		padding: 0.25rem 0.6rem;
		border-radius: 6px;
		cursor: pointer;
		font-size: 0.8rem;
	}
	.add:disabled {
		opacity: 0.4;
		cursor: default;
	}
	.full {
		color: #888;
		font-size: 0.85rem;
		text-decoration: none;
	}
	.full:hover {
		color: #e94560;
	}

	/* On narrow screens the modal becomes a bottom sheet (PLAN §6.9). */
	@media (max-width: 540px) {
		.modal {
			top: auto;
			bottom: 0;
			left: 0;
			transform: none;
			width: 100%;
			max-width: 100%;
			max-height: 78vh;
			border-radius: 14px 14px 0 0;
		}
		.add {
			padding: 0.45rem 1rem;
		}
	}
</style>
