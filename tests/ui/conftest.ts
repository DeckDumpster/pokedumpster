/**
 * Shared helpers for UI scenario tests — snapshot/restore + instance discovery.
 *
 * These tests run against a live container instance (same as integration tests).
 * The instance must already be running:
 *
 *     bash deploy/setup.sh ui-test
 *     systemctl --user start pkdump-ui-test
 *
 * Or pass an existing instance via UI_TEST_INSTANCE / UI_TEST_BASE_URL.
 *
 * This is the TypeScript port of DeckDumpster's pytest `conftest.py`. pytest
 * fixtures become plain async helpers consumed by the intents runner
 * (`test_scenarios.ts`): `discoverContainer`/`discoverBaseUrl` for setup,
 * `snapshotDb`/`restoreDb` for per-test DB isolation, `makeScreenshotDir` for
 * evidence output.
 */

import { execFile } from "node:child_process";
import { promises as fs } from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);

const __dirname = path.dirname(fileURLToPath(import.meta.url));

const log = {
  info: (...a: unknown[]) => console.log(...a),
};

// ── Configuration (via env, mirroring DeckDumpster's pytest options) ──────
//
//   UI_TEST_INSTANCE   — container instance name (default: integration-test)
//   UI_TEST_BASE_URL   — base URL to test against directly, skipping
//                        container discovery (e.g. http://localhost:8080)

export const INSTANCE_NAME = process.env.UI_TEST_INSTANCE ?? "integration-test";
export const EXPLICIT_BASE_URL = process.env.UI_TEST_BASE_URL ?? null;

// DB paths inside the container. PokeDumpster sets PKDUMP_HOME=/data, so the
// per-user collection DB and the shared catalog DB both live under /data.
// The shared DB is ATTACHed read-only to the user DB at runtime.
const CONTAINER_DB = "/data/collection.sqlite";
const CONTAINER_DB_BACKUP = "/data/collection.sqlite.bak";
const CONTAINER_SHARED_DB = "/data/shared.sqlite";
const CONTAINER_SHARED_DB_BACKUP = "/data/shared.sqlite.bak";

// Backup uses sqlite3.backup() for a safe snapshot with open server
// connections. Restore uses cp — fast and safe because the server opens fresh
// connections (with fresh ATTACH) per request, so no stale handles.
// Both collection.sqlite and shared.sqlite are backed up and restored, since
// tests may write to shared tables (e.g. price seeding).
const BACKUP_CMD =
  `python3 -c "import sqlite3, os; ` +
  `s=sqlite3.connect('${CONTAINER_DB}'); ` +
  `d=sqlite3.connect('${CONTAINER_DB_BACKUP}'); ` +
  `s.backup(d); s.close(); d.close(); ` +
  `p='${CONTAINER_SHARED_DB}'; ` +
  `b='${CONTAINER_SHARED_DB_BACKUP}'; ` +
  `exec('if os.path.exists(p):\\n s=sqlite3.connect(p)\\n d=sqlite3.connect(b)\\n s.backup(d)\\n s.close()\\n d.close()')` +
  `"`;

const RESTORE_CMD =
  `cp ${CONTAINER_DB_BACKUP} ${CONTAINER_DB}` +
  ` && { [ -f ${CONTAINER_SHARED_DB_BACKUP} ] && cp ${CONTAINER_SHARED_DB_BACKUP} ${CONTAINER_SHARED_DB}; true; }`;

/** Skip-style error: surfaces a reason when the environment is not ready. */
export class SkipError extends Error {
  constructor(reason: string) {
    super(reason);
    this.name = "SkipError";
  }
}

/**
 * Resolve the container name for an instance.
 *
 * Tries both container name patterns: `systemd-pkdump-{name}` (Linux systemd
 * Quadlet) and `pkdump-{name}` (macOS / plain `podman run`).
 * Returns null when no matching container exists.
 */
export async function discoverContainer(
  instanceName: string = INSTANCE_NAME,
): Promise<string | null> {
  for (const candidate of [
    `systemd-pkdump-${instanceName}`,
    `pkdump-${instanceName}`,
  ]) {
    try {
      await execFileAsync("podman", ["container", "exists", candidate]);
      return candidate;
    } catch {
      // not found under this pattern — try the next.
    }
  }
  return null;
}

