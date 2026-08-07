# Visual regression — the safety net for the aesthetic overhaul

A pure-aesthetic change is exactly the case unit tests cannot see. `cargo test`
and `svelte-check` will happily pass while a token migration quietly loses a
border, collapses a grid, or drops a page's spacing by 4px on every row.

This suite screenshots **every route in the app at 1440 and 768** against an
isolated container instance seeded from the committed fixture, and diffs the
result against baselines in `baselines/`. A diff fails CI. Making it pass is an
explicit, reviewed act — never an incidental one.

## Running it

```bash
bash tests/visual/run.sh              # check against the baselines
bash tests/visual/run.sh --keep       # ...and leave the instance up to poke at
```

`run.sh` builds a `--test` instance (default name `visual`), waits for it,
runs the suite, and tears the instance down again. It refuses to run against
`prod`. Instances are isolated by name, so any other name is safe.

Already have an instance running? Skip the setup cost:

```bash
PKDUMP_BASE_URL=http://localhost:8099 bash tests/visual/playwright.sh
```

`deploy/ci.sh` runs the suite this way, against the container it already
started for the smoke test.

## Reviewing a diff

When a route fails, Playwright writes three images to `test-results/`:
the baseline, what it actually rendered, and a diff with the changed pixels
highlighted. Read them side by side:

```bash
(cd tests/visual && npm run report)
```

Then decide, and the decision is binary:

- **The change is unintended** → it is a regression. Fix the CSS. Do not
  approve.
- **The change is intended** → approve it, below, and commit the new PNGs in
  the *same commit* as the code that moved them. A baseline commit with no
  style change next to it is unreviewable.

## Approving

```bash
bash tests/visual/run.sh --update     # rewrite the baselines
git add tests/visual/baselines
```

Then — this is the part that matters — **read `git diff --stat` and account
for every file in it**. If you changed the collection page's card spacing and
17 baselines moved, that is the finding. `--update` is not a way to make the
suite quiet; it is a way to record a change you have already looked at.

Approving a *subset* is just Playwright's normal filtering:

```bash
PKDUMP_BASE_URL=http://localhost:8099 bash tests/visual/playwright.sh \
    --update-snapshots --project=desktop-1440 -g collection
```

## What is covered

`routes.json` is the manifest — data, not code. Every entry becomes one test
per viewport (24 routes × 2 = 48 baselines). Adding a `+page.svelte` to the
app without adding it here fails the `every route in the app has a baseline`
test, so the list cannot silently fall behind. A route that genuinely needs no
baseline goes in `unrepresented` with a reason.

Viewports live in `playwright.config.ts`:

| project | viewport | why |
| --- | --- | --- |
| `desktop-1440` | 1440×900 | the binder page at full width |
| `mobile-768` | 768×1024 | the breakpoint where `/browse` switches to its bottom sheet |

## What makes a failure mean something

`stabilize.ts` pins everything that could churn pixels for reasons unrelated to
the design system: a frozen clock, a stubbed backup-status response, card art
replaced by a flat placeholder (the fixture points at `images.pokemontcg.io`,
and a CDN is not the thing under test), no transitions, no caret, no
scrollbar. Its header explains each one.

The tolerances in `playwright.config.ts` are tight on purpose, and the numbers
are measured rather than guessed:

- Back-to-back runs against the same instance differ by **zero** pixels, so
  `maxDiffPixels: 100` is headroom for font antialiasing, not a working
  allowance.
- The budget is **absolute**, not `maxDiffPixelRatio`. These are full-page
  screenshots, so a ratio scales with page height: 0.2% of a 2300px binder
  page is 6600 pixels — enough to swallow every accent-coloured pixel on the
  route. Swapping the whole palette in the served CSS (`#e94560` → `#22cc88`)
  failed 22 of the 24 desktop routes at the absolute budget, and only 14 at
  the ratio.

If you find yourself wanting to loosen these, the honest move is a `mask`
entry in `routes.json` naming the specific element that is genuinely
non-deterministic — not a wider tolerance for the whole app.

## Baselines are host-specific

PNGs are rendered by the Chromium build in `~/.cache/ms-playwright` against the
host's fonts. A different box will differ in antialiasing. Regenerate through
`run.sh` on the box that runs CI — which today is the same box that runs
everything else.

## Not the intents harness

`tests/ui` is the other UI suite: Claude Vision driving real interactions
against `intents/*.yaml`. It needs an `ANTHROPIC_API_KEY` and is
non-deterministic by design, which is why it is not in `deploy/ci.sh`. This
suite is pure Playwright, offline, and deterministic — the two answer
different questions. That one asks "does it still work"; this one asks "does
it still look the way we agreed".
