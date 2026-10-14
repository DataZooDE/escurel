//! The Evolve event labels, in ONE place.
//!
//! The gateway's capture gate, the event ACL, the runner's skip list and the lineage attestation
//! all used to carry their own copy of "which labels are Evolve control clicks". They share this
//! table instead, so adding a label is one line and the lists cannot drift apart.
//!
//! * A **gated** label is a user click whose capture binds the exact page revision the person
//!   reviewed (`evolve_run` plus every control label).
//! * A **control** label is a gated label that never reaches the runner and is private to the
//!   person who clicked: the owner-gated review actions. `evolve_run` is gated but not a control
//!   label: a plan-mode click is dispatchable and its target is checked with the event-target ACL.
//! * A **system** label is a receipt Evolve writes about a lineage root.

/// (label, target skill) for every owner-gated control click.
pub const CONTROL_TARGETS: &[(&str, &str)] = &[
    ("evolve_preflight", "evolve_problem"),
    ("evolve_prepare_source", "evolve_training_source"),
    ("evolve_validate", "evolve_experiment"),
    ("evolve_publish_candidate", "evolve_validation_report"),
    ("evolve_compare", "evolve_comparison"),
];

/// The run label: a spend against an owner-authored problem page.
pub const RUN_LABEL: &str = "evolve_run";
const RUN_TARGET: &str = "evolve_problem";

/// (system receipt label, the user label at the root of its lineage).
pub const SYSTEM_ROOTS: &[(&str, &str)] = &[
    ("evolve:admission", RUN_LABEL),
    ("evolve:preflight", "evolve_preflight"),
    ("evolve:training-source", "evolve_prepare_source"),
    ("evolve:validation", "evolve_validate"),
    ("evolve:candidate", "evolve_publish_candidate"),
];

/// The skill the click's target page must be an instance of, for any gated label.
pub fn target_skill(label: &str) -> Option<&'static str> {
    if label == RUN_LABEL {
        return Some(RUN_TARGET);
    }
    CONTROL_TARGETS.iter().find(|(l, _)| *l == label).map(|(_, t)| *t)
}

/// Owner-gated control click: private to its author and never run by the runner.
pub fn is_control_label(label: &str) -> bool {
    CONTROL_TARGETS.iter().any(|(l, _)| *l == label)
}

/// Any click whose capture binds a reviewed page revision (`evolve_run` or a control label).
pub fn is_gated_label(label: &str) -> bool {
    label == RUN_LABEL || is_control_label(label)
}

/// A receipt label Evolve writes about a lineage root.
pub fn is_system_label(label: &str) -> bool {
    SYSTEM_ROOTS.iter().any(|(l, _)| *l == label)
}

/// The user label a system receipt's lineage root must carry. An unknown system label falls back
/// to the run label, as the ACL always did.
pub fn root_label_for_system(label: &str) -> &'static str {
    SYSTEM_ROOTS
        .iter()
        .find(|(l, _)| *l == label)
        .map(|(_, r)| *r)
        .unwrap_or(RUN_LABEL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_control_labels_and_targets_are_unchanged() {
        let expected = [
            ("evolve_preflight", "evolve_problem"),
            ("evolve_prepare_source", "evolve_training_source"),
            ("evolve_validate", "evolve_experiment"),
            ("evolve_publish_candidate", "evolve_validation_report"),
            ("evolve_compare", "evolve_comparison"),
        ];
        for (label, target) in expected {
            assert!(is_control_label(label), "{label}");
            assert!(is_gated_label(label), "{label}");
            assert_eq!(target_skill(label), Some(target), "{label}");
        }
    }

    #[test]
    fn evolve_run_is_gated_but_not_a_control_label() {
        assert_eq!(target_skill("evolve_run"), Some("evolve_problem"));
        assert!(is_gated_label("evolve_run"));
        assert!(!is_control_label("evolve_run"));
    }

    #[test]
    fn unrelated_labels_are_not_gated() {
        for l in ["", "evolve", "evolve_rules", "evolve_rules_get", "triage", "evolve:preflight"] {
            assert!(!is_gated_label(l), "{l}");
            assert!(!is_control_label(l), "{l}");
            assert_eq!(target_skill(l), None, "{l}");
        }
    }

    #[test]
    fn system_receipts_map_to_their_root_label() {
        assert_eq!(root_label_for_system("evolve:preflight"), "evolve_preflight");
        assert_eq!(root_label_for_system("evolve:training-source"), "evolve_prepare_source");
        assert_eq!(root_label_for_system("evolve:validation"), "evolve_validate");
        assert_eq!(root_label_for_system("evolve:candidate"), "evolve_publish_candidate");
        assert_eq!(root_label_for_system("evolve:admission"), "evolve_run");
        for s in ["evolve:admission", "evolve:preflight", "evolve:training-source",
                  "evolve:validation", "evolve:candidate"] {
            assert!(is_system_label(s), "{s}");
        }
        assert!(!is_system_label("evolve:other"));
    }

    #[test]
    fn every_control_label_has_a_target() {
        for (l, t) in CONTROL_TARGETS {
            assert_eq!(target_skill(l), Some(*t), "{l}");
        }
    }
}
