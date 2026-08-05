// The display-safe view model shared by the setup wizard's organisms.
//
// Everything here is what `GET /api/setup` publishes and nothing more: counts
// and presence flags, never the allow-list itself, never an endpoint URL, and
// never any credential (decision D4). The page server component builds it from
// the BFF response; the organisms never see Moira's raw projection.

import type { SetupProvisioningState } from "@/lib/setup-steps";
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
      /**
       * The provisioning state the BFF DERIVED from Moira plus the console's
       * secret store — display-safe counts, ids and booleans. This is what
       * makes the wizard survive the sign-in navigation and any revisit of a
       * provisioned-but-unclaimed deployment: the browser's own memory of the
       * state is gone after both, and this field is where it comes back from.
       */
      readonly provisioning: SetupProvisioningState;
      /**
       * The console-issuer namespace this view model describes — `?slug=` as
       * the BFF echoed it, `null` for the incumbent.
       *
       * Not decoration: it is what the wizard puts back on the provision body,
       * the claim body and the OAuth callback URL, so a run started under a
       * replacement slug stays in that namespace across the sign-in navigation
       * and any reload.
       */
      readonly slug: string | null;
      /** Better Auth's provider id for that namespace. An identifier. */
      readonly oauthProviderId: string | null;
    }
  | { readonly kind: "claimed" }
  | {
      readonly kind: "unavailable";
      readonly messageKey: string;
      readonly message?: string;
      readonly messageArgs?: JsonValue;
    };
