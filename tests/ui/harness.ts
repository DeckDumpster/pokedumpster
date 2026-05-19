/**
 * UI testing harness — Claude Vision agent loop.
 *
 * Drives a Playwright browser by sending screenshots + interactive element lists
 * to Claude, which decides the next action at each step. Continues until Claude
 * declares the goal achieved (`done`) or unachievable (`fail`), or the step
 * limit is hit.
 *
 * Screenshots are saved after every action:
 * `{scenario}_{step:02d}_{label}.png`
 */

import { promises as fs } from "node:fs";
import * as path from "node:path";
import Anthropic from "@anthropic-ai/sdk";
import type { Page } from "@playwright/test";

export const MODEL = process.env.UI_TEST_MODEL ?? "claude-sonnet-4-6";
export const MAX_STEPS = 20;

const SYSTEM = `You are a UI testing agent. You interact with a web application to accomplish \
a goal by choosing browser actions one at a time.

Each turn you receive:
1. A screenshot of the current page
2. A numbered list of interactive elements visible on the page

Rules:
- Choose ONE action per turn using the provided tools.
- Refer to elements by their index number from the elements list.
- After filling an input, the page may update asynchronously (debounce). \
Check the next screenshot before acting on results.
- Use 'done' ONLY after you can visually confirm the goal is fully achieved.
- Use 'fail' if the goal clearly cannot be accomplished.
- Be methodical — don't skip steps or assume state you can't see.`;

const TOOLS: Anthropic.Tool[] = [
  {
    name: "navigate",
    description: "Go to a URL path on the site.",
    input_schema: {
      type: "object",
      properties: {
        path: {
          type: "string",
          description: "URL path, e.g. /sealed or /collection",
        },
      },
      required: ["path"],
    },
  },
  {
    name: "click",
    description: "Click an interactive element.",
    input_schema: {
      type: "object",
      properties: {
        element: {
          type: "integer",
          description: "Element index from the list",
        },
      },
      required: ["element"],
    },
  },
  {
    name: "fill",
    description: "Clear an input field and type new text.",
    input_schema: {
      type: "object",
      properties: {
        element: {
          type: "integer",
          description: "Element index (must be an input/textarea)",
        },
        value: {
          type: "string",
          description: "Text to type",
        },
      },
      required: ["element", "value"],
    },
  },
  {
    name: "select_option",
    description: "Choose an option from a <select> dropdown.",
    input_schema: {
      type: "object",
      properties: {
        element: {
          type: "integer",
          description: "Element index (must be a <select>)",
        },
        label: {
          type: "string",
          description: "Visible text of the option to select",
        },
      },
      required: ["element", "label"],
    },
  },
  {
    name: "press_key",
    description: "Press a keyboard key (e.g. Enter, Escape, Tab, ArrowDown).",
    input_schema: {
      type: "object",
      properties: {
        key: {
          type: "string",
          description: "Key name (e.g. Enter, Escape, Tab, ArrowDown)",
        },
        element: {
          type: "integer",
          description:
            "Optional element index to target. If omitted, presses on the focused element.",
        },
      },
      required: ["key"],
    },
  },
  {
    name: "scroll",
    description: "Scroll the page up or down.",
    input_schema: {
      type: "object",
      properties: {
        direction: {
          type: "string",
          enum: ["up", "down"],
        },
      },
      required: ["direction"],
    },
  },
  {
    name: "done",
    description: "The goal has been visually confirmed as achieved.",
    input_schema: {
      type: "object",
      properties: {
        summary: {
          type: "string",
          description: "What was accomplished",
        },
      },
      required: ["summary"],
    },
  },
  {
    name: "fail",
    description: "The goal cannot be achieved.",
    input_schema: {
      type: "object",
      properties: {
        reason: {
          type: "string",
          description: "Why the goal is unachievable",
        },
      },
      required: ["reason"],
    },
  },
];

/**
 * Injected into the page to enumerate visible interactive elements and tag
 * each one with data-uitest="N" so the harness can target them reliably.
 */
