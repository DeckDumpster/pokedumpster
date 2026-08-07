/**
 * Zero raw colour is a destination, not an aspiration.
 *
 * Intent #1 of the aesthetic-overhaul epic: the token layer is the ONLY way
 * components reference visual values, target zero raw hex in `frontend/src`
 * outside `tokens.css`. 900-odd literals do not disappear in one commit, so
 * this is a *ratchet*: `src/lib/styles/raw-color-budget.json` records what is
 * left per file, and the count may only go down. Exceed a budget and it reads
 * as a regression; drop below it and the test says so and prints the new
 * number to write. When the budget is empty, the target is met and the test
 * asserts it literally.
 *
 * The sibling file `tokens.contrast.test.js` asserts every raw colour still
 * present maps to a semantic role. This one asserts there are fewer of them
 * every time, and eventually none.
 *
 * Runs under Node's built-in runner — no test dependency:
 *   npm test        (frontend/)
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, relative, sep } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const frontendRoot = join(here, '..');
const srcRoot = join(frontendRoot, 'src');
const tokensPath = join(srcRoot, 'lib/styles/tokens.css');
const budgetPath = join(srcRoot, 'lib/styles/raw-color-budget.json');

/** @type {{ total: number, files: Record<string, number> }} */
const budget = JSON.parse(readFileSync(budgetPath, 'utf8'));

// tokens.css declares the palette; every other file is meant to consume it.
// Extensions beyond the contrast test's set are deliberate: an .svg asset can
// carry a brand colour just as effectively as a component can.
const SCANNED = /\.(svelte|ts|js|css|svg|html)$/;

/** CSS named colours that resolve to an actual paint value. */
const NAMED_COLORS =
	'white|black|red|green|blue|gray|grey|silver|gold|orange|yellow|purple|pink|' +
	'brown|navy|teal|lime|cyan|magenta|maroon|olive|aqua|fuchsia|crimson|coral|' +
	'salmon|khaki|violet|indigo|turquoise|beige|ivory|tan';

/** Properties that paint. A named colour only counts in one of these. */
const PAINTS =
	'color|background|background-color|border[a-z-]*|fill|stroke|outline[a-z-]*|' +
	'box-shadow|text-shadow|caret-color|accent-color|text-decoration-color';

const NAMED_RE = new RegExp(
	String.raw`(?:${PAINTS})\s*:\s*[^;{}]*?(?<![\w-])(?:${NAMED_COLORS})(?![\w-])`,
	'gi'
);
// `(?<![\w&])` keeps `&#8203;` and CSS ids out; `(?![\w-])` keeps Svelte
// class names and custom-property names out.
const HEX_RE = /(?<![\w&])#[0-9a-fA-F]{3,8}(?![\w-])/g;
const FUNC_RE = /(?:rgba?|hsla?)\([^)]*\)/gi;

/**
 * Strip comments so prose ("Porygon #153") and commented-out CSS don't count.
 * @param {string} text
 */
