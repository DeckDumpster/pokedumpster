/**
 * Conflict resolver — diagnoses replay failures via a single Claude call.
 *
 * Classifies failures as:
 * - test_failure:  implementation outdated, intent still valid → regenerate
 * - system_failure:  system doesn't satisfy intent → investigate regression
 * - environment_failure:  transient/config issue → fix environment, re-run
 */

import { execFileSync } from "node:child_process";
import { promises as fs } from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import Anthropic from "@anthropic-ai/sdk";
import type { ReplayStepError } from "./replay.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(__dirname, "..", "..");

export const MODEL = process.env.UI_TEST_MODEL ?? "claude-sonnet-4-6";

const DIAGNOSIS_PROMPT = `You are a test failure analyst for a UI testing framework.

You will receive:
1. An INTENT — an immutable description of what a feature should do.
2. An IMPLEMENTATION — a deterministic Playwright test script generated from that intent. \
It is a TypeScript module that exports an async \`steps(harness)\` function calling \
ReplayHarness methods.
3. A FAILURE — which step failed, the error message, and screenshots.

Classify this failure into exactly one category:

- **test_failure**: The implementation no longer matches the current system \
(e.g., a button was renamed, DOM structure changed, a selector is stale). \
The intent is still valid — the feature still works, but the test can't find it. \
Recommended action: regenerate the implementation from the intent.

- **system_failure**: The system genuinely does not satisfy the intent. \
This is a real regression — the feature is broken. \
Recommended action: investigate and fix the system.

- **environment_failure**: The test environment is misconfigured — missing test data, \
wrong instance, container not running, transient network error. \
Recommended action: fix the environment and re-run.

Respond with ONLY a JSON object (no markdown, no code fences):
{"category": "test_failure|system_failure|environment_failure", \
"explanation": "why this happened", \
"recommended_action": "what to do", \
"confidence": 0.0-1.0}`;

export type DiagnosisCategory =
  | "test_failure"
  | "system_failure"
  | "environment_failure";

/** Structured result of a conflict diagnosis. */
export interface ConflictDiagnosis {
  category: DiagnosisCategory;
  explanation: string;
  recommended_action: string;
  confidence: number;
}

const log = {
  error: (...a: unknown[]) => console.error(...a),
};

/** Diagnose replay failures with a single Claude call. */
export class ConflictResolver {
  private readonly client: Anthropic;

  constructor() {
    // Reads ANTHROPIC_API_KEY from process.env, as DeckDumpster does.
    this.client = new Anthropic();
  }

  /** Classify a replay failure. */
  async diagnose(
    intentPath: string,
    implPath: string,
    error: ReplayStepError,
  ): Promise<ConflictDiagnosis> {
    const intentText = await fs.readFile(intentPath, "utf8");
    const implText = await fs.readFile(implPath, "utf8");
    const sha = ConflictResolver.gitSha();

    // Build the user message with failure context.
    const failureInfo =
      `STEP ${error.step.number}: ${error.step.action}(${error.step.detail})\n` +
      `ERROR: ${error.step.error}\n` +
      `SYSTEM VERSION: ${sha}`;

    const content: Anthropic.ContentBlockParam[] = [
      {
        type: "text",
        text:
          `## INTENT\n\`\`\`yaml\n${intentText}\`\`\`\n\n` +
          `## IMPLEMENTATION\n\`\`\`typescript\n${implText}\`\`\`\n\n` +
          `## FAILURE\n${failureInfo}`,
      },
    ];

    // Attach the failure screenshot if available.
    if (error.step.screenshot) {
      try {
        const imgB64 = (await fs.readFile(error.step.screenshot)).toString(
          "base64",
        );
        content.push({
          type: "image",
          source: {
            type: "base64",
            media_type: "image/png",
            data: imgB64,
          },
        });
      } catch {
        // screenshot file missing — proceed without it.
      }
    }

    const response = await this.client.messages.create({
      model: MODEL,
      max_tokens: 1024,
      system: DIAGNOSIS_PROMPT,
      messages: [{ role: "user", content }],
    });

    // Parse the JSON response.
    const text = response.content
      .filter((b): b is Anthropic.TextBlock => b.type === "text")
      .map((b) => b.text)
      .join("");
    let data: Record<string, unknown>;
    try {
      data = JSON.parse(text) as Record<string, unknown>;
    } catch {
      log.error(`Failed to parse diagnosis response: ${text.slice(0, 500)}`);
      return {
        category: "environment_failure",
        explanation: `Could not parse diagnosis: ${text.slice(0, 200)}`,
        recommended_action: "Re-run diagnosis or inspect manually",
        confidence: 0.0,
      };
    }

    return {
      category: (data["category"] as DiagnosisCategory) ?? "environment_failure",
      explanation: (data["explanation"] as string) ?? "",
      recommended_action: (data["recommended_action"] as string) ?? "",
      confidence:
        data["confidence"] !== undefined ? Number(data["confidence"]) : 0.5,
    };
  }

  private static gitSha(): string {
    try {
      return execFileSync("git", ["rev-parse", "--short", "HEAD"], {
        cwd: REPO_ROOT,
        encoding: "utf8",
      }).trim();
    } catch {
      return "unknown";
    }
  }
}