export const EXTRACT_ELEMENTS_JS = `
(() => {
  document.querySelectorAll('[data-uitest]').forEach(
    el => el.removeAttribute('data-uitest')
  );

  const sels = [
    'button', 'a[href]', 'input', 'select', 'textarea',
    '[role="button"]', '[role="link"]', '[role="tab"]',
    '[role="checkbox"]', '[role="radio"]', 'summary',
    '[onclick]', '[tabindex]:not([tabindex="-1"])',
  ].join(', ');

  const results = [];
  let idx = 0;

  const vw = window.innerWidth;
  const vh = window.innerHeight;

  function tag(el) {
    const rect = el.getBoundingClientRect();
    if (rect.width === 0 && rect.height === 0) return;
    if (rect.right < 0 || rect.left > vw || rect.bottom < 0 || rect.top > vh) return;
    const style = getComputedStyle(el);
    if (style.display === 'none' || style.visibility === 'hidden') return;

    el.setAttribute('data-uitest', String(idx));

    let value = null;
    if (el.tagName === 'SELECT' && el.selectedOptions.length)
      value = el.selectedOptions[0].text;
    else if (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA')
      value = (el.value || '').slice(0, 50) || null;

    // Build a CSS selector path for fallback targeting.
    function cssPath(e) {
      const parts = [];
      while (e && e.nodeType === 1) {
        let sel = e.tagName.toLowerCase();
        if (e.id) { parts.unshift('#' + e.id); break; }
        const sib = e.parentElement ? Array.from(e.parentElement.children).filter(
          c => c.tagName === e.tagName) : [];
        if (sib.length > 1) sel += ':nth-of-type(' + (sib.indexOf(e) + 1) + ')';
        parts.unshift(sel);
        e = e.parentElement;
      }
      return parts.join(' > ');
    }

    results.push({
      idx: idx,
      tag: el.tagName.toLowerCase(),
      text: (el.innerText || '').trim().slice(0, 80) || null,
      type: el.type || null,
      placeholder: el.placeholder || null,
      id: el.id || null,
      value: value,
      disabled: el.disabled || false,
      testid: el.getAttribute('data-testid') || null,
      aria_label: el.getAttribute('aria-label') || null,
      css_path: cssPath(el),
    });
    idx++;
  }

  // Pass 1: standard interactive elements.
  for (const el of document.querySelectorAll(sels)) tag(el);

  // Pass 2: elements with cursor:pointer that weren't already tagged
  // (catches dynamically-rendered list items with JS click handlers).
  for (const el of document.querySelectorAll('li, div, span, tr, td')) {
    if (el.hasAttribute('data-uitest')) continue;
    if (el.querySelector('[data-uitest]')) continue;  // skip parents of tagged children
    const style = getComputedStyle(el);
    if (style.cursor !== 'pointer') continue;
    tag(el);
  }

  return results;
})()
`;

/** An interactive element enumerated from the page. */
export interface PageElement {
  idx: number;
  tag: string;
  text: string | null;
  type: string | null;
  placeholder: string | null;
  id: string | null;
  value: string | null;
  disabled: boolean;
  testid: string | null;
  aria_label: string | null;
  css_path: string | null;
}

/** Hint metadata loaded from `hints/*.yaml`. */
export interface Hints {
  start_page?: string;
  involves?: string[];
  fixture_data?: Record<string, unknown>;
  notes?: string;
}

/** A stable selector strategy + value, derived from an enumerated element. */
export type StableSelector = [strategy: string, value: string];

/** One recorded step of an agent run. */
export interface HistoryEntry {
  step: number;
  action: string;
  input: Record<string, unknown>;
  result: string;
  stable_selector?: StableSelector | null;
  elements_snapshot?: PageElement[];
  page_url?: string;
}

/** Result of an agent run. */
export interface RunResult {
  status: "done" | "fail";
  summary?: string;
  reason?: string;
  steps: HistoryEntry[];
  done_summary?: string;
  final_elements?: PageElement[];
  final_url?: string;
}

interface Action {
  name: string;
  input: Record<string, unknown>;
}

const log = {
  info: (...a: unknown[]) => console.log(...a),
  warning: (...a: unknown[]) => console.warn(...a),
};

