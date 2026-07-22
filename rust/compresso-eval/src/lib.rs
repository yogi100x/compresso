//! Controlled fixture loading, evaluation runners, and release-gate primitives.

pub mod cli;
pub mod fixtures;
pub mod gates;
pub mod recall;
pub mod runner;

pub use fixtures::{
    AtlasExpectations, EvalError, Expectations, Fixture, FixtureBudget, FixtureCategory,
    FixtureInput, FixtureSet, RecallProbe, RequiredEdge,
};
pub use gates::{
    GateError, GateFailureReason, GateName, GateOperator, GateOutcome, GateReport, Gates,
    LatencyGates, RecallGates, ReductionGates, SafetyGates,
};
pub use recall::{EvalRunError, FixtureRecallOutcome, RecallProbeOutcome, RecallReport};
pub use runner::{
    AggregateMetrics, AtlasMetrics, EvalReport, EvalRunner, EvaluationKind, ExpectationFailure,
    FixtureOutcome, RunnerStage,
};
