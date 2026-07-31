/**
 * Deterministic UI test replay — Playwright wrapper with zero Claude calls.
 *
 * Executes generated implementation scripts against a live instance using
 * stable selectors (text, placeholder, test ID, CSS). Every action captures
 * a screenshot and DOM snapshot for evidence.
 */

import { promises as fs } from "node:fs";
import * as path from "node:path";
import type { Page } from "@playwright/test";
import { EXTRACT_ELEMENTS_JS, type PageElement } from "./harness.js";

const log = {
  info: (...a: unknown[]) => console.log(...a),
};

/**
 * Per-action Playwright budget, and the default for the `wait_for_*` helpers.
 *
 * This was hard-coded to 500ms at every call site, which is below the floor
 * the app actually clears: card tiles load their art from images.pokemontcg.io,
 * so the renderer stays busy for a beat after first paint and a plain
 * `page.click` on the collection toolbar acks in 230-500ms — right on the old
 * wall, failing roughly one run in ten. 5s matches the value the qa-finish
 * skill has always documented for these helpers. Raising it only ever turns a
 * flaky pass into a stable one; a genuinely failing step still fails, it just
 * takes longer to say so.
 */
const ACTION_TIMEOUT_MS = 5_000;

/** One recorded replay step, with its captured evidence. */
export interface ReplayStep {
  number: number;
  action: string;
  detail: string;
  screenshot: string | null;
  dom_snapshot: PageElement[] | null;
  error: string | null;
}

/** Raised when a replay step fails. */
export class ReplayStepError extends Error {
  readonly step: ReplayStep;

  constructor(step: ReplayStep) {
    super(
      `Step ${step.number} (${step.action}: ${step.detail}) failed: ${step.error}`,
    );
    this.name = "ReplayStepError";
    this.step = step;
  }
}

/** Final result of a replay run. */
export interface ReplayResult {
  intent: string;
  status: "done" | "fail";
  steps: ReplayStep[];
  failure_step: number | null;
  error: string | null;
}

/** Execute a generated implementation with zero Claude calls. */
export class ReplayHarness {
  /** The underlying Playwright page — exposed for implementations that need it. */
  readonly page: Page;
  private readonly baseUrl: string;
  private readonly screenshotDir: string;
  private readonly scenarioName: string;
  private step = 0;
  private readonly steps: ReplayStep[] = [];

  constructor(
    page: Page,
    baseUrl: string,
    screenshotDir: string,
    scenarioName: string,
  ) {
    this.page = page;
    this.baseUrl = baseUrl;
    this.screenshotDir = screenshotDir;
    this.scenarioName = scenarioName;
  }

  // ── Navigation ─────────────────────────────────────────────────────

  async navigate(path: string): Promise<void> {
    this.record("navigate", path);
    await this.page.goto(`${this.baseUrl}${path}`, {
      waitUntil: "networkidle",
      timeout: 5_000,
    });
    await this.settle();
    await this.snap();
  }

  // ── Interaction ────────────────────────────────────────────────────

  async click_by_text(text: string, opts: { exact?: boolean } = {}): Promise<void> {
    this.record("click_by_text", text);
    await this.page
      .getByText(text, { exact: opts.exact ?? false })
      .first()
      .click({ timeout: ACTION_TIMEOUT_MS });
    await this.settle();
    await this.snap();
  }

  async click_by_selector(selector: string): Promise<void> {
    this.record("click_by_selector", selector);
    await this.page.click(selector, { timeout: ACTION_TIMEOUT_MS });
    await this.settle();
    await this.snap();
  }

  async click_by_test_id(testId: string): Promise<void> {
    this.record("click_by_test_id", testId);
    await this.page.getByTestId(testId).click({ timeout: ACTION_TIMEOUT_MS });
    await this.settle();
    await this.snap();
  }

  async fill_by_placeholder(placeholder: string, value: string): Promise<void> {
    this.record("fill_by_placeholder", `${placeholder}=${value}`);
    await this.page.getByPlaceholder(placeholder).fill(value, { timeout: ACTION_TIMEOUT_MS });
    await this.settle();
    await this.snap();
  }

  async fill_by_selector(selector: string, value: string): Promise<void> {
    this.record("fill_by_selector", `${selector}=${value}`);
    await this.page.fill(selector, value, { timeout: ACTION_TIMEOUT_MS });
    await this.settle();
    await this.snap();
  }

  /** Press a keyboard key, optionally targeting a specific element. */
  async press_key(key: string, opts: { selector?: string } = {}): Promise<void> {
    const target = opts.selector ?? "active element";
    this.record("press_key", `${key} on ${target}`);
    if (opts.selector) {
      await this.page.press(opts.selector, key, { timeout: ACTION_TIMEOUT_MS });
    } else {
      await this.page.keyboard.press(key);
    }
    await this.settle();
    await this.snap();
  }

  async set_input_files(selector: string, filePath: string): Promise<void> {
    this.record("set_input_files", `${selector} <- ${filePath}`);
    await this.page.setInputFiles(selector, filePath, { timeout: ACTION_TIMEOUT_MS });
    await this.settle();
    await this.snap();
  }

  async select_by_label(selector: string, label: string): Promise<void> {
    this.record("select_by_label", `${selector}=${label}`);
    await this.page.selectOption(selector, { label }, { timeout: ACTION_TIMEOUT_MS });
    await this.settle();
    await this.snap();
  }

  async scroll(direction: "up" | "down"): Promise<void> {
    this.record("scroll", direction);
    const delta = direction === "up" ? -500 : 500;
    await this.page.mouse.wheel(0, delta);
    await this.settle();
    await this.snap();
  }

