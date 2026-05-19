/**
 * Data-driven UI test runner — the intents architecture.
 *
 * Discovers YAML intent files in tests/ui/intents/ and runs each one. If a
 * generated implementation exists in tests/ui/implementations/, the test uses
 * deterministic replay (zero Claude calls). Otherwise it falls back to the
 * Claude Vision harness.
 *
 * The TypeScript port of DeckDumpster's `test_scenarios.py`. pytest CLI flags
 * become environment variables:
 *
 *     # Normal run — uses implementations where available ($0)
 *     npx playwright test
 *
 *     # Generate a specific implementation (~$0.15)
 *     UI_GENERATE=<name> npx playwright test
 *
 *     # Generate all missing implementations (~$4 one-time)
 *     UI_GENERATE_MISSING=1 npx playwright test
 *
 *     # Diagnose failures (~$0.05 per failure)
 *     UI_DIAGNOSE=1 npx playwright test
 */

import { test } from "@playwright/test";
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import YAML from "yaml";

import {
  cleanupSnapshot,
  discoverBaseUrl,
  discoverContainer,
  makeScreenshotDir,
  restoreDb,
  SkipError,
  snapshotDb,
} from "./conftest.js";
import { generate, loadHints } from "./generator.js";
import { UIHarness } from "./harness.js";
import { ReplayHarness, ReplayStepError } from "./replay.js";
import { ConflictResolver } from "./resolver.js";

const here = path.dirname(fileURLToPath(import.meta.url));
const INTENTS_DIR = path.join(here, "intents");
const IMPLEMENTATIONS_DIR = path.join(here, "implementations");

// DeckDumpster's pytest CLI flags become environment variables.
const GENERATE_NAME = process.env["UI_GENERATE"] ?? null;
const GENERATE_MISSING = process.env["UI_GENERATE_MISSING"] === "1";
const REGENERATE_NAME = process.env["UI_REGENERATE"] ?? null;
const REGENERATE_ALL = process.env["UI_REGENERATE_ALL"] === "1";
const DIAGNOSE = process.env["UI_DIAGNOSE"] === "1";
const INTENTS_ONLY = process.env["UI_INTENTS_ONLY"] === "1";

interface DiscoveredTest {
  name: string;
  intentPath: string;
  implPath: string | null;
}

/** Every intent, paired with its implementation if one exists. */
function discoverTests(): DiscoveredTest[] {
  if (!fs.existsSync(INTENTS_DIR)) {
    return [];
  }
  const found: DiscoveredTest[] = [];
  const walk = (dir: string): void => {
    const entries = fs
      .readdirSync(dir, { withFileTypes: true })
      .sort((a, b) => a.name.localeCompare(b.name));
    for (const entry of entries) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        walk(full);
      } else if (entry.isFile() && entry.name.endsWith(".yaml")) {
        // Mirror directory structure: intents/recents/foo.yaml →
        // implementations/recents/foo.ts.
        const rel = path
          .relative(INTENTS_DIR, full)
          .replace(/\.yaml$/, ".ts");
        const impl = path.join(IMPLEMENTATIONS_DIR, rel);
        found.push({
          name: path.basename(entry.name, ".yaml"),
          intentPath: full,
          implPath: fs.existsSync(impl) ? impl : null,
        });
      }
    }
  };
  walk(INTENTS_DIR);
  return found;
}

// ── Session state, established in beforeAll ────────────────────────────────

let baseUrl = "";
let containerName: string | null = null;
let screenshotDir = "";
let skipReason: string | null = null;

test.beforeAll(async () => {
  try {
    containerName = await discoverContainer();
    baseUrl = await discoverBaseUrl(containerName);
    screenshotDir = await makeScreenshotDir();
    await snapshotDb(containerName);
  } catch (e) {
    if (e instanceof SkipError) {
      skipReason = e.message;
      return;
    }
    throw e;
  }
});

test.afterEach(async () => {
  if (skipReason !== null) {
    return;
  }
  // Restore the DB to its snapshot so each intent starts from clean fixture
  // state (mirrors DeckDumpster's per-test conftest restore).
  await restoreDb(containerName);
});

