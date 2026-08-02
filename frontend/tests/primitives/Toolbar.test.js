import { test } from 'node:test';
import assert from 'node:assert/strict';
import Toolbar from '../../src/lib/components/ui/Toolbar.svelte';
import { markup, styles, rootClass, slot, assertTokenOnly, assertTokensDeclared } from '../support/render.js';

/** @param {Record<string, unknown>} [props] */
const body = (props = {}) =>
	markup(Toolbar, { children: slot('<button>Sort</button>'), ...props });

test('Toolbar renders its controls in a row', () => {
	const html = body();
	assert.match(html, /<button>Sort<\/button>/);
	assert.match(rootClass(html), /\btoolbar\b/);
	assert.match(rootClass(html), /\bd-row\b.*\ba-center\b/);
	assert.match(rootClass(html), /\bwrap\b/, 'controls wrap by default — /browse/[set] has a lot of them');
});

test('Toolbar respects its variants', () => {
	assert.match(rootClass(body({ direction: 'column' })), /\bd-column\b/);
	for (const gap of ['sm', 'md', 'lg']) {
		assert.match(rootClass(body({ gap })), new RegExp(`\\bg-${gap}\\b`));
	}
	for (const align of ['center', 'baseline', 'start', 'end']) {
		assert.match(rootClass(body({ align })), new RegExp(`\\ba-${align}\\b`));
	}
	for (const justify of ['start', 'between', 'end']) {
		assert.match(rootClass(body({ justify })), new RegExp(`\\bj-${justify}\\b`));
	}
	assert.doesNotMatch(rootClass(body({ wrap: false })), /\bwrap\b/);
});

test('Toolbar has no chrome of its own until asked', () => {
	assert.doesNotMatch(rootClass(body()), /\b(sticky|bordered)\b/);
	assert.match(rootClass(body({ bordered: true })), /\bbordered\b/);

	// A pinned toolbar always carries its rule: it is what separates it from
	// the content scrolling underneath.
	const pinned = rootClass(body({ sticky: true }));
	assert.match(pinned, /\bsticky\b/);
	assert.match(pinned, /\bbordered\b/);
});

test('Toolbar emits no hardcoded colour', () => {
	assertTokenOnly('Toolbar.svelte');
	const css = styles(Toolbar, { children: slot() });
	// The pinned band is the translucent chrome role, not an opaque panel —
	// content shows through instead of being stamped over.
	assert.match(css, /\.sticky[^{]*\{[^}]*var\(--color-surface-sticky\)/);
	assertTokensDeclared(css);
});
