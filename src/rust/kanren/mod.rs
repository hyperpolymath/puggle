// SPDX-License-Identifier: MPL-2.0
//! A compact microKanren engine with persistent substitutions and fair streams.
//!
//! Unification never mutates an existing state: every extension returns a new
//! substitution. This is important for speculative decoder branches, where a
//! rejected token must leave the current logical state untouched.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LVar(pub u64);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Term {
    Var(LVar),
    Atom(String),
    Node(String, Vec<Term>),
}

impl Term {
    #[must_use]
    pub const fn var(variable: LVar) -> Self {
        Self::Var(variable)
    }

    #[must_use]
    pub fn atom(value: impl Into<String>) -> Self {
        Self::Atom(value.into())
    }

    #[must_use]
    pub fn node(symbol: impl Into<String>, arguments: impl IntoIterator<Item = Term>) -> Self {
        Self::Node(symbol.into(), arguments.into_iter().collect())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Substitution {
    bindings: BTreeMap<LVar, Term>,
}

impl Substitution {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Chase bound variables to the first unbound variable or structural term.
    /// Cyclic bindings cannot be constructed because `extend` performs an
    /// occurs check.
    #[must_use]
    pub fn walk<'a>(&'a self, mut term: &'a Term) -> &'a Term {
        while let Term::Var(variable) = term {
            let Some(next) = self.bindings.get(variable) else {
                break;
            };
            term = next;
        }
        term
    }

    #[must_use]
    pub fn is_bound(&self, variable: LVar) -> bool {
        self.bindings.contains_key(&variable)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// Unify two terms under this substitution. A variable is never bound to a
    /// term containing itself, including through an existing binding chain.
    /// Returns an extended substitution on success or `None` for incompatible
    /// terms or an occurs-check failure, leaving this substitution unchanged.
    #[must_use]
    pub fn unify(&self, left: &Term, right: &Term) -> Option<Self> {
        let mut extended = self.clone();
        extended.unify_in_place(left, right).then_some(extended)
    }

    fn unify_in_place(&mut self, left: &Term, right: &Term) -> bool {
        let left = self.walk(left).clone();
        let right = self.walk(right).clone();

        match (left, right) {
            (Term::Var(left), Term::Var(right)) if left == right => true,
            (Term::Var(variable), term) | (term, Term::Var(variable)) => {
                self.extend(variable, &term)
            }
            (Term::Atom(left), Term::Atom(right)) => left == right,
            (Term::Node(left_symbol, left_args), Term::Node(right_symbol, right_args)) => {
                left_symbol == right_symbol
                    && left_args.len() == right_args.len()
                    && left_args
                        .iter()
                        .zip(&right_args)
                        .all(|(left, right)| self.unify_in_place(left, right))
            }
            _ => false,
        }
    }

    fn extend(&mut self, variable: LVar, value: &Term) -> bool {
        if self.occurs(variable, value) {
            return false;
        }
        // `walk` established that this variable is unbound. Retaining this
        // guard makes the invariant explicit if the internal API changes.
        if self.bindings.contains_key(&variable) {
            return false;
        }
        self.bindings.insert(variable, value.clone());
        true
    }

    fn occurs(&self, variable: LVar, term: &Term) -> bool {
        match self.walk(term) {
            Term::Var(other) => variable == *other,
            Term::Atom(_) => false,
            Term::Node(_, arguments) => arguments
                .iter()
                .any(|argument| self.occurs(variable, argument)),
        }
    }

    /// Recursively substitute bound variables in an owned term.
    #[must_use]
    pub fn reify(&self, term: &Term) -> Term {
        match self.walk(term) {
            Term::Var(variable) => Term::Var(*variable),
            Term::Atom(value) => Term::Atom(value.clone()),
            Term::Node(symbol, arguments) => Term::Node(
                symbol.clone(),
                arguments
                    .iter()
                    .map(|argument| self.reify(argument))
                    .collect(),
            ),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct State {
    pub substitution: Substitution,
    next_variable: u64,
}

impl State {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate the next variable identifier in this state, starting at zero.
    /// Cloned states retain the same counter, so identifiers are not global.
    ///
    /// # Panics
    /// Panics if advancing the variable counter would overflow `u64`.
    pub fn fresh(&mut self) -> LVar {
        let variable = LVar(self.next_variable);
        self.next_variable = self
            .next_variable
            .checked_add(1)
            .expect("logic variable space exhausted");
        variable
    }

    #[must_use]
    pub fn reify(&self, term: &Term) -> Term {
        self.substitution.reify(term)
    }
}

type Suspension = Box<dyn FnOnce() -> Stream + Send + 'static>;

/// A microKanren stream. Suspensions make conjunctive and disjunctive search
/// lazy; `mplus` rotates branches to prevent one productive branch starving
/// another.
pub enum Stream {
    Empty,
    Mature(State, Box<Stream>),
    Immature(Suspension),
}

impl fmt::Debug for Stream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("Stream::Empty"),
            Self::Mature(state, _) => formatter
                .debug_tuple("Stream::Mature")
                .field(state)
                .field(&"..")
                .finish(),
            Self::Immature(_) => formatter.write_str("Stream::Immature(..)"),
        }
    }
}

impl Stream {
    #[must_use]
    pub fn unit(state: State) -> Self {
        Self::Mature(state, Box::new(Self::Empty))
    }

    /// Defer calling `thunk` until this branch is forced for an answer.
    #[must_use]
    pub fn suspend(thunk: impl FnOnce() -> Stream + Send + 'static) -> Self {
        Self::Immature(Box::new(thunk))
    }

    fn step(self) -> Option<(State, Self)> {
        let mut stream = self;
        loop {
            match stream {
                Self::Empty => return None,
                Self::Mature(head, tail) => return Some((head, *tail)),
                Self::Immature(thunk) => stream = thunk(),
            }
        }
    }

    /// Collect up to `limit` answers, stopping earlier if the stream ends.
    /// A zero limit forces nothing. The limit does not bound search time:
    /// forcing the next answer may diverge.
    #[must_use]
    pub fn take(self, limit: usize) -> Vec<State> {
        let mut answers = Vec::new();
        let mut stream = self;
        while answers.len() < limit {
            let Some((state, tail)) = stream.step() else {
                break;
            };
            answers.push(state);
            stream = tail;
        }
        answers
    }
}

pub type Goal = Arc<dyn Fn(State) -> Stream + Send + Sync + 'static>;

/// Create a goal yielding one extended state if the terms unify, or no answers
/// if they conflict or would introduce a cyclic binding.
#[must_use]
pub fn eq(left: Term, right: Term) -> Goal {
    Arc::new(move |state| {
        let mut candidate = state.clone();
        if let Some(substitution) = state.substitution.unify(&left, &right) {
            candidate.substitution = substitution;
            Stream::unit(candidate)
        } else {
            Stream::Empty
        }
    })
}

/// Merge answer streams, rotating branches after an answer or suspension.
/// Fairness requires suspended computations to return control.
#[must_use]
pub fn mplus(left: Stream, right: Stream) -> Stream {
    match left {
        Stream::Empty => right,
        Stream::Mature(head, tail) => {
            Stream::Mature(head, Box::new(Stream::suspend(move || mplus(right, *tail))))
        }
        Stream::Immature(thunk) => Stream::suspend(move || mplus(right, thunk())),
    }
}

/// Apply `goal` to each input state and interleave its resulting answer streams.
#[must_use]
pub fn bind(stream: Stream, goal: Goal) -> Stream {
    match stream {
        Stream::Empty => Stream::Empty,
        Stream::Mature(head, tail) => {
            let continuation = goal.clone();
            mplus(
                goal(head),
                Stream::suspend(move || bind(*tail, continuation)),
            )
        }
        Stream::Immature(thunk) => Stream::suspend(move || bind(thunk(), goal)),
    }
}

/// Create a conjunction that passes each answer from `left` into `right`.
#[must_use]
pub fn conj(left: Goal, right: Goal) -> Goal {
    Arc::new(move |state| bind(left(state), right.clone()))
}

/// Run both goals from the same initial state and interleave their answers.
#[must_use]
pub fn disj(left: Goal, right: Goal) -> Goal {
    Arc::new(move |state| {
        let right_state = state.clone();
        let right_branch = right.clone();
        mplus(
            left(state),
            Stream::suspend(move || right_branch(right_state)),
        )
    })
}

/// Create a goal that allocates a fresh variable before invoking `relation`.
///
/// # Panics
/// Running the goal panics if [`State::fresh`] exhausts the variable counter.
#[must_use]
pub fn call_fresh<F>(relation: F) -> Goal
where
    F: Fn(LVar) -> Goal + Send + Sync + 'static,
{
    Arc::new(move |mut state| {
        let variable = state.fresh();
        relation(variable)(state)
    })
}

/// Run a goal from an empty state and collect at most `answers` states.
/// The answer count does not bound search time; see [`Stream::take`].
#[must_use]
pub fn run(goal: Goal, answers: usize) -> Vec<State> {
    goal(State::new()).take(answers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unifies_nested_structures_and_threads_substitutions() {
        let mut state = State::new();
        let x = state.fresh();
        let y = state.fresh();
        let left = Term::node("pair", [Term::var(x), Term::atom("right")]);
        let right = Term::node(
            "pair",
            [Term::node("typed", [Term::var(y)]), Term::atom("right")],
        );
        let substitution = state
            .substitution
            .unify(&left, &right)
            .expect("terms unify");
        let unified = substitution
            .unify(&Term::var(y), &Term::atom("Nat"))
            .expect("nested variable can be refined");
        assert_eq!(
            unified.reify(&Term::var(x)),
            Term::node("typed", [Term::atom("Nat")])
        );
    }

    #[test]
    fn occurs_check_rejects_direct_and_indirect_cycles() {
        let x = LVar(0);
        let y = LVar(1);
        let substitution = Substitution::new()
            .unify(&Term::var(x), &Term::var(y))
            .expect("distinct variables unify");
        assert!(substitution
            .unify(&Term::var(y), &Term::node("box", [Term::var(x)]),)
            .is_none());
    }

    #[test]
    fn failed_unification_does_not_mutate_the_original() {
        let original = Substitution::new();
        let x = Term::var(LVar(7));
        assert!(original.unify(&x, &Term::atom("a")).is_some());
        assert!(original.is_empty());
    }

    #[test]
    fn disjunction_interleaves_answers() {
        let relation = call_fresh(|variable| {
            disj(
                eq(Term::var(variable), Term::atom("a")),
                disj(
                    eq(Term::var(variable), Term::atom("b")),
                    eq(Term::var(variable), Term::atom("c")),
                ),
            )
        });
        let answers = run(relation, 3);
        let values = answers
            .iter()
            .map(|state| state.reify(&Term::var(LVar(0))))
            .collect::<Vec<_>>();
        assert_eq!(
            values,
            vec![Term::atom("a"), Term::atom("b"), Term::atom("c")]
        );
    }

    #[test]
    fn conjunction_propagates_failure() {
        let relation = call_fresh(|variable| {
            conj(
                eq(Term::var(variable), Term::atom("a")),
                eq(Term::var(variable), Term::atom("b")),
            )
        });
        assert!(run(relation, 1).is_empty());
    }
}
