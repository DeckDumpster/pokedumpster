/**
 * Contrast is a test, not a review note.
 *
 * Parses `src/lib/styles/tokens.css`, resolves every token through the
 * semantic -> reference layers, and asserts that each pairing declared in
 * `src/lib/styles/contrast-pairs.json` meets WCAG AA. Also guards the
 * two-layer split itself: semantic tokens must be pure aliases of reference
 * tokens, and nothing outside tokens.css may reach for `--pd-*`.
 *
 * Runs under Node's built-in runner — no test dependency:
 *   npm test        (frontend/)
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, relative } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const frontendRoot = join(here, '..');
const srcRoot = join(frontendRoot, 'src');
const tokensPath = join(srcRoot, 'lib/styles/tokens.css');
const pairsPath = join(srcRoot, 'lib/styles/contrast-pairs.json');

const tokensCss = readFileSync(tokensPath, 'utf8');
/** @type {{ exempt: Record<string, string>, pairs: Array<Record<string, string>> }} */
const manifest = JSON.parse(readFileSync(pairsPath, 'utf8'));

/** @type {Record<string, number>} */
const THRESHOLD = { normal: 4.5, large: 3.0, ui: 3.0 };

// --- parsing ---------------------------------------------------------------

/**
 * Every custom-property declaration in the file, last-write-wins.
 * @param {string} css
 * @returns {Map<string, string>}
 */
function parseTokens(css) {
	const stripped = css.replace(/\/\*[\s\S]*?\*\//g, '');
	/** @type {Map<string, string>} */
	const out = new Map();
	for (const m of stripped.matchAll(/(--[a-z0-9-]+)\s*:\s*([^;]+);/g)) {
		out.set(m[1], m[2].trim());
	}
	return out;
}

const tokens = parseTokens(tokensCss);

/**
 * Resolve a token to a literal value, following `var()` aliases.
 * @param {string} name
 * @param {string[]} [seen]
 * @returns {string}
 */
function resolve(name, seen = []) {
	assert.ok(!seen.includes(name), `cyclic token alias: ${[...seen, name].join(' -> ')}`);
	const value = tokens.get(name);
	assert.ok(value !== undefined, `token ${name} is not declared in tokens.css`);
	const alias = /^var\(\s*(--[a-z0-9-]+)\s*\)$/.exec(value);
	return alias ? resolve(alias[1], [...seen, name]) : value;
}

// --- colour maths ----------------------------------------------------------

/** @typedef {{ r: number, g: number, b: number, a: number }} Rgba */

/**
 * @param {string} value
 * @returns {Rgba}
 */
function parseColor(value) {
	const hex = /^#([0-9a-f]{3,8})$/i.exec(value.trim());
	if (hex) {
		let h = hex[1];
		if (h.length === 3 || h.length === 4) h = [...h].map((c) => c + c).join('');
		const n = (/** @type {number} */ i) => parseInt(h.slice(i, i + 2), 16);
		return { r: n(0), g: n(2), b: n(4), a: h.length === 8 ? n(6) / 255 : 1 };
	}
	const fn = /^rgba?\(([^)]+)\)$/i.exec(value.trim());
	assert.ok(fn, `cannot parse colour: ${value}`);
	const parts = fn[1].split(/[,\s/]+/).filter(Boolean).map(Number);
	assert.ok(parts.length >= 3 && parts.every((p) => !Number.isNaN(p)), `cannot parse colour: ${value}`);
	return { r: parts[0], g: parts[1], b: parts[2], a: parts.length > 3 ? parts[3] : 1 };
}

/**
 * Composite `fg` (which may be translucent) over an opaque `bg`.
 * @param {Rgba} fg
 * @param {Rgba} bg
 * @returns {Rgba}
 */
function flatten(fg, bg) {
	if (fg.a >= 1) return fg;
	assert.equal(bg.a, 1, 'a compositing base must be opaque');
	return {
		r: fg.r * fg.a + bg.r * (1 - fg.a),
		g: fg.g * fg.a + bg.g * (1 - fg.a),
		b: fg.b * fg.a + bg.b * (1 - fg.a),
		a: 1
	};
}

/**
 * WCAG 2.1 relative luminance.
 * @param {Rgba} color
 */
function luminance({ r, g, b }) {
	const [rl, gl, bl] = [r, g, b].map((v) => {
		const c = v / 255;
		return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
	});
	return 0.2126 * rl + 0.7152 * gl + 0.0722 * bl;
}

/**
 * WCAG 2.1 contrast ratio.
 * @param {Rgba} a
 * @param {Rgba} b
 */
function contrast(a, b) {
	const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
	return (hi + 0.05) / (lo + 0.05);
}

// --- the contract ----------------------------------------------------------

