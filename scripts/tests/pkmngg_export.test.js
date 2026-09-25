// Unit tests for scripts/pkmngg_export.user.js — the extraction and
// mapping logic, which is everything that can be tested without a live
// pkmn.gg account. Run: node scripts/tests/pkmngg_export.test.js
const assert = require('node:assert');
const t = require('./harness.js');

let pass = 0, fail = 0;
const test = (name, fn) => {
    // Each case starts from an empty capture buffer.
    t.captures.length = 0;
    try { fn(); console.log('  ok  ', name); pass++; }
    catch (e) { console.log('  FAIL', name, '\n        ', e.message); fail++; }
};
const rowsOf = () => t.extract();

// ── Fixtures modelled on what a collection entry plausibly looks like ──
// Nested shape: the entry owns quantity, the printing owns identity.
const nested = (id, num, variant, qty, extra = {}) => ({
    id, quantity: qty, variant,
    card: { id: `card-${num}`, name: 'Pikachu', number: num,
            set: { code: 'sv3pt5', ptcgoCode: 'MEW', name: '151' } },
    ...extra,
});

console.log('\nextraction');

test('nested entry: quantity from parent, identity from card', () => {
    t.record('https://api.tcg.gg/pkmn/v1/collections/1/cards', 'GET', {},
        { data: [nested('e1', '25', 'Reverse Holo', 3)] });
    const r = rowsOf();
    assert.strictEqual(r.rows.length, 1);
    assert.strictEqual(r.rows[0].quantity, 3, 'quantity must come from the entry');
    assert.strictEqual(r.rows[0].number, '25');
    assert.strictEqual(r.rows[0].set_code, 'sv3pt5');
    assert.strictEqual(r.rows[0].ptcgo_code, 'MEW');
    assert.strictEqual(r.rows[0].variant, 'reverse_holo');
});

test('nested card is not counted a second time', () => {
    t.record('https://api.tcg.gg/pkmn/v1/collections/1/cards', 'GET', {},
        { data: [nested('e1', '25', 'Normal', 2)] });
    assert.strictEqual(rowsOf().rows.length, 1);
});

test('flat entry: identity and quantity on the same object', () => {
    t.record('https://api.tcg.gg/pkmn/v1/x', 'GET', {},
        [{ id: 'f1', setCode: 'sv3pt5', number: '6', quantity: 4, printing: 'Holofoil' }]);
    const r = rowsOf();
    assert.strictEqual(r.rows.length, 1);
    assert.strictEqual(r.rows[0].quantity, 4);
    assert.strictEqual(r.rows[0].variant, 'holo');
});

test('an identical response replayed does not double-count', () => {
    const body = { data: [nested('e1', '25', 'Normal', 2)] };
    t.record('https://api.tcg.gg/pkmn/v1/c', 'GET', {}, body);
    t.record('https://api.tcg.gg/pkmn/v1/c', 'GET', {}, JSON.parse(JSON.stringify(body)));
    const r = rowsOf();
    assert.strictEqual(r.rows.length, 1);
    assert.strictEqual(r.rows[0].quantity, 2, 'quantity must not double');
});

test('same entry id seen on two endpoints collapses to one row', () => {
    t.record('https://api.tcg.gg/pkmn/v1/collections/1', 'GET', {},
        { data: [nested('e1', '25', 'Normal', 2)] });
    t.record('https://api.tcg.gg/pkmn/v1/recent', 'GET', {},
        { items: [nested('e1', '25', 'Normal', 2)] });
    const r = rowsOf();
    assert.strictEqual(r.rows.length, 1);
    assert.strictEqual(r.rows[0].quantity, 2);
});

test('id-less duplicates across responses take the max, not the sum', () => {
    const mk = (qty) => ({ setCode: 'sv3pt5', number: '25', quantity: qty, variant: 'Normal' });
    t.record('https://api.tcg.gg/pkmn/v1/a', 'GET', {}, [mk(2)]);
    t.record('https://api.tcg.gg/pkmn/v1/b', 'GET', {}, [mk(2)]);
    const r = rowsOf();
    assert.strictEqual(r.rows.length, 1);
    assert.strictEqual(r.rows[0].quantity, 2, 'must not inflate to 4');
    assert.strictEqual(r.idless, 2);
});

test('id-less duplicates within one response are summed', () => {
    const mk = (qty) => ({ setCode: 'sv3pt5', number: '25', quantity: qty, variant: 'Normal' });
    t.record('https://api.tcg.gg/pkmn/v1/a', 'GET', {}, [mk(1), mk(2)]);
    assert.strictEqual(rowsOf().rows[0].quantity, 3);
});

