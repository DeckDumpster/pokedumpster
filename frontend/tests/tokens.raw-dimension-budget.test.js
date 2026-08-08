/**
 * Colour is not the whole token layer.
 *
 * `tokens.css` declares four more ramps — space (13 steps), type (7), radius
 * (8), elevation (5) — and until this file existed nothing made a component
 * use them. A declared ramp nobody is obliged to use is documentation, not a
 * system: a spacing change stays a find-and-replace across 400 sites, which is
 * the exact failure Intent #1 of the aesthetic-overhaul epic names.
 *
 * So: the same ratchet the colour gate runs, pointed at dimensions.
 * `src/lib/styles/raw-dimension-budget.json` records what each file still
 * holds and the count may only go DOWN. Exceed a budget and it reads as a
 * regression; drop below it and this fails too, printing the line to paste.
 * When `files` empties, every dimension in the app comes from a token and the
 * test asserts that literally.
 *
 * This is deliberately NOT a migration. It is seeded at the counts of the day
 * it landed; later work brings them down as a side effect of touching routes.
 *
 * The unit is the DECLARATION, not the literal — `padding: 0.4rem 0.6rem` is
 * one raw decision, the same way the bead that commissioned this counted them.
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
const budgetPath = join(srcRoot, 'lib/styles/raw-dimension-budget.json');

/** @type {{ total: number, files: Record<string, number> }} */
const budget = JSON.parse(readFileSync(budgetPath, 'utf8'));

// Same scope as the colour ratchet: tokens.css declares the ramps, everything
// else consumes them. A dimension chosen in a .ts map is still a visual
// decision outside the token layer, so .ts/.js are scanned too.
const SCANNED = /\.(svelte|ts|js|css|svg|html)$/;

// The properties each ramp exists to serve. Longhands and the logical-property
// spellings are included so `padding-inline-start: 6px` is not a way out.
const SPACE =
	String.raw`(?:padding|margin)(?:-(?:top|right|bottom|left|inline|block)(?:-(?:start|end))?)?` +
	String.raw`|(?:row-|column-)?gap`;
const TYPE = String.raw`font-size`;
const RADIUS =
	String.raw`border-radius|border-(?:top|bottom)-(?:left|right)-radius` +
	String.raw`|border-(?:start|end)-(?:start|end)-radius`;
const ELEVATION = String.raw`box-shadow`;

/**
 * A declaration of a ramp-backed property, with its value. The lookbehind
 * keeps `--card-padding: 6px` — a local custom property, which is a component
 * naming a value, not spending one — out of the count; it is caught where it
 * is *used* instead. The value stops at the newline because nothing in
 * `frontend/src` wraps one, and the alternative swallows whole TS objects.
 */
const DECL_RE = new RegExp(
	String.raw`(?<![\w-])(${SPACE}|${TYPE}|${RADIUS}|${ELEVATION})\s*:\s*([^;{}\n]*)`,
	'gi'
);

/**
 * A length as written. Unitless `0` is not here on purpose: zero carries no
 * design decision, and `--space-0` exists for symmetry rather than for use.
 * A bare multiplier (`calc(var(--space-4) * 2)`) is not a length either.
 */
const LENGTH_RE =
	/(?<![\w.#-])-?(?:\d+\.?\d*|\.\d+)(?:px|rem|em|%|vh|vw|vmin|vmax|ch|ex|pt|cm|mm|in|pc|q)(?![\w-])/gi;

/**
 * Strip comments so a commented-out rule and prose about pixels don't count.
 * @param {string} text
 */
function decomment(text) {
	return text
		.replace(/\/\*[\s\S]*?\*\//g, '')
		.replace(/<!--[\s\S]*?-->/g, '')
		.split('\n')
		.filter((line) => !/^\s*(\/\/|\*)/.test(line))
		.join('\n');
}

/**
 * Blank out every plain `var(--token)` reference, so what remains is only what
 * the file spelled out itself. Deliberately does not match a var() carrying a
 * fallback: `var(--space-2, 8px)` keeps its `8px` and counts, because a
 * fallback is a second value and it was chosen raw.
 * @param {string} value
 */
function devar(value) {
	return value.replace(/var\(\s*--[\w-]+\s*\)/g, ' ');
}

/**
 * Every ramp-backed declaration in a file that spends a raw length.
 * @param {string} text
 * @returns {string[]} the declarations, as written
 */
function rawDimensions(text) {
	const body = decomment(text);
	/** @type {string[]} */
	const out = [];
	for (const [, property, value] of body.matchAll(DECL_RE)) {
		if (LENGTH_RE.test(devar(value))) out.push(`${property}: ${value.trim()}`);
		LENGTH_RE.lastIndex = 0; // `g` regexes are stateful across .test()
	}
	return out;
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
			rawDimensions(readFileSync(f, 'utf8'))
		]))
		.filter(([, decls]) => decls.length > 0)
);

