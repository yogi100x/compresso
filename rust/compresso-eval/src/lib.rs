//! Controlled fixture loading, evaluation runners, and release-gate primitives.

pub mod fixtures;
pub mod runner;

pub use fixtures::{
    AtlasExpectations, EvalError, Expectations, Fixture, FixtureBudget, FixtureCategory,
    FixtureInput, FixtureSet, RecallProbe, RequiredEdge,
};
pub use runner::{
    AggregateMetrics, AtlasMetrics, EvalReport, EvalRunner, EvaluationKind, ExpectationFailure,
    FixtureOutcome, RunnerStage,
};