test('every declared pairing meets WCAG AA', () => {
	/** @type {string[]} */
	const failures = [];
	for (const pair of manifest.pairs) {
		const threshold = THRESHOLD[pair.size];
		assert.ok(threshold, `unknown size class "${pair.size}" on ${pair.fg}/${pair.bg}`);

		const base = pair.base ? parseColor(resolve(pair.base)) : null;
		let bg = parseColor(resolve(pair.bg));
		if (bg.a < 1) {
			assert.ok(base, `${pair.bg} is translucent — the pairing needs a "base" surface`);
			bg = flatten(bg, base);
		}
		const fg = flatten(parseColor(resolve(pair.fg)), bg);

		const ratio = contrast(fg, bg);
		// PD_CONTRAST_REPORT=1 npm test — prints the whole table, so reviewing
		// the palette does not mean recomputing it.
		if (process.env.PD_CONTRAST_REPORT) {
			console.log(
				`${ratio.toFixed(2).padStart(6)}:1  (needs ${threshold.toFixed(1)})  ` +
					`${pair.fg} on ${pair.bg}${pair.base ? ` over ${pair.base}` : ''}`
			);
		}
		if (ratio + 1e-9 < threshold) {
			failures.push(
				`${pair.fg} on ${pair.bg}${pair.base ? ` (over ${pair.base})` : ''}: ` +
					`${ratio.toFixed(2)}:1 — needs ${threshold.toFixed(1)}:1 (${pair.size})`
			);
		}
	}
	assert.deepEqual(failures, [], `\n  ${failures.join('\n  ')}\n`);
});

test('the three pre-token AA failures are actually fixed', () => {
	// The regression this bead exists to prevent. These exact pairings shipped
	// below AA before tokens.css; if a future ramp reintroduces that, say so
	// in the failure message rather than making the reviewer diff hex values.
	const cases = /** @type {Array<[string, string, number]>} */ ([
		['--color-text-subtle', '--color-surface-panel', 4.48],
		['--color-text-accent', '--color-surface-page', 4.46],
		['--color-text-accent', '--color-surface-panel', 4.15]
	]);
	for (const [fg, bg, was] of cases) {
		const ratio = contrast(parseColor(resolve(fg)), parseColor(resolve(bg)));
		assert.ok(
			ratio >= 4.5,
			`${fg} on ${bg} is ${ratio.toFixed(2)}:1 — the pre-token palette shipped ${was}:1 here and it must not regress`
		);
	}
});

test('semantic colour tokens are pure aliases of the reference layer', () => {
	/** @type {string[]} */
	const offenders = [];
	for (const [name, value] of tokens) {
		if (!name.startsWith('--color-') || name.startsWith('--pd-')) continue;
		if (!/^var\(\s*--pd-[a-z0-9-]+\s*\)$/.test(value)) {
			offenders.push(`${name}: ${value}`);
		}
	}
	assert.deepEqual(
		offenders,
		[],
		'semantic --color-* tokens must be var(--pd-*) aliases — a literal here collapses the two layers:\n  ' +
			offenders.join('\n  ')
	);
});

test('every reference colour token is reachable from the semantic layer', () => {
	const referenced = new Set();
	for (const [name, value] of tokens) {
		if (name.startsWith('--pd-')) continue;
		for (const m of value.matchAll(/var\(\s*(--pd-[a-z0-9-]+)\s*\)/g)) referenced.add(m[1]);
	}
	const orphans = [...tokens.keys()].filter((n) => n.startsWith('--pd-') && !referenced.has(n));
	assert.deepEqual(orphans, [], `reference tokens no semantic role points at: ${orphans.join(', ')}`);
});

test('every reference ramp is monotonic in luminance', () => {
	/** @type {Map<string, Array<[number, string]>>} */
	const families = new Map();
	for (const [name, value] of tokens) {
		const m = /^--pd-([a-z]+)-(\d+)$/.exec(name);
		if (!m) continue;
		const color = parseColor(value);
		if (color.a < 1) continue;
		const steps = families.get(m[1]) ?? [];
		steps.push([Number(m[2]), name]);
		families.set(m[1], steps);
	}
	assert.ok(families.size >= 6, 'expected the reference layer to declare several ramps');
	for (const [family, steps] of families) {
		steps.sort((a, b) => a[0] - b[0]);
		for (let i = 1; i < steps.length; i++) {
			const lighter = luminance(parseColor(resolve(steps[i - 1][1])));
			const darker = luminance(parseColor(resolve(steps[i][1])));
			assert.ok(
				lighter > darker,
				`--pd-${family}: ${steps[i - 1][1]} must be lighter than ${steps[i][1]} ` +
					`(${lighter.toFixed(4)} vs ${darker.toFixed(4)}) — a ramp that isn't monotonic makes every "one step darker" call site a guess`
			);
		}
	}
});

