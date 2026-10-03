//! `TranslationReport` — the record of what lowering changed.
//!
//! # The property this type exists to make checkable
//!
//! "Nothing is silently dropped." That is the crate's claim, and a claim is
//! worth nothing without a way to check it. This report is the check: after any
//! translation, the caller can enumerate every rule that fired, see what it
//! cost, and decide whether the cost was one they agreed to.
//!
//! It matters most in the *quiet* case. A translation that succeeds and drops a
//! resource link is the one nobody notices, because there is no error to grep
//! for. A report turns it into a value that can be counted, logged, or
//! asserted on in a test — and the distinction between
//! [`Loss::None`] and [`Loss::Flagged`] is exactly the distinction between "we
//! agreed to this" and "we did not know".
//!
//! # Why it accumulates rather than summarising
//!
//! A count of "3 rules fired" loses the *which*, and the which is the part a
//! reviewer needs. Two results that both dropped content can be perfectly fine
//! and a serious bug, and the difference is whether it was a resource link the
//! caller agreed to lose or an image nobody mentioned.
//!
//! The count is still available via [`TranslationReport::len`], so both
//! questions can be asked.

use super::rule::{DegradeRule, Loss};
use crate::domain::ProtocolVersion;

/// One thing lowering did, and what it cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TranslationStep {
    /// The rule that fired.
    pub rule: DegradeRule,
    /// The revision the value was being lowered *to*, which is the only
    /// context that makes the step meaningful.
    pub target: ProtocolVersion,
}

impl TranslationStep {
    /// A step, with the revision it happened at.
    #[must_use]
    pub const fn new(rule: DegradeRule, target: ProtocolVersion) -> Self {
        Self { rule, target }
    }

    /// What the step cost.
    #[must_use]
    pub const fn loss(&self) -> Loss {
        self.rule.loss()
    }
}

/// What one translation changed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TranslationReport {
    steps: Vec<TranslationStep>,
    target: Option<ProtocolVersion>,
}

impl TranslationReport {
    /// A report for a translation to `target`, with nothing changed yet.
    #[must_use]
    pub const fn new(target: ProtocolVersion) -> Self {
        Self {
            steps: Vec::new(),
            target: Some(target),
        }
    }

    /// Records a rule firing.
    pub fn record(&mut self, rule: DegradeRule) {
        let target = self.target.unwrap_or_else(ProtocolVersion::latest_known);
        self.steps.push(TranslationStep::new(rule, target));
    }