test.afterAll(async () => {
  await cleanupSnapshot(containerName);
});

// ── One test per discovered intent ─────────────────────────────────────────

for (const t of discoverTests()) {
  test(t.name, async ({ page }) => {
    test.skip(skipReason !== null, skipReason ?? "");

    // ── Generation mode ────────────────────────────────────────────────
    const generateThis =
      (GENERATE_NAME !== null && GENERATE_NAME === t.name) ||
      (REGENERATE_NAME !== null && REGENERATE_NAME === t.name) ||
      REGENERATE_ALL ||
      (GENERATE_MISSING && t.implPath === null);
    if (generateThis) {
      const impl = await generate(t.intentPath, page, baseUrl, screenshotDir);
      console.log(`Generated: ${impl}`);
      return;
    }
    if (GENERATE_NAME !== null && GENERATE_NAME !== t.name) {
      test.skip(true, `Not generating: ${t.name} (UI_GENERATE=${GENERATE_NAME})`);
      return;
    }
    if (REGENERATE_NAME !== null && REGENERATE_NAME !== t.name) {
      test.skip(true, `Not regenerating: ${t.name} (UI_REGENERATE=${REGENERATE_NAME})`);
      return;
    }

    // ── Replay mode (default when an implementation exists) ─────────────
    if (t.implPath !== null && !INTENTS_ONLY) {
      // Auto-accept JS dialogs (prompt() defaults to "Test View").
      page.on("dialog", (dialog) => {
        if (dialog.type() === "prompt") {
          void dialog.accept(dialog.defaultValue() || "Test View");
        } else {
          void dialog.accept();
        }
      });

      const harness = new ReplayHarness(page, baseUrl, screenshotDir, t.name);
      // The vision harness always navigates before recording, so generated
      // implementations never include the initial navigate(). Replay it from
      // the hint file's start_page.
      const hints = await loadHints(t.intentPath);
      await harness.navigate(hints?.start_page ?? "/");

      const mod = (await import(pathToFileURL(t.implPath).href)) as {
        steps: (h: ReplayHarness) => Promise<void>;
      };
      try {
        await mod.steps(harness);
      } catch (e) {
        if (e instanceof ReplayStepError) {
          if (DIAGNOSE) {
            await runDiagnosis(t, e);
          }
          throw new Error(
            `Replay failed at step ${e.step.number} ` +
              `(${e.step.action}: ${e.step.detail}): ${e.step.error}`,
          );
        }
        throw e;
      }
      return;
    }

    // ── Harness mode (fallback when no implementation exists) ───────────
    if (t.implPath === null) {
      console.warn(
        `[${t.name}] No implementation found — running Claude harness ` +
          `(expensive). Generate with: UI_GENERATE=${t.name}`,
      );
    }

    const intent = YAML.parse(fs.readFileSync(t.intentPath, "utf8")) as {
      description: string;
    };
    const hints = await loadHints(t.intentPath);
    const harness = new UIHarness(page, baseUrl, screenshotDir, t.name, {
      hints,
    });
    const result = await harness.run(intent.description);
    if (result.status !== "done") {
      throw new Error(
        `Scenario failed: ${result.reason ?? "unknown"}\n` +
          `Steps taken: ${result.steps.length}`,
      );
    }
  });
}

/** Run the conflict resolver and print its diagnosis of a replay failure. */
async function runDiagnosis(
  t: DiscoveredTest,
  error: ReplayStepError,
): Promise<void> {
  try {
    const resolver = new ConflictResolver();
    const d = await resolver.diagnose(t.intentPath, t.implPath ?? "", error);
    console.warn(
      `\n╔══ CONFLICT DIAGNOSIS: ${t.name} ══╗\n` +
        `║ Category: ${d.category}\n` +
        `║ Confidence: ${(d.confidence * 100).toFixed(0)}%\n` +
        `║ Explanation: ${d.explanation}\n` +
        `║ Recommended: ${d.recommended_action}\n` +
        `╚══════════════════════════════╝`,
    );
  } catch (e) {
    console.error(`Diagnosis failed: ${String(e)}`);
  }
}
