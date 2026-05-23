#!/usr/bin/env node
//
// pkmngg_fetch.js — drive a headless browser against pkmn.gg, capture
// the authenticated collection JSON, write a PokeDumpster-native CSV.
//
// pkmn.gg login is passwordless: the site emails a magic link. First
// run: pass the link, the script visits it, persists the session state.
// Every subsequent run reuses the saved state (no link needed) until
// the session expires.
//
// Usage:
//   node scripts/pkmngg_fetch.js --link "https://pkmn.gg/.../?token=..."
//   node scripts/pkmngg_fetch.js                       # reuse saved state
//
// Options:
//   --link URL       Magic-link URL from the login email (single-use).
//   --storage PATH   Storage state file. Default: ~/.pkdump/pkmngg-state.json
//   --out FILE       Output CSV. Default: ./pokedumpster-pkmngg-<ts>.csv
//   --debug FILE     Log of every captured JSON response (URL + truncated
//                    body), for teaching this script new pkmn.gg API
//                    shapes. Default: ~/.pkdump/pkmngg-debug.log
//   --url URL        Where to land after login. Default: https://pkmn.gg/
//   --headed         Run a visible browser (for debugging the flow).
//   --help, -h       Print this help.

'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

// Reuse the Playwright install that already lives in tests/ui/ — saves
// ~18 MB of duplicated node_modules and shares the Chromium binary cache.
const PLAYWRIGHT_PATH = path.join(__dirname, '..', 'tests', 'ui', 'node_modules', 'playwright');
const { chromium } = require(PLAYWRIGHT_PATH);

// ── CLI ─────────────────────────────────────────────────────────────────

function parseArgs(argv) {
    const out = {
        link: null,
        storage: path.join(os.homedir(), '.pkdump', 'pkmngg-state.json'),
        out: path.join(
            process.cwd(),
            `pokedumpster-pkmngg-${new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19)}.csv`,
        ),
        debug: path.join(os.homedir(), '.pkdump', 'pkmngg-debug.log'),
        url: 'https://pkmn.gg/',
        headed: false,
    };
    const a = argv.slice(2);
    while (a.length) {
        const k = a.shift();
        switch (k) {
            case '--link':
                out.link = a.shift();
                break;
            case '--storage':
                out.storage = a.shift();
                break;
            case '--out':
                out.out = a.shift();
                break;
            case '--debug':
                out.debug = a.shift();
                break;
            case '--url':
                out.url = a.shift();
                break;
            case '--headed':
                out.headed = true;
                break;
            case '-h':
            case '--help':
                printHelp();
                process.exit(0);
                break;
            default:
                console.error('Unknown argument:', k);
                process.exit(2);
        }
    }
    return out;
}

function printHelp() {
    const lines = fs.readFileSync(__filename, 'utf8').split('\n');
    for (const l of lines.slice(1)) {
        if (!l.startsWith('//')) break;
        console.log(l.replace(/^\/\/ ?/, ''));
    }
}

// ── Session bootstrap ───────────────────────────────────────────────────

async function establishSession(link, storagePath, headed) {
    await fs.promises.mkdir(path.dirname(storagePath), { recursive: true });
    const browser = await chromium.launch({ headless: !headed });
    try {
        const ctx = await browser.newContext();
        const page = await ctx.newPage();
        console.error('Visiting magic link …');
        await page.goto(link, { waitUntil: 'networkidle', timeout: 60_000 });
        // The link target usually redirects through one or two intermediate
        // pages before landing on the authenticated home; give it a beat.
        await page.waitForLoadState('networkidle', { timeout: 30_000 }).catch(() => {});
        console.error('Post-login URL:', page.url());
        await ctx.storageState({ path: storagePath });
        await fs.promises.chmod(storagePath, 0o600).catch(() => {});
        console.error('Saved storage state →', storagePath);
    } finally {
        await browser.close();
    }
}

// ── Collection capture ─────────────────────────────────────────────────