/** Drive a Playwright page toward a UX goal via Claude Vision agent loop. */
export class UIHarness {
  private readonly page: Page;
  private readonly baseUrl: string;
  private readonly screenshotDir: string;
  private readonly scenarioName: string;
  private readonly recording: boolean;
  private readonly hints: Hints | null;
  private readonly client: Anthropic;

  private step = 0;
  private history: HistoryEntry[] = [];
  private messages: Anthropic.MessageParam[] = [];
  private pendingToolResults: Anthropic.ToolResultBlockParam[] = [];
  private lastElements: PageElement[] = [];
  private lastDialogMessage: string | null = null;

  constructor(
    page: Page,
    baseUrl: string,
    screenshotDir: string,
    scenarioName: string,
    opts: { recording?: boolean; hints?: Hints | null } = {},
  ) {
    this.page = page;
    this.baseUrl = baseUrl;
    this.screenshotDir = screenshotDir;
    this.scenarioName = scenarioName;
    this.recording = opts.recording ?? false;
    this.hints = opts.hints ?? null;
    // Reads ANTHROPIC_API_KEY from process.env, as DeckDumpster does.
    this.client = new Anthropic();
  }

  // ── public API ────────────────────────────────────────────────────────

  /** Run the agent loop. Returns `{status, summary|reason, steps}`. */
  async run(goal: string, maxSteps: number = MAX_STEPS): Promise<RunResult> {
    // Build augmented goal with hints if available.
    let augmentedGoal = goal;
    if (this.hints) {
      const hintParts: string[] = [];
      if (this.hints.start_page) {
        hintParts.push(`Start on page: ${this.hints.start_page}`);
      }
      if (this.hints.involves) {
        hintParts.push(`Key UI elements: ${this.hints.involves.join(", ")}`);
      }
      if (this.hints.fixture_data) {
        const dataItems = Object.entries(this.hints.fixture_data).map(
          ([k, v]) => `${k}=${v}`,
        );
        hintParts.push(`Test data to use: ${dataItems.join(", ")}`);
      }
      if (this.hints.notes) {
        hintParts.push(`Notes: ${this.hints.notes}`);
      }
      if (hintParts.length) {
        augmentedGoal =
          goal + "\n\nHints:\n" + hintParts.map((h) => `- ${h}`).join("\n");
      }
    }

    log.info(`[${this.scenarioName}] Goal: ${augmentedGoal.trim().slice(0, 500)}`);

    // Auto-accept JS dialogs (confirm/alert) and provide a default for prompt().
    this.lastDialogMessage = null;
    this.page.on("dialog", (dialog) => {
      this.lastDialogMessage = dialog.message();
      if (dialog.type() === "prompt") {
        void dialog.accept(dialog.defaultValue() || "Test View");
      } else {
        void dialog.accept();
      }
    });

    // Navigate to start page (from hints) or homepage.
    let start = "/";
    if (this.hints?.start_page) {
      start = this.hints.start_page;
    }
    await this.page.goto(`${this.baseUrl}${start}`, { waitUntil: "networkidle" });

    for (let i = 0; i < maxSteps; i++) {
      const { screenshotB64, elements } = await this.observe();
      // Log element summary so callers can follow along.
      const elSummary = elements
        .slice(0, 20)
        .map(
          (e) =>
            `[${e.idx}] ${e.tag}` +
            (e.text ? ` "${e.text.slice(0, 40)}"` : "") +
            (e.placeholder ? ` placeholder="${e.placeholder}"` : "") +
            (e.value ? ` value="${e.value}"` : ""),
        )
        .join("; ");
      log.info(
        `[${this.scenarioName}] Step ${this.step} — ${elements.length} elements: ` +
          elSummary +
          (elements.length > 20 ? " ..." : ""),
      );
      const action = await this.decide(augmentedGoal, screenshotB64, elements);

      const name = action.name;
      const inputs = action.input;

      if (name === "done") {
        log.info(`[${this.scenarioName}] DONE: ${inputs["summary"]}`);
        await this.snap("done");
        const result: RunResult = {
          status: "done",
          summary: inputs["summary"] as string,
          steps: this.history,
        };
        if (this.recording) {
          result.done_summary = inputs["summary"] as string;
          result.final_elements = this.lastElements;
          result.final_url = this.page.url();
        }
        return result;
      }

      if (name === "fail") {
        log.warning(`[${this.scenarioName}] FAIL: ${inputs["reason"]}`);
        await this.snap("fail");
        return {
          status: "fail",
          reason: inputs["reason"] as string,
          steps: this.history,
        };
      }

      log.info(
        `[${this.scenarioName}] Step ${this.step} → ${name}(${JSON.stringify(inputs)})`,
      );
      const result = await this.execute(name, inputs);
      log.info(`[${this.scenarioName}] Step ${this.step} result: ${result}`);
      const entry: HistoryEntry = {
        step: this.step,
        action: name,
        input: inputs,
        result,
      };
      if (this.recording) {
        const elementIdx = inputs["element"];
        entry.stable_selector =
          elementIdx !== undefined
            ? this.stableSelector(elementIdx as number)
            : null;
        entry.elements_snapshot = this.lastElements;
        entry.page_url = this.page.url();
      }
      this.history.push(entry);
      await this.settle();
    }

    await this.snap("max_steps");
    log.warning(`[${this.scenarioName}] Exceeded ${maxSteps} steps`);
    return {
      status: "fail",
      reason: `Exceeded ${maxSteps} steps`,
      steps: this.history,
    };
  }

