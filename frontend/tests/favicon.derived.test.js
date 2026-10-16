/**
 * The favicon is the header's brand art, derived from it — not a second copy.
 * Source: src/lib/components/Pokeball.svelte (the mark every header renders).
 * Generator: scripts/brand.mjs.
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { build, shapes } from '../scripts/brand.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const read = (p, enc) => readFileSync(join(root, p), enc);

test('favicon.svg carries exactly the header art\'s shapes', () => {
	const header = shapes(read('src/lib/components/Pokeball.svelte', 'utf8')).map((s) => s.tag);
	const favicon = shapes(read('static/favicon.svg', 'utf8')).map((s) => s.tag);
	assert.ok(header.length >= 3, 'found no shapes in Pokeball.svelte');
	assert.deepEqual(favicon, header, 'favicon path data drifted from Pokeball.svelte; run node scripts/brand.mjs');
});

test('the header art is square, so nothing is stretched', () => {
	assert.match(read('src/lib/components/Pokeball.svelte', 'utf8'), /viewBox="0 0 24 24"/);
});

test('every committed icon equals what the generator derives from the header art', () => {
	for (const [path, buf] of Object.entries(build())) {
		assert.ok(read(path).equals(buf), `${path} is stale; run node scripts/brand.mjs`);
	}
});

test('PNG fallbacks have the required dimensions', () => {
	for (const [path, n] of [['static/favicon-32.png', 32], ['static/apple-touch-icon.png', 180]]) {
		const b = read(path);
		assert.equal(b.readUInt32BE(16), n);
		assert.equal(b.readUInt32BE(20), n);
	}
});

test('every served HTML page links the icons (app.html is the shell of all of them)', () => {
	const html = read('src/app.html', 'utf8');
	assert.match(html, /rel="icon" href="\/favicon\.svg"/);
	assert.match(html, /rel="icon" href="\/favicon-32\.png"/);
	assert.match(html, /rel="apple-touch-icon" href="\/apple-touch-icon\.png"/);
	assert.ok(!/rel="icon"/.test(read('src/routes/+layout.svelte', 'utf8')), 'layout must not carry a second icon link');
});
