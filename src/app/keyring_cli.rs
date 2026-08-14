//! `moira keyring …` — argument parsing and rendering for the rotation verbs.
//!
//! The verbs themselves are [`crate::security::KeyringAdmin`]. This module is the thin half:
//! it turns argv into a [`KeyringCommand`], and turns a result into the text an operator reads.
//! Keeping it in the library rather than in `src/main.rs` is what lets the parser be tested —
//! including against the command lines Moira's own boot refusals tell operators to run.
//!
//! # The verbs are named apart on purpose
//!
//! `add` mints, `promote` switches, `rewrap` re-seals the keys, `reseal` re-seals the *rows*.
//! Conflating any two of them is how a rotation goes wrong, because the operator picks the
//! wrong one under pressure — so there is no `rotate`, and no verb that does two of these.
//! `docs/decision-encryption-at-rest.md` §9 makes the same point at greater length.

use uuid::Uuid;

use crate::{
    error::AppError,
    security::{AadProfile, DataKeyPurpose, KeyringAdmin, KeyringAdminError, ResealOptions},
};

/// The help text, printed for `moira keyring` with no verb and for an unknown one.
pub const KEYRING_USAGE: &str = "\
moira keyring <verb>

  status                              every key: id, version, state, purpose, master key,
                                      key check value, and per-table row counts
  usage <id>                          per-table reference counts for one key
  add [--purpose content|memory_dedupe]
                                      mint a data key, wrap it, insert it 'pending'
  promote <id>                        R1: 'pending' -> 'active', old active -> 'retiring'.
                                      Nothing is re-encrypted and no restart is needed.
  rewrap --to <master-key-id>         R2/R3: re-wrap every loadable data key under a
                                      different MASTER key. No row of user data is read
                                      or written.
  retire <id>                         mark 'retired'. Refused while any row still names it.
  abandon <id> --confirm --reason \"<text>\"
                                      acknowledge a permanently lost master key. Rows
                                      sealed under <id> become permanently unreadable.
  reseal --from <id> --to <id> [--batch N] [--sleep-ms M] [--max-batches N]
                                      R4: re-encrypt rows onto a newer data key. The only
                                      expensive verb; resumable and safe under live traffic.
                                      Re-run it as often as you like: each pass selects only
                                      what is left, so it converges.
";

/// One parsed verb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyringCommand {
    Status,
    Usage {
        id: Uuid,
    },
    Add {
        purpose: DataKeyPurpose,
    },
    Promote {
        id: Uuid,
    },
    Rewrap {
        to: String,
    },
    Retire {
        id: Uuid,
    },
    /// `confirm` and `reason` are carried **unvalidated**, on purpose.
    ///
    /// Refusing a missing `--confirm` here would put the guard text in the parser, and the
    /// same guard is reachable from anything else that ever calls `KeyringAdmin::abandon`.
    /// One implementation, in the place that performs the act.
    Abandon {
        id: Uuid,
        confirm: bool,
        reason: Option<String>,
    },
    Reseal {
        from: Uuid,
        to: Uuid,
        options: ResealOptions,
    },
}