  // ── Waiting ────────────────────────────────────────────────────────

  async wait_for_visible(selector: string, timeoutMs = ACTION_TIMEOUT_MS): Promise<void> {
    this.record("wait_for_visible", selector);
    await this.page.waitForSelector(selector, {
      state: "visible",
      timeout: timeoutMs,
    });
    await this.snap();
  }

  async wait_for_hidden(selector: string, timeoutMs = ACTION_TIMEOUT_MS): Promise<void> {
    this.record("wait_for_hidden", selector);
    await this.page.waitForSelector(selector, {
      state: "hidden",
      timeout: timeoutMs,
    });
    await this.snap();
  }

  async wait_for_text(text: string, timeoutMs = ACTION_TIMEOUT_MS): Promise<void> {
    this.record("wait_for_text", text);
    await this.page
      .getByText(text)
      .first()
      .waitFor({ state: "visible", timeout: timeoutMs });
    await this.snap();
  }

  // ── Assertions ─────────────────────────────────────────────────────

  async assert_visible(selector: string): Promise<void> {
    this.record("assert_visible", selector);
    const visible = await this.page.isVisible(selector, { timeout: 500 });
    if (!visible) {
      await this.fail(`Expected visible: ${selector}`);
    }
    await this.snap();
  }

  async assert_hidden(selector: string): Promise<void> {
    this.record("assert_hidden", selector);
    const hidden = await this.page.isHidden(selector, { timeout: 500 });
    if (!hidden) {
      await this.fail(`Expected hidden: ${selector}`);
    }
    await this.snap();
  }

  async assert_text_present(text: string): Promise<void> {
    this.record("assert_text_present", text);
    const count = await this.page.getByText(text).count();
    if (count === 0) {
      await this.fail(`Expected text present: ${text}`);
    }
    await this.snap();
  }

  async assert_text_absent(text: string): Promise<void> {
    this.record("assert_text_absent", text);
    const count = await this.page.getByText(text).count();
    if (count > 0) {
      await this.fail(
        `Expected text absent but found ${count} matches: ${text}`,
      );
    }
    await this.snap();
  }

  async assert_element_count(selector: string, count: number): Promise<void> {
    this.record("assert_element_count", `${selector} == ${count}`);
    const actual = await this.page.locator(selector).count();
    if (actual !== count) {
      await this.fail(
        `Expected ${count} elements for ${selector}, found ${actual}`,
      );
    }
    await this.snap();
  }

  // ── Evidence capture ───────────────────────────────────────────────

  /** Take an explicitly-labeled screenshot (in addition to automatic ones). */
  async screenshot(label: string): Promise<void> {
    const name = `${this.scenarioName}_${String(this.step).padStart(2, "0")}_${label}.png`;
    const filePath = path.join(this.screenshotDir, name);
    await this.page.screenshot({ path: filePath });
    log.info(`[${this.scenarioName}] Screenshot: ${name}`);
  }

  /** Capture the current interactive element list. */
  async snapshot_dom(label: string): Promise<PageElement[]> {
    const elements = (await this.page.evaluate(
      EXTRACT_ELEMENTS_JS,
    )) as PageElement[];
    const name = `${this.scenarioName}_${String(this.step).padStart(2, "0")}_${label}.json`;
    const filePath = path.join(this.screenshotDir, name);
    await fs.writeFile(filePath, JSON.stringify(elements, null, 2));
    log.info(`[${this.scenarioName}] DOM snapshot: ${name}`);
    return elements;
  }

  /** Build the final result after all steps have executed. */
  result(intentName: string): ReplayResult {
    const failed = this.steps.some((s) => s.error);
    const failureStepObj = this.steps.find((s) => s.error);
    const failureStep = failureStepObj ? failureStepObj.number : null;
    return {
      intent: intentName,
      status: failed ? "fail" : "done",
      steps: this.steps,
      failure_step: failureStep,
      error:
        failureStep !== null ? this.steps[failureStep - 1]!.error : null,
    };
  }

  // ── Internals ──────────────────────────────────────────────────────

  private record(action: string, detail: string): void {
    this.step += 1;
    const step: ReplayStep = {
      number: this.step,
      action,
      detail,
      screenshot: null,
      dom_snapshot: null,
      error: null,
    };
    this.steps.push(step);
    log.info(`[${this.scenarioName}] Step ${this.step}: ${action}(${detail})`);
  }

  /** Auto-snapshot after every action. */
  private async snap(): Promise<void> {
    const step = this.steps[this.steps.length - 1]!;
    const name = `${this.scenarioName}_${String(this.step).padStart(2, "0")}_${step.action}.png`;
    const filePath = path.join(this.screenshotDir, name);
    await this.page.screenshot({ path: filePath });
    step.screenshot = filePath;

    const elements = (await this.page.evaluate(
      EXTRACT_ELEMENTS_JS,
    )) as PageElement[];
    step.dom_snapshot = elements;
  }

  /** Wait for async page updates: one animation frame + networkidle. */
  private async settle(): Promise<void> {
    await this.page.waitForTimeout(50);
    try {
      await this.page.waitForLoadState("networkidle", { timeout: 500 });
    } catch {
      // ignore — page may stay busy with long-poll connections.
    }
  }

  /** Mark the current step as failed and raise. */
  private async fail(message: string): Promise<never> {
    const step = this.steps[this.steps.length - 1]!;
    step.error = message;
    await this.snap();
    throw new ReplayStepError(step);
  }
}
