<script lang="ts">
	// The PokeDumpster mark — red strokes on transparent, DD-style.
	// One source of truth for every place that renders the logo
	// (breadcrumb chrome, /collection brand, home hero).
	let {
		size = 22,
		stroke = '#e94560'
	}: { size?: number; stroke?: string } = $props();
</script>

<svg
	viewBox="0 0 24 24"
	width={size}
	height={size}
	xmlns="http://www.w3.org/2000/svg"
	aria-hidden="true"
>
	<circle cx="12" cy="12" r="10" fill="none" {stroke} stroke-width="2" />
	<line x1="2.5" y1="12" x2="21.5" y2="12" {stroke} stroke-width="2" />
	<circle cx="12" cy="12" r="3.2" fill="none" {stroke} stroke-width="2" />
</svg>