test('two variants of one card stay two rows', () => {
    t.record('https://api.tcg.gg/pkmn/v1/c', 'GET', {}, { data: [
        nested('e1', '25', 'Normal', 1),
        nested('e2', '25', 'Reverse Holo', 2),
    ]});
    const r = rowsOf();
    assert.strictEqual(r.rows.length, 2);
    assert.deepStrictEqual(r.rows.map((x) => x.variant).sort(), ['normal', 'reverse_holo']);
});

console.log('\nfield mapping');

test('condition is always Near Mint (pkmn.gg does not model it)', () => {
    t.record('https://api.tcg.gg/pkmn/v1/c', 'GET', {}, [nested('e1', '25', 'Normal', 1)]);
    assert.strictEqual(rowsOf().rows[0].condition, 'Near Mint');
});

test('price and currency are always blank', () => {
    t.record('https://api.tcg.gg/pkmn/v1/c', 'GET', {},
        [nested('e1', '25', 'Normal', 1, { marketPrice: '$12.50 USD' })]);
    const r = rowsOf().rows[0];
    assert.strictEqual(r.purchase_price, '');
    assert.strictEqual(r.currency, '');
});

test('japanese cards keep their language', () => {
    t.record('https://api.tcg.gg/pkmn/v1/c', 'GET', {},
        [nested('e1', '25', 'Normal', 1, { language: 'ja' })]);
    assert.strictEqual(rowsOf().rows[0].language, 'Japanese');
});

test('graded copies survive in notes', () => {
    t.record('https://api.tcg.gg/pkmn/v1/c', 'GET', {}, [nested('e1', '25', 'Holo', 1,
        { gradingCompany: 'PSA', grade: 10, certNumber: '12345678' })]);
    assert.strictEqual(rowsOf().rows[0].notes, 'graded PSA 10; cert 12345678');
});

test('unknown variants pass through and are reported', () => {
    t.record('https://api.tcg.gg/pkmn/v1/c', 'GET', {},
        [nested('e1', '25', 'Glitter Bomb Foil', 1)]);
    const r = rowsOf();
    assert.strictEqual(r.rows[0].variant, 'glitter_bomb_foil');
    assert.deepStrictEqual(r.unknownVariants, ['Glitter Bomb Foil']);
});

test('variant given as an object is unwrapped', () => {
    t.record('https://api.tcg.gg/pkmn/v1/c', 'GET', {}, [{
        id: 'v1', quantity: 1, variant: { slug: 'master-ball', name: 'Master Ball' },
        card: { number: '9', set: { code: 'sv8' } },
    }]);
    assert.strictEqual(rowsOf().rows[0].variant, 'masterball_rh');
});

test('rows without a set identifier are ignored', () => {
    t.record('https://api.tcg.gg/pkmn/v1/c', 'GET', {}, [{ number: '25', quantity: 1 }]);
    assert.strictEqual(rowsOf().rows.length, 0);
});

test('unrelated JSON produces no rows', () => {
    t.record('https://api.tcg.gg/pkmn/v1/auth/me', 'GET', {},
        { id: 'u1', email: 'a@b.c', roles: [], areWritesLocked: false });
    assert.strictEqual(rowsOf().rows.length, 0);
});

console.log('\nnext.js flight payload');

test('json embedded in a flight chunk is recovered', () => {
    const blobs = t.extractJsonBlobs(
        '3:["$","div",null,{"children":' +
        '{"entries":[{"id":"e9","quantity":5,"variant":"Holofoil",' +
        '"card":{"number":"199","set":{"code":"sv3pt5","ptcgoCode":"MEW"}}}]}}]'
    );
    const found = [];
    blobs.forEach((b) => t.collectFrom(b, found, 'flight', 0));
    assert.ok(found.length >= 1, 'expected at least one row from the flight chunk');
    assert.strictEqual(found[0].row.number, '199');
    assert.strictEqual(found[0].row.quantity, 5);
});

console.log('\ncsv');

test('csv header matches the pkdump parser contract', () => {
    const csv = t.toCsv([]);
    assert.strictEqual(csv.trim(),
        'set_code,ptcgo_code,number,variant,condition,language,quantity,' +
        'purchase_price,currency,source,notes');
});

test('commas and quotes in notes are escaped', () => {
    const csv = t.toCsv([{ set_code: 'sv3pt5', number: '1', variant: 'normal',
        quantity: 1, notes: 'he said "hi", loudly' }]);
    assert.ok(csv.includes('"he said ""hi"", loudly"'), csv);
});

