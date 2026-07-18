//! Deterministic trigger evaluation over explicit item and lifecycle context.

pub mod adapters;
mod evaluator;

pub use evaluator::{CheckContext, ConditionEvaluation, EvaluationResult, evaluate};
