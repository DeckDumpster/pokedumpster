/**
 * Implementation generator — runs the Claude harness once in recording mode
 * and emits a deterministic ReplayHarness script.
 *
 * Usage (via the intents runner, see test_scenarios.ts):
 *     UI_TEST_GENERATE=<intent_name>  npx playwright test
 *     UI_TEST_GENERATE_MISSING=1      npx playwright test
 *
 * The emitted module is a TypeScript file under implementations/ that exports
 * `async function steps(h: ReplayHarness)`.
 */

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { promises as fs } from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import * as YAML from "yaml";
import {
  UIHarness,
  type Hints,
  type HistoryEntry,
  type PageElement,
  type StableSelector,
} from "./harness.js";
import type { Page } from "@playwright/test";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

export const INTENTS_DIR = path.join(__dirname, "intents");
export const HINTS_DIR = path.join(__dirname, "hints");
export const IMPLEMENTATIONS_DIR = path.join(__dirname, "implementations");

const REPO_ROOT = path.resolve(__dirname, "..", "..");

/**
 * Maps URL paths to SvelteKit route source files for data-testid insertion.
 *
 * PokeDumpster's frontend is SvelteKit with file-based routing, so a URL path
 * like `/collection` is served by `routes/collection/+page.svelte`. Prefix
 * entries (trailing `/`) cover parameterized routes (e.g. `/card/:set/:cn`).
 */
const FRONTEND_ROUTES = path.join(REPO_ROOT, "frontend", "src", "routes");

const _PAGE_TO_SOURCE: Record<string, string> = {
  "/": "+page.svelte",
  "/collection": "collection/+page.svelte",
  "/browse": "browse/+page.svelte",
  "/sealed": "sealed/+page.svelte",
  "/recent": "recent/+page.svelte",
  "/decks": "decks/+page.svelte",
  "/binders": "binders/+page.svelte",
  "/orders/": "orders/[id]/+page.svelte",
  "/orders": "orders/+page.svelte",
  "/batches": "batches/+page.svelte",
  "/wishlist": "wishlist/+page.svelte",
  "/ingest": "ingest/+page.svelte",
  "/card/": "card/[set]/[cn]/+page.svelte",
};

const log = {
  info: (...a: unknown[]) => console.log(...a),
  warning: (...a: unknown[]) => console.warn(...a),
  error: (...a: unknown[]) => console.error(...a),
};

/** Return the current short git SHA, or 'unknown'. */
function gitSha(): string {
  try {
    return execFileSync("git", ["rev-parse", "--short", "HEAD"], {
      cwd: REPO_ROOT,
      encoding: "utf8",
    }).trim();
  } catch {
    return "unknown";
  }
}

/** SHA-256 of the intent file contents (first 16 hex chars). */
async function intentHash(intentPath: string): Promise<string> {
  const data = await fs.readFile(intentPath);
  return createHash("sha256").update(data).digest("hex").slice(0, 16);
}

/** Extract the path portion from a full URL relative to base. */
function urlPath(fullUrl: string, baseUrl: string): string {
  if (fullUrl.startsWith(baseUrl)) {
    const p = fullUrl.slice(baseUrl.length);
    return p ? p : "/";
  }
  return "/";
}

/** Map a page URL to its SvelteKit route source file. */
async function findSourceForUrl(
  pageUrl: string,
  baseUrl: string,
): Promise<string | null> {
  let p = urlPath(pageUrl, baseUrl);
  // Strip query string.
  p = p.split("?")[0]!;
  let filename = _PAGE_TO_SOURCE[p];
  if (!filename) {
    // Try prefix matching for parameterized routes (e.g. /card/:set/:cn).
    for (const [prefix, fname] of Object.entries(_PAGE_TO_SOURCE)) {
      if (prefix.endsWith("/") && p.startsWith(prefix)) {
        filename = fname;
        break;
      }
    }
  }
  if (filename) {
    const sourcePath = path.join(FRONTEND_ROUTES, filename);
    try {
      await fs.access(sourcePath);
      return sourcePath;
    } catch {
      return null;
    }
  }
  return null;
}

/** Generate a data-testid value for an element. */
function makeTestid(scenarioName: string, step: number, element: PageElement): string {
  const tag = element.tag || "el";
  const text = element.text || "";
  // Build a readable slug from the text.
  const slug = text
    .toLowerCase()
    .trim()
    .replace(/[^a-z0-9]+/g, "-")
    .slice(0, 30)
    .replace(/^-+|-+$/g, "");
  if (slug) {
    return `${tag}-${slug}`;
  }
  return `${scenarioName}-step${step}-${tag}`;
}

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * Add a data-testid attribute to the element in the source file.
 *
 * Tries to find the element by id, then by tag+text pattern.
 * Returns true if the attribute was added.
 */
