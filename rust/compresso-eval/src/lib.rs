//! Controlled fixture loading, evaluation runners, and release-gate primitives.

pub mod fixtures;

pub use fixtures::{
    AtlasExpectations, EvalError, Expectations, Fixture, FixtureBudget, FixtureCategory,
    FixtureInput, FixtureSet, RecallProbe, RequiredEdge,
};
