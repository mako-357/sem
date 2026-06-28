//! `sem conflicts` — semantic conflict detection between two branches.
//!
//! Detects "textually clean but semantically broken" merges: entities that two
//! branches both changed, divergently, relative to their merge-base. git's
//! line-based merge can resolve these cleanly while the result is broken (e.g.
//! two branches edit `authenticateUser()` differently); sem flags them at the
//! entity (function/class/method) level *before* the merge.
//!
//! Foundation (all already in sem-core): merge-base resolution, entity-level diff
//! (`compute_semantic_diff`), and `structural_hash` for ignoring formatting-only
//! divergence. Deliberately does NOT depend on the weaker token-based reference
//! resolution, so the signal is trustworthy.
//!
//! STATUS: scaffold. Candidate detection (entities modified on both sides) is
//! implemented; the structural-divergence refinement is the next-session core
//! (see `diverges` TODO).

use std::collections::HashMap;
use std::path::Path;

use sem_core::git::bridge::GitBridge;
use sem_core::git::types::DiffScope;
use sem_core::model::change::{ChangeType, SemanticChange};
use sem_core::parser::differ::compute_semantic_diff;
use sem_core::parser::registry::ParserRegistry;

pub struct ConflictsOptions {
    pub cwd: String,
    pub base: String,
    pub head: String,
    pub json: bool,
}

/// One entity that both branches changed divergently relative to the merge-base.
pub struct ConflictCandidate {
    pub entity_id: String,
    pub entity_name: String,
    pub entity_type: String,
    pub file_path: String,
}

pub fn conflicts_command(opts: ConflictsOptions) {
    match detect_conflicts(&opts) {
        Ok(conflicts) => {
            report(&conflicts, opts.json);
            // Non-zero exit when conflicts exist, so CI and agent gates can branch on it.
            if !conflicts.is_empty() {
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    }
}

fn detect_conflicts(opts: &ConflictsOptions) -> Result<Vec<ConflictCandidate>, String> {
    let git = GitBridge::open(Path::new(&opts.cwd))
        .map_err(|e| format!("not a git repository ({}): {e}", opts.cwd))?;
    let merge_base = git.resolve_merge_base(&opts.base, &opts.head).map_err(|e| {
        format!(
            "cannot resolve merge-base of {} and {}: {e}",
            opts.base, opts.head
        )
    })?;
    let registry = super::create_registry(&opts.cwd);

    // Entities changed on each side relative to the common ancestor.
    let base_changes = side_changes(&git, &registry, &merge_base, &opts.base)?;
    let head_changes = side_changes(&git, &registry, &merge_base, &opts.head)?;

    // Index the head side by entity id for O(1) cross-referencing.
    let head_by_id: HashMap<&str, &SemanticChange> = head_changes
        .iter()
        .filter(|c| c.change_type == ChangeType::Modified)
        .map(|c| (c.entity_id.as_str(), c))
        .collect();

    let mut conflicts = Vec::new();
    for base_change in base_changes
        .iter()
        .filter(|c| c.change_type == ChangeType::Modified)
    {
        // Only an entity changed on BOTH sides can be a semantic conflict.
        let Some(head_change) = head_by_id.get(base_change.entity_id.as_str()) else {
            continue;
        };
        if diverges(
            base_change.after_content.as_deref(),
            head_change.after_content.as_deref(),
        ) {
            conflicts.push(ConflictCandidate {
                entity_id: base_change.entity_id.clone(),
                entity_name: base_change.entity_name.clone(),
                entity_type: base_change.entity_type.clone(),
                file_path: base_change.file_path.clone(),
            });
        }
    }
    Ok(conflicts)
}

/// Entities changed on one branch relative to the merge-base.
fn side_changes(
    git: &GitBridge,
    registry: &ParserRegistry,
    merge_base: &str,
    head: &str,
) -> Result<Vec<SemanticChange>, String> {
    let files = git
        .get_changed_files(
            &DiffScope::Range {
                from: merge_base.to_string(),
                to: head.to_string(),
            },
            &[],
        )
        .map_err(|e| format!("diff {merge_base}..{head} failed: {e}"))?;
    Ok(compute_semantic_diff(files.as_slice(), registry, None, None).changes)
}

/// Whether two same-entity modifications actually diverge, given each side's
/// resulting entity content.
///
/// SCAFFOLD: a coarse check on the resulting content. TODO(next session — the core
/// of this feature): compare the AST-normalized `structural_hash` of each side's
/// resulting entity (re-extract from the content via the registry) so that
/// **formatting/comment-only divergence is NOT reported as a conflict**. That
/// refinement is the whole reason to build on `structural_hash`: it makes the signal
/// trustworthy (no false positives from two branches reformatting the same function).
/// If both sides reach the same structure, it is safe to merge.
fn diverges(base_after: Option<&str>, head_after: Option<&str>) -> bool {
    base_after != head_after
}

fn report(conflicts: &[ConflictCandidate], json: bool) {
    if json {
        let items: Vec<_> = conflicts
            .iter()
            .map(|c| {
                serde_json::json!({
                    "entityId": c.entity_id,
                    "entityName": c.entity_name,
                    "entityType": c.entity_type,
                    "filePath": c.file_path,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({ "conflicts": items, "count": conflicts.len() })
        );
        return;
    }

    if conflicts.is_empty() {
        println!("No semantic conflicts: no entity was changed divergently on both branches.");
        return;
    }
    println!("{} semantic conflict(s):", conflicts.len());
    for c in conflicts {
        println!(
            "  ⚠ {} {} — changed divergently on both branches ({})",
            c.entity_type, c.entity_name, c.file_path
        );
    }
}

#[cfg(test)]
mod tests {
    use super::diverges;

    #[test]
    fn diverges_when_after_content_differs() {
        assert!(diverges(Some("fn f() { 1 }"), Some("fn f() { 2 }")));
    }

    #[test]
    fn same_change_on_both_sides_is_not_a_conflict() {
        // Both branches made the identical edit — safe to merge.
        assert!(!diverges(Some("fn f() { 1 }"), Some("fn f() { 1 }")));
    }
}
