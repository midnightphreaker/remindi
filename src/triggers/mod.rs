//! Deterministic trigger evaluation over explicit item and lifecycle context.

mod evaluator;

pub use evaluator::{CheckContext, ConditionEvaluation, EvaluationResult, evaluate};