impl KeyringCommand {
    /// Parses `argv` **after** the `keyring` word.
    ///
    /// Every failure is [`AppError::Config`] carrying [`KEYRING_USAGE`], because a mistyped
    /// rotation command at three in the morning should answer itself rather than send the
    /// operator to a document.
    pub fn parse(args: &[String]) -> Result<Self, AppError> {
        let mut args = args.iter().map(String::as_str);
        let verb = args.next().ok_or_else(|| usage("a verb is required"))?;
        let rest: Vec<&str> = args.collect();

        match verb {
            "status" => {
                no_extra(verb, &rest)?;
                Ok(Self::Status)
            }
            "usage" => Ok(Self::Usage {
                id: positional_uuid(verb, &rest)?,
            }),
            "promote" => Ok(Self::Promote {
                id: positional_uuid(verb, &rest)?,
            }),
            "retire" => Ok(Self::Retire {
                id: positional_uuid(verb, &rest)?,
            }),
            "add" => {
                let flags = Flags::parse(verb, &rest, &[])?;
                let purpose = match flags.value("--purpose") {
                    None => DataKeyPurpose::Content,
                    Some(value) => DataKeyPurpose::from_db(value).ok_or_else(|| {
                        usage(&format!(
                            "unknown --purpose {value:?}; expected \"content\" or \
                             \"memory_dedupe\""
                        ))
                    })?,
                };
                Ok(Self::Add { purpose })
            }
            "rewrap" => {
                let flags = Flags::parse(verb, &rest, &[])?;
                let to = flags
                    .value("--to")
                    .ok_or_else(|| usage("`keyring rewrap` requires --to <master-key-id>"))?;
                Ok(Self::Rewrap { to: to.to_string() })
            }
            "abandon" => {
                let id = rest
                    .first()
                    .ok_or_else(|| usage("`keyring abandon` requires a data key id"))?;
                let id = parse_uuid(id)?;
                let flags = Flags::parse(verb, &rest[1..], &["--confirm"])?;
                Ok(Self::Abandon {
                    id,
                    confirm: flags.is_set("--confirm"),
                    reason: flags.value("--reason").map(str::to_string),
                })
            }
            "reseal" => {
                let flags = Flags::parse(verb, &rest, &[])?;
                let from = parse_uuid(
                    flags
                        .value("--from")
                        .ok_or_else(|| usage("`keyring reseal` requires --from <id>"))?,
                )?;
                let to = parse_uuid(
                    flags
                        .value("--to")
                        .ok_or_else(|| usage("`keyring reseal` requires --to <id>"))?,
                )?;
                let defaults = ResealOptions::default();
                Ok(Self::Reseal {
                    from,
                    to,
                    options: ResealOptions {
                        batch: number(&flags, "--batch")?.unwrap_or(defaults.batch),
                        sleep_ms: number(&flags, "--sleep-ms")?.unwrap_or(defaults.sleep_ms),
                        max_batches: number(&flags, "--max-batches")?,
                    },
                })
            }
            other => Err(usage(&format!("unknown keyring verb {other:?}"))),
        }
    }
}

