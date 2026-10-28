// SPDX-License-Identifier: MPL-2.0

use puggle::ast::{AstError, Binding, FormalAst, Sort, TargetProver};

#[test]
fn declaration_and_binder_identifiers_reject_injection_and_reserved_words() {
    for name in [
        "",
        "0name",
        "two words",
        "a\nb",
        "a-b",
        "λ",
        "x);",
        "axiom",
        "Type",
        "module",
    ] {
        for target in [TargetProver::Lean, TargetProver::Agda, TargetProver::Smt2] {
            assert_eq!(
                FormalAst::new(target, name, vec![], Sort::named("Nat")),
                Err(AstError::InvalidIdentifier {
                    kind: "declaration",
                    value: name.into()
                }),
                "target={target:?}, name={name:?}"
            );
            assert_eq!(
                FormalAst::new(
                    target,
                    "valid",
                    vec![Binding::new(name, Sort::named("Nat"))],
                    Sort::named("Nat")
                ),
                Err(AstError::InvalidIdentifier {
                    kind: "binder",
                    value: name.into()
                })
            );
        }
    }
}

#[test]
fn ascii_identifiers_and_unresolved_named_sorts_are_accepted() {
    for name in ["_", "_x0", "a", "UPPER_123"] {
        let ast = FormalAst::new(
            TargetProver::Lean,
            name,
            vec![],
            Sort::named("ExternalSort"),
        )
        .expect("named sorts are not resolved against a prover environment");
        assert_eq!(ast.name(), name);
        assert!(ast.validate().is_ok());
    }
}

#[test]
fn binder_cannot_reference_itself_or_a_later_binder() {
    for reference in ["x", "later"] {
        let result = FormalAst::new(
            TargetProver::Agda,
            "scoped",
            vec![
                Binding::new("x", Sort::variable(reference)),
                Binding::new("later", Sort::named("Type")),
            ],
            Sort::named("Nat"),
        );
        assert_eq!(result, Err(AstError::UnboundVariable(reference.into())));
    }
}

#[test]
fn duplicate_binders_are_rejected_even_with_different_sorts() {
    assert_eq!(
        FormalAst::new(
            TargetProver::Lean,
            "duplicate",
            vec![
                Binding::new("x", Sort::named("Nat")),
                Binding::new("x", Sort::named("Bool"))
            ],
            Sort::named("Nat")
        ),
        Err(AstError::DuplicateBinder("x".into()))
    );
}

#[test]
fn nested_function_sorts_validate_both_domain_and_codomain() {
    for sort in [
        Sort::function(Sort::variable("missing"), Sort::named("Nat")),
        Sort::function(
            Sort::named("Nat"),
            Sort::function(Sort::named("Bool"), Sort::variable("missing")),
        ),
    ] {
        assert_eq!(
            FormalAst::new(TargetProver::Lean, "nested", vec![], sort),
            Err(AstError::UnboundVariable("missing".into()))
        );
    }
    assert_eq!(
        FormalAst::new(
            TargetProver::Lean,
            "bad_sort",
            vec![],
            Sort::named("Nat -> Bool")
        ),
        Err(AstError::InvalidIdentifier {
            kind: "sort",
            value: "Nat -> Bool".into()
        })
    );
}

#[test]
fn smt_rejects_dependent_and_function_sorts_in_binders_and_results() {
    for (sort, error) in [
        (Sort::variable("A"), AstError::UnsupportedDependentSort),
        (
            Sort::function(Sort::named("Int"), Sort::named("Bool")),
            AstError::UnsupportedFunctionSort,
        ),
    ] {
        assert_eq!(
            FormalAst::new(TargetProver::Smt2, "f", vec![], sort.clone()),
            Err(error.clone())
        );
        assert_eq!(
            FormalAst::new(
                TargetProver::Smt2,
                "f",
                vec![
                    Binding::new("A", Sort::named("Type")),
                    Binding::new("x", sort)
                ],
                Sort::named("Int")
            ),
            Err(error)
        );
    }
}

