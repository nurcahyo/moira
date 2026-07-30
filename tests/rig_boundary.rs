//! The Rig containment boundary, enforced by a test instead of by memory (ledger finding F7).
//!
//! Moira's stated architecture is that Rig owns AI *execution* primitives and Moira owns runtime
//! config, identity, credentials, routing and streaming (`CLAUDE.md`; `docs/project-structure.md`).
//! The load-bearing consequence is that `rig_core` types must not leak into `src/domain/` — the
//! moment a domain type is defined in terms of a `rig_core` type, upgrading Rig becomes a change to
//! Moira's domain model, and the seam that makes the provider layer replaceable is gone.
//!
//! **Why this file exists.** Plan 06 Module 7 established the boundary and verified it with a
//! one-off `grep`, after which it was described as enforced by a test. It was not. A rule that
//! holds by absence alone is one careless `use` away from being silently untrue, and nothing in the
//! build would have said so. This is that test.
//!
//! **Scope, stated honestly.** This is a source scan, not a type-level proof. It catches the way
//! the leak actually happens — someone writes `use rig_core::…` in a domain module — and it does
//! not catch a leak laundered through a type alias defined elsewhere. That is a real limit, and it
//! is still strictly more than the zero enforcement that preceded it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Files permitted to name `rig_core`. Both are the Rig seam itself:
///
/// - `src/orchestration/runtime_factory.rs` — builds Rig clients and completion models. This is
///   *the* boundary file; `src/orchestration/executor.rs` was often mistaken for it and is deleted.
/// - `src/application/execution.rs` — assembles `CompletionRequest` and converts tool schemas.
/// - `src/orchestration/embedding.rs` — the embedding twin of `runtime_factory.rs`, added by
///   plan 11 Sub-Phase B. It builds Rig embedding clients and classifies `EmbeddingError`, and
///   it is a *widening of the same seam*, not a second one: embeddings are an AI execution
///   primitive, so Rig owns them and Moira must not grow its own. Everything above it —
///   `chunking.rs`, `ingestion.rs`, the repository and the application service — names no Rig
///   type, which is what keeps `Vec<f32>` rather than `rig_core::embeddings::Embedding` the
///   currency of the ingestion pipeline.
///
/// Adding an entry here is the deliberate act of widening the boundary. It should be rare and it
/// should be argued for in review, which is the entire point of making it a diff.
const RIG_BOUNDARY_FILES: &[&str] = &[
    "src/application/execution.rs",
    "src/orchestration/embedding.rs",
    "src/orchestration/runtime_factory.rs",
];

/// Every `.rs` file under `src/`, as repo-relative slash-separated paths.
fn source_files() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found = Vec::new();
    let mut stack = vec![root.join("src")];

    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .unwrap_or_else(|err| panic!("cannot read {}: {err}", dir.display()));
        for entry in entries {
            let path: PathBuf = entry.expect("unreadable directory entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let relative = path
                    .strip_prefix(root)
                    .expect("walked path is under the manifest dir")
                    .to_string_lossy()
                    .replace('\\', "/");
                let body = std::fs::read_to_string(&path)
                    .unwrap_or_else(|err| panic!("cannot read {relative}: {err}"));
                found.push((relative, body));
            }
        }
    }

    found.sort();
    found
}

/// Drops `//`-style comment bodies.
///
/// Without this the doc comment on `runtime_factory.rs:412` — which explains the boundary in prose,
/// naming `rig_core` to do so — would count as a reference, and so would this rule's own
/// documentation if it ever moved into `src/`. A test that fires on its own explanation of itself
/// teaches people to add allow-list entries to silence it, which is worse than no test.
fn without_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| match line.find("//") {
            Some(index) => &line[..index],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn references_rig(source: &str) -> bool {
    let code = without_comments(source);
    code.contains("rig_core")
}

#[test]
fn rig_core_appears_only_in_the_two_boundary_files() {
    let files = source_files();

    // Vacuity guard. A walker that silently finds nothing asserts nothing, and every emptiness
    // failure mode here — wrong root, extension filter typo, `src/` moved — presents as a green
    // run. The tree is ~70 files; 40 is a floor that a real reorganisation would survive and a
    // broken walk would not.
    assert!(
        files.len() >= 40,
        "expected to scan the whole of src/ but found only {} .rs files — the walk is broken, and \
         a broken walk passes this test while checking nothing",
        files.len()
    );

    let referencing: BTreeSet<&str> = files
        .iter()
        .filter(|(_, body)| references_rig(body))
        .map(|(path, _)| path.as_str())
        .collect();
    let permitted: BTreeSet<&str> = RIG_BOUNDARY_FILES.iter().copied().collect();

    let leaked: Vec<&str> = referencing.difference(&permitted).copied().collect();
    assert!(
        leaked.is_empty(),
        "these files name `rig_core` but are not part of the Rig boundary: {leaked:?}\n\
         Rig owns AI execution primitives; Moira owns config, identity, credentials and routing. \
         Route the dependency through `src/orchestration/runtime_factory.rs` instead, or — if the \
         boundary genuinely needs to widen — add the file to RIG_BOUNDARY_FILES so the decision is \
         visible in the diff."
    );

    // The other direction: if a boundary file stops using Rig, the allow-list entry is stale and
    // is now silently permitting a file that a future edit could leak through unnoticed.
    let stale: Vec<&str> = permitted.difference(&referencing).copied().collect();
    assert!(
        stale.is_empty(),
        "RIG_BOUNDARY_FILES lists {stale:?}, which no longer reference `rig_core` — drop the \
         entries so the allow-list keeps meaning what it says"
    );
}

#[test]
fn src_domain_is_free_of_rig() {
    let domain: Vec<(String, String)> = source_files()
        .into_iter()
        .filter(|(path, _)| path.starts_with("src/domain/"))
        .collect();

    assert!(
        domain.len() >= 5,
        "expected several modules under src/domain/ but found {} — the filter or the layout \
         changed, and this assertion would otherwise pass vacuously",
        domain.len()
    );

    let leaked: Vec<&str> = domain
        .iter()
        .filter(|(_, body)| references_rig(body))
        .map(|(path, _)| path.as_str())
        .collect();

    assert!(
        leaked.is_empty(),
        "src/domain/ must not depend on `rig_core` (plan 06, P2-2), but these do: {leaked:?}. A \
         domain type defined in terms of a Rig type makes every Rig upgrade a domain-model change."
    );
}
