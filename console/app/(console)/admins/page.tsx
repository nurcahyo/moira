// `/admins` — admin grants, invitations, and ownership.
//
// Thin by design: guard (inherited from `(console)/layout.tsx`), fetch, render.
// Every mutation belongs to a route handler under `app/api/**` (decision W5-D5),
// so this page performs reads only.
//
// ============================================================================
// WHAT IS FETCHED, AND WHY THE THIRD CALL EXISTS
// ============================================================================
//
//   listAdminIdentities   the grants table.
//   listAdminInvites      the invitation list.
//   getSetupAuthMethods   the pre-submit gate's input. AUTHENTICATED — it is
//                         `PublicAuthMethod`, which carries
//                         `allowed_email_domains`, i.e. plan 07 decision D3's
//                         admin-claim policy. The ANONYMOUS projection
//                         (`PublicSignInMethod`) deliberately omits that field,
//                         so it cannot serve this purpose and must not be
//                         reached for here.
//
// The gate that consumes the third call is a WARNING above one enabled provider,
// not a guarantee — see `InviteAdminForm`'s header and decision W5-D11. This
// page computes the union and the count; the component decides what to do with
// them, and says which of the two it is doing.
//
// ============================================================================
// A FAILED READ RENDERS AS A PAGE, NOT AS A 500
// ============================================================================
//
// `force-dynamic` because the gate above it is per-request. If Moira is
// unreachable the page still answers below 400 with a keyed explanation: the
// a11y walker asserts `status < 400` on every discovered route, and a 500 here
// would take the whole gate red for a backend outage.

import { consoleRuntime } from "@/lib/auth-runtime";
import { consoleEnv } from "@/lib/env";
import { isMoiraRequestError } from "@/lib/errors";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { inviteBaseUrl } from "@/lib/invite-bounds";
import { moiraClientForSession } from "@/lib/moira-session";
import type { AdminIdentityRecord, AdminInviteRecord } from "@/lib/types";
import { InviteAdminForm, type InviteProviderPolicy } from "@/modules/admins/InviteAdminForm";
import { InviteList } from "@/modules/admins/InviteList";
import { TransferPrimaryPanel } from "@/modules/admins/TransferPrimaryPanel";
import { headers } from "next/headers";

import styles from "./page.module.css";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

interface ScreenData {
  readonly identities: readonly AdminIdentityRecord[];
  readonly invites: readonly AdminInviteRecord[];
  readonly policy: InviteProviderPolicy;
}

/** Everything the screen renders, or `null` when Moira could not be reached. */
async function load(): Promise<ScreenData | null> {
  const runtimeState = await consoleRuntime();
  if (!runtimeState.ok) return null;

  const env = consoleEnv();
  const client = moiraClientForSession(env, runtimeState.auth, await headers());

  const [identities, invites, methods] = await Promise.all([
    client.listAdminIdentities({ limit: 100 }),
    client.listAdminInvites({ limit: 100 }),
    client.getSetupAuthMethods(),
  ]);

  // The UNION, computed here rather than in the component, so the component
  // holds no opinion about how the union was derived and the limits of it are
  // documented in one place.
  const allowedEmailDomains = [
    ...new Set(
      methods.methods.flatMap((method) =>
        method.allowed_email_domains.map((domain) => domain.trim().toLowerCase()),
      ),
    ),
  ];

  return {
    identities: identities.data,
    invites: invites.data,
    // `configs` is what `consoleRuntime()` RESOLVED — a provider with an enabled
    // Moira row but no usable client secret is not one an invitee could sign in
    // through, so counting rows instead of resolutions would over-count the ways
    // out of the gate.
    policy: { enabledProviderCount: runtimeState.configs.length, allowedEmailDomains },
  };
}

export default async function AdminsPage() {
  let data: ScreenData | null;
  try {
    data = await load();
  } catch (error) {
    if (!isMoiraRequestError(error)) throw error;
    data = null;
  }

  return (
    <main className={styles.main}>
      <h1 className={styles.title}>{t(CONSOLE_MESSAGE_KEYS.page_admins_title)}</h1>

      {data === null ? (
        <p className={styles.problem} role="alert">
          {t(CONSOLE_MESSAGE_KEYS.admins_request_failed)}
        </p>
      ) : (
        <>
          <TransferPrimaryPanel identities={data.identities} />
          <InviteAdminForm
            policy={data.policy}
            inviteBaseUrl={inviteBaseUrl(consoleEnv().consoleOrigin)}
          />
          <InviteList invites={data.invites} />
        </>
      )}
    </main>
  );
}
