<!--
	ProgressBar — set-completion, the app's most repeated non-text figure.

	/browse draws one per set tile (and colours the base bar green against the
	master bar's crimson), /browse/[set] draws two in its header, and
	/browse/[set]/stats draws one per metric row at two thicknesses. All three
	hand-rolled the same track/fill pair.

	The fill's meaning is the tone, and the tone names a role rather than a
	colour: `complete` / `partial` / `empty` are the progress roles the token
	layer already declares, so a bar that later wants to colour itself BY
	completion changes one prop instead of three hex values.

	It is a real progressbar to assistive tech, which the hand-rolled
	`<div class="bar"><span style="width:40%">` never was.
-->
<script lang="ts">
	import type { HTMLAttributes } from 'svelte/elements';

	type Props = {
		value: number;
		max?: number;
		tone?: 'accent' | 'complete' | 'partial' | 'empty';
		size?: 'sm' | 'md';
		/** Caption above the track — "Base 42/102". Also the accessible name. */
		label?: string;
		class?: string;
	} & HTMLAttributes<HTMLDivElement>;

	let {
		value,
		max = 100,
		tone = 'accent',
		size = 'sm',
		label = undefined,
		class: extra = '',
		...rest
	}: Props = $props();

	// A set with no cards is 0%, not NaN% — /browse hits this on an
	// unpopulated synthesized set.
	const percent = $derived(max > 0 ? Math.min(100, Math.max(0, (value / max) * 100)) : 0);
	const classes = $derived(['bar', `t-${tone}`, `s-${size}`, extra].filter(Boolean).join(' '));
</script>

<div class={classes} {...rest}>
	{#if label}<span class="label">{label}</span>{/if}
	<div
		class="track"
		role="progressbar"
		aria-valuenow={value}
		aria-valuemin={0}
		aria-valuemax={max}
		aria-label={label}
	>
		<span class="fill" style:width="{percent}%"></span>
	</div>
</div>

<style>
	.bar {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
	}
	.label {
		font-size: var(--text-md);
		color: var(--color-text-muted);
	}
	.track {
		width: 100%;
		background: var(--color-progress-track);
		border-radius: var(--radius-pill);
		overflow: hidden;
	}
	.fill {
		display: block;
		height: 100%;
		background: var(--bar-fill);
		transition: width var(--dur-slow) var(--ease-standard);
	}

	/* --- size -------------------------------------------------------------
	   The hand-rolled bars were 6px and 8px; 6px is off the 4px ramp, so the
	   thin one snaps to --space-1. Snapping is the point of a ramp — the
	   alternative is a calc() that rebuilds an off-ramp value and re-opens
	   "every bar is its own thickness". */
	.s-sm .track {
		height: var(--space-1);
	}
	.s-md .track {
		height: var(--space-2);
	}

	/* --- tone ------------------------------------------------------------- */
	.t-accent {
		--bar-fill: var(--color-accent);
	}
	.t-complete {
		--bar-fill: var(--color-progress-fill);
	}
	.t-partial {
		--bar-fill: var(--color-progress-fill-partial);
	}
	.t-empty {
		--bar-fill: var(--color-progress-fill-empty);
	}
</style>
