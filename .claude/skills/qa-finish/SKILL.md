---
name: qa-finish
description: Run the QA finish-up workflow after implementing a feature. Proposes UI scenario tests, gathers real interaction hints from a container, and writes intent/hint/implementation files.
user-invocable: true
disable-model-invocation: false
argument-hint: "[issue-number] [instance-name]"
---

# QA Finish-Up Workflow

Run this after completing a feature implementation to produce UI scenario tests. This is a multi-phase workflow that produces intent YAML, hint YAML, and hand-written implementation TypeScript files under `tests/ui/`.

**CRITICAL: You MUST run the tests (Phase 6) before teardown. Every test must pass. Do NOT skip this phase or proceed to teardown with failing/unrun tests.**

## Phase 1: Analyze & Propose Intents

Use a subagent to analyze the changes made in this session (or for issue `$0` if provided). The subagent should:

1. Read the diff (`git diff main...HEAD` or recent changes) to understand what was built
2. Read existing intents in `tests/ui/intents/` to understand coverage and avoid duplicates
3. Propose 2-5 intent-based UI scenario tests that:
   - Cover the new feature's happy path(s)
   - Cover meaningful edge cases or interactions with existing features
   - Complement (not duplicate) existing test scenarios
   - Follow the user-centric description style: "I can...", "When I..."

Each proposed intent needs: a filename, a description, and related issue/PR numbers.

**Do NOT write files yet** -- present the proposals for review.

## Phase 2: Deploy & Walk the Feature

After intents are approved (or adjusted), deploy a test container:

```bash
bash deploy/setup.sh ${1:-qa-finish} --test
systemctl --user start pkdump-${1:-qa-finish}
sleep 5
PORT=$(podman port systemd-pkdump-${1:-qa-finish} 8080/tcp | grep -oP ':\K[0-9]+' | head -1)
```

Then walk each scenario yourself by:
1. Hitting the relevant API endpoints under `/api` with `curl -s` to understand responses
2. Checking the actual DOM elements, `data-testid`s, placeholders, and selectors rendered by the SvelteKit pages
3. Testing the exact user flow described in each intent
4. Noting any prerequisite data setup needed (e.g., creating a binder first)

The app is a SvelteKit single-page app served by an Axum backend. Pages render
client-side after hydration, so when probing with `curl` the initial HTML may be
a shell -- use the `/api` JSON endpoints to understand data shapes, and inspect
the Svelte route components under `frontend/src/routes/` for element IDs and
selectors.

Gather concrete information: `data-testid` values, button text, placeholder values, CSS selectors, API response shapes.

## Phase 3: Write Intent Files

Write YAML files to `tests/ui/intents/` following this exact format:

```yaml
# Scenario: Human-readable title
#
# Related:
#   issues: [<issue numbers>]
#   pull_requests: [<PR numbers>]
#
# Prerequisites: (optional, only if special data setup is needed)
#   <description>

description: >
  <User-centric description of the scenario. Describes what the user
  does and what they expect to see. Written in first person.>
```

## Phase 4: Write Hint Files

Write YAML files to `tests/ui/hints/` mirroring the intent filenames:

```yaml
start_page: <URL path, e.g., /binders>
involves:
  - "description of UI element 1"
  - "description of UI element 2"
fixture_data:
  key1: "value1"
  key2: "value2"
notes: >
  Step-by-step narrative of the exact interactions.
  Use real element names, IDs, and text from Phase 2.
```

## Phase 5: Write Implementation Files

Write TypeScript files to `tests/ui/implementations/` following this format:

```typescript
/**
 * Hand-written implementation for <intent_name>.
 *
 * <Brief description of what the test does.>
 */
import type { ReplayHarness } from '../replay';

export async function steps(h: ReplayHarness) {
  // Step description comment
  await h.<method>(args);
  // ...
  await h.screenshot('final_state');
}
```

### Available ReplayHarness methods:

**Navigation:** `navigate(path)`

**Interaction:**
- `click_by_text(text, { exact?: boolean })`
- `click_by_selector(selector)`
- `click_by_test_id(testId)`
- `fill_by_placeholder(placeholder, value)`
- `fill_by_selector(selector, value)`
- `select_by_label(selector, label)`
- `press_key(key, { selector?: string })`
- `scroll(direction)` -- `'up'` or `'down'`

**Waiting:**
- `wait_for_visible(selector, timeoutMs?)` -- default 5000
- `wait_for_hidden(selector, timeoutMs?)` -- default 5000
- `wait_for_text(text, timeoutMs?)` -- default 5000

**Assertions:**
- `assert_visible(selector)`
- `assert_hidden(selector)`
- `assert_text_present(text)`
- `assert_text_absent(text)`
- `assert_element_count(selector, count)`

**Evidence:** `screenshot(label)` -- always end with `screenshot('final_state')`

Every method is `async`; always `await` each call.

### Selector priority (most to least stable):
1. `data-testid` attribute
2. Unique text content (`click_by_text`)
3. Input placeholder (`fill_by_placeholder`)
4. Element ID (`#id`)
5. CSS selector (last resort)

### Rules:
- Every implementation must end with `await h.screenshot('final_state')`
- Use comments to explain each step's intent
- Prefer stable selectors (text > placeholder > id > css)
- If a modal/overlay appears asynchronously, add `wait_for_visible` before interacting
- SvelteKit hydrates and renders client-side -- after `navigate`, wait for a
  known element or text before interacting rather than assuming the page is ready
- Keep implementations minimal -- test one thing per scenario

## Phase 6: Run the Tests (MANDATORY)

**This phase is NOT optional. You MUST run the tests before teardown.**

After writing all implementation files, run the new tests against the still-running container:

```bash
cd tests/ui && npx playwright test --grep "<test_name_1>|<test_name_2>|..."
```

Build the `--grep` pattern from the filenames of the newly written scenarios (e.g. `--grep "sealed_open_product|sealed_open_no_contents"`). Pass the instance name through the environment if the harness needs it (e.g. `PKDUMP_INSTANCE=${1:-qa-finish}`).

If any test fails:
1. Read the error output to understand the failure
2. Fix the implementation file (wrong selector, missing wait, incorrect text match, etc.)
3. Re-run until all new tests pass
4. Repeat until you get 0 failures

**Do NOT proceed to teardown until every new test passes.** Untested test code is worse than no tests at all. If you skip this phase the tests will almost certainly have bugs (wrong button text, missing waits, bad selectors) that could have been caught.

## Phase 7: Teardown

```bash
bash deploy/teardown.sh ${1:-qa-finish} --purge
```

## Instance name

Use `$1` as the instance name if provided, otherwise default to `qa-finish`.
