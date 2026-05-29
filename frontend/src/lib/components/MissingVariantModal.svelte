<script lang="ts">
	import { api } from '$lib/api';

	// "Missing Variant" escape hatch (decision pokedumpster-x7k). One
	// submit creates a user_printing row, adds N copies to the
	// collection, and optionally records the first manual price — all
	// in a single transaction on the backend.
	let {
		cardId,
		cardLabel,
		onClose,
		onCreated
	}: {
		cardId: string;
		cardLabel: string;
		onClose: () => void;
		onCreated: () => void;
	} = $props();

	let description = $state('');
	let priceInput = $state('');
	let timeInput = $state(''); // datetime-local, optional
	let noteInput = $state('');
	let qty = $state(1);
	let busy = $state(false);
	let error = $state<string | null>(null);

	function stepQty(delta: number) {
		const next = Math.max(0, qty + delta);
		qty = next;
	}

	async function submit(e: SubmitEvent) {
		e.preventDefault();
		const trimmedDesc = description.trim();
		let price: number | null = null;
		if (priceInput) {
			const parsed = parseFloat(priceInput);
			if (!Number.isFinite(parsed) || parsed < 0) {
				error = 'Price must be a non-negative number.';
				return;
			}
			price = parsed;
		}
		busy = true;
		error = null;
		try {
			await api.addMissingVariant({
				card_id: cardId,
				description: trimmedDesc || null,
				qty,
				price,
				observed_at: timeInput ? new Date(timeInput).toISOString() : null,
				note: noteInput.trim() || null
			});
			onCreated();
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			busy = false;
		}
	}
</script>

<svelte:window
	onkeydown={(e) => {
		if (e.key === 'Escape') onClose();
	}}
/>

<div class="backdrop" role="presentation" onclick={onClose}></div>
<div class="modal" role="dialog" aria-modal="true" aria-label="Add missing variant for {cardLabel}">
	<header>
		<h3>Add missing variant — {cardLabel}</h3>
		<button class="x" onclick={onClose} aria-label="Close">×</button>
	</header>

	<p class="help">
		Use this for a copy whose variant isn't yet in the catalog (a misprint,
		undocumented promo, etc.). Add details now and update the data model later.
	</p>

	<form onsubmit={submit}>
		<label class="full">
			<span>Variant description <em>(optional)</em></span>
			<input
				type="text"
				bind:value={description}
				placeholder="e.g. no set stamp misprint"
				disabled={busy}
			/>
		</label>

		<div class="row">
			<div class="qty-wrap">
				<span class="lbl">Copies to add</span>
				<div class="qty">
					<button
						type="button"
						class="step"
						disabled={busy || qty <= 0}
						onclick={() => stepQty(-1)}
						aria-label="One fewer copy"
					>−</button>
					<span class="qty-val">{qty}</span>
					<button
						type="button"
						class="step"
						disabled={busy}
						onclick={() => stepQty(1)}
						aria-label="One more copy"
					>+</button>
				</div>
			</div>
			<label>
				<span>Price <em>(optional)</em></span>
				<input
					type="number"
					step="0.01"
					min="0"
					bind:value={priceInput}
					placeholder="e.g. 200.00"
					disabled={busy}
				/>
			</label>
		</div>

		<div class="row">
			<label>
				<span>Price observed <em>(optional)</em></span>
				<input type="datetime-local" bind:value={timeInput} disabled={busy} />
			</label>
			<label>
				<span>Price note <em>(optional)</em></span>
				<input
					type="text"
					bind:value={noteInput}
					placeholder="eBay sold, comp, …"
					disabled={busy}
				/>
			</label>
		</div>

		{#if error}<p class="error">{error}</p>{/if}

		<div class="actions">
			<button type="button" class="cancel" onclick={onClose} disabled={busy}>Cancel</button>
			<button type="submit" class="save" disabled={busy}>
				{busy ? 'Saving…' : 'Add variant'}
			</button>
		</div>
	</form>
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
		width: 540px;
		max-width: 94vw;
		max-height: 88vh;
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
		font-size: 1.05rem;
	}
	.x {
		background: none;
		border: none;
		color: #888;
		font-size: 1.4rem;
		cursor: pointer;
		line-height: 1;
	}
	.help {
		color: #888;
		font-size: 0.8rem;
		margin: 0.75rem 0 0.75rem;
	}
	form {
		display: flex;
		flex-direction: column;
		gap: 0.7rem;
	}
	.row {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 0.7rem;
	}
	label {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		font-size: 0.8rem;
		color: #aaa;
	}
	label em {
		color: #666;
		font-style: normal;
	}
	label.full {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		font-size: 0.8rem;
		color: #aaa;
	}
	input {
		background: #1a1a2e;
		border: 1px solid #0f3460;
		color: #e0e0e0;
		padding: 0.45rem 0.55rem;
		border-radius: 6px;
		font: inherit;
	}
	input:focus {
		outline: none;
		border-color: #e94560;
	}
	.qty-wrap {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		font-size: 0.8rem;
		color: #aaa;
	}
	.qty-wrap .lbl {
		color: #aaa;
	}
	.qty {
		display: inline-flex;
		align-items: center;
		gap: 0;
		width: max-content;
	}
	.step {
		background: #0f3460;
		border: none;
		color: #e0e0e0;
		width: 34px;
		height: 34px;
		font: inherit;
		font-size: 1.1rem;
		line-height: 1;
		cursor: pointer;
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
	.qty-val {
		min-width: 38px;
		height: 34px;
		line-height: 34px;
		text-align: center;
		font: inherit;
		font-variant-numeric: tabular-nums;
		font-weight: 600;
		color: #e0e0e0;
		background: #1a1a2e;
	}
	.actions {
		display: flex;
		justify-content: flex-end;
		gap: 0.6rem;
		margin-top: 0.25rem;
	}
	.cancel {
		background: none;
		border: 1px solid #0f3460;
		color: #ccc;
		padding: 0.45rem 0.95rem;
		border-radius: 6px;
		font: inherit;
		cursor: pointer;
	}
	.cancel:hover:not(:disabled) {
		border-color: #888;
	}
	.save {
		background: #e94560;
		border: none;
		color: white;
		padding: 0.45rem 1.1rem;
		border-radius: 6px;
		font: inherit;
		cursor: pointer;
	}
	.save:disabled,
	.cancel:disabled {
		opacity: 0.5;
		cursor: default;
	}
	.error {
		color: #e94560;
		font-size: 0.85rem;
		margin: 0;
	}

	@media (max-width: 540px) {
		.modal {
			top: auto;
			bottom: 0;
			left: 0;
			transform: none;
			width: 100%;
			max-width: 100%;
			max-height: 82vh;
			border-radius: 14px 14px 0 0;
		}
		.row {
			grid-template-columns: 1fr;
		}
	}
</style>