async function captureCollection(args) {
    const haveState = await fs.promises
        .access(args.storage)
        .then(() => true)
        .catch(() => false);
    if (!haveState) {
        throw new Error(
            `No storage state at ${args.storage}. ` +
                'Run once with --link "<magic-link>" first.',
        );
    }
    await fs.promises.mkdir(path.dirname(args.debug), { recursive: true });
    const debugStream = fs.createWriteStream(args.debug, { flags: 'w' });

    const browser = await chromium.launch({ headless: !args.headed });
    try {
        const ctx = await browser.newContext({ storageState: args.storage });
        const page = await ctx.newPage();

        // Every JSON response gets dumped to the debug log + buffered for
        // later flattening. Errors here are swallowed: the body may not
        // be valid JSON, or the request may have been aborted.
        const captured = [];
        page.on('response', async (res) => {
            const url = res.url();
            const ct = (res.headers()['content-type'] || '').toLowerCase();
            if (!ct.includes('application/json')) return;
            try {
                const body = await res.json();
                captured.push({ url, body });
                debugStream.write(
                    `--- ${url}\n${JSON.stringify(body).slice(0, 1200)}\n`,
                );
            } catch (_) {}
        });

        console.error('Loading', args.url, '…');
        await page.goto(args.url, { waitUntil: 'networkidle', timeout: 60_000 });

        // Try a handful of likely collection routes. The first one whose
        // navigation doesn't throw wins — pkmn.gg's exact path may
        // change, and this gives the request handler a chance to fire on
        // the collection-specific API regardless.
        for (const slug of ['/collection', '/me/collection', '/dashboard', '/me']) {
            const dest = new URL(slug, args.url).toString();
            try {
                console.error('Trying', dest);
                await page.goto(dest, { waitUntil: 'networkidle', timeout: 30_000 });
                break;
            } catch (e) {
                console.error('  → failed:', e.message);
            }
        }

        // Auto-scroll: many card lists are virtualised and only fetch the
        // next page on demand. Scroll until the page height stops growing.
        await page.evaluate(async () => {
            await new Promise((resolve) => {
                const start = window.scrollY;
                let lastHeight = document.body.scrollHeight;
                let stable = 0;
                const id = setInterval(() => {
                    window.scrollBy(0, 800);
                    const h = document.body.scrollHeight;
                    if (h === lastHeight) {
                        stable += 1;
                        if (
                            stable >= 5 ||
                            window.scrollY + window.innerHeight >= h - 2
                        ) {
                            clearInterval(id);
                            window.scrollTo(0, start);
                            resolve();
                        }
                    } else {
                        stable = 0;
                        lastHeight = h;
                    }
                }, 350);
            });
        });
        await page.waitForLoadState('networkidle', { timeout: 20_000 }).catch(() => {});

        // Persist any session refresh that happened while browsing.
        await ctx.storageState({ path: args.storage });
        return captured;
    } finally {
        debugStream.end();
        await browser.close();
    }
}

// ── JSON → PokeDumpster rows ───────────────────────────────────────────

// pkmn.gg variant names → PokeDumpster's flat variant enum. Whatever we
// don't recognise passes through verbatim (snake-cased), so a later
// manual override or a code change can pick it up.
const VARIANT_MAP = {
    normal: 'normal',
    'non-holo': 'normal',
    nonholo: 'normal',
    holo: 'holo',
    holofoil: 'holo',
    foil: 'holo',
    reverse: 'reverse_holo',
    'reverse holo': 'reverse_holo',
    'reverse holofoil': 'reverse_holo',
    reverseholofoil: 'reverse_holo',
    '1st edition': 'first_ed_normal',
    '1st edition holo': 'first_ed_holo',
    '1st edition holofoil': 'first_ed_holo',
    'unlimited holo': 'unlimited_holo',
    'unlimited holofoil': 'unlimited_holo',
    pokeball: 'pokeball_rh',
    'pokeball holo': 'pokeball_rh',
    masterball: 'masterball_rh',
    'masterball holo': 'masterball_rh',
    cosmos: 'cosmos_holo',
    'cosmos holo': 'cosmos_holo',
};

