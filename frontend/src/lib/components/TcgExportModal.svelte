<script lang="ts">
	import { onMount } from 'svelte';
	import { api } from '$lib/api';
	import type { MissingExport } from '$lib/types/MissingExport';
	import type { MissingCard } from '$lib/types/MissingCard';

	let {
		setCode,
		onClose
	}: {
		setCode: string;
		onClose: () => void;
	} = $props();

	let data = $state<MissingExport | null>(null);
	let error = $state<string | null>(null);
	let loading = $state(true);

	/** `base` = numbered cards only; `master` = base + subsets + promos.
	    Secret rares are never swept in by scope — they're confirmed per-card. */
	let scope = $state<'base' | 'master'>('base');
	/** card_ids of secret rares the user has ticked (default: none). */
	let confirmedSecrets = $state<Set<string>>(new Set());
	let copied = $state(false);

	onMount(async () => {
		try {
			data = await api.tcgExport(setCode);
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	});

	const inScope = (c: MissingCard) =>
		c.section === 'base' || (scope === 'master' && (c.section === 'subset' || c.section === 'promo'));

	const baseCount = $derived(data ? data.cards.filter((c) => c.section === 'base').length : 0);
	const masterCount = $derived(
		data
			? data.cards.filter(
					(c) => c.section === 'base' || c.section === 'subset' || c.section === 'promo'
				).length
			: 0
	);
	const secretCards = $derived(data ? data.cards.filter((c) => c.section === 'secret') : []);

	/** Cards the current scope + secret confirmations select. */
	const selected = $derived(
		data
			? data.cards.filter((c) =>
					c.section === 'secret' ? confirmedSecrets.has(c.card_id) : inScope(c)
				)
			: []
	);
	const lines = $derived(
		selected
			.map((c) => c.mass_entry_line)
			.filter((l): l is string => l != null)
			.join('\n')
	);
	/** Selected cards we can't build a line for (set has no TCGplayer code). */
	const unmappable = $derived(selected.filter((c) => c.mass_entry_line == null));
	const lineCount = $derived(selected.length - unmappable.length);

	function toggleSecret(id: string) {
		const next = new Set(confirmedSecrets);
		if (next.has(id)) next.delete(id);
		else next.add(id);
		confirmedSecrets = next;
	}

	function allSecretsChecked() {
		return secretCards.length > 0 && secretCards.every((c) => confirmedSecrets.has(c.card_id));
	}
	function toggleAllSecrets() {
		confirmedSecrets = allSecretsChecked()
			? new Set()
			: new Set(secretCards.map((c) => c.card_id));
	}

	let copyTimer: ReturnType<typeof setTimeout> | undefined;

	// navigator.clipboard only exists in a secure context (HTTPS/localhost).
	// The app is reached over WireGuard at an HTTP IP, so fall back to
	// selecting the textarea + execCommand, and finally to just selecting it
	// so the user can Ctrl+C.
	function legacyCopy(text: string): boolean {
		const ta = document.getElementById('tcg-lines');
		if (!(ta instanceof HTMLTextAreaElement)) return false;
		ta.focus();
		ta.select();
		try {
			return document.execCommand('copy');
		} catch {
			return false;
		}
	}

	async function copy() {
		if (!lines) return;
		let ok = false;
		try {
			if (navigator.clipboard?.writeText) {
				await navigator.clipboard.writeText(lines);
				ok = true;
			}
		} catch {
			// secure-context API present but blocked — fall through.
		}
		if (!ok) ok = legacyCopy(lines);
		if (ok) {
			copied = true;
			clearTimeout(copyTimer);
			copyTimer = setTimeout(() => (copied = false), 1600);
		} else {
			// Last resort: leave the text selected so the user can copy manually.
			legacyCopy(lines);
		}
	}
</script>

<svelte:window
	onkeydown={(e) => {
		if (e.key === 'Escape') onClose();
	}}
/>

<div class="backdrop" role="presentation" onclick={onClose}></div>
<div class="modal" role="dialog" aria-modal="true" aria-label="Buy missing cards on TCGplayer">
	<div class="head">
		<h2>Buy missing — TCGplayer</h2>
		<button class="x" onclick={onClose} aria-label="Close">×</button>
	</div>

	<div class="body">
		{#if loading}
			<p class="muted">Loading missing cards…</p>
		{:else if error}
			<p class="err">Couldn't load: {error}</p>
		{:else if data}
			{#if data.cards.length === 0}
				<p class="muted">You're not missing any cards in this set. 🎉</p>
			{:else}
				<fieldset class="scope">
					<legend>Scope</legend>
					<label>
						<input type="radio" bind:group={scope} value="base" />
						Base set <span class="count">{baseCount} missing</span>
					</label>
					<label>
						<input type="radio" bind:group={scope} value="master" />
						Master set <span class="count">{masterCount} missing</span>
						<span class="hint">(base + subsets + promos)</span>
					</label>
				</fieldset>

				{#if secretCards.length > 0}
					<fieldset class="secrets">
						<legend>Secret rares — confirm each</legend>
						<label class="all">
							<input
								type="checkbox"
								checked={allSecretsChecked()}
								onchange={toggleAllSecrets}
							/>
							Select all ({secretCards.length})
						</label>
						<ul>
							{#each secretCards as c (c.card_id)}
								<li>
									<label>
										<input
											type="checkbox"
											checked={confirmedSecrets.has(c.card_id)}
											onchange={() => toggleSecret(c.card_id)}
										/>
										<span class="num">#{c.number}</span>
										{c.name}
									</label>
								</li>
							{/each}
						</ul>
					</fieldset>
				{/if}

				{#if data.ptcgo_code == null}
					<p class="warn">
						This set has no TCGplayer set code, so no Mass Entry lines can be built.
						Search for these cards manually on TCGplayer.
					</p>
				{:else if unmappable.length > 0}
					<p class="warn">
						Couldn't include {unmappable.length}
						{unmappable.length === 1 ? 'card' : 'cards'} (no TCGplayer code):
						{unmappable.map((c) => `#${c.number} ${c.name}`).join(', ')}
					</p>
				{/if}

				<label class="outlabel" for="tcg-lines">
					Mass Entry list <span class="count">{lineCount} {lineCount === 1 ? 'card' : 'cards'}</span>
				</label>
				<textarea
					id="tcg-lines"
					readonly
					rows="8"
					placeholder="Pick a scope (and any secret rares) to build your list."
					value={lines}
				></textarea>

				<div class="actions">
					<button class="primary" onclick={copy} disabled={!lines}>
						{copied ? 'Copied!' : 'Copy to clipboard'}
					</button>
					<a
						class="link"
						href="https://www.tcgplayer.com/massentry"
						target="_blank"
						rel="noopener"
					>
						Open TCGplayer Mass Entry ↗
					</a>
				</div>
				<p class="muted small">
					Paste the list into Mass Entry, then pick the printing (holo / reverse / normal)
					during cart review.
				</p>
			{/if}
		{/if}
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
		top: 6vh;
		left: 50%;
		transform: translateX(-50%);
		z-index: 101;
		width: 540px;
		box-sizing: border-box;
		max-width: 92vw;
		max-height: 88vh;
		display: flex;
		flex-direction: column;
		overflow: hidden;
		background: var(--color-surface-overlay);
		border: 2px solid var(--color-border);
		border-radius: 12px;
	}
	.head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 0.9rem 1.1rem;
		border-bottom: 1px solid var(--color-border);
	}
	.head h2 {
		margin: 0;
		font-size: 1.05rem;
		color: var(--color-text);
	}
	.x {
		background: none;
		border: none;
		color: var(--color-text-subtle);
		font-size: 1.4rem;
		line-height: 1;
		cursor: pointer;
		padding: 0 0.2rem;
	}
	.x:hover {
		color: var(--color-text-accent);
	}
	.body {
		flex: 1;
		overflow-y: auto;
		padding: 1rem 1.1rem 1.25rem;
		display: flex;
		flex-direction: column;
		gap: 0.85rem;
	}
	fieldset {
		border: 1px solid var(--color-border);
		border-radius: 8px;
		padding: 0.6rem 0.8rem 0.7rem;
		margin: 0;
	}
	legend {
		font-size: 0.75rem;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		color: var(--color-text-subtle);
		padding: 0 0.3rem;
	}
	.scope label {
		display: flex;
		align-items: baseline;
		gap: 0.45rem;
		padding: 0.2rem 0;
		color: var(--color-text);
		cursor: pointer;
	}
	.count {
		color: var(--color-warning-text);
		font-size: 0.82rem;
	}
	.hint {
		color: var(--color-text-subtle);
		font-size: 0.78rem;
	}
	.secrets .all {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		color: var(--color-text-muted);
		font-size: 0.85rem;
		padding-bottom: 0.35rem;
		border-bottom: 1px solid var(--color-border);
		margin-bottom: 0.35rem;
	}
	.secrets ul {
		list-style: none;
		margin: 0;
		padding: 0;
		max-height: 11rem;
		overflow-y: auto;
	}
	.secrets li label {
		display: flex;
		align-items: center;
		gap: 0.45rem;
		padding: 0.18rem 0;
		color: var(--color-text);
		cursor: pointer;
	}
	.num {
		color: var(--color-text-subtle);
		font-variant-numeric: tabular-nums;
		min-width: 2.6rem;
	}
	.outlabel {
		display: flex;
		align-items: baseline;
		gap: 0.5rem;
		font-size: 0.8rem;
		color: var(--color-text-subtle);
	}
	textarea {
		width: 100%;
		box-sizing: border-box;
		background: var(--color-surface-page);
		border: 1px solid var(--color-border);
		border-radius: 8px;
		color: var(--color-text);
		font-family: ui-monospace, monospace;
		font-size: 0.85rem;
		padding: 0.55rem 0.65rem;
		resize: vertical;
	}
	.actions {
		display: flex;
		align-items: center;
		gap: 0.8rem;
		flex-wrap: wrap;
	}
	.primary {
		background: var(--color-accent);
		border: none;
		border-radius: 8px;
		color: var(--color-on-accent);
		font-size: 0.9rem;
		padding: 0.5rem 0.95rem;
		cursor: pointer;
	}
	.primary:disabled {
		background: var(--color-neutral-surface);
		color: var(--color-text-subtle);
		cursor: not-allowed;
	}
	.link {
		color: var(--color-info-text);
		font-size: 0.88rem;
		text-decoration: none;
	}
	.link:hover {
		text-decoration: underline;
	}
	.warn {
		margin: 0;
		background: var(--color-warning-surface);
		border: 1px solid var(--color-warning-border);
		border-radius: 8px;
		color: var(--color-warning-text);
		font-size: 0.82rem;
		padding: 0.5rem 0.65rem;
	}
	.err {
		color: var(--color-text-accent);
	}
	.muted {
		color: var(--color-text-subtle);
	}
	.small {
		font-size: 0.78rem;
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
			border-radius: 14px 14px 0 0;
		}
	}
</style>
