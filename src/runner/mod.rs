//! `moira-runner` — the containerised Claude runner control service (issue #273, workstream
//! R1 of #272).
//!
//! # Why this is a second process and not a module of the API
//!
//! Access to the Docker socket is **root-equivalent on the host**: anything that can create a
//! container can bind-mount `/` and read or write every file the daemon's user can. Neither
//! the console nor the Moira API process may hold that capability, so the capability lives in
//! its own binary that binds loopback, carries its own control token, and is never exposed to
//! the internet.
//!
//! ```text
//! Console (no Docker) -> Moira (no Docker) -> moira-runner -> Docker Engine API
//! ```
//!
//! The wire contract this module implements is frozen and is documented in full on issue #272;
//! [`http`] is the authority for the routes and [`state`] for the state machine.
//!
//! # What it does
//!
//! It provisions a container running `claude setup-token`, scrapes the authorization URL out
//! of that container's tty stream, writes the operator's pasted authorization code back into
//! the container's stdin, and yields the minted token exactly once.
//!
//! # The four measured facts the design rests on
//!
//! Every one of these was measured on 2026-08-15 against Docker Desktop 29.6.2 and
//! `@anthropic-ai/claude-code` 2.1.233; the evidence is at
//! <https://github.com/nurcahyo/moira/issues/272#issuecomment-5303491808>. None of them is
//! inferred, and none of them should be re-derived.
//!
//! 1. **There is no loopback callback to reach.** Inside a container `claude setup-token`
//!    falls back to Anthropic's hosted redirect (`platform.claude.com/oauth/code/callback`)
//!    and a paste prompt. Nothing is published, forwarded or port-mapped, and
//!    [`engine::ContainerSpec`] consequently has no port field at all.
//! 2. **The daemon supplies the pty.** A container created with `Tty: true` renders the CLI's
//!    Ink UI even though this process has no terminal of its own; a plain pipe yields zero
//!    bytes. This is what `script(1)` could not do and what made issue #269 conclude the
//!    interactive acquisition was impossible without `node-pty`.
//! 3. **The Engine API attach endpoint accepts piped stdin; `docker exec -i -t` does not.**
//!    The CLI refuses with "cannot attach stdin to a TTY-enabled container because stdin is
//!    not a terminal"; `POST /containers/{id}/attach?stream=1&stdin=1` returns HTTP 101 and
//!    the write reaches the prompt. [`engine::ContainerEngine::write_stdin`] is that call.
//! 4. **The Ink UI hard-wraps the authorization URL across lines at terminal width.** True,
//!    and incomplete — a byte-level capture taken while building this module showed the CLI
//!    also emits the *complete* URL inside an **OSC 8 hyperlink**, with the wrapped fragments
//!    as its visible text. [`scrape`] prefers the hyperlink and keeps the rejoin as a
//!    fallback; its module docs carry the capture and the bug the incomplete version shipped.
//!
//! # What is NOT proven — the open defect
//!
//! **Submitting an authorization code does not work from this process.** The write reaches the
//! prompt (the CLI echoes it back masked) and the carriage return that submits it does not take
//! effect, so nothing past `awaiting_authorization` is verified — including capturing a minted
//! token, which was already unproven on issue #272.
//!
//! The container and the payload are measured *not* to be at fault: a reference client submits
//! successfully against a container this service created, and a bare carriage return from that
//! client submits a code this service had already typed in. [`docker_engine`]'s module docs
//! carry the full account, the eight things already tried, and the next diagnostic step.
//! `docs/moira-runner.md` is the operator-facing version.
//!
//! # Module layout
//!
//! | Module | Responsibility |
//! |---|---|
//! | [`config`] | `MOIRA_RUNNER__*` environment configuration and its startup validation |
//! | [`token`] | the two redacting secret newtypes and the constant-time control-token check |
//! | [`engine`] | the `ContainerEngine` trait seam, its value types, and the in-memory fake |
//! | [`docker_engine`] | the `bollard` implementor — **the only file in the crate that may import `bollard`** |
//! | [`scrape`] | ANSI stripping, wrapped-line rejoining, URL and token extraction |
//! | [`state`] | the state machine, the label vocabulary, and state derivation |
//! | [`service`] | the use-case layer: create, observe, submit a code, take the token, reap |
//! | [`http`] | the Axum surface: routes, bearer auth, DTOs, and the frozen error envelope |
//!
//! This mirrors the layering `docs/project-structure.md` already uses for the API process
//! (`http` -> `application` -> `infra`), scoped to one binary.
//!
//! # Deliberately not shared with the API process
//!
//! [`config::RunnerConfig`] is separate from `crate::config::Settings` and
//! [`http::RunnerError`] is separate from `crate::error::AppError`. Both are deliberate: the
//! runner's error envelope is `{"error": {"code", "message"}}`, frozen by the contract and
//! *narrower* than Moira's i18n envelope, and merging them would either widen the frozen
//! contract or force a machine-to-machine control plane through a translation catalogue built
//! for human-facing responses. The runner's consumer is the Moira process, which maps these
//! codes onto its own i18n keys on its side of the boundary.

pub mod config;
pub mod docker_engine;
pub mod engine;
pub mod http;
pub mod scrape;
pub mod service;
pub mod state;
pub mod token;

#[cfg(test)]
mod tests {
    /// Docker access is root-equivalent, so the blast radius of a mistake here is the whole
    /// host. The confinement is a design rule, and a design rule with no test is a comment.
    ///
    /// This asserts the rule the module docs state: `bollard` is imported by exactly one
    /// file. Anything else — a handler reaching for `Docker` directly, a service growing a
    /// convenience call, the API binary picking it up — moves Docker access out from behind
    /// [`super::engine::ContainerEngine`] and is a red build rather than a review catch.
    #[test]
    fn bollard_is_imported_by_exactly_one_file() {
        let sources: &[(&str, &str)] = &[
            ("src/runner/mod.rs", include_str!("mod.rs")),
            ("src/runner/config.rs", include_str!("config.rs")),
            (
                "src/runner/docker_engine.rs",
                include_str!("docker_engine.rs"),
            ),
            ("src/runner/engine.rs", include_str!("engine.rs")),
            ("src/runner/http.rs", include_str!("http.rs")),
            ("src/runner/scrape.rs", include_str!("scrape.rs")),
            ("src/runner/service.rs", include_str!("service.rs")),
            ("src/runner/state.rs", include_str!("state.rs")),
            ("src/runner/token.rs", include_str!("token.rs")),
            (
                "src/bin/moira-runner.rs",
                include_str!("../bin/moira-runner.rs"),
            ),
        ];

        let importers: Vec<&str> = sources
            .iter()
            .filter(|(_, body)| {
                body.lines().any(|line| {
                    let line = line.trim_start();
                    line.starts_with("use bollard") || line.starts_with("use ::bollard")
                })
            })
            .map(|(path, _)| *path)
            .collect();

        assert_eq!(
            importers,
            vec!["src/runner/docker_engine.rs"],
            "`bollard` must be imported by src/runner/docker_engine.rs and nothing else — \
             every other caller goes through the ContainerEngine trait"
        );
    }
}
