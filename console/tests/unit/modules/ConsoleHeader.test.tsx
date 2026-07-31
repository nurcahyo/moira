// The authenticated console chrome.

import { describe, expect, test } from "bun:test";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { CONSOLE_CATALOG } from "@/lib/i18n";
import { CONSOLE_MESSAGE_KEYS, type ConsoleMessageKey } from "@/lib/i18n/keys";
import { ConsoleHeader } from "@/modules/chrome/ConsoleHeader";

const copy = (key: string): string => CONSOLE_CATALOG[key as ConsoleMessageKey].message;

describe("navigation", () => {
  test("the landmark has a catalog-resolved accessible name", () => {
    // `no-hardcoded-copy.test.tsx` forbids a literal `aria-label`, so this is
    // also the assertion that the name went through `t()` rather than being
    // typed into the JSX.
    render(<ConsoleHeader />);
    expect(
      screen.getByRole("navigation", { name: copy(CONSOLE_MESSAGE_KEYS.chrome_nav_label) }),
    ).toBeDefined();
  });

  test("/admins is reachable without typing the URL — the reason this component exists", () => {
    render(<ConsoleHeader />);
    const link = screen.getByRole("link", { name: copy(CONSOLE_MESSAGE_KEYS.chrome_nav_admins) });
    expect(link.getAttribute("href")).toBe("/admins");
  });
});

describe("sign-out is a POST, not a link", () => {
  test("it posts to Better Auth's endpoint and then navigates to /login", async () => {
    // A GET sign-out is triggerable by any third-party page that can make the
    // browser load a URL — the classic logout-CSRF. Asserting the METHOD is what
    // keeps an "improvement" to an `<a href>` from shipping.
    const calls: Array<{ url: string; method: string }> = [];
    const fetchImpl = (async (url: string, init?: RequestInit) => {
      calls.push({ url: String(url), method: init?.method ?? "GET" });
      return new Response("{}", { status: 200 });
    }) as unknown as typeof fetch;
    const visited: string[] = [];

    render(<ConsoleHeader fetchImpl={fetchImpl} navigate={(url) => visited.push(url)} />);
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.chrome_sign_out) }),
    );

    expect(calls).toEqual([{ url: "/api/auth/sign-out", method: "POST" }]);
    expect(visited).toEqual(["/login"]);
  });

  test("a refusal keeps the operator where they are and gives a client-side remedy", async () => {
    const fetchImpl = (async () =>
      new Response("{}", { status: 500 })) as unknown as typeof fetch;
    const visited: string[] = [];

    render(<ConsoleHeader fetchImpl={fetchImpl} navigate={(url) => visited.push(url)} />);
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.chrome_sign_out) }),
    );

    expect(screen.getByRole("alert").textContent).toBe(
      copy(CONSOLE_MESSAGE_KEYS.chrome_sign_out_failed),
    );
    // Navigating anyway would tell the operator they are signed out when the
    // cookie is still live.
    expect(visited).toEqual([]);
  });

  test("a thrown fetch does not echo the cause", async () => {
    const fetchImpl = (async () => {
      throw new Error("https://user:hunter2@console.internal unreachable");
    }) as unknown as typeof fetch;
    render(<ConsoleHeader fetchImpl={fetchImpl} navigate={() => {}} />);
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.chrome_sign_out) }),
    );
    expect(document.body.textContent).not.toContain("hunter2");
  });
});