async function addTestidToSource(
  sourcePath: string,
  element: PageElement,
  testid: string,
): Promise<boolean> {
  let content = await fs.readFile(sourcePath, "utf8");
  const tag = element.tag || "";
  const elId = element.id;
  const text = element.text || "";
  const base = path.basename(sourcePath);

  // Already has this testid?
  if (content.includes(`data-testid="${testid}"`)) {
    return true;
  }

  // Strategy 1: find by id attribute.
  if (elId) {
    const pattern = `id="${elId}"`;
    if (content.includes(pattern)) {
      const replacement = `id="${elId}" data-testid="${testid}"`;
      content = content.replace(pattern, replacement);
      await fs.writeFile(sourcePath, content);
      log.info(`Added data-testid="${testid}" to ${base} (by id=${elId})`);
      return true;
    }
  }

  // Strategy 2: find opening tag with matching text nearby.
  if (text && tag) {
    const escapedText = escapeRegExp(text.slice(0, 40));
    const pattern = new RegExp(`(<${tag}\\b[^>]*>)\\s*${escapedText}`, "i");
    const match = pattern.exec(content);
    if (match) {
      const openingTag = match[1]!;
      if (!openingTag.includes("data-testid")) {
        const newTag =
          openingTag.slice(0, -1) + ` data-testid="${testid}">`;
        content = content.replace(openingTag, newTag);
        await fs.writeFile(sourcePath, content);
        log.info(
          `Added data-testid="${testid}" to ${base} (by tag=${tag} text=${text.slice(0, 30)})`,
        );
        return true;
      }
    }
  }

  log.warning(
    `Could not add data-testid="${testid}" to ${base} — element not found in source`,
  );
  return false;
}

/**
 * Escape a string for use in a TypeScript double-quoted string literal.
 *
 * Also truncates multiline innerText to the first meaningful line,
 * since innerText often includes child element text.
 */
function escapeForTs(value: string): string {
  // Take only the first line of multiline text.
  const firstLine = value.split("\n")[0]!.trim();
  return firstLine.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
}

/** Translate a recorded harness step into a ReplayHarness call. */
function translateStep(step: HistoryEntry): string | null {
  const action = step.action;
  const inputs = step.input;
  const selectorInfo = step.stable_selector ?? null;

  if (action === "navigate") {
    const p = escapeForTs(inputs["path"] as string);
    return `  await h.navigate("${p}");`;
  }

  if (action === "scroll") {
    const direction = inputs["direction"] as string;
    return `  await h.scroll("${direction}");`;
  }

  if (action === "click") {
    if (!selectorInfo) {
      return `  // WARNING: no stable selector for click step ${step.step}`;
    }
    const [strategy, value] = selectorInfo;
    const escaped = escapeForTs(value);
    if (strategy === "test_id") {
      return `  await h.click_by_test_id("${escaped}");`;
    }
    if (strategy === "text") {
      return `  await h.click_by_text("${escaped}");`;
    }
    if (strategy === "aria_label") {
      return `  await h.click_by_selector('[aria-label="${escaped}"]');`;
    }
    if (strategy === "placeholder") {
      return `  await h.click_by_selector('[placeholder="${escaped}"]');`;
    }
    return `  await h.click_by_selector("${escaped}");`;
  }

  if (action === "fill") {
    const value = escapeForTs(inputs["value"] as string);
    if (!selectorInfo) {
      return `  // WARNING: no stable selector for fill step ${step.step}`;
    }
    const [strategy, selValue] = selectorInfo;
    const escaped = escapeForTs(selValue);
    if (strategy === "placeholder") {
      return `  await h.fill_by_placeholder("${escaped}", "${value}");`;
    }
    if (strategy === "test_id") {
      return `  await h.fill_by_selector('[data-testid="${escaped}"]', "${value}");`;
    }
    return `  await h.fill_by_selector("${escaped}", "${value}");`;
  }

  if (action === "select_option") {
    const label = escapeForTs(inputs["label"] as string);
    if (!selectorInfo) {
      return `  // WARNING: no stable selector for select step ${step.step}`;
    }
    const [strategy, selValue] = selectorInfo;
    const escaped = escapeForTs(selValue);
    if (strategy === "test_id") {
      return `  await h.select_by_label('[data-testid="${escaped}"]', "${label}");`;
    }
    if (strategy === "selector") {
      return `  await h.select_by_label("${escaped}", "${label}");`;
    }
    // text/placeholder/aria_label are not valid CSS selectors for <select> —
    // fall back to the element's CSS path if available.
    const elementIdx = inputs["element"];
    const elements = step.elements_snapshot ?? [];
    const el = elements.find((e) => e.idx === elementIdx);
    if (el && el.css_path) {
      const css = escapeForTs(el.css_path);
      return `  await h.select_by_label("${css}", "${label}");`;
    }
    return `  // WARNING: no CSS selector for select step ${step.step} (strategy=${strategy})`;
  }

  if (action === "press_key") {
    const key = escapeForTs(inputs["key"] as string);
    if (selectorInfo && "element" in inputs) {
      const [strategy, selValue] = selectorInfo;
      const escaped = escapeForTs(selValue);
      if (strategy === "placeholder") {
        return `  await h.press_key("${key}", { selector: '[placeholder="${escaped}"]' });`;
      }
      if (strategy === "test_id") {
        return `  await h.press_key("${key}", { selector: '[data-testid="${escaped}"]' });`;
      }
      if (strategy === "selector") {
        return `  await h.press_key("${key}", { selector: "${escaped}" });`;
      }
    }
    return `  await h.press_key("${key}");`;
  }

  return null;
}

