import fs from "node:fs";
import path from "node:path";
import type { Page } from "@playwright/test";

import { type Leak, type Needle, scanForLeaks } from "./secrets";

/** A blob of content the browser could observe, tagged with its origin. */
export interface CapturedBlob {
  readonly where: string;
  readonly content: string;
}

/** Content types whose bodies cannot plausibly carry a text secret. */
const BINARY_CONTENT_TYPE = /^(image|font|audio|video)\//i;

/**
 * Instrument a page so that *everything the browser sees* is captured:
 * every text-ish response body (HTML, RSC flight data, JS chunks, JSON, CSS),
 * every browser console message, and every uncaught page error.
 *
 * This is the "browser-visible response" half of the secret-leak gate. The
 * on-disk build scan below is the "client bundle" half.
 */
export function attachLeakTap(page: Page): {
  drain: () => Promise<CapturedBlob[]>;
} {
  const blobs: CapturedBlob[] = [];
  const pending: Promise<void>[] = [];

  page.on("response", (response) => {
    const contentType = response.headers()["content-type"] ?? "";
    if (BINARY_CONTENT_TYPE.test(contentType)) return;

    pending.push(
      (async () => {
        try {
          const body = await response.text();
          blobs.push({
            where: `response ${response.status()} ${response.url()}`,
            content: body,
          });
        } catch {
          // Redirects, aborted requests and 204s have no readable body.
          // Nothing observable by the browser, so nothing to scan.
        }
      })(),
    );
  });

  page.on("console", (message) => {
    blobs.push({
      where: `browser console [${message.type()}]`,
      content: message.text(),
    });
  });

  page.on("pageerror", (error) => {
    blobs.push({
      where: "uncaught page error",
      content: `${error.message}\n${error.stack ?? ""}`,
    });
  });

  return {
    async drain() {
      await Promise.all(pending);
      return blobs;
    },
  };
}

const STATIC_SCANNABLE = new Set([".js", ".mjs", ".cjs", ".css", ".json", ".map", ".txt", ".html"]);

/** Prerendered output that is shipped to the browser verbatim. */
const SERVER_SCANNABLE = new Set([".html", ".rsc", ".body"]);

function collectFiles(dir: string, extensions: ReadonlySet<string>, out: string[]): void {
  let entries: fs.Dirent[];
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true });
  } catch {
    return;
  }
  for (const entry of entries) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      collectFiles(full, extensions, out);
    } else if (entry.isFile() && extensions.has(path.extname(entry.name))) {
      out.push(full);
    }
  }
}

export interface BuildScanResult {
  readonly filesScanned: number;
  readonly leaks: Leak[];
}

/**
 * Scan the built output on disk.
 *
 * `.next/static/**` is served verbatim to the browser, so a secret inlined
 * there (or captured in a sourcemap) is leaked to every visitor. Prerendered
 * `.next/server/**\/*.html` / `.rsc` are the HTML/flight payloads shipped for
 * static routes.
 *
 * Server *code* under `.next/server` is deliberately NOT scanned: it legitimately
 * runs with secrets in scope and never reaches the browser. Scanning it would
 * make the gate noisy and it would be turned off, which is the real risk.
 */
export function scanBuildOutput(nextDir: string, needles: readonly Needle[]): BuildScanResult {
  const files: string[] = [];
  collectFiles(path.join(nextDir, "static"), STATIC_SCANNABLE, files);
  collectFiles(path.join(nextDir, "server"), SERVER_SCANNABLE, files);

  const leaks: Leak[] = [];
  for (const file of files) {
    let content: string;
    try {
      content = fs.readFileSync(file, "utf8");
    } catch {
      continue;
    }
    leaks.push(...scanForLeaks(path.relative(nextDir, file), content, needles));
  }

  return { filesScanned: files.length, leaks };
}
