// ==UserScript==
// @name         PokeDumpster · pkmn.gg → CSV export
// @namespace    https://github.com/pokedumpster
// @version      0.2.0
// @description  Export a pkmn.gg collection to a PokeDumpster-native CSV (and a lossless JSON capture) from inside a logged-in tab.
// @match        https://pkmn.gg/*
// @match        https://*.pkmn.gg/*
// @grant        none
// @run-at       document-start
// ==/UserScript==

/*
 * USAGE
 *   1. Install in Tampermonkey / Violentmonkey.
 *   2. Log in to pkmn.gg and open your collection. A small panel appears
 *      bottom-right showing how many API responses have been captured.
 *   3. Click "Export". The script walks the collection API to the end
 *      (it does NOT rely on scrolling) and downloads:
 *        - pokedumpster-pkmngg-<ts>.csv   → /ingest/csv, format "PokeDumpster"
 *        - pokedumpster-pkmngg-<ts>.json  → lossless capture, yours to keep
 *   4. If it finds nothing, click "Dump capture" and send that file back —
 *      it contains everything needed to teach the extractor pkmn.gg's shape.
 *
 * WHY v0.1 FOUND NOTHING (pokedumpster-p6y.3)
 *   v0.1 only buffered responses whose URL matched /\/(api|trpc)\/.../ .
 *   pkmn.gg's API is `https://api.tcg.gg/pkmn/v1/...` — no `/api/` or
 *   `/trpc/` path segment anywhere — so the filter never matched, `captured`
 *   was always empty, and it silently fell through to a DOM scrape whose
 *   selectors were guesses. v0.2 captures by *response shape*, not URL, and
 *   drops the DOM scrape entirely (the card grid is virtualised, so scraping
 *   it can only ever see the rows currently on screen).
 *
 * STRATEGY
 *   1. CAPTURE (from document-start, before the app boots): wrap fetch and
 *      XMLHttpRequest, keep every JSON response body along with the request
 *      headers that fetched it. Also read Next.js's `self.__next_f` flight
 *      buffer, in case the collection arrives server-rendered instead.
 *   2. WALK: find which captured request actually yielded collection rows,
 *      then re-issue it with the same credentials, advancing whatever
 *      pagination it uses (page / offset / cursor) until it runs dry. This
 *      is what makes the export complete rather than "whatever you scrolled
 *      past".
 *   3. EXTRACT: walk the captured JSON for objects carrying a set + collector
 *      number, pulling quantity / variant / language / grading from the
 *      entry that owns them. Shape-agnostic, so it survives an API redesign.
 *
 * WHAT pkmn.gg DOES AND DOESN'T MODEL (RESEARCH.md §3.2) — this drives the
 * column mapping and is not guesswork:
 *   - Variant is first-class: one row per printing. Carried through.
 *   - Quantity is per printing. Carried through.
 *   - Condition is NOT tracked; every raw card is implicitly Near Mint.
 *   - Acquisition price / date are NOT tracked → purchase_price, currency
 *     are always empty. (This also removes a whole class of import failure:
 *     pkdump's parser aborts the entire file on a non-numeric price cell.)
 *   - Graded copies and private notes (Pro) have no PokeDumpster column, so
 *     they ride out in `notes`, which the importer turns into a tag.
 *
 * The CSV columns match the PokeDumpster-native parser in
 * crates/pkdump-core/src/import/pokedumpster.rs.
 */