/// Runs one verb and renders its result.
///
/// Returns the text rather than printing it, so the rendering is assertable and so `main`
/// keeps the only `println!` in the process.
pub async fn run(command: KeyringCommand, admin: &KeyringAdmin) -> Result<String, AppError> {
    match command {
        KeyringCommand::Status => {
            let status = admin.status().await.map_err(config_error)?;
            let mut out = String::new();
            out.push_str(&format!(
                "custody backend       {}\nactive master key id  {}\nconfigured master keys {:?}\n\n",
                status.backend, status.active_master_key_id, status.configured_master_key_ids,
            ));
            if status.keys.is_empty() {
                out.push_str("content_data_keys is empty.\n");
            }
            for key in &status.keys {
                out.push_str(&format!(
                    "v{version} {id}\n  state             {state}\n  purpose           {purpose}\n  \
                     custody backend   {backend}\n  master key id     {master} ({held})\n  \
                     key check value   {kcv}\n  created           {created}\n",
                    version = key.key_version,
                    id = key.id,
                    state = key.state,
                    purpose = key.purpose,
                    backend = key.custody_backend,
                    master = key.master_key_id,
                    held = if key.master_key_held {
                        "held by this process"
                    } else {
                        "NOT HELD — this process could not boot on this keyring"
                    },
                    kcv = key.key_check_value,
                    created = key.created_at,
                ));
                if let Some(reason) = &key.abandon_reason {
                    out.push_str(&format!("  abandon reason    {reason}\n"));
                }
                out.push_str(&format!("  rows sealed       {}\n", key.usage.total()));
                for (column, count) in &key.usage.per_column {
                    out.push_str(&format!("    {column} {count}\n"));
                }
                out.push('\n');
            }
            Ok(out)
        }
        KeyringCommand::Usage { id } => {
            let usage = admin.usage(id).await.map_err(config_error)?;
            let mut out = format!("rows sealed under {id}: {}\n", usage.total());
            for (column, count) in &usage.per_column {
                out.push_str(&format!("  {column} {count}\n"));
            }
            // From the registry, not a literal: a sixth sealed column that this verb did not
            // count would otherwise be invisible in the one report retirement depends on.
            out.push_str(&format!(
                "counted over {} sealed column(s)\n",
                AadProfile::ALL.len()
            ));
            Ok(out)
        }
        KeyringCommand::Add { purpose } => {
            let added = admin.add(purpose).await.map_err(config_error)?;
            Ok(format!(
                "minted data key {id}\n  version           {version}\n  purpose           \
                 {purpose}\n  state             pending\n  master key id     {master}\n  \
                 key check value   {kcv}\n\nNothing writes under it yet. Run `moira keyring \
                 promote {id}` to make it the active key.\n",
                id = added.id,
                version = added.key_version,
                purpose = added.purpose.as_str(),
                master = added.master_key_id,
                kcv = added.key_check_value,
            ))
        }
        KeyringCommand::Promote { id } => {
            let promotion = admin.promote(id).await.map_err(config_error)?;
            let mut out = format!(
                "promoted {id} to active for purpose {purpose}\n",
                purpose = promotion.purpose.as_str()
            );
            match promotion.demoted {
                Some(demoted) => out.push_str(&format!(
                    "demoted  {demoted} to retiring\n\nRows already sealed under {demoted} stay \
                     under it and stay readable, forever. Nothing was re-encrypted, no restart \
                     is needed, and each instance seals new content under {id} at its next \
                     keyring refresh.\n"
                )),
                None => out.push_str(
                    "\nThere was no active key for this purpose to demote.\n\
                     Nothing was re-encrypted.\n",
                ),
            }
            Ok(out)
        }
        KeyringCommand::Rewrap { to } => {
            let report = admin.rewrap(&to).await.map_err(config_error)?;
            let mut out = format!(
                "re-wrapped {count} data key(s) under master key {to} (backend {backend})\n",
                count = report.rewrapped.len(),
                backend = report.target_backend,
            );
            for id in &report.rewrapped {
                out.push_str(&format!("  {id}\n"));
            }
            for (id, state) in &report.left_alone {
                out.push_str(&format!("  {id} left alone (state {state})\n"));
            }
            out.push_str(
                "\nNO ROW OF USER DATA WAS READ OR WRITTEN. Only the wrapping of the data keys \
                 changed; the ciphertext in every *_encrypted column is untouched, byte for \
                 byte.\n\nRunning instances are unaffected — they already hold unwrapped data \
                 keys. Next: set MOIRA_CONTENT_ENCRYPTION__ACTIVE_KEY_ID to this master key and \
                 do a rolling restart, then drop the previous master key from \
                 MOIRA_CONTENT_ENCRYPTION__KEYS and restart again.\n",
            );
            Ok(out)
        }
        KeyringCommand::Retire { id } => {
            admin.retire(id).await.map_err(config_error)?;
            Ok(format!(
                "retired {id}. It is no longer loaded at boot, and any row that names it from \
                 now on fails cleanly rather than lazily.\n"
            ))
        }
        KeyringCommand::Abandon {
            id,
            confirm,
            reason,
        } => {
            let abandonment = admin
                .abandon(id, confirm, reason.as_deref().unwrap_or_default())
                .await
                .map_err(config_error)?;
            let mut out = format!(
                "ABANDONED {id}\n  reason  {reason}\n\nEvery row sealed under {id} is now \
                 permanently unreadable. The ciphertext was NOT deleted: if the master key ever \
                 resurfaces those rows can be recovered. Moira will start, and every other key \
                 keeps working.\n",
                reason = abandonment.reason,
            );
            if abandonment.active_content_key_remaining.is_none() {
                out.push_str(
                    "\nWARNING: no active data key remains for this purpose. Moira will refuse \
                     to start until you run `moira keyring add` and then `moira keyring promote \
                     <id>`.\n",
                );
            }
            Ok(out)
        }
        KeyringCommand::Reseal { from, to, options } => {
            let report = admin
                .reseal(from, to, options)
                .await
                .map_err(config_error)?;
            Ok(format!(
                "resealed {resealed} row(s) from {from} onto {to} in {batches} pass(es)\n  \
                 skipped {skipped} row(s) a concurrent writer had already moved\n\nRe-run this \
                 command at any time: each pass selects only what is left, so it converges. \
                 When `moira keyring usage {from}` reports zero, `moira keyring retire {from}` \
                 will be accepted.\n",
                resealed = report.resealed,
                skipped = report.skipped,
                batches = report.batches,
            ))
        }
    }
}