    /// Whether anything changed.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// How many rules fired.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.steps.len()
    }

    /// The steps, in the order they were applied.
    #[must_use]
    pub fn steps(&self) -> &[TranslationStep] {
        &self.steps
    }

    /// The worst loss any step caused.
    ///
    /// [`None`] when nothing fired, so an empty report is lossless rather than
    /// accidentally alarming.
    #[must_use]
    pub fn worst_loss(&self) -> Loss {
        self.steps
            .iter()
            .map(TranslationStep::loss)
            .max()
            .unwrap_or(Loss::None)
    }

    /// Whether every step was one a caller could accept as "lossless or
    /// flagged".
    #[must_use]
    pub fn is_lossless_or_flagged(&self) -> bool {
        self.worst_loss().is_acceptable()
    }

    /// Whether the translation was lossless.
    #[must_use]
    pub fn is_lossless(&self) -> bool {
        self.worst_loss().is_lossless()
    }

    /// The features that were given up, deduplicated.
    ///
    /// Skips reductions, which name no feature — a caller asking "which features
    /// did this cost me" wants the answerable half, and a reduction has no
    /// feature because no revision forced it.
    #[must_use]
    pub fn lost_features(&self) -> Vec<crate::domain::Feature> {
        let mut out: Vec<crate::domain::Feature> = Vec::new();
        for step in &self.steps {
            let Some(feature) = step.rule.feature() else {
                continue;
            };
            if !out.contains(&feature) {
                out.push(feature);
            }
        }
        out
    }

    /// Merges another report into this one.
    ///
    /// Used when a batch of messages is translated and the caller wants one
    /// answer. The target is the receiver's — the reports being merged came
    /// from translations to it — so it is taken from `other`.
    pub fn absorb(&mut self, other: &Self) {
        if self.target.is_none() {
            self.target = other.target;
        }
        self.steps.extend_from_slice(&other.steps);
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::panic,
        reason = "tests panic with the failing precondition, which is the useful part"
    )]

    use super::TranslationReport;
    use crate::domain::ir::lower::rule::{DegradeRule, Loss};
    use crate::domain::{Feature, ProtocolVersion};

    fn report() -> TranslationReport {
        TranslationReport::new(ProtocolVersion::V2025_03_26)
    }

    #[test]
    fn a_fresh_report_is_lossless() {
        let report = report();
        assert!(report.is_empty());
        assert_eq!(report.len(), 0);
        assert_eq!(report.worst_loss(), Loss::None);
        assert!(report.is_lossless());
        assert!(report.is_lossless_or_flagged());
    }

    #[test]
    fn a_lossless_step_stays_lossless() {
        let mut report = report();
        report.record(DegradeRule::StructuredContentToText);
        assert_eq!(report.len(), 1);
        assert!(report.is_lossless());
        assert!(report.is_lossless_or_flagged());
        assert!(!report.is_empty());
    }

    /// The distinction that makes the report worth having.
    #[test]
    fn flagged_is_not_lossless_but_is_acceptable() {
        let mut report = report();
        report.record(DegradeRule::DropAnnotations);
        assert_eq!(report.worst_loss(), Loss::Flagged);
        assert!(!report.is_lossless());
        assert!(report.is_lossless_or_flagged());
    }

    #[test]
    fn a_destructive_step_is_neither() {
        let mut report = report();
        report.record(DegradeRule::AudioToTextPlaceholder);
        assert_eq!(report.worst_loss(), Loss::Destructive);
        assert!(!report.is_lossless());
        assert!(!report.is_lossless_or_flagged());
    }

    /// The worst loss wins, so a report containing one acceptable and one
    /// destructive step is destructive. Anything else would let a caller scan
    /// for a bad step and miss it.
    #[test]
    fn the_worst_loss_dominates() {
        let mut report = report();
        report.record(DegradeRule::StructuredContentToText);
        report.record(DegradeRule::AudioToTextPlaceholder);
        report.record(DegradeRule::DropAnnotations);
        assert_eq!(report.len(), 3);
        assert_eq!(report.worst_loss(), Loss::Destructive);
        assert!(!report.is_lossless_or_flagged());
    }

    #[test]
    fn every_step_keeps_the_target_revision() {
        let mut report = report();
        report.record(DegradeRule::DropAudio);
        let Some(step) = report.steps().first() else {
            panic!("expected one step");
        };
        assert_eq!(step.target, ProtocolVersion::V2025_03_26);
    }

    /// The *which* is the part a reviewer needs, so deduplication is only for
    /// the summary — the steps themselves keep every occurrence.
    #[test]
    fn steps_accumulate_and_features_deduplicate() {
        let mut report = report();
        report.record(DegradeRule::DropAudio);
        report.record(DegradeRule::AudioToTextPlaceholder);
        report.record(DegradeRule::DropImage);
        assert_eq!(report.len(), 3);
        // Both audio rules give up the same feature, so it appears once.
        let lost = report.lost_features();
        assert_eq!(lost, vec![Feature::ToolAudioContent]);
    }

    #[test]
    fn absorbing_merges_steps() {
        let mut first = report();
        first.record(DegradeRule::DropAnnotations);

        let mut second = TranslationReport::new(ProtocolVersion::V2025_03_26);
        second.record(DegradeRule::StructuredContentToText);

        first.absorb(&second);
        assert_eq!(first.len(), 2);
        assert!(first.is_lossless_or_flagged());
    }
}