  // ── internals ─────────────────────────────────────────────────────────

  /** Screenshot the page and extract interactive elements. */
  private async observe(): Promise<{
    screenshotB64: string;
    elements: PageElement[];
  }> {
    this.step += 1;
    const filePath = await this.snap(`step_${String(this.step).padStart(2, "0")}`);
    const screenshotB64 = (await fs.readFile(filePath)).toString("base64");
    const elements = (await this.page.evaluate(
      EXTRACT_ELEMENTS_JS,
    )) as PageElement[];
    this.lastElements = elements;
    return { screenshotB64, elements };
  }

  /** Send current state to Claude and get back a tool-use action. */
  private async decide(
    goal: string,
    screenshotB64: string,
    elements: PageElement[],
  ): Promise<Action> {
    const elementsText = UIHarness.formatElements(elements);

    const userContent: Anthropic.ContentBlockParam[] = [];

    // Include pending tool_result(s) from previous turn.
    if (this.pendingToolResults.length) {
      userContent.push(...this.pendingToolResults);
      this.pendingToolResults = [];
    }

    userContent.push({
      type: "text",
      text:
        this.messages.length === 0
          ? `Goal: ${goal}\n\nCurrent page elements:\n${elementsText}`
          : `Current page elements:\n${elementsText}`,
    });
    userContent.push({
      type: "image",
      source: {
        type: "base64",
        media_type: "image/png",
        data: screenshotB64,
      },
    });

    this.messages.push({ role: "user", content: userContent });

    const response = await this.client.messages.create({
      model: MODEL,
      max_tokens: 1024,
      system: SYSTEM,
      tools: TOOLS,
      messages: this.messages,
    });

    // Append the full assistant response to maintain conversation history.
    this.messages.push({ role: "assistant", content: response.content });

    // Log any reasoning text before the tool call.
    const reasoning = response.content
      .filter((b): b is Anthropic.TextBlock => b.type === "text")
      .map((b) => b.text)
      .join("");
    if (reasoning) {
      log.info(`[${this.scenarioName}] Reasoning: ${reasoning.trim().slice(0, 500)}`);
    }

    // Collect tool_results for ALL tool_use blocks in this response.
    // The API requires every tool_use to have a matching tool_result.
    const toolUses = response.content.filter(
      (b): b is Anthropic.ToolUseBlock => b.type === "tool_use",
    );
    if (toolUses.length) {
      // Stash results for all tool_uses — execute only the first.
      this.pendingToolResults = toolUses.map((b) => ({
        type: "tool_result",
        tool_use_id: b.id,
        content: "OK",
      }));
      const first = toolUses[0]!;
      return {
        name: first.name,
        input: first.input as Record<string, unknown>,
      };
    }

    // No tool call — treat as failure.
    return {
      name: "fail",
      input: { reason: `No action chosen: ${reasoning.slice(0, 200)}` },
    };
  }

