# Pokémon TCG energy-type icons

Authentic energy symbols for the 11 modern types (Grass, Fire, Water,
Lightning, Psychic, Fighting, Darkness, Metal, Fairy, Dragon, Colorless).

## Source

All icons from **pkmn.gg**:

    https://site.pkmn.gg/assets/type-energy-<lowercase>/dark/default-v{5,6,7}.png

The version suffix varies per type — most are `v5`; Fire and Water are
`v6`; Dragon is `v7`. They were probed and saved here as `<type>.png`.

Used by the collection table's Cost column (attack-energy pips) and
elsewhere where an energy symbol is needed.

## Refreshing

```bash
for t in grass fire water lightning psychic fighting darkness metal fairy dragon colorless; do
    # try v5, fall back to v6, then v7
    for v in 5 6 7; do
        u="https://site.pkmn.gg/assets/type-energy-${t}/dark/default-v${v}.png"
        if curl -sko "frontend/static/energy/${t}.png" -w "%{http_code}" "$u" | grep -q 200; then
            break
        fi
    done
done
```