/**
 * A paste-ready budget line, so seeding and ratcheting are copy, not arithmetic.
 * @param {string} file
 * @param {number} count
 */
const entry = (file, count) => `\t\t"${file}": ${count},`;

// --- the ratchet -----------------------------------------------------------

test('no file spends more raw dimensions than its budget', () => {
	/** @type {string[]} */
	const regressions = [];
	for (const [file, decls] of actual) {
		const allowed = budget.files[file];
		if (allowed === undefined) {
			regressions.push(
				`${file}: ${decls.length} raw dimension(s), but this file has no budget — ` +
					`it is meant to be token-only. Offending: ${[...new Set(decls)].join('; ')}`
			);
		} else if (decls.length > allowed) {
			regressions.push(
				`${file}: ${decls.length} raw dimension(s), budget is ${allowed}. ` +
					`Use --space-*/--text-*/--radius-*/--shadow-*; do not raise the budget.\n` +
					`      ${[...new Set(decls)].join('\n      ')}`
			);
		}
	}
	assert.deepEqual(
		regressions,
		[],
		'\n  ' +
			regressions.join('\n  ') +
			'\n\n  tokens.css declares the space, type, radius and elevation ramps so a ' +
			'change to one is a change in one place. A value that fits no step needs a ' +
			'step added there, not a literal here.\n'
	);
});

test('a file that shed raw dimensions lowers its budget in the same commit', () => {
	/** @type {string[]} */
	const stale = [];
	for (const [file, allowed] of Object.entries(budget.files)) {
		const count = actual.get(file)?.length ?? 0;
		if (count < allowed) {
			stale.push(
				count === 0
					? `${file}: now clean — delete its entry from raw-dimension-budget.json`
					: `${file}: down to ${count} (budget still says ${allowed}) — write:\n${entry(file, count)}`
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
		`raw-dimension-budget.json says total ${budget.total} but its files sum to ${sum}`
	);
});

test('zero raw dimensions outside tokens.css', () => {
	// The destination. Until the budget empties this asserts against it; the day
	// it does, it asserts against nothing at all — and from then on any raw
	// length in a ramp-backed property anywhere in frontend/src fails the build.
	const offenders = [...actual.keys()]
		.filter((f) => budget.files[f] === undefined)
		.map((f) => entry(f, /** @type {string[]} */ (actual.get(f)).length));
	assert.deepEqual(
		offenders,
		[],
		`files spending raw dimensions with no budget — paste into raw-dimension-budget.json:\n${offenders.join('\n')}`
	);

	if (Object.keys(budget.files).length === 0) {
		assert.equal(actual.size, 0, 'the budget is empty, so frontend/src must be too');
	}
});

test('the budget names only files that exist', () => {
	const known = new Set(walk(srcRoot).map((f) => relative(srcRoot, f).split(sep).join('/')));
	const ghosts = Object.keys(budget.files).filter((f) => !known.has(f));
	assert.deepEqual(
		ghosts,
		[],
		`raw-dimension-budget.json budgets files that no longer exist: ${ghosts.join(', ')}`
	);
});

// --- the scanner itself ----------------------------------------------------

test('the scanner reads a value the way CSS does', () => {
	// A ratchet nobody can trust gets suppressed, so the detector states its own
	// rules here rather than in prose. Each pair is a claim about what counts.
	const raw = [
		'.a { padding: 6px; }',
		'.a { padding-inline-start: 0.5rem; }',
		'.a { gap: 4px }',
		'.a { font-size: 0.85rem; }',
		'.a { border-radius: 50%; }',
		'.a { border-top-left-radius: 14px; }',
		'.a { box-shadow: 0 1px 2px #000; }',
		'.a { margin: 0 0 1rem; }',
		'.a { padding: var(--space-2, 8px); }',
		'<div style="padding: 4px"></div>'
	];
	const clean = [
		'.a { padding: 0; }',
		'.a { margin: 0 auto; }',
		'.a { gap: var(--space-2); }',
		'.a { padding: var(--space-1) var(--space-3); }',
		'.a { font-size: inherit; }',
		'.a { border-radius: var(--radius-pill); }',
		'.a { box-shadow: var(--shadow-md); }',
		'.a { padding: calc(var(--space-4) * 2); }',
		'.a { --card-padding: 6px; }', // naming a value is not spending one
		'.a { width: 240px; height: 12px; top: 8px; }', // no ramp behind these
		'@media (min-width: 900px) { .a { padding: var(--space-4); } }',
		'/* padding: 6px */',
		'// font-size: 12px'
	];
	assert.deepEqual(
		raw.filter((s) => rawDimensions(s).length === 0),
		[],
		'these spend a raw dimension and the scanner missed them'
	);
	assert.deepEqual(
		clean.filter((s) => rawDimensions(s).length > 0),
		[],
		'these are token-clean and the scanner flagged them anyway'
	);
});
