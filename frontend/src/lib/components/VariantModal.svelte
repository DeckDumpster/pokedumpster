<script lang="ts">
	import { variantLabel, variantSortCmp, variantProvenance } from '$lib/variants.svelte';
	import { CONDITIONS } from '$lib/conditions';
	import { money } from '$lib/format';
	import type { BinderSlot } from '$lib/types/BinderSlot';

	let {
		slot,
		setCode,
		condition = $bindable('Near Mint'),
		onAdd,
		onRemove,
		onClose
	}: {
		slot: BinderSlot;
		setCode: string;
		/** Bindable. Drives the condition used when the user clicks "+".
		 *  Parent owns it so the picked tier sticks across modal opens
		 *  within the same /browse/[set] session. */
		condition?: string;
		onAdd: (printingId: string, variant: string) => void;
		onRemove: (printingId: string, variant: string) => void;
		onClose: () => void;
	} = $props();

	// Hide deprecated printings the user doesn't own (e.g. a bogus
	// reverse_holo for a base-set card that variant expansion has since
	// dropped). Keep deprecated printings the user *does* own visible — and
	// dimmed — so they can still see and remove those copies. Sort by
	// (rank, tiebreak, code) so the modal mirrors the browse slot chip order.
	const visible = $derived(
		slot.printings
			.filter((p) => !p.deprecated || p.owned_count > 0)
			.slice()
			.sort((a, b) => variantSortCmp(a.variant, b.variant))
	);
	// For bundle slots whose card lives in another set, route the
	// card-detail link to the home set rather than the bundle slug.
	const linkSet = $derived(slot.external_set?.set_code ?? setCode);
</script>

<svelte:window
	onkeydown={(e) => {
		if (e.key === 'Escape') onClose();
	}}
/>

<div class="backdrop" role="presentation" onclick={onClose}></div>
<div class="modal" role="dialog" aria-modal="true" aria-label="{slot.name} printings">
	<header>
		<h3>#{slot.number} · {slot.name}</h3>
		<button class="x" onclick={onClose} aria-label="Close">×</button>
	</header>

	<!-- Condition for new adds. Bound to the parent so the choice persists
	     across modal opens within the same page visit; both the modal "+"
	     and any inline pip "+" on the page use this same value. -->
	<label class="cond">
		<span>Condition</span>
		<select bind:value={condition}>
			{#each CONDITIONS as c (c)}<option value={c}>{c}</option>{/each}
		</select>
	</label>

	<ul>
		{#each visible as p (p.printing_id)}
			<li class:dim={p.deprecated}>
				<div class="vlabel">
					<span class="variant">{variantLabel(p.variant)}</span>
					{#if variantProvenance(p.variant)}
						<span class="provenance">{variantProvenance(p.variant)}</span>
					{/if}
				</div>
				<span class="price">
					{p.market_price != null ? money(p.market_price) : ''}
				</span>
				<div class="stepper">
					<button
						class="step"
						disabled={p.deprecated || p.owned_count <= 0}
						onclick={() => onRemove(p.printing_id, p.variant)}
						aria-label="Remove one {variantLabel(p.variant)}"
					>
						−
					</button>
					<span class="count" class:has={p.owned_count > 0}>{p.owned_count}</span>
					<button
						class="step"
						disabled={p.deprecated}
						onclick={() => onAdd(p.printing_id, p.variant)}
						aria-label="Add one {variantLabel(p.variant)}"
					>
						+
					</button>
				</div>
			</li>
		{/each}
	</ul>

	<a class="full" href="/card/{linkSet}/{slot.number}">Full card details →</a>
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
		box-sizing: border-box;
		width: 440px;
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
		grid-template-columns: 1fr auto auto;
		gap: 0.75rem;
		align-items: center;
		padding: 0.45rem 0;
		border-bottom: 1px solid #0f3460;
	}
	li.dim {
		opacity: 0.5;
	}
	.vlabel {
		display: flex;
		flex-direction: column;
		gap: 0.1rem;
		min-width: 0;
	}
	.provenance {
		color: #888;
		font-size: 0.72rem;
		line-height: 1.25;
	}
	.price {
		font-size: 0.8rem;
		color: #888;
	}
	.stepper {
		display: flex;
		align-items: center;
	}
	.step {
		background: #0f3460;
		border: none;
		color: #e0e0e0;
		width: 30px;
		height: 30px;
		cursor: pointer;
		font-size: 1.1rem;
		line-height: 1;
	}
	.step:first-child {
		border-radius: 6px 0 0 6px;
	}
	.step:last-child {
		border-radius: 0 6px 6px 0;
	}
	.step:hover:not(:disabled) {
		background: #e94560;
	}
	.step:disabled {
		opacity: 0.35;
		cursor: default;
	}
	.count {
		min-width: 34px;
		height: 30px;
		line-height: 30px;
		text-align: center;
		font-size: 0.9rem;
		color: #888;
		background: #1a1a2e;
	}
	.count.has {
		color: #9fe7a0;
	}
	.full {
		color: #888;
		font-size: 0.85rem;
		text-decoration: none;
	}
	.full:hover {
		color: #e94560;
	}
	/* Condition picker row — sits between the header and the printings list. */
	.cond {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		margin: 0.75rem 0 0;
		font-size: 0.85rem;
		color: #ccc;
	}
	.cond > span {
		color: #888;
	}
	.cond select {
		flex: 1;
		background: #1a1a2e;
		border: 1px solid #0f3460;
		color: #e0e0e0;
		border-radius: 6px;
		padding: 0.3rem 0.5rem;
		font: inherit;
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
		.step {
			width: 38px;
			height: 38px;
		}
		.count {
			height: 38px;
			line-height: 38px;
		}
	}
</style>
