// The collection table's columns, as data — the counterpart of DeckDumpster's
// ALL_COLUMNS in collection.html, mapped to Pokémon fields (Cost is the attack
// energy costs, Energy the Pokémon's own type). `width` names the CSS custom
// property that declares the column's width, so the table's minimum is the sum
// of the columns that are really there.

export type ColumnKey =
	| 'qty'
	| 'name'
	| 'class'
	| 'etype'
	| 'cost'
	| 'rarity'
	| 'set'
	| 'number'
	| 'nm'
	| 'adj';

export type ColumnDef = { key: ColumnKey; label: string; width: string };

export const COLUMNS: ColumnDef[] = [
	{ key: 'qty', label: 'Qty', width: '--col-qty' },
	{ key: 'name', label: 'Card', width: '--col-name-min' },
	{ key: 'class', label: 'Type', width: '--col-class' },
	{ key: 'etype', label: 'Energy', width: '--col-etype' },
	{ key: 'cost', label: 'Cost', width: '--col-cost' },
	{ key: 'rarity', label: 'Rarity', width: '--col-rarity' },
	{ key: 'set', label: 'Set', width: '--col-set' },
	{ key: 'number', label: '#', width: '--col-number' },
	{ key: 'nm', label: 'Price', width: '--col-price' },
	{ key: 'adj', label: 'Adj. price', width: '--col-price' }
];

/** DeckDumpster's desktop default (Card, Type, Cost, Set, #, Price), mapped:
    Price is poke's two figures, NM and condition-adjusted. Qty stays on here
    though deck's is off, because a row is an aggregate of copies and the count
    is the one thing the row would otherwise not say. */
export const DESKTOP_COLUMNS: ColumnKey[] = [
	'qty',
	'name',
	'class',
	'cost',
	'set',
	'number',
	'nm',
	'adj'
];

/** DeckDumpster's phone default: Card and Type, nothing else. */
export const PHONE_COLUMNS: ColumnKey[] = ['name', 'class'];

/** The width at which the phone default applies (and the stylesheet's phone block). */
export const PHONE_QUERY = '(max-width: 540px)';