#[test]
fn empty_telescopes_render_exactly_for_every_target() {
    for (target, rendered) in [
        (TargetProver::Lean, "axiom constant : Bool"),
        (TargetProver::Agda, "postulate\n  constant : Bool"),
        (TargetProver::Smt2, "(declare-fun constant () Bool)"),
    ] {
        assert_eq!(
            FormalAst::new(target, "constant", vec![], Sort::named("Bool"))
                .unwrap()
                .render(),
            rendered
        );
    }
}

#[test]
fn agda_renders_dependent_binders_and_nested_function_parentheses() {
    let ast = FormalAst::new(
        TargetProver::Agda,
        "apply",
        vec![
            Binding::new("A", Sort::named("Set")),
            Binding::new(
                "f",
                Sort::function(Sort::variable("A"), Sort::variable("A")),
            ),
        ],
        Sort::function(Sort::variable("A"), Sort::variable("A")),
    )
    .unwrap();
    assert_eq!(
        ast.render(),
        "postulate\n  apply : (A : Set) -> (f : (A) -> (A)) -> (A) -> (A)"
    );
    assert_eq!(
        ast.bindings()[1].sort(),
        &Sort::function(Sort::variable("A"), Sort::variable("A"))
    );
}

#[test]
fn canonical_format_has_a_stable_version_and_length_prefixes() {
    let ast = FormalAst::new(
        TargetProver::Lean,
        "id",
        vec![Binding::new("x", Sort::named("Nat"))],
        Sort::named("Nat"),
    )
    .unwrap();
    let canonical = "puggle-ast-v1;4:lean;2:id;bindings:1;1:x;7:n3:Nat;;result:7:n3:Nat;;";
    assert_eq!(ast.canonical(), canonical);
    assert_eq!(ast.digest(), *blake3::hash(canonical.as_bytes()).as_bytes());
    assert_eq!(ast.clone().digest(), ast.digest());
}

#[test]
fn digest_binds_target_binder_order_and_sort_constructor() {
    assert_ne!(
        Sort::named("A").canonical(),
        Sort::variable("A").canonical()
    );
    let make = |target, bindings, result| FormalAst::new(target, "f", bindings, result).unwrap();
    let base = make(
        TargetProver::Lean,
        vec![
            Binding::new("A", Sort::named("Type")),
            Binding::new("x", Sort::named("Nat")),
        ],
        Sort::named("Nat"),
    );
    for variant in [
        make(
            TargetProver::Agda,
            base.bindings().to_vec(),
            Sort::named("Nat"),
        ),
        make(
            TargetProver::Lean,
            base.bindings().iter().rev().cloned().collect(),
            Sort::named("Nat"),
        ),
        make(
            TargetProver::Lean,
            base.bindings().to_vec(),
            Sort::variable("A"),
        ),
    ] {
        assert_ne!(variant.canonical(), base.canonical());
        assert_ne!(variant.digest(), base.digest());
    }
    assert_ne!(
        Sort::function(
            Sort::named("A"),
            Sort::function(Sort::named("B"), Sort::named("C"))
        )
        .canonical(),
        Sort::function(
            Sort::function(Sort::named("A"), Sort::named("B")),
            Sort::named("C")
        )
        .canonical()
    );
}

#[test]
fn serde_target_labels_are_lowercase_and_unknown_targets_fail() {
    for target in [TargetProver::Lean, TargetProver::Agda, TargetProver::Smt2] {
        let json = format!("\"{}\"", target.as_str());
        assert_eq!(serde_json::to_string(&target).unwrap(), json);
        assert_eq!(serde_json::from_str::<TargetProver>(&json).unwrap(), target);
    }
    assert!(serde_json::from_str::<TargetProver>("\"Lean\"").is_err());
    assert!(serde_json::from_str::<TargetProver>("\"coq\"").is_err());
}
