// ==UserScript==
// @name         PokeDumpster · pkmn.gg → CSV export
// @namespace    https://github.com/pokedumpster
// @version      0.1.0
// @description  On a logged-in pkmn.gg collection page, walk every card and download a PokeDumpster-native CSV ready for /ingest/csv.
// @match        https://pkmn.gg/*
// @match        https://*.pkmn.gg/*
// @grant        none
// @run-at       document-idle
// ==/UserScript==

/*
 * USAGE
 *   1. Install in Tampermonkey / Violentmonkey.
 *   2. Log in to pkmn.gg in the same browser profile and open your
 *      collection. The script adds a small floating "Export CSV"
 *      button to the bottom-right of the page.
 *   3. Click it. The script scans the rendered card list, scrolling
 *      where needed to materialise lazy rows, then downloads
 *      `pokedumpster-pkmngg-<timestamp>.csv`. Import it via the
 *      PokeDumpster /ingest/csv page with format = "PokeDumpster".
 *
 * STRATEGY
 *   pkmn.gg is a Next.js app with a private API that requires
 *   session auth, so this runs inside the logged-in page rather than
 *   from a CLI. It tries two strategies, in order:
 *     1. Intercept the JSON responses pkmn.gg's own fetch() calls
 *        return while you browse your collection. Buffered until you
 *        click Export, then flushed into the CSV.
 *     2. Fallback: walk the DOM of card tiles / table rows and pull
 *        the fields out of data attributes + visible text.
 *   The two-pass design keeps it working when pkmn.gg redesigns the
 *   page chrome but keeps the JSON API stable, and vice-versa.
 *
 * The CSV columns match the PokeDumpster-native parser in
 * crates/pkdump-core/src/import/pokedumpster.rs.
 */