/**
 * Discover the base URL for the running server.
 *
 * Set UI_TEST_BASE_URL to point at a local dev server directly (e.g.
 * http://localhost:8080), or UI_TEST_INSTANCE for Podman container discovery.
 * Throws SkipError when nothing is reachable.
 */
export async function discoverBaseUrl(
  containerName: string | null,
): Promise<string> {
  if (EXPLICIT_BASE_URL) {
    if (!(await urlResponds(EXPLICIT_BASE_URL))) {
      throw new SkipError(`Server at ${EXPLICIT_BASE_URL} not responding`);
    }
    return EXPLICIT_BASE_URL;
  }

  if (containerName === null) {
    throw new SkipError(
      `No container found for instance '${INSTANCE_NAME}'. ` +
        `Start it with: bash deploy/setup.sh ${INSTANCE_NAME} && ` +
        `systemctl --user start pkdump-${INSTANCE_NAME}`,
    );
  }

  // The container exposes the server on port 8080 internally.
  let portLine: string;
  try {
    const { stdout } = await execFileAsync("podman", [
      "port",
      containerName,
      "8080/tcp",
    ]);
    portLine = stdout.trim();
  } catch {
    throw new SkipError(`Could not query port for '${containerName}'`);
  }

  if (!portLine) {
    throw new SkipError(`Could not determine port for '${containerName}'`);
  }

  const port = portLine.split(":").pop();
  const url = `http://localhost:${port}`;

  if (!(await urlResponds(url))) {
    throw new SkipError(`Instance at ${url} not responding`);
  }
  return url;
}

/** Probe a URL's root — returns true if the server answers. */
async function urlResponds(baseUrl: string): Promise<boolean> {
  try {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 5_000);
    try {
      // Accept self-signed certs (mirrors DeckDumpster's relaxed SSL context).
      const prev = process.env["NODE_TLS_REJECT_UNAUTHORIZED"];
      process.env["NODE_TLS_REJECT_UNAUTHORIZED"] = "0";
      try {
        await fetch(`${baseUrl}/`, { signal: controller.signal });
      } finally {
        if (prev === undefined) {
          delete process.env["NODE_TLS_REJECT_UNAUTHORIZED"];
        } else {
          process.env["NODE_TLS_REJECT_UNAUTHORIZED"] = prev;
        }
      }
      return true;
    } finally {
      clearTimeout(timer);
    }
  } catch {
    return false;
  }
}

/**
 * Create a one-time DB snapshot inside the container for per-test restore.
 *
 * No-op (returns null) when running against a local server (no container).
 */
export async function snapshotDb(
  containerName: string | null,
): Promise<string | null> {
  if (containerName === null) {
    return null;
  }
  log.info(`Creating DB snapshot in container ${containerName}`);
  await execFileAsync("podman", [
    "exec",
    containerName,
    "bash",
    "-c",
    BACKUP_CMD,
  ]);
  return containerName;
}

/**
 * Restore the DB to its snapshot state. Call after each test.
 *
 * No-op when running against a local server (containerName is null).
 */
export async function restoreDb(containerName: string | null): Promise<void> {
  if (containerName === null) {
    return;
  }
  log.info(`Restoring DB snapshot in container ${containerName}`);
  await execFileAsync("podman", [
    "exec",
    containerName,
    "bash",
    "-c",
    RESTORE_CMD,
  ]);
}

/** Remove the snapshot backup file from the container (cleanup at session end). */
export async function cleanupSnapshot(
  containerName: string | null,
): Promise<void> {
  if (containerName === null) {
    return;
  }
  try {
    await execFileAsync("podman", [
      "exec",
      containerName,
      "rm",
      "-f",
      CONTAINER_DB_BACKUP,
      CONTAINER_SHARED_DB_BACKUP,
    ]);
  } catch {
    // best-effort cleanup.
  }
}

/** Create a timestamped screenshot output directory and return its path. */
export async function makeScreenshotDir(): Promise<string> {
  const now = new Date();
  const stamp =
    now.getFullYear().toString() +
    String(now.getMonth() + 1).padStart(2, "0") +
    String(now.getDate()).padStart(2, "0") +
    "_" +
    String(now.getHours()).padStart(2, "0") +
    String(now.getMinutes()).padStart(2, "0") +
    String(now.getSeconds()).padStart(2, "0");
  const dir = path.join(__dirname, "..", "..", "screenshots", "ui", stamp);
  await fs.mkdir(dir, { recursive: true });
  return dir;
}
