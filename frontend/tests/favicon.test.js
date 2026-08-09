/**
 * The browser tab is the one piece of chrome no screenshot covers.
 *
 * `static/favicon.svg` shipped as the stock SvelteKit scaffold mark for the
 * life of the project — a Svelte logo in the tab of a Pokemon collection
 * tracker — and nothing failed, because no route renders it and the token
 * gates scan `src/` only. A favicon is a static asset fetched outside the
 * document: it cannot reference a custom property, so it is deliberately out
 * of the token layer's scope (see the note on pd-xqvg). Out of scope for the
 * ratchet is not the same as unowned, and this file is the ownership.
 *
 * Runs under Node's built-in runner — no test dependency:
 *   npm test        (frontend/)
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const frontendRoot = join(here, '..');
const faviconPath = join(frontendRoot, 'static/favicon.svg');
const tokensPath = join(frontendRoot, 'src/lib/styles/tokens.css');

const favicon = readFileSync(faviconPath, 'utf8');

test('the favicon is the PokeDumpster mark, not the scaffold logo', () => {
	// Both fingerprints of the scaffold asset, not the word "svelte" — the file
	// legitimately names Pokeball.svelte in a comment.
	for (const scaffold of [/svelte-logo/i, /#ff3e00/i]) {
		assert.ok(
			!scaffold.test(favicon),
			'static/favicon.svg is the stock SvelteKit logo again. The tab icon is the ' +
				'Pokeball — same geometry as src/lib/components/Pokeball.svelte.'
		);
	}
	assert.match(
		favicon,
		/<title>PokeDumpster<\/title>/,
		'the mark should name itself, so the next person to open the file knows what it is'
	);
});

test('the favicon is painted in the brand crimson', () => {
	// It cannot spend a token, so it spells the reference step out. What this
	// asserts is that the two stay the same colour: a re-skin that edits the
	// reference block and forgets the tab icon fails here rather than shipping
	// a favicon in the old brand.
	const crimson = readFileSync(tokensPath, 'utf8').match(
		/--pd-crimson-500:\s*(#[0-9a-fA-F]{3,8})\s*;/
	);
	assert.ok(crimson, 'tokens.css no longer declares --pd-crimson-500');
	assert.ok(
		favicon.toLowerCase().includes(crimson[1].toLowerCase()),
		`static/favicon.svg does not carry ${crimson[1]}, the crimson-500 reference step. ` +
			'A favicon cannot resolve var(), so re-skinning the brand means editing ' +
			'this hex alongside the reference block in tokens.css.'
	);
});

test('the favicon is well-formed XML', () => {
	// Served as image/svg+xml, so it is parsed as XML, not HTML. XML forbids
	// `--` inside a comment — and a token name like `--pd-crimson-500` in an
	// explanatory comment is exactly how that gets in. The browser then drops
	// the whole image with no console error, which is unfalsifiable by eye if
	// the old icon is still cached. Caught for real while writing this file.
	const comments = [...favicon.matchAll(/<!--([\s\S]*?)-->/g)];
	for (const [, body] of comments) {
		assert.ok(
			!body.includes('--'),
			'an XML comment may not contain `--`; the browser silently drops the ' +
				'entire image. Write token names without the leading hyphens.'
		);
	}
	// The cheap structural checks that survive without an XML parser dependency.
	assert.match(favicon, /^<svg[^>]*\bxmlns="http:\/\/www\.w3\.org\/2000\/svg"/);
	assert.match(favicon, /<\/svg>\s*$/);
	assert.equal(
		(favicon.match(/</g) ?? []).length,
		(favicon.match(/>/g) ?? []).length,
		'unbalanced angle brackets'
	);
});
