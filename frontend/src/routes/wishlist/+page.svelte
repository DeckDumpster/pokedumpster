<script lang="ts">
	import { api } from '$lib/api';
	import { money } from '$lib/format';
	import type { WishlistEntry } from '$lib/types/WishlistEntry';

	let wishes = $state<WishlistEntry[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let showFulfilled = $state(false);
	let busy = $state(false);

	// Add form.
	let setCode = $state('');
	let number = $state('');
	let priority = $state(0);
	let maxPrice = $state('');

	async function load() {
		try {
			wishes = await api.wishlist(showFulfilled);
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void showFulfilled;
		load();
	});

	async function add() {
		if (!setCode.trim() || !number.trim()) {
			error = 'Set code and collector number are required.';
			return;
		}
		busy = true;
		error = null;
		try {
			const card = await api.card(setCode.trim(), number.trim());
			await api.addWish({
				card_id: card.card.card_id,
				priority: priority || undefined,
				max_price: maxPrice ? Number(maxPrice) : undefined
			});
			setCode = '';
			number = '';
			priority = 0;
			maxPrice = '';
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}

	async function act(fn: () => Promise<unknown>) {
		busy = true;
		error = null;
		try {
			await fn();
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}

</script>

<svelte:head><title>Wishlist — PokeDumpster</title></svelte:head>

<h1>Wishlist</h1>

<form
	class="addform"
	onsubmit={(e) => {
		e.preventDefault();
		add();
	}}
>
	<label>Set <input type="text" placeholder="sv3pt5" bind:value={setCode} /></label>
	<label>Number <input type="text" placeholder="6" bind:value={number} /></label>
	<label>Priority <input type="number" min="0" bind:value={priority} /></label>
	<label>Max price <input type="number" min="0" step="0.01" bind:value={maxPrice} /></label>
	<button type="submit" disabled={busy}>Add wish</button>
</form>

<label class="toggle"><input type="checkbox" bind:checked={showFulfilled} /> Show fulfilled</label>

{#if error}<p class="error">{error}</p>{/if}

{#if loading}
	<p class="muted">Loading…</p>
{:else if wishes.length === 0}
	<p class="muted">Nothing on your wishlist{showFulfilled ? '' : ' yet'}.</p>
{:else}
	<table>
		<thead>
			<tr><th>Card</th><th>Set</th><th>#</th><th>Priority</th><th>Max</th><th></th></tr>
		</thead>
		<tbody>
			{#each wishes as w (w.id)}
				<tr class:dim={w.fulfilled_at != null}>
					<td><a href="/card/{w.set_code}/{w.number}">{w.name}</a></td>
					<td><a href="/browse/{w.set_code}">{w.set_name}</a></td>
					<td>{w.number}</td>
					<td>{w.priority}</td>
					<td>{money(w.max_price)}</td>
					<td class="actions">
						{#if w.fulfilled_at == null}
							<button class="link" disabled={busy} onclick={() => act(() => api.fulfillWish(w.id, true))}>
								Fulfill
							</button>
						{:else}
							<button class="link" disabled={busy} onclick={() => act(() => api.fulfillWish(w.id, false))}>
								Reopen
							</button>
						{/if}
						<button class="link" disabled={busy} onclick={() => act(() => api.deleteWish(w.id))}>
							Remove
						</button>
					</td>
				</tr>
			{/each}
		</tbody>
	</table>
{/if}

<style>
	h1 {
		color: #e94560;
	}
	.muted {
		color: #888;
	}
	.error {
		color: #e94560;
	}
	.addform {
		display: flex;
		gap: 0.75rem;
		align-items: flex-end;
		flex-wrap: wrap;
		margin: 1rem 0;
	}
	label {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		font-size: 0.8rem;
		color: #888;
	}
	input {
		padding: 0.4rem;
		background: #1a1a2e;
		border: 1px solid #0f3460;
		border-radius: 6px;
		color: #e0e0e0;
	}
	.toggle {
		flex-direction: row;
		align-items: center;
		gap: 0.4rem;
		margin-bottom: 1rem;
	}
	button {
		background: #e94560;
		border: none;
		color: #fff;
		padding: 0.45rem 0.9rem;
		border-radius: 6px;
		cursor: pointer;
	}
	button.link {
		background: none;
		color: #888;
		padding: 0;
	}
	button.link:hover {
		color: #e94560;
	}
	button:disabled {
		opacity: 0.5;
	}
	table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.9rem;
	}
	th {
		text-align: left;
		padding: 0.4rem 0.6rem;
		border-bottom: 2px solid #0f3460;
		color: #888;
		font-size: 0.75rem;
		text-transform: uppercase;
	}
	td {
		padding: 0.4rem 0.6rem;
		border-bottom: 1px solid #0f3460;
	}
	tr.dim {
		opacity: 0.5;
	}
	.actions {
		display: flex;
		gap: 0.75rem;
	}
	a {
		color: #e0e0e0;
	}
	a:hover {
		color: #e94560;
	}
</style>
