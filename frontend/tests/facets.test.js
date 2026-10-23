import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

// facets.ts is plain functions with only `: string` annotations; strip them and
// import the result so the test runs under plain node.
const src = readFileSync(new URL('../src/lib/facets.ts', import.meta.url), 'utf8').replace(
	/: string/g,
	''
);
const { facetHref } = await import('data:text/javascript;base64,' + Buffer.from(src).toString('base64'));

/** @param {string} href */
const q = (href) => new URL(href, 'http://x').searchParams;

test('facetHref carries no all param', () => {
	assert.equal(q(facetHref('type', 'Fire')).has('all'), false);
});
test('a plain query yields an owned-only search', () => {
	assert.equal(q(facetHref('rarity', 'Rare Holo', 't:fire')).get('q'), 'rarity:"Rare Holo"');
});
test('is:unowned in the current query is kept', () => {
	assert.equal(q(facetHref('trait', 'Rebel', 'is:unowned t:fire')).get('q'), 'is:unowned trait:Rebel');
});
