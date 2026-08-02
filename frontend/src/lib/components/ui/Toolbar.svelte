<!--
	Toolbar — a row of controls.

	The routes call it `.newform`, `.controls`, `.seltools`, `.actions` and
	`.topbar`, and all five are `display: flex` with a gap and some subset of
	{align, wrap, sticky}. /collection and /sealed pin theirs to the top of the
	viewport over scrolling content, which is what `--color-surface-sticky`
	exists for — a translucent chrome that lets rows show through instead of
	stamping an opaque band across the page.

	No visual weight of its own unless `sticky` or `bordered` says so: a
	toolbar is a layout, and chrome that recedes is the point.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { HTMLAttributes } from 'svelte/elements';

	type Props = {
		direction?: 'row' | 'column';
		gap?: 'sm' | 'md' | 'lg';
		align?: 'center' | 'baseline' | 'start' | 'end';
		justify?: 'start' | 'between' | 'end';
		wrap?: boolean;
		/** Pins to the top of the scroll container over the content beneath. */
		sticky?: boolean;
		/** A rule under the row — implied by `sticky`. */
		bordered?: boolean;
		class?: string;
		children: Snippet;
	} & HTMLAttributes<HTMLDivElement>;

	let {
		direction = 'row',
		gap = 'md',
		align = 'center',
		justify = 'start',
		wrap = true,
		sticky = false,
		bordered = false,
		class: extra = '',
		children,
		...rest
	}: Props = $props();

	const classes = $derived(
		[
			'toolbar',
			`d-${direction}`,
			`g-${gap}`,
			`a-${align}`,
			`j-${justify}`,
			wrap && 'wrap',
			sticky && 'sticky',
			(bordered || sticky) && 'bordered',
			extra
		]
			.filter(Boolean)
			.join(' ')
	);
</script>

<div class={classes} {...rest}>{@render children()}</div>

<style>
	.toolbar {
		display: flex;
	}
	.wrap {
		flex-wrap: wrap;
	}

	.d-row {
		flex-direction: row;
	}
	.d-column {
		flex-direction: column;
	}

	.g-sm {
		gap: var(--space-2);
	}
	.g-md {
		gap: var(--space-3);
	}
	.g-lg {
		gap: var(--space-4);
	}

	.a-center {
		align-items: center;
	}
	.a-baseline {
		align-items: baseline;
	}
	.a-start {
		align-items: flex-start;
	}
	.a-end {
		align-items: flex-end;
	}

	.j-start {
		justify-content: flex-start;
	}
	.j-between {
		justify-content: space-between;
	}
	.j-end {
		justify-content: flex-end;
	}

	.sticky {
		position: sticky;
		top: 0;
		/* Above the rows it scrolls over, below modals and their scrim. */
		z-index: 50;
		padding: var(--space-2) var(--space-3);
		background: var(--color-surface-sticky);
	}
	.bordered {
		border-bottom: 1px solid var(--color-border);
	}
</style>
