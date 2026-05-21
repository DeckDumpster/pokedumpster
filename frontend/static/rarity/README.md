# Pokémon TCG rarity icons

Authentic rarity-symbol SVGs covering every rarity the pokemontcg.io
catalog currently uses (38 tiers, from Common through Hyper Rare and
the long tail of `Rare Holo EX/GX/V/VMAX/VSTAR`, `Rare Prism Star`,
`Mega Attack Rare`, etc.).

## Source

All icons are pulled directly from **pkmn.gg**:

    https://site.pkmn.gg/images/rarities/<Rarity Name>.svg

where `<Rarity Name>` is the URL-encoded pokemontcg.io rarity string —
e.g. `Special%20Illustration%20Rare.svg`.

Files are renamed to lowercase kebab-case so they're filesystem- and
URL-friendly (`common.svg`, `rare-holo.svg`, `special-illustration-rare.svg`).
One catalog rarity is spelled `MEGA_ATTACK_RARE` in pokemontcg.io and
served as `Mega Attack Rare.svg` on pkmn.gg — saved here as
`mega-attack-rare.svg`.

## Usage

`frontend/src/routes/collection/+page.svelte` maps each rarity string to
its kebab slug and renders `<img src="/rarity/{slug}.svg">` in the
collection table's Rarity column.

## Refreshing

```bash
# In a shell at the repo root:
RARITIES=("Common" "Uncommon" "Rare" ...)
for r in "${RARITIES[@]}"; do
    slug=$(echo -n "$r" | tr '[:upper:]' '[:lower:]' \
        | sed 's/ /-/g; s/[._]/-/g')
    enc=$(python3 -c "import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1]))" "$r")
    curl -sko "frontend/static/rarity/${slug}.svg" \
        "https://site.pkmn.gg/images/rarities/${enc}.svg"
done
```