function mapVariant(raw) {
    if (raw == null) return 'normal';
    const k = String(raw).trim().toLowerCase();
    if (k === '') return 'normal';
    return VARIANT_MAP[k] || k.replace(/\s+/g, '_');
}

function mapCondition(raw) {
    if (!raw) return 'Near Mint';
    const k = String(raw).trim().toLowerCase().replace(/[-_]/g, ' ');
    if (k.includes('damaged') || k === 'poor' || k === 'd') return 'Damaged';
    if (k.includes('heavily') || k === 'hp') return 'Heavily Played';
    if (k.includes('moderately') || k === 'mp' || k === 'played') return 'Moderately Played';
    if (k.includes('lightly') || k === 'lp' || k === 'good' || k === 'excellent') return 'Lightly Played';
    return 'Near Mint';
}

// Walk every captured response, find objects that look like collection
// rows (a set identifier + a card number), normalise into the
// PokeDumpster CSV shape.
function flattenToRows(captured) {
    const rows = [];
    const seen = new Set();

    const visit = (v) => {
        if (!v || typeof v !== 'object') return;
        if (Array.isArray(v)) {
            v.forEach(visit);
            return;
        }
        const setCode = v.setCode || v.set_code || (v.set && (v.set.id || v.set.code));
        const ptcgo = v.ptcgoCode || v.ptcgo_code || (v.set && v.set.ptcgoCode);
        const number = v.number || v.collectorNumber || v.collector_number;
        if ((setCode || ptcgo) && number) {
            const variant = v.variant || v.printing || v.foil || (v.finish && v.finish.name);
            const qty = parseInt(v.quantity ?? v.qty ?? v.count ?? 1, 10);
            const cond = v.condition || (v.copy && v.copy.condition);
            const price = v.purchasePrice ?? v.purchase_price ?? '';
            const row = {
                set_code: setCode || '',
                ptcgo_code: ptcgo || '',
                number: String(number),
                variant: mapVariant(variant),
                condition: mapCondition(cond),
                language: 'English',
                quantity: Number.isFinite(qty) && qty > 0 ? qty : 1,
                purchase_price: price === '' || price == null ? '' : String(price),
                currency: v.currency || '',
                source: 'pkmn.gg',
                notes: '',
            };
            const key = `${row.set_code}|${row.ptcgo_code}|${row.number}|${row.variant}|${row.condition}`;
            if (!seen.has(key)) {
                seen.add(key);
                rows.push(row);
            }
        }
        for (const k in v) {
            if (Object.prototype.hasOwnProperty.call(v, k)) visit(v[k]);
        }
    };

    captured.forEach((c) => visit(c.body));
    return rows;
}

const COLUMNS = [
    'set_code',
    'ptcgo_code',
    'number',
    'variant',
    'condition',
    'language',
    'quantity',
    'purchase_price',
    'currency',
    'source',
    'notes',
];

function toCsv(rows) {
    const cell = (v) => {
        if (v == null) return '';
        const s = String(v);
        return /[,"\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
    };
    return (
        [COLUMNS.join(','), ...rows.map((r) => COLUMNS.map((c) => cell(r[c])).join(','))].join('\n') +
        '\n'
    );
}

// ── Entry point ─────────────────────────────────────────────────────────

(async () => {
    const args = parseArgs(process.argv);
    if (args.link) {
        await establishSession(args.link, args.storage, args.headed);
    }
    const captured = await captureCollection(args);
    const rows = flattenToRows(captured);

    if (rows.length === 0) {
        console.error(
            `\nNo rows extracted from the ${captured.length} captured JSON ` +
                `responses.\nSee ${args.debug} for what pkmn.gg actually served — ` +
                'paste a relevant entry back so we can teach the script.\n',
        );
        process.exit(1);
    }

    await fs.promises.writeFile(args.out, toCsv(rows));
    console.error(`\nWrote ${rows.length} rows → ${args.out}`);
    console.error(`Captured ${captured.length} JSON responses. Debug log → ${args.debug}\n`);
})().catch((e) => {
    console.error(e);
    process.exit(1);
});