(function () {
    'use strict';

    /* ─── Configuration ──────────────────────────────────────────────── */

    // Hosts whose responses are worth keeping even when the content-type is
    // unhelpful. pkmn.gg's SPA talks to api.tcg.gg (pokedumpster-p6y.3).
    const API_HOST_RE = /(^|\.)(tcg\.gg|pkmn\.gg)$/i;

    // Cap on the pagination walk, so a misread cursor can't spin forever.
    const MAX_PAGES = 200;
    const PAGE_DELAY_MS = 150;

    // The variant codes PokeDumpster's catalog actually knows, mirrored from
    // crates/pkdump-core/src/variant.rs KNOWN_VARIANTS. Used only to WARN:
    // an unrecognised variant is still exported verbatim, because the
    // importer parks unresolvable rows for manual resolution, and a parked
    // row you can see beats a row silently rewritten to "normal".
    // The catalog's actual variant vocabulary, mirrored from
    // data/variants.json. Resolution is an exact `printings.variant = ?`
    // match, so a code outside this set cannot resolve — the row is rejected
    // with "variant 'X' not available".
    //
    // NOTE: this is deliberately NOT variant.rs's KNOWN_VARIANTS, which is a
    // documentation list that also contains rarity names (double_rare,
    // illustration_rare, …). Those are not variants and never resolve.
    const KNOWN_VARIANTS = new Set([
        'black_dot_error', 'cosmos_foil', 'cosmos_holo', 'cosmos_holo_trick_or_trade',
        'cracked_ice_holo', 'duskball_rh', 'energy_holo', 'energy_symbol_rh',
        'first_ed_holo', 'first_ed_normal', 'friendball_rh', 'holo',
        'line_holo', 'loveball_rh', 'masterball_rh', 'metal_card',
        'mirage_holo', 'mirror_holo', 'missing_variant', 'normal',
        'peelable_ditto', 'pixel_holo', 'pokeball_rh', 'promo',
        'quickball_rh', 'reverse_cosmos_holo', 'reverse_holo', 'shadowless_holo',
        'shadowless_normal', 'sheen_holo', 'sparkle_holo', 'stamp_buildbattle',
        'stamp_holiday_calendar', 'stamp_pokemoncenter', 'stamp_prerelease', 'stamp_prerelease_staff',
        'stamp_staff', 'stamp_trick_or_trade', 'stamp_winner', 'team_rocket_rh',
        'unlimited_holo', 'unlimited_normal', 'water_web_holo',
    ]);

    // pkmn.gg variant vocabulary → PokeDumpster variant codes. Keys are
    // lowercased and space-normalised before lookup.
    const VARIANT_MAP = {
        'normal': 'normal',
        'non holo': 'normal',
        'nonholo': 'normal',
        'unlimited': 'unlimited_normal',
        'holo': 'holo',
        'holofoil': 'holo',
        'holo rare': 'holo',
        'foil': 'holo',
        'reverse': 'reverse_holo',
        'reverse holo': 'reverse_holo',
        'reverse holofoil': 'reverse_holo',
        'reverseholofoil': 'reverse_holo',
        'rh': 'reverse_holo',
        '1st edition': 'first_ed_normal',
        'first edition': 'first_ed_normal',
        '1st edition holo': 'first_ed_holo',
        '1st edition holofoil': 'first_ed_holo',
        'first edition holo': 'first_ed_holo',
        'unlimited holo': 'unlimited_holo',
        'unlimited holofoil': 'unlimited_holo',
        'shadowless': 'shadowless_normal',
        'shadowless holo': 'shadowless_holo',
        'pokeball': 'pokeball_rh',
        'poke ball': 'pokeball_rh',
        'poké ball': 'pokeball_rh',
        'pokeball holo': 'pokeball_rh',
        'masterball': 'masterball_rh',
        'master ball': 'masterball_rh',
        'masterball holo': 'masterball_rh',
        'quick ball': 'quickball_rh',
        'dusk ball': 'duskball_rh',
        'love ball': 'loveball_rh',
        'friend ball': 'friendball_rh',
        'energy symbol': 'energy_symbol_rh',
        'team rocket': 'team_rocket_rh',
        'cosmos': 'cosmos_holo',
        'cosmos holo': 'cosmos_holo',
        'reverse cosmos holo': 'reverse_cosmos_holo',
        'pixel holo': 'pixel_holo',
        'prerelease': 'stamp_prerelease',
        'prerelease stamp': 'stamp_prerelease',
        'build and battle': 'stamp_buildbattle',
        'build & battle': 'stamp_buildbattle',
        'pokemon center': 'stamp_pokemoncenter',
        'pokémon center': 'stamp_pokemoncenter',
        'pokemon center stamp': 'stamp_pokemoncenter',
        'staff': 'stamp_staff',
        'staff stamp': 'stamp_staff',

        // ── pkmn.gg's own slugs ──────────────────────────────────────
        // These are what the API actually returns; the label forms above
        // were written from the UI and never fire. Confirmed against a
        // 2,383-row export where these accounted for every rejected row.
        'pokeballpattern': 'pokeball_rh',
        'masterballpattern': 'masterball_rh',
        'quickballpattern': 'quickball_rh',
        'duskballpattern': 'duskball_rh',
        'loveballpattern': 'loveball_rh',
        'friendballpattern': 'friendball_rh',
        'energypattern': 'energy_symbol_rh',
        'rocketpattern': 'team_rocket_rh',
        // `holidaystamp` is Trick or Trade, NOT the advent calendar
        // (`stamp_holiday_calendar` is a real but unrelated treatment). The
        // catalog splits it two ways and pkmn.gg does not: measured over 60
        // real rows, 50 carry `stamp_trick_or_trade` and 10 carry
        // `cosmos_holo_trick_or_trade`. Nothing in the payload distinguishes
        // them, so take the majority — the other ~1 in 6 parks as
        // unresolved, which is visible and fixable, unlike a wrong guess.
        'holidaystamp': 'stamp_trick_or_trade',
        'prereleasestamp': 'stamp_prerelease',
        'staffstamp': 'stamp_staff',
        'pokemoncenterstamp': 'stamp_pokemoncenter',
        'trickortradestamp': 'stamp_trick_or_trade',
        'winnerstamp': 'stamp_winner',
        'crackedice': 'cracked_ice_holo',
        'crackedholo': 'cracked_ice_holo',
        'mirrorholo': 'mirror_holo',
        'cosmosholo': 'cosmos_holo',
        'blackstarpromo': 'promo',
        'promo': 'promo',
        'black star promo': 'promo',

        // Deliberately NOT mapped, so they park for manual resolution
        // rather than being guessed at:
        //   'stamp'             — ambiguous, and resolvable only per card:
        //                         these are set-specific promo stamps
        //                         (stamp_journey_together, stamp_black_bolt,
        //                         stamp_prismatic_evolutions, …). Each card
        //                         has exactly one, but picking it needs the
        //                         catalog, which is what the unresolved
        //                         queue is for.
        //   'jumbo'             — oversized cards. Zero printings in the
        //                         catalog carry this; not modelled at all.
        //   'holofoilalternate' — likewise zero printings. Leave it visible
        //                         rather than coercing it to `holo`.
    };

    const LANGUAGE_MAP = {
        'en': 'English', 'eng': 'English', 'english': 'English',
        'ja': 'Japanese', 'jp': 'Japanese', 'jpn': 'Japanese', 'japanese': 'Japanese',
        'zh': 'Chinese', 'chinese': 'Chinese',
        'ko': 'Korean', 'korean': 'Korean',
        'de': 'German', 'fr': 'French', 'es': 'Spanish', 'it': 'Italian',
        'pt': 'Portuguese', 'ru': 'Russian',
    };

    /* ─── Capture store ──────────────────────────────────────────────── */
    //
    // Keyed on url + a cheap body hash so the SPA re-fetching a page (React
    // re-renders, StrictMode double-invokes, back-navigation) cannot make the
    // same cards land twice. v0.1 papered over this by dropping duplicate
    // rows outright, which also deleted genuinely distinct copies.

    const captures = [];          // {url, method, headers, body, at}
    const captureKeys = new Set();
    let droppedDuplicates = 0;

    function hashString(s) {
        let h = 5381;
        for (let i = 0; i < s.length; i++) h = ((h << 5) + h + s.charCodeAt(i)) | 0;
        return h.toString(36);
    }

    function record(url, method, headers, body) {
        let serialised;
        try {
            serialised = JSON.stringify(body);
        } catch (_) {
            return; // circular / unserialisable — not something we can use.
        }
        if (!serialised || serialised.length < 2) return;
        const key = `${url}#${serialised.length}#${hashString(serialised)}`;
        if (captureKeys.has(key)) {
            droppedDuplicates += 1;
            return;
        }
        captureKeys.add(key);
        captures.push({ url, method: method || 'GET', headers: headers || {}, body, at: Date.now() });
        refreshPanel();
    }

    function isInteresting(url, contentType) {
        if (contentType && contentType.toLowerCase().includes('json')) return true;
        try {
            return API_HOST_RE.test(new URL(url, location.href).hostname);
        } catch (_) {
            return false;
        }
    }

    /* ─── Interceptors ───────────────────────────────────────────────── */
    //
    // Installed at document-start, before the app's own code runs. v0.1 ran
    // at document-idle and so missed every request the page made while
    // booting — which, on a Next.js app, is most of them.

    const headersToObject = (h) => {
        const out = {};
        if (!h) return out;
        try {
            if (typeof Headers !== 'undefined' && h instanceof Headers) {
                h.forEach((v, k) => { out[k] = v; });
            } else if (Array.isArray(h)) {
                for (const [k, v] of h) out[k] = v;
            } else if (typeof h === 'object') {
                for (const k in h) out[k] = h[k];
            }
        } catch (_) {}
        return out;
    };

    const origFetch = window.fetch;
    window.fetch = function (input, init) {
        const p = origFetch.apply(this, arguments);
        try {
            const isReq = typeof Request !== 'undefined' && input instanceof Request;
            const url = typeof input === 'string' ? input : (isReq ? input.url : String(input));
            const method = (init && init.method) || (isReq && input.method) || 'GET';
            // Request headers are the reason the pagination walk can
            // re-issue an authenticated call: pkmn.gg rides a bearer token,
            // and credentials:'include' alone would come back 401.
            const headers = Object.assign(
                {},
                isReq ? headersToObject(input.headers) : {},
                headersToObject(init && init.headers),
            );
            p.then((res) => {
                if (!res || !isInteresting(url, res.headers && res.headers.get('content-type'))) return;
                res.clone().json().then((body) => record(url, method, headers, body)).catch(() => {});
            }).catch(() => {});
        } catch (_) {}
        return p;
    };

    const XHR = window.XMLHttpRequest;
    if (XHR && XHR.prototype) {
        const origOpen = XHR.prototype.open;
        const origSend = XHR.prototype.send;
        const origSetHeader = XHR.prototype.setRequestHeader;
        XHR.prototype.open = function (method, url) {
            this.__pkdump = { method, url, headers: {} };
            return origOpen.apply(this, arguments);
        };
        XHR.prototype.setRequestHeader = function (k, v) {
            if (this.__pkdump) this.__pkdump.headers[k] = v;
            return origSetHeader.apply(this, arguments);
        };
        XHR.prototype.send = function () {
            const meta = this.__pkdump;
            if (meta) {
                this.addEventListener('load', () => {
                    try {
                        const ct = this.getResponseHeader && this.getResponseHeader('content-type');
                        if (!isInteresting(meta.url, ct)) return;
                        const text = this.responseType === '' || this.responseType === 'text'
                            ? this.responseText
                            : (this.responseType === 'json' ? JSON.stringify(this.response) : null);
                        if (!text) return;
                        record(meta.url, meta.method, meta.headers, JSON.parse(text));
                    } catch (_) {}
                });
            }
            return origSend.apply(this, arguments);
        };
    }

    // Next.js streams server-rendered data into `self.__next_f` as an array
    // of flight chunks. If pkmn.gg ever renders the collection on the server
    // there is no XHR to intercept, and this is the only place the rows
    // exist. Cheap to scan, so we always do.
    // Flight chunks are append-only, and extractJsonBlobs is superlinear in
    // chunk length, so remember how far we've read. boot() calls this every
    // couple of seconds; rescanning the whole buffer each time would wedge
    // the tab on a large collection.
    let flightScanned = 0;
    function harvestFlightPayload() {
        const f = window.__next_f;
        if (!Array.isArray(f)) return;
        for (let i = flightScanned; i < f.length; i++) {
            const chunk = f[i];
            const text = Array.isArray(chunk) ? chunk[1] : chunk;
            if (typeof text !== 'string' || text.length < 32) continue;
            for (const blob of extractJsonBlobs(text)) {
                record(`__next_f[${i}]`, 'FLIGHT', {}, blob);
            }
        }
        flightScanned = f.length;
    }

    // Pull balanced {...} / [...] runs out of a flight chunk and keep the
    // ones that parse. Deliberately crude: we only need candidates, and
    // extract() ignores anything that carries no card identity.
    function extractJsonBlobs(text) {
        const out = [];
        for (let i = 0; i < text.length; i++) {
            const open = text[i];
            if (open !== '{' && open !== '[') continue;
            const close = open === '{' ? '}' : ']';
            let depth = 0, inStr = false, esc = false;
            for (let j = i; j < text.length; j++) {
                const c = text[j];
                if (esc) { esc = false; continue; }
                if (c === '\\') { esc = true; continue; }
                if (c === '"') { inStr = !inStr; continue; }
                if (inStr) continue;
                if (c === open) depth++;
                else if (c === close) {
                    depth--;
                    if (depth === 0) {
                        const slice = text.slice(i, j + 1);
                        if (slice.length > 32) {
                            try { out.push(JSON.parse(slice)); } catch (_) {}
                        }
                        i = j;
                        break;
                    }
                }
            }
        }
        return out;
    }

    /* ─── Extraction ─────────────────────────────────────────────────── */

    const norm = (s) => String(s).trim().toLowerCase().replace(/[\s_-]+/g, ' ');

    function mapVariant(raw) {
        if (raw == null) return { code: 'normal', known: true, raw: '' };
        let v = raw;
        if (typeof v === 'object') v = v.code ?? v.slug ?? v.name ?? v.label ?? v.type ?? '';
        const k = norm(v);
        if (k === '') return { code: 'normal', known: true, raw: '' };
        const mapped = VARIANT_MAP[k];
        if (mapped) return { code: mapped, known: true, raw: String(v) };
        // pkmn.gg names pattern overlays `<thing>pattern`; the catalog names
        // them `<thing>_rh`. Derive rather than wait for the table to catch
        // up, but only when the derived code really exists.
        const derived = /^([a-z]+)pattern$/.exec(k);
        if (derived && KNOWN_VARIANTS.has(`${derived[1]}_rh`)) {
            return { code: `${derived[1]}_rh`, known: true, raw: String(v) };
        }
        // Unrecognised: snake-case it and pass through. The importer will
        // park the row rather than resolve it — which is the point.
        const code = k.replace(/\s+/g, '_');
        return { code, known: KNOWN_VARIANTS.has(code), raw: String(v) };
    }

    function mapLanguage(raw) {
        if (raw == null) return 'English';
        let v = raw;
        if (typeof v === 'object') v = v.code ?? v.slug ?? v.name ?? '';
        const k = norm(v);
        if (k === '') return 'English';
        return LANGUAGE_MAP[k] || (k.charAt(0).toUpperCase() + k.slice(1));
    }

    // Does this object name a specific printing? Requires a collector number
    // plus at least one set identifier — the same pair pkdump's resolver
    // needs, so anything weaker would only produce unresolvable rows.
    // pkmn.gg hands every card two ready-made interchange strings:
    //   tcgLiveCode        "Team Rocket's Mewtwo ex DRI 81"
    //   tcgPlayerMassEntry "Team Rocket's Mewtwo ex - 081/182 [DRI]"
    // Both carry the PTCGO set code and the collector number, which is
    // exactly the pair pkdump's parser accepts. Reading identity out of
    // these is far more robust than guessing at field names, and it keeps
    // working if pkmn.gg renames `set`/`number` underneath.
    function identityFromCodes(o) {
        const live = o.tcgLiveCode || o.tcg_live_code;
        if (typeof live === 'string') {
            const m = live.trim().match(/\s([A-Z0-9-]{2,6})\s+([A-Za-z0-9-]+)$/);
            if (m) return { number: m[2], setCode: '', ptcgo: m[1] };
        }
        const mass = o.tcgPlayerMassEntry || o.tcg_player_mass_entry;
        if (typeof mass === 'string') {
            const m = mass.trim().match(/-\s*([A-Za-z0-9-]+)\/\S*\s*\[([A-Z0-9-]{2,6})\]/);
            if (m) return { number: m[1].replace(/^0+(?=\d)/, ''), setCode: '', ptcgo: m[2] };
        }
        return null;
    }

    function identityOf(o) {
        if (!o || typeof o !== 'object' || Array.isArray(o)) return null;
        const number = o.number ?? o.collectorNumber ?? o.collector_number ??
            o.cardNumber ?? o.card_number;
        if (number == null || number === '') return identityFromCodes(o);
        const set = o.set || o.expansion || o.series || o.setInfo;
        const pick = (obj, ...keys) => {
            for (const k of keys) {
                const v = obj && obj[k];
                if (v != null && v !== '') return String(v);
            }
            return '';
        };
        const setCode = pick(o, 'setCode', 'set_code', 'expansionCode') ||
            pick(set, 'code', 'id', 'slug', 'setCode');
        const ptcgo = pick(o, 'ptcgoCode', 'ptcgo_code', 'ptcgoAbbr') ||
            pick(set, 'ptcgoCode', 'ptcgo_code', 'abbreviation', 'abbr');
        if (!setCode && !ptcgo) {
            const viaCode = identityFromCodes(o);
            if (viaCode) return { number: String(number), setCode: '', ptcgo: viaCode.ptcgo };
            return null;
        }
        return { number: String(number), setCode, ptcgo };
    }

    // Where a collection entry parks the printing it refers to.
    const CARD_KEYS = ['card', 'printing', 'cardPrinting', 'product', 'variant', 'item'];

    function firstDefined(sources, keys) {
        for (const src of sources) {
            if (!src || typeof src !== 'object') continue;
            for (const k of keys) {
                const v = src[k];
                if (v != null && v !== '') return v;
            }
        }
        return undefined;
    }

    function buildRow(entry, cardNode, ident) {
        const scope = [entry, cardNode];

        // `variant` and `printing` are both places a printing can hide AND
        // names for the treatment itself. When the card identity came out of
        // one of them, that key must not also be read as the treatment name —
        // it holds the printing object, whose `name` is the card's name.
        const holder = cardNode !== entry
            ? CARD_KEYS.find((k) => entry[k] === cardNode)
            : undefined;
        const variantKeys = ['variant', 'printing', 'finish', 'foil', 'treatment',
            'subType', 'sub_type', 'variantName', 'printingName']
            .filter((k) => k !== holder);
        const variant = mapVariant(firstDefined(scope, variantKeys));

        let qty = firstDefined(scope, ['quantity', 'qty', 'count', 'amount', 'owned', 'numOwned']);
        if (typeof qty === 'boolean') qty = qty ? 1 : 0;
        qty = parseInt(qty, 10);
        if (!Number.isFinite(qty) || qty < 1) qty = 1;

        // Grading and private notes have no column of their own; the
        // importer folds `notes` into collection.tags, so that is where they
        // go rather than being dropped on the floor.
        const noteBits = [];
        const gradeCo = firstDefined(scope, ['gradingCompany', 'grader', 'gradedBy', 'company']);
        const grade = firstDefined(scope, ['grade', 'gradeValue']);
        const cert = firstDefined(scope, ['certNumber', 'cert', 'certificateNumber', 'serial']);
        if (gradeCo || grade) noteBits.push(`graded ${[gradeCo, grade].filter(Boolean).join(' ')}`.trim());
        if (cert) noteBits.push(`cert ${cert}`);
        const userNote = firstDefined(scope, ['note', 'notes', 'comment', 'privateNote']);
        if (typeof userNote === 'string' && userNote.trim()) noteBits.push(userNote.trim());

        // A collection entry on /page/u/<user>/collection is
        // `{card, variant, quantity}` — no entry id of its own. But
        // (card.id, variant) *is* a stable identity, and pkmn.gg models one
        // row per printing, so synthesising it here is exact, not a
        // heuristic. This is what keeps those rows out of the id-less
        // reconciliation path below.
        let id = firstDefined([entry], ['id', '_id', 'uuid', 'entryId']);
        if (id == null && cardNode !== entry) {
            const cardId = firstDefined([cardNode], ['id', '_id', 'uuid', 'cardId']);
            if (cardId != null) id = `${cardId}|${variant.code}`;
        }

        return {
            id,
            massEntry: firstDefined(scope, ['tcgPlayerMassEntry', 'tcg_player_mass_entry']),
            liveCode: firstDefined(scope, ['tcgLiveCode', 'tcg_live_code']),
            unknownVariant: variant.known ? null : variant.raw,
            row: {
                set_code: ident.setCode,
                ptcgo_code: ident.ptcgo,
                number: ident.number,
                variant: variant.code,
                // pkmn.gg does not track condition for raw cards
                // (RESEARCH.md §3.2) — every copy is implicitly Near Mint.
                condition: 'Near Mint',
                language: mapLanguage(
                    firstDefined(scope, ['language', 'lang', 'locale', 'region']),
                ),
                quantity: qty,
                // Not modelled by pkmn.gg. Left blank deliberately: pkdump's
                // parser aborts the whole file on a cell it can't read as a
                // number, so inventing one here would be worse than useless.
                purchase_price: '',
                currency: '',
                source: 'pkmn.gg',
                notes: noteBits.join('; '),
                // Not CSV columns (toCsv only emits COLUMNS); carried so the
                // Mass Entry / PTCG Live outputs need no second pass.
                _massEntry: firstDefined(scope, ['tcgPlayerMassEntry', 'tcg_player_mass_entry']) || '',
                _liveCode: firstDefined(scope, ['tcgLiveCode', 'tcg_live_code']) || '',
            },
        };
    }

    // Walk a captured body, emitting one candidate per collection entry.
    // Recursion stops at an entry: a printing's own children (its set, its
    // prices, its images) are not further entries, and descending into them
    // is how v0.1 managed to count the same card twice.
    function collectFrom(node, out, srcUrl, depth) {
        if (!node || typeof node !== 'object' || depth > 24) return;
        if (Array.isArray(node)) {
            for (const child of node) collectFrom(child, out, srcUrl, depth + 1);
            return;
        }

        let ident = identityOf(node);
        let cardNode = node;
        if (!ident) {
            for (const k of CARD_KEYS) {
                const c = identityOf(node[k]);
                if (c) { ident = c; cardNode = node[k]; break; }
            }
        }
        if (ident) {
            const built = buildRow(node, cardNode, ident);
            built.srcUrl = srcUrl;
            out.push(built);
            return;
        }

        for (const k in node) {
            if (Object.prototype.hasOwnProperty.call(node, k)) {
                collectFrom(node[k], out, srcUrl, depth + 1);
            }
        }
    }

    function cardKey(r) {
        return [r.set_code, r.ptcgo_code, r.number, r.variant, r.language, r.notes].join('|');
    }

    // Reconcile candidates into final rows.
    //
    // An entry that carries its own id is trivial: one id, one row, keep its
    // quantity. Entries without an id are the awkward case — the same card
    // can legitimately arrive in two different responses (a set page and a
    // "recently added" strip), and summing those would inflate the count. So
    // within one response URL we sum, and across responses we take the
    // largest total rather than adding them. Under-counting a duplicate beats
    // fabricating cards the collection doesn't contain, and the summary line
    // says how many rows took this path.
    function reconcile(candidates) {
        const byId = new Map();
        const byKey = new Map();   // key → Map(srcUrl → qty)
        const keyRow = new Map();
        let idless = 0;

        for (const c of candidates) {
            if (c.id != null && c.id !== '') {
                // Same id twice means the same entry seen twice, not two
                // entries — keep the larger quantity rather than whichever
                // response happened to arrive last.
                const prev = byId.get(String(c.id));
                if (!prev || c.row.quantity > prev.quantity) byId.set(String(c.id), c.row);
                continue;
            }
            idless += 1;
            const k = cardKey(c.row);
            keyRow.set(k, c.row);
            if (!byKey.has(k)) byKey.set(k, new Map());
            const perUrl = byKey.get(k);
            perUrl.set(c.srcUrl, (perUrl.get(c.srcUrl) || 0) + c.row.quantity);
        }

        const rows = Array.from(byId.values());
        for (const [k, perUrl] of byKey) {
            const row = Object.assign({}, keyRow.get(k));
            row.quantity = Math.max(...perUrl.values());
            rows.push(row);
        }

        rows.sort((a, b) =>
            (a.set_code || a.ptcgo_code).localeCompare(b.set_code || b.ptcgo_code) ||
            String(a.number).localeCompare(String(b.number), undefined, { numeric: true }) ||
            a.variant.localeCompare(b.variant));

        return { rows, idless, withIds: byId.size };
    }

    function extract() {
        harvestFlightPayload();
        const candidates = [];
        for (const c of captures) collectFrom(c.body, candidates, c.url, 0);
        const result = reconcile(candidates);
        result.unknownVariants = Array.from(new Set(
            candidates.filter((c) => c.unknownVariant).map((c) => c.unknownVariant),
        ));
        result.candidates = candidates.length;
        return result;
    }

    /* ─── Pagination walk ────────────────────────────────────────────── */
    //
    // Passive capture only ever sees what the app chose to request. A
    // virtualised grid requests a window at a time, so "scroll to the bottom
    // and hope" is both slow and lossy. Instead: find the request that
    // actually produced rows, then drive its own pagination to exhaustion.

    const PAGE_PARAMS = ['page', 'pageNumber', 'pageIndex', 'p'];
    const OFFSET_PARAMS = ['offset', 'skip', 'start', 'from'];
    const LIMIT_PARAMS = ['limit', 'pageSize', 'per_page', 'perPage', 'take', 'size', 'count'];
    const CURSOR_PARAMS = ['cursor', 'nextCursor', 'after', 'startAfter', 'continuation'];

    function productiveCaptures() {
        const scored = [];
        for (const c of captures) {
            if (c.method === 'FLIGHT') continue;
            const found = [];
            collectFrom(c.body, found, c.url, 0);
            if (found.length) scored.push({ capture: c, rows: found.length });
        }
        scored.sort((a, b) => b.rows - a.rows);
        return scored;
    }

    function nextCursorIn(body) {
        if (!body || typeof body !== 'object') return null;
        const direct = body.nextCursor ?? body.next_cursor ?? body.cursor ??
            body.nextPage ?? body.next;
        if (typeof direct === 'string' && direct) return direct;
        for (const k of ['meta', 'pagination', 'page', 'pageInfo', 'data']) {
            const nested = body[k];
            if (nested && typeof nested === 'object') {
                const v = nextCursorIn(nested);
                if (v) return v;
            }
        }
        return null;
    }

    function replayHeaders(headers) {
        // Keep the auth-bearing headers, drop the ones the browser must set
        // itself (sending them by hand throws).
        const banned = /^(host|connection|content-length|origin|referer|cookie|user-agent|accept-encoding|sec-|:)/i;
        const out = {};
        for (const k in headers) {
            if (!banned.test(k)) out[k] = headers[k];
        }
        return out;
    }

    // The collection endpoint, confirmed by recon rather than guessed:
    //   GET api.tcg.gg/pkmn/v1/page/u/<username>/collection?pageSize=60[&cursor=]
    //   → {cards: [{card, variant, quantity}], nextCursor, facets, showPrivate}
    // Cursor-paginated, pageSize caps at 60. Walking it directly is exact and
    // complete, where passive capture only ever sees what the app asked for.
    // Called from our own session, so `showPrivate` covers cards a public
    // profile view would omit.
    const COLLECTION_ENDPOINT = (user) =>
        `https://api.tcg.gg/pkmn/v1/page/u/${encodeURIComponent(user)}/collection`;

    function discoverUsername() {
        // 1. A capture already naming the route settles it.
        for (const c of captures) {
            const m = /\/page\/u\/([^/?#]+)\/collection/.exec(c.url);
            if (m) return decodeURIComponent(m[1]);
        }
        // 2. The profile page we're standing on.
        const m = /^\/u\/([^/?#]+)/.exec(location.pathname);
        if (m) return decodeURIComponent(m[1]);
        // 3. Whatever /auth/me called us.
        for (const c of captures) {
            if (!/auth\/me|\/me$|profile/i.test(c.url)) continue;
            const b = c.body || {};
            for (const src of [b, b.profile, b.user, b.data]) {
                if (!src || typeof src !== 'object') continue;
                for (const k of ['username', 'handle', 'slug', 'name']) {
                    if (typeof src[k] === 'string' && src[k].trim()) return src[k].trim();
                }
            }
        }
        return null;
    }

    // pkmn.gg mints a short-lived bearer from an HttpOnly cookie, so the
    // token is only ever in memory — the one the app most recently used is
    // the one that still works.
    function latestAuthHeaders() {
        for (let i = captures.length - 1; i >= 0; i--) {
            const h = captures[i].headers || {};
            for (const k in h) {
                if (/^authorization$/i.test(k) && h[k]) {
                    return replayHeaders(h);
                }
            }
        }
        return { accept: 'application/json' };
    }

    async function directCollectionWalk(onProgress) {
        const user = discoverUsername();
        if (!user) return { ok: false, reason: 'username unknown', pages: 0 };
        const headers = latestAuthHeaders();
        let cursor = null;
        let pages = 0;
        const before = captures.length;

        for (let i = 0; i < MAX_PAGES; i++) {
            const u = new URL(COLLECTION_ENDPOINT(user));
            u.searchParams.set('pageSize', '60');
            if (cursor) u.searchParams.set('cursor', cursor);

            let body;
            try {
                const res = await origFetch(u.toString(), { headers, credentials: 'include' });
                if (!res.ok) {
                    return { ok: pages > 0, reason: `HTTP ${res.status}`, pages, user };
                }
                body = await res.json();
            } catch (e) {
                return { ok: pages > 0, reason: e.message, pages, user };
            }

            record(u.toString(), 'GET', headers, body);
            pages += 1;
            const n = Array.isArray(body.cards) ? body.cards.length : 0;
            if (onProgress) onProgress(pages, n);

            cursor = body.nextCursor || nextCursorIn(body);
            if (!cursor || n === 0) break;
            await new Promise((r) => setTimeout(r, PAGE_DELAY_MS));
        }

        return { ok: pages > 0, pages, user, added: captures.length - before };
    }

    async function walkPagination(onProgress) {
        const productive = productiveCaptures();
        if (!productive.length) return { walked: 0, added: 0, endpoint: null };

        const { capture } = productive[0];
        let url;
        try {
            url = new URL(capture.url, location.href);
        } catch (_) {
            return { walked: 0, added: 0, endpoint: null };
        }

        const params = url.searchParams;
        const pageParam = PAGE_PARAMS.find((p) => params.has(p));
        const offsetParam = OFFSET_PARAMS.find((p) => params.has(p));
        const limitParam = LIMIT_PARAMS.find((p) => params.has(p));
        const limit = limitParam ? parseInt(params.get(limitParam), 10) || 0 : 0;
        const cursorParam = CURSOR_PARAMS.find((p) => params.has(p)) || 'cursor';
        let cursor = nextCursorIn(capture.body);

        const headers = replayHeaders(capture.headers);
        const before = captures.length;
        let walked = 0;

        // Nothing to advance and no cursor: the endpoint returns everything
        // in one shot, so the capture we already hold is the whole thing.
        if (!pageParam && !offsetParam && !cursor) {
            return { walked: 0, added: 0, endpoint: url.toString(), single: true };
        }

        for (let i = 0; i < MAX_PAGES; i++) {
            const nextUrl = new URL(url.toString());
            if (cursor) {
                nextUrl.searchParams.set(cursorParam, cursor);
            } else if (pageParam) {
                const cur = parseInt(params.get(pageParam), 10);
                nextUrl.searchParams.set(pageParam, String((Number.isFinite(cur) ? cur : 1) + i + 1));
            } else if (offsetParam) {
                const cur = parseInt(params.get(offsetParam), 10) || 0;
                const step = limit || 50;
                nextUrl.searchParams.set(offsetParam, String(cur + step * (i + 1)));
            }

            let body;
            try {
                const res = await origFetch(nextUrl.toString(), {
                    method: capture.method === 'FLIGHT' ? 'GET' : capture.method,
                    headers,
                    credentials: 'include',
                });
                if (!res.ok) break;
                body = await res.json();
            } catch (_) {
                break;
            }

            const found = [];
            collectFrom(body, found, nextUrl.toString(), 0);
            const sizeBefore = captures.length;
            record(nextUrl.toString(), 'GET', capture.headers, body);
            walked += 1;
            if (onProgress) onProgress(walked, found.length);

            // A page that returned nothing new — either no rows, or a body
            // byte-identical to one we already hold — is the end of the list.
            if (!found.length || captures.length === sizeBefore) break;

            cursor = nextCursorIn(body);
            if (!cursor && !pageParam && !offsetParam) break;
            await new Promise((r) => setTimeout(r, PAGE_DELAY_MS));
        }

        return { walked, added: captures.length - before, endpoint: url.toString() };
    }

    /* ─── Output ─────────────────────────────────────────────────────── */

    const COLUMNS = [
        'set_code', 'ptcgo_code', 'number', 'variant', 'condition',
        'language', 'quantity', 'purchase_price', 'currency', 'source', 'notes',
    ];

    function csvCell(v) {
        if (v == null) return '';
        const s = String(v);
        return /[,"\n\r]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
    }

    function toCsv(rows) {
        return [COLUMNS.join(','), ...rows.map((r) => COLUMNS.map((c) => csvCell(r[c])).join(','))]
            .join('\n') + '\n';
    }

    // pkmn.gg hands us TCGplayer Mass Entry and PTCG Live strings per card,
    // so an exit path to the rest of the ecosystem costs nothing but the
    // join. This is the friend's data in a form that needs no PokeDumpster.
    function toMassEntry(rows) {
        const lines = rows.filter((r) => r._massEntry)
            .map((r) => `${r.quantity} ${r._massEntry}`);
        return lines.length ? lines.join('\n') + '\n' : '';
    }

    function toLiveList(rows) {
        const lines = rows.filter((r) => r._liveCode)
            .map((r) => `${r.quantity} ${r._liveCode}`);
        return lines.length ? lines.join('\n') + '\n' : '';
    }

    function stamp() {
        return new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19);
    }

    function download(filename, text, mime) {
        const blob = new Blob([text], { type: `${mime};charset=utf-8` });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = filename;
        a.style.display = 'none';
        document.body.appendChild(a);
        a.click();
        setTimeout(() => { a.remove(); URL.revokeObjectURL(url); }, 1000);
    }

    // The raw capture is both the friend's own data in lossless form and the
    // only thing that can teach this script a new API shape. Redact the
    // Authorization header — the file is meant to be shareable.
    function captureBundle(summary) {
        return JSON.stringify({
            exportedAt: new Date().toISOString(),
            script: 'pkmngg_export.user.js 0.2.0',
            page: location.href,
            summary,
            duplicateResponsesDropped: droppedDuplicates,
            responses: captures.map((c) => ({
                url: c.url,
                method: c.method,
                headers: Object.fromEntries(
                    Object.entries(c.headers).filter(([k]) => !/^(authorization|cookie)$/i.test(k)),
                ),
                body: c.body,
            })),
        }, null, 2);
    }

    /* ─── Panel ──────────────────────────────────────────────────────── */

    let panel, statusEl, exportBtn;

    function refreshPanel() {
        if (statusEl && !statusEl.dataset.busy) {
            statusEl.textContent = `${captures.length} responses captured`;
        }
    }

    function say(text, busy) {
        if (!statusEl) return;
        if (busy) statusEl.dataset.busy = '1'; else delete statusEl.dataset.busy;
        statusEl.textContent = text;
    }

    function ensurePanel() {
        if (!document.body || document.getElementById('pkdump-panel')) return;
        panel = document.createElement('div');
        panel.id = 'pkdump-panel';
        Object.assign(panel.style, {
            position: 'fixed', right: '16px', bottom: '16px', zIndex: '2147483647',
            background: '#16213e', color: '#fff', padding: '10px 12px',
            borderRadius: '10px', font: '13px/1.4 system-ui, sans-serif',
            boxShadow: '0 6px 20px rgba(0,0,0,.4)', minWidth: '200px',
        });

        statusEl = document.createElement('div');
        statusEl.style.marginBottom = '8px';
        statusEl.style.opacity = '.85';
        statusEl.textContent = '0 responses captured';

        const mkBtn = (label, bg, fn) => {
            const b = document.createElement('button');
            b.textContent = label;
            Object.assign(b.style, {
                padding: '6px 10px', marginRight: '6px', border: 'none',
                borderRadius: '6px', background: bg, color: '#fff',
                cursor: 'pointer', font: '600 13px system-ui, sans-serif',
            });
            b.addEventListener('click', fn);
            return b;
        };

        exportBtn = mkBtn('Export', '#e94560', onExport);
        const dumpBtn = mkBtn('Dump capture', '#0f3460', onDump);

        panel.append(statusEl, exportBtn, dumpBtn);
        document.body.appendChild(panel);
    }

    async function onExport() {
        exportBtn.disabled = true;
        try {
            // Preferred: walk the known collection endpoint. Falls back to
            // replaying whatever request the app itself made, which is the
            // only option if pkmn.gg moves the route.
            say('Walking your collection…', true);
            let walk = await directCollectionWalk(
                (n, rows) => say(`Page ${n} (${rows} cards)…`, true));
            if (!walk.ok) {
                console.warn('[PokeDumpster] direct walk unavailable:', walk.reason,
                    '— falling back to replaying the captured request');
                say('Replaying captured requests…', true);
                walk = await walkPagination((n, rows) => say(`Page ${n} (+${rows} rows)…`, true));
            }

            say('Extracting…', true);
            const result = extract();

            if (!result.rows.length) {
                say(`No cards found in ${captures.length} responses`, false);
                alert(
                    'PokeDumpster export: no cards found.\n\n' +
                    `Captured ${captures.length} API responses but none carried a ` +
                    'set + collector number.\n\n' +
                    'Open your collection, let it render, then try again. If it ' +
                    'still finds nothing, click "Dump capture" and send that file ' +
                    'back — it has everything needed to teach the extractor ' +
                    "pkmn.gg's current API shape."
                );
                return;
            }

            const total = result.rows.reduce((n, r) => n + r.quantity, 0);
            const summary = {
                rows: result.rows.length,
                cards: total,
                entriesWithIds: result.withIds,
                entriesWithoutIds: result.idless,
                unknownVariants: result.unknownVariants,
                pagesWalked: walk.pages ?? walk.walked,
                endpoint: walk.user ? COLLECTION_ENDPOINT(walk.user) : walk.endpoint,
            };

            const ts = stamp();
            download(`pokedumpster-pkmngg-${ts}.csv`, toCsv(result.rows), 'text/csv');
            download(`pokedumpster-pkmngg-${ts}.json`, captureBundle(summary), 'application/json');

            // Only emitted when pkmn.gg actually supplied the codes.
            const mass = toMassEntry(result.rows);
            if (mass) {
                download(`pkmngg-tcgplayer-mass-entry-${ts}.txt`, mass, 'text/plain');
                summary.massEntryLines = mass.trim().split('\n').length;
            }
            const live = toLiveList(result.rows);
            if (live) {
                download(`pkmngg-ptcg-live-${ts}.txt`, live, 'text/plain');
            }

            console.log('[PokeDumpster] export summary', summary);
            say(`${result.rows.length} rows / ${total} cards ✓`, false);

            if (result.unknownVariants.length) {
                console.warn(
                    '[PokeDumpster] variants PokeDumpster does not recognise — these ' +
                    'rows will park as unresolved on import, add them to VARIANT_MAP:',
                    result.unknownVariants,
                );
            }
        } catch (e) {
            console.error('[PokeDumpster] export failed', e);
            say('Export failed — see console', false);
        } finally {
            exportBtn.disabled = false;
        }
    }

    function onDump() {
        const result = extract();
        download(
            `pokedumpster-pkmngg-capture-${stamp()}.json`,
            captureBundle({ rows: result.rows.length, candidates: result.candidates }),
            'application/json',
        );
    }

    /* ─── Boot ───────────────────────────────────────────────────────── */

    // document-start means there is no body yet, and pkmn.gg is an SPA that
    // replaces its own DOM on navigation, so keep re-asserting the panel.
    const boot = () => {
        ensurePanel();
        harvestFlightPayload();
    };
    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', boot);
    } else {
        boot();
    }
    setInterval(boot, 2000);
})();
