/**
 * Teach Node's module loader to import `.svelte` files.
 *
 * The primitives in `src/lib/components/ui/` need render tests, and the test
 * runner is Node's built-in one — no jsdom, no testing-library, no extra
 * dependency (the same rule `tokens.contrast.test.js` set). So instead of a
 * DOM we compile each component for the server and render it to a string:
 * markup and the scoped stylesheet are both assertable that way, which is
 * exactly what "renders, respects variants, emits no hardcoded colour" needs.
 *
 * Loaded via `node --import ./tests/support/svelte-hooks.js` (see package.json).
 * Compilation is `generate: 'server'` + `css: 'injected'`, so the component's
 * own CSS travels with it into the rendered `head` rather than being dropped.
 *
 * `<script lang="ts">` needs no preprocessor: Svelte 5 strips type annotations
 * itself.
 */

import { registerHooks } from 'node:module';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { compile } from 'svelte/compiler';

const isSvelte = (/** @type {string} */ specifier) => specifier.endsWith('.svelte');

registerHooks({
	resolve(specifier, context, nextResolve) {
		if (isSvelte(specifier) && context.parentURL) {
			return {
				url: new URL(specifier, context.parentURL).href,
				format: 'module',
				shortCircuit: true
			};
		}
		return nextResolve(specifier, context);
	},

	load(url, context, nextLoad) {
		if (!isSvelte(url)) return nextLoad(url, context);
		const filename = fileURLToPath(url);
		const { js } = compile(readFileSync(filename, 'utf8'), {
			filename,
			generate: 'server',
			css: 'injected'
		});
		return { format: 'module', source: js.code, shortCircuit: true };
	}
});