test('capture bundle redacts authorization', () => {
    t.record('https://api.tcg.gg/pkmn/v1/redaction-probe', 'GET',
        { Authorization: 'Bearer SECRET', Accept: 'application/json' },
        [nested('e1', '25', 'Normal', 1)]);
    const bundle = t.captureBundle({});
    assert.ok(!bundle.includes('SECRET'), 'auth header must not be in the shareable dump');
    assert.ok(bundle.includes('application/json'));
});

console.log('\nvariant / identity collisions');

test('identity hidden under `variant` does not become the variant name', () => {
    t.record('https://api.tcg.gg/pkmn/v1/vk1', 'GET', {}, [{
        id: 'x1', quantity: 2, treatment: 'Reverse Holo',
        variant: { number: '25', name: 'Pikachu', set: { code: 'sv3pt5' } },
    }]);
    const r = t.extract();
    assert.strictEqual(r.rows.length, 1);
    assert.strictEqual(r.rows[0].number, '25');
    assert.strictEqual(r.rows[0].variant, 'reverse_holo', 'got: ' + r.rows[0].variant);
});

test('identity under `printing`, treatment named on the printing', () => {
    t.record('https://api.tcg.gg/pkmn/v1/vk2', 'GET', {}, [{
        id: 'x2', quantity: 1,
        printing: { number: '9', subType: 'Master Ball', set: { ptcgoCode: 'SSP' } },
    }]);
    const r = t.extract();
    assert.strictEqual(r.rows[0].variant, 'masterball_rh', 'got: ' + r.rows[0].variant);
});

test('flat entry still reads its own variant field', () => {
    t.record('https://api.tcg.gg/pkmn/v1/vk3', 'GET', {},
        [{ id: 'x3', setCode: 'sv8', number: '1', quantity: 1, variant: 'Holofoil' }]);
    assert.strictEqual(t.extract().rows[0].variant, 'holo');
});


console.log('\nthe real /page/u/<user>/collection shape');

// What recon confirmed the endpoint returns: no entry id of its own, a
// nested card, and the interchange strings on the card.
const apiEntry = (cardId, live, mass, variant, qty) => ({
    variant, quantity: qty,
    card: { id: cardId, name: "Team Rocket's Mewtwo ex",
            tcgLiveCode: live, tcgPlayerMassEntry: mass },
});

test('identity comes out of tcgLiveCode when no set/number field exists', () => {
    t.record('https://api.tcg.gg/pkmn/v1/page/u/ryan/collection?pageSize=60', 'GET', {}, {
        cards: [apiEntry('c1', "Team Rocket's Mewtwo ex DRI 81",
                         "Team Rocket's Mewtwo ex - 081/182 [DRI]", 'Normal', 2)],
        nextCursor: null,
    });
    const r = t.extract();
    assert.strictEqual(r.rows.length, 1);
    assert.strictEqual(r.rows[0].ptcgo_code, 'DRI');
    assert.strictEqual(r.rows[0].number, '81');
    assert.strictEqual(r.rows[0].quantity, 2);
});

test('mass-entry zero padding is stripped to match the catalog', () => {
    const id = t.identityFromCodes({ tcgPlayerMassEntry: 'Pikachu - 006/165 [MEW]' });
    assert.strictEqual(id.ptcgo, 'MEW');
    assert.strictEqual(id.number, '6');
});

test('(card.id, variant) is a stable identity — no id-less fallback', () => {
    const page = (qty) => ({ cards: [apiEntry('c1', 'Pikachu MEW 25', 'Pikachu - 025/165 [MEW]', 'Normal', qty)] });
    t.record('https://api.tcg.gg/pkmn/v1/page/u/ryan/collection?a=1', 'GET', {}, page(3));
    t.record('https://api.tcg.gg/pkmn/v1/page/u/ryan/collection?b=2', 'GET', {}, page(3));
    const r = t.extract();
    assert.strictEqual(r.rows.length, 1);
    assert.strictEqual(r.rows[0].quantity, 3, 'must not sum to 6');
    assert.strictEqual(r.idless, 0, 'the under-count heuristic must not be reached');
});

test('same card, two variants, stay two rows under the synthetic id', () => {
    t.record('https://api.tcg.gg/pkmn/v1/page/u/ryan/collection?c=3', 'GET', {}, {
        cards: [
            apiEntry('c1', 'Pikachu MEW 25', 'Pikachu - 025/165 [MEW]', 'Normal', 1),
            apiEntry('c1', 'Pikachu MEW 25', 'Pikachu - 025/165 [MEW]', 'Reverse Holo', 4),
        ],
    });
    const r = t.extract();
    assert.strictEqual(r.rows.length, 2);
    assert.strictEqual(r.idless, 0);
});

