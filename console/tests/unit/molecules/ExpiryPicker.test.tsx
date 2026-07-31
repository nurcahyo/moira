// The invitation lifetime picker, and the two bounds it exists to respect.

import { describe, expect, test } from "bun:test";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import {
  EXPIRY_CANDIDATE_SECONDS,
  ExpiryPicker,
  expiryOptionLabel,
  expiryOptions,
} from "@/components/molecules/ExpiryPicker";
import { CONSOLE_CATALOG } from "@/lib/i18n";
import { CONSOLE_MESSAGE_KEYS, type ConsoleMessageKey } from "@/lib/i18n/keys";
import {
  MAX_INVITE_EXPIRY_SECONDS,
  MIN_INVITE_EXPIRY_SECONDS,
  isAcceptableInviteLifetime,
} from "@/lib/invite-bounds";

const copy = (key: string): string => CONSOLE_CATALOG[key as ConsoleMessageKey].message;

describe("the bounds are Moira's, not this component's", () => {
  test("they match the constants Moira pins in its own test", () => {
    // `src/domain/identity.rs` asserts `MAX_INVITE_EXPIRY_SECONDS == 259_200`
    // ("72 hours") and `MIN_INVITE_EXPIRY_SECONDS == 60`. Mirrored numbers drift
    // silently unless something says what they are mirrors OF.
    expect(MAX_INVITE_EXPIRY_SECONDS).toBe(259_200);
    expect(MIN_INVITE_EXPIRY_SECONDS).toBe(60);
  });

  test("both ends are refusals, and the floor is not implied by the cap", () => {
    // Plan 09 names the cap and never mentions the floor. Moira refuses below it
    // with a DIFFERENT code (`invalid_request`, not
    // `admin_invite_expiry_too_long`), so a UI that only knew about the cap
    // would render the floor's refusal as a generic validation failure.
    expect(isAcceptableInviteLifetime(MIN_INVITE_EXPIRY_SECONDS)).toBe(true);
    expect(isAcceptableInviteLifetime(MIN_INVITE_EXPIRY_SECONDS - 1)).toBe(false);
    expect(isAcceptableInviteLifetime(MAX_INVITE_EXPIRY_SECONDS)).toBe(true);
    expect(isAcceptableInviteLifetime(MAX_INVITE_EXPIRY_SECONDS + 1)).toBe(false);
    expect(isAcceptableInviteLifetime(3600.5)).toBe(false);
  });
});

describe("the offered options", () => {
  test("the filter removed nothing — a silently shrunk picker is the failure", () => {
    // `expiryOptions()` filters `CANDIDATE_SECONDS` through the bounds. That is
    // what makes an out-of-range option impossible; it is ALSO what would make
    // one disappear without a word. Asserting the two lists are equal is what
    // turns a silent shrink into a red test.
    expect(expiryOptions()).toEqual([...EXPIRY_CANDIDATE_SECONDS]);
    expect(expiryOptions().length).toBeGreaterThanOrEqual(4);
  });

  test("every offered option is one Moira accepts", () => {
    for (const seconds of expiryOptions()) {
      expect(isAcceptableInviteLifetime(seconds), `${seconds}s is outside Moira's bounds`).toBe(
        true,
      );
    }
  });

  test("the longest option is exactly the cap, so the cap is reachable", () => {
    expect(Math.max(...expiryOptions())).toBe(MAX_INVITE_EXPIRY_SECONDS);
  });

  test("one hour has its own key — `1 hours` would read as a bug", () => {
    expect(expiryOptionLabel(3600)).toBe(copy(CONSOLE_MESSAGE_KEYS.expiry_option_one_hour));
    expect(expiryOptionLabel(72 * 3600)).toBe(
      copy(CONSOLE_MESSAGE_KEYS.expiry_option_hours).replace("{hours}", "72"),
    );
  });
});

describe("rendering", () => {
  test("the label and hint come from the catalog, and the hint is described-by", () => {
    render(<ExpiryPicker seconds={86_400} onChange={() => {}} />);
    const select = screen.getByRole("combobox", {
      name: new RegExp(copy(CONSOLE_MESSAGE_KEYS.expiry_label)),
    });
    expect(select).toBeDefined();
    const describedBy = select.getAttribute("aria-describedby");
    expect(describedBy).not.toBeNull();
    expect(document.getElementById(describedBy!)?.textContent).toBe(
      copy(CONSOLE_MESSAGE_KEYS.expiry_hint),
    );
  });

  test("choosing an option reports seconds, not a label", async () => {
    const chosen: number[] = [];
    render(<ExpiryPicker seconds={86_400} onChange={(seconds) => chosen.push(seconds)} />);
    await userEvent.selectOptions(screen.getByRole("combobox"), String(72 * 3600));
    expect(chosen).toEqual([72 * 3600]);
  });
});
