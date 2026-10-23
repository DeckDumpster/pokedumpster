// @ts-nocheck
import test from 'node:test';
import assert from 'node:assert/strict';
import { groupDeck } from '../src/lib/deckLines.js';

const copy = (id, name, supertype, over = {}) => ({
	id, printing_id: `p-${name}`, condition: 'NM', set_code: 'sv1', number: String(id), name,
	supertype, types: null, hp: null, image_large: `/img/${id}.png`, image_small: null, ...over
});

const copies = [
	copy(1, 'Fire Energy', 'Energy', { types: '["Fire"]' }),
	copy(2, 'Charizard ex', 'Pokémon', { types: '["Fire"]', hp: 330 }),
	copy(3, 'Fire Energy', 'Energy', { types: '["Fire"]', id: 3 }),
	copy(4, 'Boss’s Orders', 'Trainer'),
	copy(5, 'Fire Energy', 'Energy', { types: '["Fire"]', condition: 'LP' })
];

test('sections are Pokémon, Trainer, Energy with counts', () => {
	const g = groupDeck(copies);
	assert.deepEqual(g.map((x) => [x.supertype, x.count]), [['Pokémon', 1], ['Trainer', 1], ['Energy', 3]]);
});

test('identical printing@condition copies fold into one line with a quantity', () => {
	const energy = groupDeck(copies)[2];
	assert.equal(energy.lines.length, 2);
	const nm = energy.lines.find((l) => l.href.includes('condition=NM'));
	assert.equal(nm.quantity, 2);
	assert.deepEqual(nm.copyIds, [1, 3]);
});

test('lines carry per-line attributes and a printing@condition link', () => {
	const [poke] = groupDeck(copies);
	const l = poke.lines[0];
	assert.equal(l.hp, 330);
	assert.deepEqual(l.types, ['Fire']);
	assert.equal(l.href, '/card/sv1/2?printing=p-Charizard+ex&condition=NM');
	assert.equal(l.image, '/img/2.png');
});

test('lines expose no set, collector number or condition text', () => {
	for (const g of groupDeck(copies)) for (const l of g.lines) {
		assert.deepEqual(Object.keys(l).sort(), ['copyIds', 'href', 'hp', 'image', 'key', 'name', 'quantity', 'types'].sort());
	}
});