test('every colour role is either exercised by a pairing or explicitly exempt', () => {
	const used = new Set();
	for (const pair of manifest.pairs) {
		used.add(pair.fg);
		used.add(pair.bg);
		if (pair.base) used.add(pair.base);
	}
	// `$`-prefixed keys are prose for the reader, not token names.
	const exempt = new Set(Object.keys(manifest.exempt).filter((k) => k.startsWith('--')));
	const uncovered = [...tokens.keys()].filter(
		(n) => n.startsWith('--color-') && !used.has(n) && !exempt.has(n)
	);
	assert.deepEqual(
		uncovered,
		[],
		'these colour roles are neither contrast-tested nor listed in "exempt" — ' +
			'add a pairing or say why it needs none:\n  ' +
			uncovered.join('\n  ')
	);

	const stale = [...exempt].filter((n) => !tokens.has(n));
	assert.deepEqual(stale, [], `"exempt" lists tokens that no longer exist: ${stale.join(', ')}`);
	const both = [...exempt].filter((n) => used.has(n));
	assert.deepEqual(both, [], `tokens are both exempt and paired: ${both.join(', ')}`);
});

test('every token a pairing names exists, and no pairing is declared twice', () => {
	/** @type {Set<string>} */
	const seen = new Set();
	for (const pair of manifest.pairs) {
		for (const key of ['fg', 'bg', 'base']) {
			if (pair[key]) assert.ok(tokens.has(pair[key]), `pairing names unknown token ${pair[key]}`);
		}
		const key = `${pair.fg} on ${pair.bg}${pair.base ? ` over ${pair.base}` : ''}`;
		assert.ok(!seen.has(key), `duplicate pairing: ${key}`);
		seen.add(key);
	}
});

// --- the split, enforced across the app ------------------------------------

/**
 * @param {string} dir
 * @returns {string[]} every file under frontend/src
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

const srcFiles = walk(srcRoot);

test('tokens.css is imported exactly once', () => {
	const importers = srcFiles.filter(
		(f) =>
			f !== tokensPath &&
			/\.(svelte|ts|js|css)$/.test(f) &&
			/(^|\s)(import\s+['"][^'"]*tokens\.css['"]|@import\s+[^;]*tokens\.css)/m.test(
				readFileSync(f, 'utf8')
			)
	);
	assert.deepEqual(
		importers.map((f) => relative(frontendRoot, f)),
		['src/routes/+layout.svelte'],
		'tokens.css must be imported once, from the root layout'
	);
});

test('every raw colour still in frontend/src maps to a semantic role', () => {
	/** @type {{ map: Record<string, { tokens: string[], note?: string }> }} */
	const legacy = JSON.parse(readFileSync(join(srcRoot, 'lib/styles/legacy-color-map.json'), 'utf8'));

	/** Strip comments so prose like "Porygon #153" isn't read as a colour. */
	const decolour = (/** @type {string} */ text) =>
		text
			.replace(/\/\*[\s\S]*?\*\//g, '')
			.replace(/<!--[\s\S]*?-->/g, '')
			.split('\n')
			.filter((/** @type {string} */ line) => !/^\s*(\/\/|\*)/.test(line))
			.join('\n');

	const found = new Set();
	for (const file of srcFiles) {
		if (file === tokensPath || !/\.(svelte|ts|js|css)$/.test(file)) continue;
		const text = decolour(readFileSync(file, 'utf8'));
		for (const m of text.matchAll(/(?<![\w&])(#[0-9a-fA-F]{3,8})(?![\w-])/g)) {
			found.add(m[1].toLowerCase());
		}
		for (const m of text.matchAll(/rgba?\([0-9\s,.]+\)/g)) found.add(m[0]);
	}

	const unmapped = [...found].filter((c) => !(c in legacy.map)).sort();
	assert.deepEqual(
		unmapped,
		[],
		'these raw colours have no semantic role — either add the role to tokens.css ' +
			'and map it, or map it to an existing one:\n  ' +
			unmapped.join('\n  ')
	);

	const dangling = [];
	for (const [literal, entry] of Object.entries(legacy.map)) {
		assert.ok(entry.tokens?.length, `${literal} maps to no token`);
		for (const name of entry.tokens) if (!tokens.has(name)) dangling.push(`${literal} -> ${name}`);
	}
	assert.deepEqual(dangling, [], `the map names tokens that do not exist:\n  ${dangling.join('\n  ')}`);
});

test('components reference only the semantic layer', () => {
	const offenders = srcFiles
		.filter(
			(f) =>
				f !== tokensPath &&
				/\.(svelte|ts|js|css)$/.test(f) &&
				/var\(\s*--pd-/.test(readFileSync(f, 'utf8'))
		)
		.map((f) => relative(frontendRoot, f));
	assert.deepEqual(
		offenders,
		[],
		'reference tokens (--pd-*) are theme-owned; components must use a semantic --color-* role:\n  ' +
			offenders.join('\n  ')
	);
});
