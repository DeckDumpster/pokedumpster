# Browser tier (`tests/visual`)

Property assertions in a real browser, offline and deterministic, against a
throwaway `--test` container instance seeded from the committed fixture.
**There are no pixel baselines** (sp-tk1oyu): full-page screenshot comparison
caught no real defect and failed on renderer differences and intended UI
changes. `law-ui-changes-need-screenshots` still applies to PRs as review
evidence (before/after images from the builder), not as a gate.

```bash
bash tests/visual/run.sh              # stand up an instance, run, tear down
(cd tests/visual && npm run report)   # HTML report of a failure
PKDUMP_BASE_URL=http://localhost:8099 bash tests/visual/playwright.sh   # existing instance
```

## What it asserts

- `routes.spec.ts` — every route in `routes.json`, at both viewports, renders
  (status < 400), settles (two back-to-back captures agree on size) and does
  not overflow horizontally. A `+page.svelte` with no entry in `routes.json`
  (or in `unrepresented`, with a reason) fails the suite.
- `collection-window.spec.ts` — `/collection` renders a viewport-sized WINDOW
  of a 56,635-row result (synthesised by intercepting
  `/api/collection/search`), the document stays as tall as the whole result,
  scrolling moves the window instead of growing it, selection survives a row
  leaving the window, the sort stays server-side, and there is no pager.

| project | viewport | why |
| --- | --- | --- |
| `desktop-1440` | 1440×900 | the binder page at full width |
| `mobile-768` | 768×1024 | the breakpoint where `/browse` switches to its bottom sheet |

`stabilize.ts` pins the clock, stubs the backup-status response, replaces card
art with a flat placeholder, and disables transitions, so a failure is a
property of the page. If the size guard fires on a route it needs a `waitFor`
in `routes.json`, or its own reason found and fixed (pd-832w).
