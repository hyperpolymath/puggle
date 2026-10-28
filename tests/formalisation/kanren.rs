// SPDX-License-Identifier: MPL-2.0

use puggle::kanren::{
    bind, call_fresh, conj, disj, eq, mplus, run, LVar, State, Stream, Substitution, Term,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[test]
fn reflexive_unification_does_not_create_a_binding() {
    let substitution = Substitution::new();
    assert!(substitution
        .unify(&Term::var(LVar(0)), &Term::var(LVar(0)))
        .unwrap()
        .is_empty());
    assert!(substitution
        .unify(&Term::atom("same"), &Term::atom("same"))
        .unwrap()
        .is_empty());
}

#[test]
fn structural_mismatches_and_partial_failures_preserve_existing_bindings() {
    let x = Term::var(LVar(0));
    let original = Substitution::new()
        .unify(&Term::var(LVar(1)), &Term::atom("kept"))
        .unwrap();
    for (left, right) in [
        (Term::atom("a"), Term::atom("b")),
        (Term::atom("pair"), Term::node("pair", [])),
        (Term::node("left", []), Term::node("right", [])),
        (Term::node("pair", [x.clone()]), Term::node("pair", [])),
        (
            Term::node("pair", [x.clone(), Term::atom("a")]),
            Term::node("pair", [Term::atom("temporary"), Term::atom("b")]),
        ),
    ] {
        assert!(original.unify(&left, &right).is_none());
        assert_eq!(original.len(), 1);
        assert!(!original.is_bound(LVar(0)));
        assert_eq!(original.reify(&Term::var(LVar(1))), Term::atom("kept"));
    }
}

#[test]
fn occurs_check_follows_aliases_inside_nested_terms() {
    let x = Term::var(LVar(0));
    let y = Term::var(LVar(1));
    let z = Term::var(LVar(2));
    assert!(Substitution::new()
        .unify(&x, &Term::node("self", [x.clone()]))
        .is_none());
    let aliases = Substitution::new()
        .unify(&x, &y)
        .unwrap()
        .unify(&y, &z)
        .unwrap();
    assert!(aliases
        .unify(&z, &Term::node("outer", [Term::node("inner", [x.clone()])]))
        .is_none());
    assert_eq!(aliases.walk(&x), &z);
    assert_eq!(aliases.len(), 2);
}

#[test]
fn reification_leaves_unbound_variables_and_resolves_nested_aliases() {
    let x = Term::var(LVar(0));
    let y = Term::var(LVar(1));
    let free = Term::var(LVar(2));
    let substitution = Substitution::new()
        .unify(&x, &y)
        .unwrap()
        .unify(&y, &Term::atom("Nat"))
        .unwrap();
    assert_eq!(
        substitution.reify(&Term::node("pair", [x, Term::node("box", [free.clone()])])),
        Term::node("pair", [Term::atom("Nat"), Term::node("box", [free])])
    );
}

#[test]
fn conjunction_filters_each_disjunct_without_leaking_substitutions() {
    let goal = call_fresh(|x| {
        conj(
            disj(
                eq(Term::var(x), Term::atom("a")),
                eq(Term::var(x), Term::atom("b")),
            ),
            eq(Term::var(x), Term::atom("b")),
        )
    });
    let answers = run(goal, 10);
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].reify(&Term::var(LVar(0))), Term::atom("b"));
}

#[test]
fn nested_fresh_variables_are_distinct_and_state_clones_keep_local_counters() {
    let goal = call_fresh(|x| {
        call_fresh(move |y| {
            conj(
                eq(Term::var(x), Term::atom("left")),
                eq(Term::var(y), Term::atom("right")),
            )
        })
    });
    let answers = run(goal, 1);
    assert_eq!(answers[0].reify(&Term::var(LVar(0))), Term::atom("left"));
    assert_eq!(answers[0].reify(&Term::var(LVar(1))), Term::atom("right"));
    let mut state = State::new();
    assert_eq!(state.fresh(), LVar(0));
    let mut branch = state.clone();
    assert_eq!(state.fresh(), LVar(1));
    assert_eq!(branch.fresh(), LVar(1));
}

#[test]
fn taking_zero_answers_does_not_force_a_suspension() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let stream = Stream::suspend(move || {
        observed.fetch_add(1, Ordering::SeqCst);
        Stream::unit(State::new())
    });
    assert!(stream.take(0).is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

fn repeating(value: &'static str) -> Stream {
    let state = run(eq(Term::var(LVar(0)), Term::atom(value)), 1)
        .pop()
        .unwrap();
    Stream::Mature(state, Box::new(Stream::suspend(move || repeating(value))))
}

#[test]
fn productive_infinite_branch_does_not_starve_a_finite_sibling() {
    let finite = eq(Term::var(LVar(0)), Term::atom("finite"))(State::new());
    let answers = mplus(repeating("infinite"), finite).take(3);
    let values: Vec<_> = answers
        .iter()
        .map(|state| state.reify(&Term::var(LVar(0))))
        .collect();
    assert_eq!(
        values,
        vec![
            Term::atom("infinite"),
            Term::atom("finite"),
            Term::atom("infinite")
        ]
    );
}

#[test]
fn suspended_nonproductive_branch_does_not_starve_a_ready_answer() {
    fn pending_forever() -> Stream {
        Stream::suspend(pending_forever)
    }
    let ready = eq(Term::var(LVar(0)), Term::atom("ready"))(State::new());
    let answers = mplus(pending_forever(), ready).take(1);
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].reify(&Term::var(LVar(0))), Term::atom("ready"));
}

#[test]
fn bind_handles_suspended_and_empty_input_streams() {
    let goal = eq(Term::var(LVar(0)), Term::atom("bound"));
    assert!(bind(Stream::Empty, goal.clone()).take(1).is_empty());
    let answers = bind(Stream::suspend(|| Stream::unit(State::new())), goal).take(2);
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].reify(&Term::var(LVar(0))), Term::atom("bound"));
}
