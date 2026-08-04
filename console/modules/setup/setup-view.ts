// The display-safe view model shared by the setup wizard's organisms.
//
// Everything here is what `GET /api/setup` publishes and nothing more: counts
// and presence flags, never the allow-list itself, never an endpoint URL, and
// never any credential (decision D4). The page server component builds it from
// the BFF response; the organisms never see Moira's raw projection.

import type { AuthMethod, JsonValue } from "@/lib/types";

/** One provider row, as the BFF's narrowed `SetupMethodView` projected it. */
export interface SetupMethodSummary {
  readonly id: string;
  readonly method: AuthMethod;
  readonly displayName: string;
  readonly interactive: boolean;
  /** The row carries a client id. Presence, never the value. */
  readonly clientIdConfigured: boolean;
  readonly discoveryUrlConfigured: boolean;
  readonly allowedEmailDomainCount: number;
}

/** What `/setup`'s server component resolved for the wizard. */
export type SetupViewModel =
  | {
      readonly kind: "ready";
      readonly claimed: boolean;
      readonly methods: readonly SetupMethodSummary[];
    }
  | { readonly kind: "claimed" }
  | {
      readonly kind: "unavailable";
      readonly messageKey: string;
      readonly message?: string;
      readonly messageArgs?: JsonValue;
    };