/** Load hints for an intent if they exist. */
export async function loadHints(intentPath: string): Promise<Hints | null> {
  const rel = path.relative(INTENTS_DIR, intentPath);
  const hintsPath = path.join(HINTS_DIR, rel);
  try {
    const text = await fs.readFile(hintsPath, "utf8");
    log.info(`Loaded hints from: ${hintsPath}`);
    return YAML.parse(text) as Hints;
  } catch {
    return null;
  }
}

/**
 * Run the harness in recording mode and emit an implementation module.
 *
 * Returns the path to the generated implementation file.
 */
export async function generate(
  intentPath: string,
  page: Page,
  baseUrl: string,
  screenshotDir: string,
): Promise<string> {
  const scenarioName = path.basename(intentPath, path.extname(intentPath));
  const intentText = await fs.readFile(intentPath, "utf8");
  const intent = YAML.parse(intentText) as { description: string };
  const goal = intent.description;
  const hints = await loadHints(intentPath);

  log.info(`Generating implementation for intent: ${scenarioName}`);

  // Run the harness in recording mode.
  const harness = new UIHarness(page, baseUrl, screenshotDir, scenarioName, {
    recording: true,
    hints,
  });
  // Generation is a one-time cost — allow more steps than normal replay.
  const result = await harness.run(goal, 35);

  if (result.status !== "done") {
    throw new Error(
      `Harness failed for ${scenarioName}: ${result.reason ?? "unknown"}`,
    );
  }

  // Translate recorded steps into ReplayHarness calls.
  // Also detect async element appearances (modals, overlays) between steps
  // and emit wait_for_visible calls.
  const lines: string[] = [];
  let prevElements: PageElement[] | null = null;
  for (const step of result.steps) {
    let selectorInfo: StableSelector | null = step.stable_selector ?? null;
    const curElements = step.elements_snapshot ?? [];

    // Detect async elements that appeared since the previous step.
    if (prevElements !== null) {
      const prevIds = new Set(
        prevElements.filter((e) => e.id).map((e) => e.id),
      );
      for (const el of curElements) {
        const elId = el.id ?? "";
        if (!elId) continue;
        const isAsyncEl = ["modal", "overlay", "dialog", "popup"].some((kw) =>
          elId.toLowerCase().includes(kw),
        );
        if (isAsyncEl && !prevIds.has(elId)) {
          lines.push(`  await h.wait_for_visible("#${elId}", 10_000);`);
        }
      }
    }
    prevElements = curElements;

    // If the stable selector fell back to data-uitest (ephemeral), try to
    // add a data-testid to the frontend source.
    if (
      selectorInfo &&
      selectorInfo[0] === "selector" &&
      selectorInfo[1].includes("data-uitest")
    ) {
      const elementIdx = step.input["element"];
      const el = curElements.find((e) => e.idx === elementIdx);
      if (el) {
        const pageUrl = step.page_url ?? "";
        const sourcePath = await findSourceForUrl(pageUrl, baseUrl);
        if (sourcePath) {
          const testid = makeTestid(scenarioName, step.step, el);
          if (await addTestidToSource(sourcePath, el, testid)) {
            selectorInfo = ["test_id", testid];
            step.stable_selector = selectorInfo;
          }
        }
      }
    }

    const line = translateStep(step);
    if (line) {
      lines.push(line);
    }
  }

  // Add a final screenshot.
  lines.push('  await h.screenshot("final_state");');

  // Build the module.
  const timestamp = new Date().toISOString().replace(/\.\d+Z$/, "Z");
  const sha = gitSha();
  const ihash = await intentHash(intentPath);

  const moduleContent =
    `/**\n` +
    ` * Generated from intent: ${scenarioName}\n` +
    ` * Generated at: ${timestamp}\n` +
    ` * System version: ${sha}\n` +
    ` * Intent hash: ${ihash}\n` +
    ` */\n\n` +
    `import type { ReplayHarness } from "../replay.js";\n\n` +
    `export async function steps(h: ReplayHarness): Promise<void> {\n` +
    lines.join("\n") +
    `\n}\n`;

  // Write the implementation file, mirroring the intent directory structure.
  const rel = path
    .relative(INTENTS_DIR, intentPath)
    .replace(/\.ya?ml$/, ".ts");
  const implPath = path.join(IMPLEMENTATIONS_DIR, rel);

  // Ensure parent directories exist.
  await fs.mkdir(path.dirname(implPath), { recursive: true });

  await fs.writeFile(implPath, moduleContent);
  log.info(`Generated implementation: ${implPath}`);

  return implPath;
}