  /** Dispatch an action to Playwright. Returns a short result string. */
  private async execute(
    action: string,
    inputs: Record<string, unknown>,
  ): Promise<string> {
    // Re-tag elements right before acting — DOM may have re-rendered
    // since observe() (e.g. async search results replacing innerHTML).
    await this.page.evaluate(EXTRACT_ELEMENTS_JS);

    const timeout = 5_000; // 5s max per action
    try {
      if (action === "navigate") {
        await this.page.goto(`${this.baseUrl}${inputs["path"]}`, {
          waitUntil: "networkidle",
          timeout,
        });
        return "navigated";
      }

      if (action === "click") {
        const selector = `[data-uitest="${inputs["element"]}"]`;
        await this.page.click(selector, { timeout });
        return "clicked";
      }

      if (action === "fill") {
        const selector = `[data-uitest="${inputs["element"]}"]`;
        await this.page.fill(selector, inputs["value"] as string, { timeout });
        return "filled";
      }

      if (action === "select_option") {
        const selector = `[data-uitest="${inputs["element"]}"]`;
        await this.page.selectOption(
          selector,
          { label: inputs["label"] as string },
          { timeout },
        );
        return "selected";
      }

      if (action === "press_key") {
        const key = inputs["key"] as string;
        if ("element" in inputs) {
          const selector = `[data-uitest="${inputs["element"]}"]`;
          await this.page.press(selector, key, { timeout });
        } else {
          await this.page.keyboard.press(key);
        }
        return `pressed ${key}`;
      }

      if (action === "scroll") {
        const delta = inputs["direction"] === "up" ? -500 : 500;
        await this.page.mouse.wheel(0, delta);
        return "scrolled";
      }

      return `unknown action: ${action}`;
    } catch (e) {
      return `error: ${e instanceof Error ? e.message : String(e)}`;
    }
  }

  /** Wait briefly for async page updates to land. */
  private async settle(): Promise<void> {
    await this.page.waitForTimeout(500);
    try {
      await this.page.waitForLoadState("networkidle", { timeout: 3000 });
    } catch {
      // ignore — page may stay busy with long-poll connections.
    }
  }

  /** Take a viewport screenshot and return the file path. */
  private async snap(label: string): Promise<string> {
    const name = `${this.scenarioName}_${String(this.step).padStart(2, "0")}_${label}.png`;
    const filePath = path.join(this.screenshotDir, name);
    await this.page.screenshot({ path: filePath });
    return filePath;
  }

  /**
   * Return the best stable selector for an element by index.
   *
   * Returns a tuple of [strategy, value] where strategy is one of:
   * test_id, text, placeholder, selector, aria_label.
   */
  private stableSelector(idx: number): StableSelector {
    const el = this.lastElements.find((e) => e.idx === idx);
    if (el === undefined) {
      return ["selector", `[data-uitest="${idx}"]`];
    }

    if (el.testid) {
      return ["test_id", el.testid];
    }

    // Unique text — only usable if single-line and unique on the page.
    const text = el.text;
    if (text && !text.includes("\n")) {
      const sameText = this.lastElements.filter((e) => e.text === text);
      if (sameText.length === 1) {
        return ["text", text];
      }
    }

    if (el.placeholder) {
      return ["placeholder", el.placeholder];
    }

    if (el.id) {
      return ["selector", `#${el.id}`];
    }

    if (el.aria_label) {
      return ["aria_label", el.aria_label];
    }

    if (el.css_path) {
      return ["selector", el.css_path];
    }

    return ["selector", `[data-uitest="${idx}"]`];
  }

  private static formatElements(elements: PageElement[]): string {
    const lines: string[] = [];
    for (const el of elements) {
      const parts: string[] = [`[${el.idx}]`, el.tag];
      if (el.id) parts.push(`#${el.id}`);
      if (el.type) parts.push(`type=${el.type}`);
      if (el.text) parts.push(`"${el.text}"`);
      if (el.placeholder) parts.push(`placeholder="${el.placeholder}"`);
      if (el.value) parts.push(`value="${el.value}"`);
      if (el.disabled) parts.push("(disabled)");
      lines.push(parts.join(" "));
    }
    return lines.join("\n");
  }
}
