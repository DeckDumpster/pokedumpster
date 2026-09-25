// Load the userscript inside a minimal browser-ish sandbox and expose its
// internals for testing. No network, no DOM rendering.
const fs = require('fs');
const path = require('path');

const SRC = process.env.PKDUMP_SCRIPT ||
    path.join(__dirname, '..', 'pkmngg_export.user.js');

let code = fs.readFileSync(SRC, 'utf8');

// Splice an export of the closure internals in just before the IIFE closes.
const EXPORTS = `
    globalThis.__pkdump = {
        record, captures, extract, collectFrom, reconcile, toCsv,
        mapVariant, mapLanguage, identityOf, buildRow, extractJsonBlobs,
        walkPagination, productiveCaptures, captureBundle,
        identityFromCodes, discoverUsername, toMassEntry, toLiveList,
        directCollectionWalk, COLLECTION_ENDPOINT,
        get droppedDuplicates() { return droppedDuplicates; },
    };
`;
const tail = '})();';
const at = code.lastIndexOf(tail);
if (at < 0) throw new Error('could not find IIFE tail');
code = code.slice(0, at) + EXPORTS + code.slice(at);

// --- Browser stubs ------------------------------------------------------
const noopEl = () => ({
    style: {}, dataset: {}, textContent: '', append() {}, appendChild() {},
    addEventListener() {}, remove() {}, click() {}, querySelector: () => null,
    querySelectorAll: () => [],
});
globalThis.window = globalThis;
globalThis.document = {
    readyState: 'complete',
    body: noopEl(),
    createElement: noopEl,
    getElementById: () => null,
    addEventListener() {},
    querySelectorAll: () => [],
};
globalThis.location = { href: 'https://pkmn.gg/collections', hostname: 'pkmn.gg' };
globalThis.Blob = class { constructor(parts) { this.parts = parts; } };
globalThis.setInterval = () => 0;          // don't keep the process alive
globalThis.alert = () => {};
if (!globalThis.fetch) globalThis.fetch = async () => { throw new Error('no network in tests'); };

// Evaluate.
new Function(code)();
module.exports = globalThis.__pkdump;
