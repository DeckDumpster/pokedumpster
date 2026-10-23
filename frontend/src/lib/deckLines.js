// Deck page model: owned copies grouped by supertype, identical copies folded
// into one line. A line still names the exact printing@condition it holds (for
// its link) but exposes no set / collector number / condition text.

/**
 * @typedef {{ id: number, printing_id: string, condition: string, set_code: string,
 *   number: string, name: string, supertype: string | null, types: string | null,
 *   hp: number | null, image_large: string | null, image_small: string | null }} DeckCopy
 * @typedef {{ key: string, name: string, quantity: number, copyIds: number[],
 *   types: string[], hp: number | null, href: string, image: string | null }} DeckLine
 * @typedef {{ supertype: string, count: number, lines: DeckLine[] }} DeckGroup
 */

const GROUP_ORDER = ['Pokémon', 'Trainer', 'Energy'];

/** @param {string | null} raw @returns {string[]} */
function parseTypes(raw) {
	if (!raw) return [];
	const v = JSON.parse(raw);
	return Array.isArray(v) ? v.map(String) : [];
}

/** The card page opened on this exact printing at this exact condition.
 *  @param {Pick<DeckCopy, 'set_code' | 'number' | 'printing_id' | 'condition'>} c */
export function lineHref(c) {
	const q = new URLSearchParams({ printing: c.printing_id, condition: c.condition });
	return `/card/${c.set_code}/${c.number}?${q}`;
}

/** @param {DeckCopy[]} copies @returns {DeckGroup[]} */
export function groupDeck(copies) {
	/** @type {Map<string, DeckGroup>} */
	const groups = new Map();
	/** @type {Map<string, DeckLine>} */
	const lines = new Map();
	for (const c of copies) {
		const supertype = c.supertype ?? 'Other';
		let g = groups.get(supertype);
		if (!g) groups.set(supertype, (g = { supertype, count: 0, lines: [] }));
		const key = `${c.printing_id}@${c.condition}`;
		let l = lines.get(key);
		if (!l) {
			l = {
				key,
				name: c.name,
				quantity: 0,
				copyIds: [],
				types: parseTypes(c.types),
				hp: c.hp,
				href: lineHref(c),
				image: c.image_large ?? c.image_small
			};
			lines.set(key, l);
			g.lines.push(l);
		}
		l.quantity++;
		l.copyIds.push(c.id);
		g.count++;
	}
	/** @param {string} s */
	const rank = (s) => {
		const i = GROUP_ORDER.indexOf(s);
		return i < 0 ? GROUP_ORDER.length : i;
	};
	const out = [...groups.values()].sort((a, b) => rank(a.supertype) - rank(b.supertype));
	for (const g of out) g.lines.sort((a, b) => a.name.localeCompare(b.name));
	return out;
}
