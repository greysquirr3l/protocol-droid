//! Per-revision codecs.
//!
//! One module per wire type, each speaking one revision. They are adapters:
//! they know the generated shapes and the IR, and nothing else. Every
//! judgement about what a revision can express is made by the gate in
//! [`crate::domain::ir::lower`], not here — a codec that formed a second
//! opinion is how two revisions start disagreeing about what they support.

pub mod result;