function decolour(text) {
	return text
		.replace(/\/\*[\s\S]*?\*\//g, '')
		.replace(/<!--[\s\S]*?-->/g, '')
		.split('\n')
		.filter((line) => !/^\s*(\/\/|\*)/.test(line))
		.join('\n');
}

/**
 * Every raw colour literal in a file, as written.
 * @param {string} text
 * @returns {string[]}
 */
function rawColors(text) {
	const body = decolour(text);
	return [
		...[...body.matchAll(HEX_RE)].map((m) => m[0]),
		...[...body.matchAll(FUNC_RE)].map((m) => m[0].replace(/\s+/g, ' ')),
		...[...body.matchAll(NAMED_RE)].map((m) => m[0].trim())
	];
}

/**
 * @param {string} dir
 * @returns {string[]}
 */
function walk(dir) {
	/** @type {string[]} */
	const out = [];
	for (const entry of readdirSync(dir)) {
		const full = join(dir, entry);
		if (statSync(full).isDirectory()) out.push(...walk(full));
		else out.push(full);
	}
	return out;
}

/** Actual counts, keyed the same way as the budget: src-relative, `/` separated. */
const actual = new Map(
	walk(srcRoot)
		.filter((f) => f !== tokensPath && SCANNED.test(f))
		.map((f) => /** @type {[string, string[]]} */ ([
			relative(srcRoot, f).split(sep).join('/'),
			rawColors(readFileSync(f, 'utf8'))
		]))
		.filter(([, colors]) => colors.length > 0)
);

// --- the ratchet -----------------------------------------------------------

test('no file carries more raw colour than its budget', () => {
	/** @type {string[]} */
	const regressions = [];
	for (const [file, colors] of actual) {
		const allowed = budget.files[file];
		if (allowed === undefined) {
			regressions.push(
				`${file}: ${colors.length} raw colour(s), but this file has no budget — ` +
					`it is meant to be token-only. Offending: ${[...new Set(colors)].join(', ')}`
			);
		} else if (colors.length > allowed) {
			regressions.push(
				`${file}: ${colors.length} raw colour(s), budget is ${allowed}. ` +
					`Use a semantic --color-* role; do not raise the budget.`
			);
		}
	}
	assert.deepEqual(
		regressions,
		[],
		'\n  ' +
			regressions.join('\n  ') +
			'\n\n  tokens.css is the only file that may hold a colour literal. ' +
			'If a new value is genuinely needed, add a reference step + a semantic role there.\n'
	);
});

test('a file that shed raw colour lowers its budget in the same commit', () => {
	/** @type {string[]} */
	const stale = [];
	for (const [file, allowed] of Object.entries(budget.files)) {
		const count = actual.get(file)?.length ?? 0;
		if (count < allowed) {
			stale.push(
				count === 0
					? `${file}: now clean — delete its entry from raw-color-budget.json`
					: `${file}: down to ${count} (budget still says ${allowed}) — write ${count}`
			);
		}
	}
	assert.deepEqual(
		stale,
		[],
		'\n  ' +
			stale.join('\n  ') +
			'\n\n  The budget only ratchets down if you tighten it. Leaving slack lets ' +
			'the next change spend it.\n'
	);
});

test('the recorded total matches the per-file budget', () => {
	// The one number a reviewer reads in the diff. It has to be the real one.
	const sum = Object.values(budget.files).reduce((a, b) => a + b, 0);
	assert.equal(
		budget.total,
		sum,
		`raw-color-budget.json says total ${budget.total} but its files sum to ${sum}`
	);
});

test('zero raw colour outside tokens.css', () => {
	// The destination. Until the migration beads land this asserts against the
	// budget; the day the budget empties, it asserts against nothing at all —
	// and from then on any literal anywhere in frontend/src fails the build.
	const offenders = [...actual.keys()].filter((f) => budget.files[f] === undefined);
	assert.deepEqual(offenders, [], `raw colour in unbudgeted files: ${offenders.join(', ')}`);

	if (Object.keys(budget.files).length === 0) {
		assert.equal(actual.size, 0, 'the budget is empty, so frontend/src must be too');
	}
});

test('the budget names only files that exist', () => {
	const known = new Set(
		walk(srcRoot).map((f) => relative(srcRoot, f).split(sep).join('/'))
	);
	const ghosts = Object.keys(budget.files).filter((f) => !known.has(f));
	assert.deepEqual(
		ghosts,
		[],
		`raw-color-budget.json budgets files that no longer exist: ${ghosts.join(', ')}`
	);
});

// --- the layer split, strictly ---------------------------------------------

test('the reference layer is named nowhere outside tokens.css', () => {
	// Stronger than "no var(--pd-*)": a component may not *declare* a --pd-*
	// either, nor name one in a var() fallback or a color-mix(). One escape
	// hatch and the two layers collapse back into one.
	/** @type {string[]} */
	const offenders = [];
	for (const file of walk(srcRoot)) {
		if (file === tokensPath || !SCANNED.test(file)) continue;
		const text = decolour(readFileSync(file, 'utf8'));
		for (const [i, line] of text.split('\n').entries()) {
			if (line.includes('--pd-')) {
				offenders.push(`${relative(frontendRoot, file)}:${i + 1}: ${line.trim()}`);
			}
		}
	}
	assert.deepEqual(
		offenders,
		[],
		'reference tokens (--pd-*) are theme-owned. Components use a semantic ' +
			'--color-* role, so that a re-skin is a new reference block and not a refactor:\n  ' +
			offenders.join('\n  ')
	);
});