/// Every rotation failure is a configuration or custody fault an operator has to act on, and
/// `main` turns [`AppError::Config`] into a non-zero exit. The `Display` of
/// [`KeyringAdminError`] is the whole message — see its variants.
fn config_error(error: KeyringAdminError) -> AppError {
    AppError::Config(error.to_string())
}

fn usage(problem: &str) -> AppError {
    AppError::Config(format!("{problem}\n\n{KEYRING_USAGE}"))
}

/// An optional numeric flag. A typo in `--sleep-ms` must be a refusal, never a silent zero:
/// a reseal that runs at full speed against live traffic because a digit was fat-fingered is
/// the one mistake this verb can make that an operator cannot undo by re-running it.
fn number<T: std::str::FromStr>(flags: &Flags<'_>, flag: &str) -> Result<Option<T>, AppError> {
    flags
        .value(flag)
        .map(|value| {
            value
                .parse()
                .map_err(|_| usage(&format!("{flag} {value:?} is not a number")))
        })
        .transpose()
}

fn parse_uuid(value: &str) -> Result<Uuid, AppError> {
    value
        .parse()
        .map_err(|_| usage(&format!("{value:?} is not a data key id (a UUID)")))
}

fn positional_uuid(verb: &str, rest: &[&str]) -> Result<Uuid, AppError> {
    let [id] = rest else {
        return Err(usage(&format!(
            "`keyring {verb}` takes exactly one data key id"
        )));
    };
    parse_uuid(id)
}

fn no_extra(verb: &str, rest: &[&str]) -> Result<(), AppError> {
    if rest.is_empty() {
        Ok(())
    } else {
        Err(usage(&format!(
            "`keyring {verb}` takes no arguments (got {rest:?})"
        )))
    }
}

/// `--flag value` pairs plus a declared set of boolean flags.
///
/// Deliberately strict: an unknown flag is a refusal, never ignored. A `keyring abandon` that
/// quietly dropped a misspelled `--confim` would refuse for the wrong reason, and a
/// `--sleep-ms` typo on a reseal would silently run it at full speed against live traffic.
struct Flags<'a> {
    values: Vec<(&'a str, &'a str)>,
    set: Vec<&'a str>,
}

impl<'a> Flags<'a> {
    fn parse(verb: &str, args: &[&'a str], booleans: &[&str]) -> Result<Self, AppError> {
        let mut values = Vec::new();
        let mut set = Vec::new();
        let mut index = 0;
        while index < args.len() {
            let flag = args[index];
            if !flag.starts_with("--") {
                return Err(usage(&format!(
                    "`keyring {verb}` did not expect the argument {flag:?}"
                )));
            }
            if booleans.contains(&flag) {
                set.push(flag);
                index += 1;
                continue;
            }
            let value = args
                .get(index + 1)
                .ok_or_else(|| usage(&format!("`keyring {verb}`: {flag} requires a value")))?;
            values.push((flag, *value));
            index += 2;
        }
        Ok(Self { values, set })
    }

    fn value(&self, flag: &str) -> Option<&'a str> {
        self.values
            .iter()
            .find(|(name, _)| *name == flag)
            .map(|(_, value)| *value)
    }

