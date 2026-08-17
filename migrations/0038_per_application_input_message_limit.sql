-- A per-application ceiling on the number of input messages, nested inside the deployment one.
--
-- `validate_public_request` (`src/application/public.rs:1331`) bounded `request.input.len()`
-- against `settings.public_api.maximum_messages` — a DEPLOYMENT value. So a tenant that needed a
-- longer input array could only be accommodated by raising the ceiling for every tenant on the
-- deployment, including the ones whose limit was deliberately low, and the bound could not be
-- lowered for one noisy caller either. It cuts both ways, which is what makes it a gap rather
-- than a preference.
--
-- WHY A NEW COLUMN RATHER THAN WIRING UP AN EXISTING ONE. #347 was filed believing
-- `maximum_input_items` was the per-application twin of this check and simply went unread. It is
-- read (`src/application/public.rs:1342`) and it is a different dimension: `PublicInputMessage`
-- carries `content: Vec<PublicContentPart>`, so `maximum_input_items` bounds the total content
-- PARTS across all messages. One message holding 200 parts and 200 messages holding one each are
-- identical to it and opposite to `maximum_messages`. Nothing in
-- `application_execution_policies` bounded the message count, so there was no wiring to do.
--
-- 128 matches `PublicApiSettings::default()` and `config/default.toml`, so every application
-- that exists when this lands keeps exactly the bound it had. The change is only that the bound
-- is now movable per application.
--
-- The deployment value stays, as the ceiling no application may exceed — two bounds, both
-- enforced, the application one nested inside the deployment one. Raising an application above
-- the deployment ceiling is refused by the application layer rather than by a constraint here,
-- because the deployment value is configuration and a `check` cannot see it.

alter table application_execution_policies
    add column if not exists maximum_input_messages integer not null default 128
        check (maximum_input_messages > 0);
