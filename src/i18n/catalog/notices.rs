//! Success and notice translation keys.
//!
//! Add new `moira.notice.*` entries here when API payloads expose readable notices.

use super::I18nEntry;

pub const RESPONSE_NOTICE_CATALOG: &[I18nEntry] = &[
    I18nEntry {
        key: "moira.notice.response_accepted",
        default_message: "The request was accepted for processing.",
        description: "Used for success acknowledgements that return before execution completes.",
    },
    I18nEntry {
        key: "moira.notice.response_completed",
        default_message: "The response completed successfully.",
        description: "Used when a response finishes successfully.",
    },
    I18nEntry {
        key: "moira.notice.response_persisted",
        default_message: "The response was saved successfully.",
        description: "Used when a response is written to durable storage.",
    },
    I18nEntry {
        key: "moira.notice.stream_started",
        default_message: "Streaming has started.",
        description: "Used when a streaming response is opened successfully.",
    },
    // Plan 07 — the catalog's first production notice consumer. A notice entry
    // exists only where the response actually carries prose a console shows a
    // human: `GET …/setup/claim-status` (a bare boolean), the auth-provider
    // records and `GET …/setup/auth-methods` (pure configuration data) carry
    // none and get none.
    I18nEntry {
        key: "moira.notice.admin_identity_claimed",
        default_message: "Admin access has been granted to this identity.",
        description: "Used on both the 201 of a fresh admin-identity claim and the 200 of an idempotent replay, which returns the stored body verbatim. The status code, not the notice, distinguishes the two. Also carried by the admin-identity list and by an ownership transfer, whose response is the same grant record.",
    },
    // Plan 09 wave 2. Each of the three has a pinned emitter; there is
    // deliberately no admin_identity_recovered notice, because the recovery
    // swap is not built in this wave (decision D-W2-1) and a key with no
    // emitter is worse than the gap.
    I18nEntry {
        key: "moira.notice.admin_invite_created",
        default_message: "An admin invitation has been created.",
        description: "Used on the 201 of POST /api/v1/admin/admin-invites, and on its idempotent replay - which returns the stored body with the token omitted, since the token is shown exactly once.",
    },
    I18nEntry {
        key: "moira.notice.admin_invite_redeemed",
        default_message: "The invitation has been redeemed and admin access granted.",
        description: "Used on the 201 of POST /api/v1/admin/admin-invites/redeem. Distinct from admin_identity_claimed so a console can tell an invited admin from one granted with the bootstrap system key - the two paths differ in who authorised them.",
    },
    I18nEntry {
        key: "moira.notice.admin_identity_revoked",
        default_message: "Admin access has been revoked for this identity.",
        description: "Used on the 200 of DELETE /api/v1/admin/admin-identities/{id}. The revocation is a soft one - status becomes 'revoked' and the row stays - so the response carries the updated record rather than an empty 204, and this notice is what makes that record readable to a human.",
    },
];