test('username is discovered from a captured route', () => {
    t.record('https://api.tcg.gg/pkmn/v1/page/u/someone/collection?pageSize=60', 'GET', {},
        { cards: [] });
    assert.strictEqual(t.discoverUsername(), 'someone');
});

console.log('\ninterchange outputs');

test('tcgplayer mass entry lines carry the quantity', () => {
    t.record('https://api.tcg.gg/pkmn/v1/page/u/ryan/collection?d=4', 'GET', {}, {
        cards: [apiEntry('c1', 'Pikachu MEW 25', 'Pikachu - 025/165 [MEW]', 'Normal', 3)],
    });
    const rows = t.extract().rows;
    assert.strictEqual(t.toMassEntry(rows), '3 Pikachu - 025/165 [MEW]\n');
    assert.strictEqual(t.toLiveList(rows), '3 Pikachu MEW 25\n');
});

test('no interchange output when pkmn.gg supplied no codes', () => {
    t.record('https://api.tcg.gg/pkmn/v1/plain', 'GET', {},
        [{ id: 'z', setCode: 'sv8', number: '1', quantity: 1, variant: 'Normal' }]);
    assert.strictEqual(t.toMassEntry(t.extract().rows), '');
});

test('interchange fields never leak into the CSV', () => {
    t.record('https://api.tcg.gg/pkmn/v1/page/u/ryan/collection?e=5', 'GET', {}, {
        cards: [apiEntry('c1', 'Pikachu MEW 25', 'Pikachu - 025/165 [MEW]', 'Normal', 1)],
    });
    const csv = t.toCsv(t.extract().rows);
    assert.ok(!csv.includes('_massEntry') && !csv.includes('[MEW]'), csv);
    assert.strictEqual(csv.trim().split('\n').length, 2);
});

console.log('\npkmn.gg variant slugs (from a real 2,383-row export)');

const v = (raw) => t.mapVariant(raw);

test('ball/energy pattern slugs map to the catalog _rh codes', () => {
    assert.strictEqual(v('pokeballpattern').code, 'pokeball_rh');
    assert.strictEqual(v('quickballpattern').code, 'quickball_rh');
    assert.strictEqual(v('duskballpattern').code, 'duskball_rh');
    assert.strictEqual(v('loveballpattern').code, 'loveball_rh');
    assert.strictEqual(v('friendballpattern').code, 'friendball_rh');
    assert.strictEqual(v('energypattern').code, 'energy_symbol_rh');
    assert.strictEqual(v('rocketpattern').code, 'team_rocket_rh');
});

test('holidaystamp is Trick or Trade, not the advent calendar', () => {
    // Verified against the live catalog: the cards pkmn.gg tags
    // `holidaystamp` carry stamp_trick_or_trade printings.
    assert.strictEqual(v('holidaystamp').code, 'stamp_trick_or_trade');
});

test('an unseen <x>pattern slug is derived, but only if the code exists', () => {
    assert.strictEqual(v('masterballpattern').code, 'masterball_rh');
    const made_up = v('sparklyballpattern');
    assert.strictEqual(made_up.code, 'sparklyballpattern');
    assert.strictEqual(made_up.known, false, 'must not invent sparklyball_rh');
});

test('ambiguous slugs are left alone to park, not guessed', () => {
    for (const raw of ['stamp', 'jumbo', 'holofoilalternate']) {
        const m = v(raw);
        assert.strictEqual(m.known, false, `${raw} must be reported as unknown`);
        assert.strictEqual(m.code, raw, `${raw} must pass through verbatim`);
    }
});

test('rarity names are not variants and stay unknown', () => {
    // variant.rs's KNOWN_VARIANTS lists these; data/variants.json does not,
    // and resolution goes against the latter.
    assert.strictEqual(v('illustration rare').known, false);
    assert.strictEqual(v('double rare').known, false);
});

test('every mapped target is a real catalog variant code', () => {
    // Guards against a typo in VARIANT_MAP silently producing a code that
    // can never resolve.
    for (const raw of ['normal', 'holo', 'reverse holo', 'pokeballpattern',
                       'holidaystamp', 'energypattern', 'masterballpattern',
                       '1st edition holo', 'cosmos', 'promo']) {
        const m = v(raw);
        assert.strictEqual(m.known, true, `${raw} → ${m.code} is not a catalog code`);
    }
});

console.log(`\n${pass} passed, ${fail} failed\n`);
process.exit(fail ? 1 : 0);