(function () {
    'use strict';

    /* ─── Configuration ──────────────────────────────────────────────── */

    // pokemontcg.io expansion code → variant codes the
    // PokeDumpster catalog uses. Falls back to "normal".
    const VARIANT_MAP = {
        'normal': 'normal',
        'non-holo': 'normal',
        'nonholo': 'normal',
        'holo': 'holo',
        'holofoil': 'holo',
        'foil': 'holo',
        'reverse': 'reverse_holo',
        'reverse holo': 'reverse_holo',
        'reverse holofoil': 'reverse_holo',
        'reverseholofoil': 'reverse_holo',
        '1st edition': 'first_ed_normal',
        '1st edition holo': 'first_ed_holo',
        '1st edition holofoil': 'first_ed_holo',
        'unlimited holo': 'unlimited_holo',
        'unlimited holofoil': 'unlimited_holo',
        'pokeball': 'pokeball_rh',
        'pokeball holo': 'pokeball_rh',
        'masterball': 'masterball_rh',
        'masterball holo': 'masterball_rh',
        'cosmos': 'cosmos_holo',
        'cosmos holo': 'cosmos_holo'
    };

    function mapVariant(raw) {
        if (!raw) return 'normal';
        const k = String(raw).trim().toLowerCase();
        return VARIANT_MAP[k] || (k === '' ? 'normal' : k.replace(/\s+/g, '_'));
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

    /* ─── Captured-API buffer ────────────────────────────────────────── */
    //
    // Hook window.fetch so any pkmn.gg API response that looks like a
    // collection page lands in `captured`. We don't know the exact route
    // shapes, so we accept anything whose URL contains "collection" or
    // "card" and whose body looks like a JSON array (or has an obvious
    // `data`/`results`/`cards` array). Best-effort.

    const captured = [];
    const origFetch = window.fetch;
    window.fetch = async function (input, init) {
        const res = await origFetch.apply(this, arguments);
        try {
            const url = typeof input === 'string' ? input : (input && input.url) || '';
            if (/\/(api|trpc)\/.*(collection|card|inventory|owned)/i.test(url)) {
                const clone = res.clone();
                clone.json()
                    .then((data) => captured.push({ url, data }))
                    .catch(() => {});
            }
        } catch (_) {}
        return res;
    };

    /* ─── DOM scraping fallback ──────────────────────────────────────── */

    function autoscrollPage(stepPx = 800, delayMs = 250) {
        return new Promise((resolve) => {
            const start = window.scrollY;
            let lastHeight = document.body.scrollHeight;
            let stableTicks = 0;
            const t = setInterval(() => {
                window.scrollBy(0, stepPx);
                const h = document.body.scrollHeight;
                if (h === lastHeight) {
                    stableTicks += 1;
                    if (stableTicks >= 4 || window.scrollY + window.innerHeight >= h - 2) {
                        clearInterval(t);
                        window.scrollTo(0, start);
                        resolve();
                    }
                } else {
                    stableTicks = 0;
                    lastHeight = h;
                }
            }, delayMs);
        });
    }

    function scrapeDom() {
        // Card tiles + table rows tend to carry data-attributes naming
        // the set + number; we read a wide net of common shapes.
        const sels = [
            '[data-card-id]',
            '[data-set][data-number]',
            'a[href*="/cards/"]',
            '[class*="card"][data-set]'
        ];
        const seen = new Set();
        const rows = [];
        for (const sel of sels) {
            for (const el of document.querySelectorAll(sel)) {
                const row = extractFromEl(el);
                if (!row) continue;
                const key = `${row.set_code}|${row.ptcgo_code}|${row.number}|${row.variant}|${row.condition}`;
                if (seen.has(key)) continue;
                seen.add(key);
                rows.push(row);
            }
        }
        return rows;
    }

    function extractFromEl(el) {
        const d = el.dataset || {};
        let set_code = d.setCode || d.set || '';
        let ptcgo = d.ptcgo || d.ptcgoCode || '';
        let number = d.number || d.collectorNumber || d.cardNumber || '';
        let variant = d.variant || d.printing || d.foil || '';
        let qty = d.qty || d.quantity || d.count || '';
        let condition = d.condition || '';

        // Fallback: parse the href when present, /cards/<set>/<number>.
        if ((!set_code || !number) && el.tagName === 'A' && el.href) {
            const m = el.href.match(/\/cards\/([^\/?#]+)\/([^\/?#]+)/i);
            if (m) {
                if (!set_code) set_code = m[1];
                if (!number) number = m[2];
            }
        }
        // Last-ditch: read a visible qty badge.
        if (!qty) {
            const q = el.querySelector('[class*="qty"], [class*="quantity"], [class*="count"]');
            if (q) qty = (q.textContent || '').trim().replace(/[^\d]/g, '');
        }
        if (!set_code && !ptcgo) return null;
        if (!number) return null;

        const n = parseInt(qty, 10);
        return {
            set_code: set_code || '',
            ptcgo_code: ptcgo || '',
            number: String(number),
            variant: mapVariant(variant),
            condition: mapCondition(condition),
            language: 'English',
            quantity: Number.isFinite(n) && n > 0 ? n : 1,
            purchase_price: '',
            currency: '',
            source: 'pkmn.gg',
            notes: ''
        };
    }

    /* ─── Captured-API flatten ───────────────────────────────────────── */
    //
    // Walks every captured JSON blob, looks for objects that look like
    // collection rows (an id/set/number triple) and normalises them.

    function flattenCaptured() {
        const rows = [];
        const seen = new Set();
        const visit = (v) => {
            if (!v || typeof v !== 'object') return;
            if (Array.isArray(v)) {
                v.forEach(visit);
                return;
            }
            // Heuristic: a row has a set identifier + a card number, and
            // optionally a quantity / variant / condition.
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
                    notes: ''
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
        captured.forEach((c) => visit(c.data));
        return rows;
    }

    /* ─── CSV writer ─────────────────────────────────────────────────── */

    const COLUMNS = [
        'set_code', 'ptcgo_code', 'number', 'variant', 'condition',
        'language', 'quantity', 'purchase_price', 'currency', 'source', 'notes'
    ];

    function toCsv(rows) {
        const lines = [COLUMNS.join(',')];
        for (const r of rows) {
            lines.push(COLUMNS.map((c) => csvCell(r[c])).join(','));
        }
        return lines.join('\n') + '\n';
    }

    function csvCell(v) {
        if (v == null) return '';
        const s = String(v);
        if (/[,"\n]/.test(s)) return `"${s.replace(/"/g, '""')}"`;
        return s;
    }

    function downloadCsv(csv) {
        const ts = new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19);
        const blob = new Blob([csv], { type: 'text/csv;charset=utf-8' });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = `pokedumpster-pkmngg-${ts}.csv`;
        document.body.appendChild(a);
        a.click();
        setTimeout(() => {
            document.body.removeChild(a);
            URL.revokeObjectURL(url);
        }, 0);
    }

    /* ─── Floating button ────────────────────────────────────────────── */

    function ensureButton() {
        if (document.getElementById('pkdump-export-btn')) return;
        const btn = document.createElement('button');
        btn.id = 'pkdump-export-btn';
        btn.textContent = 'Export CSV';
        Object.assign(btn.style, {
            position: 'fixed',
            right: '16px',
            bottom: '16px',
            zIndex: 2147483647,
            padding: '10px 14px',
            background: '#e94560',
            color: '#fff',
            border: 'none',
            borderRadius: '8px',
            cursor: 'pointer',
            fontWeight: '600',
            fontSize: '14px',
            boxShadow: '0 4px 14px rgba(0,0,0,0.35)'
        });
        btn.addEventListener('click', onExport);
        document.body.appendChild(btn);
    }

    async function onExport() {
        const btn = document.getElementById('pkdump-export-btn');
        const orig = btn.textContent;
        btn.disabled = true;
        btn.textContent = 'Scrolling…';
        try {
            // Trigger lazy-load if the page is virtualised.
            await autoscrollPage();
            btn.textContent = 'Collecting…';

            let rows = flattenCaptured();
            if (rows.length === 0) rows = scrapeDom();

            if (rows.length === 0) {
                alert(
                    'PokeDumpster export: no cards found.\n\n' +
                    'Open your collection page first and let it load, ' +
                    'then click Export again. If the page uses an API the ' +
                    'script does not recognise, drop a paste of any captured ' +
                    'JSON URL into the PokeDumpster repo so we can teach it.'
                );
                btn.textContent = orig;
                btn.disabled = false;
                return;
            }

            const csv = toCsv(rows);
            console.log(
                `[PokeDumpster] Exporting ${rows.length} rows ` +
                `(${captured.length} captured API responses).`
            );
            downloadCsv(csv);
            btn.textContent = `Exported ${rows.length} ✓`;
        } catch (e) {
            console.error('[PokeDumpster] Export failed:', e);
            alert('Export failed — see the browser console.');
            btn.textContent = orig;
        } finally {
            btn.disabled = false;
            setTimeout(() => (btn.textContent = orig), 3000);
        }
    }

    if (document.readyState === 'complete' || document.readyState === 'interactive') {
        ensureButton();
    } else {
        window.addEventListener('DOMContentLoaded', ensureButton);
    }
    // Re-add the button across SPA navigations.
    setInterval(ensureButton, 2000);
})();