    fn is_set(&self, flag: &str) -> bool {
        self.set.contains(&flag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::KeyringError;

    fn argv(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn every_verb_parses_into_the_shape_the_admin_functions_take() {
        let id = Uuid::from_u128(0x1234);
        let other = Uuid::from_u128(0x5678);

        assert_eq!(
            KeyringCommand::parse(&argv("status")).unwrap(),
            KeyringCommand::Status
        );
        assert_eq!(
            KeyringCommand::parse(&argv(&format!("usage {id}"))).unwrap(),
            KeyringCommand::Usage { id }
        );
        assert_eq!(
            KeyringCommand::parse(&argv("add")).unwrap(),
            KeyringCommand::Add {
                purpose: DataKeyPurpose::Content
            }
        );
        assert_eq!(
            KeyringCommand::parse(&argv("add --purpose memory_dedupe")).unwrap(),
            KeyringCommand::Add {
                purpose: DataKeyPurpose::MemoryDedupe
            }
        );
        assert_eq!(
            KeyringCommand::parse(&argv(&format!("promote {id}"))).unwrap(),
            KeyringCommand::Promote { id }
        );
        assert_eq!(
            KeyringCommand::parse(&argv("rewrap --to prod-2026-08")).unwrap(),
            KeyringCommand::Rewrap {
                to: "prod-2026-08".to_string()
            }
        );
        assert_eq!(
            KeyringCommand::parse(&argv(&format!("retire {id}"))).unwrap(),
            KeyringCommand::Retire { id }
        );
        assert_eq!(
            KeyringCommand::parse(&argv(&format!(
                "reseal --from {id} --to {other} --batch 25 --sleep-ms 40 --max-batches 3"
            )))
            .unwrap(),
            KeyringCommand::Reseal {
                from: id,
                to: other,
                options: ResealOptions {
                    batch: 25,
                    sleep_ms: 40,
                    max_batches: Some(3),
                },
            }
        );
        // The defaults are the ones an operator gets by omitting the knobs. `max_batches:
        // None` is the one that matters: a bare reseal must run to completion, because the
        // operator's next step is `retire`, which refuses until the count reaches zero.
        assert_eq!(
            KeyringCommand::parse(&argv(&format!("reseal --from {id} --to {other}"))).unwrap(),
            KeyringCommand::Reseal {
                from: id,
                to: other,
                options: ResealOptions::default(),
            }
        );
        assert_eq!(ResealOptions::default().max_batches, None);
        assert_eq!(ResealOptions::default().sleep_ms, 0);
    }

    #[test]
    fn abandon_carries_its_guards_through_without_deciding_them() {
        let id = Uuid::from_u128(9);
        let mut args = argv(&format!("abandon {id} --confirm --reason"));
        args.push("master key vault deleted 2026-08-06".to_string());

        assert_eq!(
            KeyringCommand::parse(&args).unwrap(),
            KeyringCommand::Abandon {
                id,
                confirm: true,
                reason: Some("master key vault deleted 2026-08-06".to_string()),
            }
        );
        // Both omissions parse. The refusal is `KeyringAdmin::abandon`'s, so that anything
        // else reaching that function gets the same two guards rather than a parser's copy.
        assert_eq!(
            KeyringCommand::parse(&argv(&format!("abandon {id}"))).unwrap(),
            KeyringCommand::Abandon {
                id,
                confirm: false,
                reason: None,
            }
        );
    }

    #[test]
    fn a_mistyped_flag_is_refused_rather_than_ignored() {
        let id = Uuid::from_u128(9);
        // The one that matters: `--confim` silently dropped would make the refusal say
        // "requires --confirm" to an operator who is looking straight at one.
        let error = KeyringCommand::parse(&argv(&format!("abandon {id} --confim")))
            .expect_err("an unknown boolean flag must be refused");
        assert!(error.to_string().contains("--confim"), "{error}");

        for line in [
            "status --please",
            "rewrap",
            "rewrap --to",
            "promote",
            "promote not-a-uuid",
            "promote 00000000-0000-0000-0000-000000000001 --force",
            "reseal --from",
            "conjure",
            "add --purpose embeddings",
        ] {
            assert!(
                KeyringCommand::parse(&argv(line)).is_err(),
                "{line:?} must be refused"
            );
        }
    }

    /// Every refusal above carries the usage text, so a mistyped rotation command answers
    /// itself instead of sending a tired operator to a document.
    #[test]
    fn every_parse_refusal_prints_the_usage() {
        for line in ["", "conjure", "promote", "rewrap"] {
            let error = KeyringCommand::parse(&argv(line)).expect_err("must refuse");
            let text = error.to_string();
            assert!(text.contains("moira keyring <verb>"), "{line:?}: {text}");
            assert!(text.contains("rewrap --to"), "{line:?}: {text}");
        }
    }

    /// **The debt from PR 3, repaid mechanically rather than by eye.**
    ///
    /// `KeyringError::UnopenableDataKey` is the FATAL message a lost master key produces at
    /// boot, and it tells the operator to run a command. Until this PR that command did not
    /// exist. Asserting the two agree by *reading* both is exactly how they drift, so this
    /// test lifts the command line out of the rendered refusal and feeds it to the real
    /// parser: a rename of any verb or flag on either side fails here.
    #[test]
    fn the_boot_refusals_name_command_lines_this_parser_accepts() {
        let data_key_id = Uuid::from_u128(0xfeed);
        let refusals = [
            KeyringError::UnopenableDataKey {
                data_key_id,
                master_key_id: "lost-2026-01".to_string(),
                backend: "environment",
                configured: vec!["current".to_string()],
                source: crate::security::KeyCustodyError::UnknownMasterKey {
                    master_key_id: "lost-2026-01".to_string(),
                },
            },
            KeyringError::KeyCheckValueMismatch {
                data_key_id,
                master_key_id: "current".to_string(),
                stored: "0011223344556677".to_string(),
                computed: "8899aabbccddeeff".to_string(),
            },
            KeyringError::NoActiveKey {
                purpose: "content",
                loaded: 2,
            },
        ];

        let mut checked = 0;
        for refusal in refusals {
            for command in extract_moira_commands(&refusal.to_string()) {
                let args = argv(&command);
                assert_eq!(
                    args.first().map(String::as_str),
                    Some("keyring"),
                    "{command}"
                );
                let parsed = KeyringCommand::parse(&args[1..]).unwrap_or_else(|error| {
                    panic!(
                        "a boot refusal tells the operator to run `moira {command}`, which this \
                         binary does not accept: {error}"
                    )
                });
                match parsed {
                    KeyringCommand::Abandon {
                        id,
                        confirm,
                        reason,
                    } => {
                        // Not merely parseable — the *right* key, with both guards present.
                        assert_eq!(id, data_key_id, "{command}");
                        assert!(
                            confirm,
                            "the printed abandon line omits --confirm: {command}"
                        );
                        assert!(
                            reason.is_some(),
                            "the printed abandon line omits --reason: {command}"
                        );
                    }
                    KeyringCommand::Promote { .. } => {}
                    other => panic!("unexpected command in a boot refusal: {other:?}"),
                }
                checked += 1;
            }
        }
        // One command line per refusal: two `abandon` and one `promote`. The count is here
        // because without it the whole loop is vacuous — an `extract_moira_commands` that
        // found nothing, or a refusal that stopped naming a command, would leave every
        // assertion above unexecuted and this test green. (It earned its keep immediately:
        // the first draft asserted 4, and this is what said so.)
        assert_eq!(
            checked, 3,
            "the boot refusals stopped naming the command lines this test was written for"
        );
    }

    /// Lifts every `` `moira …` `` span out of a rendered refusal.
    ///
    /// `<id>` and `"<text>"` placeholders are replaced with values, exactly as an operator
    /// would when carrying the instruction out — the point is that the *shape* of the command
    /// is one this binary accepts, not that a literal placeholder parses.
    fn extract_moira_commands(text: &str) -> Vec<String> {
        let mut commands = Vec::new();
        let mut rest = text;
        while let Some(start) = rest.find("`moira ") {
            let after = &rest[start + "`moira ".len()..];
            let Some(end) = after.find('`') else { break };
            let command = after[..end]
                .replace("\"<text>\"", "placeholder-reason")
                .replace("<id>", &Uuid::from_u128(0xfeed).to_string());
            commands.push(command.split_whitespace().collect::<Vec<_>>().join(" "));
            rest = &after[end + 1..];
        }
        commands
    }
}
