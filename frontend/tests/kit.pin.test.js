/**
 * The kit is consumed at a pinned revision, and nothing else is the kit.
 *
 * `src/lib/kit/` is dumpster-kit's `crates/dumpster-kit-ui/assets` at the revision
 * `kit.lock` names (`scripts/sync-kit.sh` writes both together). The lock holds a sha256
 * per vendored file, so a hand edit to a kit file — or a file added there that the kit never
 * shipped — fails here rather than becoming a second implementation of a primitive.
 *
 * The other direction is asserted too: the primitives and the token file have ONE
 * implementation, and it is the kit's. `components/ui/` re-exports; it declares nothing.
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { existsSync, readFileSync, readdirSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, relative } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const frontendRoot = join(here, '..');
const kitRoot = join(frontendRoot, 'src/lib/kit');
const lockPath = join(frontendRoot, 'kit.lock');

/** @param {string} dir @returns {string[]} */
function walk(dir) {
	return readdirSync(dir).flatMap((e) => {
		const f = join(dir, e);
		return statSync(f).isDirectory() ? walk(f) : [f];
	});
}

test('kit.lock pins a full revision and names every vendored file', () => {
	assert.ok(existsSync(lockPath), 'frontend/kit.lock is missing — run scripts/sync-kit.sh');
	const lock = JSON.parse(readFileSync(lockPath, 'utf8'));
	assert.match(lock.rev, /^[0-9a-f]{40}$/, 'kit.lock must pin a full 40-hex revision, not a branch');
	assert.ok(Object.keys(lock.files).length > 0, 'kit.lock lists no files');
});

test('every vendored kit file is byte-identical to the pinned revision', () => {
	const lock = JSON.parse(readFileSync(lockPath, 'utf8'));
	const onDisk = walk(kitRoot).map((f) => relative(kitRoot, f)).sort();
	assert.deepEqual(onDisk, Object.keys(lock.files).sort(), 'src/lib/kit and kit.lock disagree on the file set');
	for (const [rel, sha] of Object.entries(lock.files)) {
		const got = createHash('sha256').update(readFileSync(join(kitRoot, rel))).digest('hex');
		assert.equal(got, sha, `${rel} differs from kit revision ${lock.rev.slice(0, 10)} — change the kit, not the copy`);
	}
});

test('components/ui declares no primitive of its own', () => {
	const ui = join(frontendRoot, 'src/lib/components/ui');
	assert.deepEqual(readdirSync(ui), ['index.ts'], 'a primitive in components/ui is a second implementation');
	const index = readFileSync(join(ui, 'index.ts'), 'utf8');
	assert.match(index, /\$lib\/kit\/components/, 'ui/index.ts must re-export the kit');
});

test('the theme overrides the reference layer and nothing else', () => {
	const theme = readFileSync(join(frontendRoot, 'src/lib/styles/tokens.css'), 'utf8')
		.replace(/\/\*[\s\S]*?\*\//g, '');
	assert.match(theme, /@import\s+['"][^'"]*kit\/tokens\.css['"]/, 'the theme must build on the kit tokens');
	const decls = [...theme.matchAll(/(--[a-z0-9-]+)\s*:/g)].map((m) => m[1]);
	const semantic = decls.filter((d) => !d.startsWith('--pd-'));
	assert.deepEqual(semantic, [], 'the theme redefines semantic roles — those belong to the kit');
});
