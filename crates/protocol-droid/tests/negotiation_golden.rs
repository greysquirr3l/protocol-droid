//! Golden snapshot of the negotiation table.
//!
//! # Why this is a snapshot and not the unit test
//!
//! `the_whole_offered_by_supported_table` in the domain module already asserts
//! the *properties* — echo when supported, answer with latest when not, never
//! answer with something you do not speak. Those assertions are the contract
//! and they stay.
//!
//! This file snapshots the *table*, because property assertions cannot catch a
//! change that is still correct but different. If the supported set is ever
//! reordered or a revision added, the table moves and a reviewer sees exactly
//! which cells changed. A property test passes silently.
//!
//! What the table makes visible is the shape of the protocol: a run of echoes,
//! an "unknown" row that answers rather than erroring, and a client table that
//! fails only where the two supported sets are disjoint.

#![expect(
    clippy::unwrap_used,
    reason = "a golden table needs every cell to exist, so the fixture sets are unwrapped with the failing precondition in the panic message"
)]

use std::fmt::Write as _;

use protocol_droid::domain::{
    ProtocolVersion, ServerDecision, SupportedVersions, negotiate_client, negotiate_server,
};

/// A future revision, which is a real input rather than an error case.
fn future() -> ProtocolVersion {
    ProtocolVersion::unknown_after_known("2099-01-01".parse().unwrap()).unwrap()
}

/// The fixture: a deliberately awkward supported set, so the table has
/// interesting interior cells rather than being all-echo or all-counter-offer.
/// It has a gap (`2025-06-18` is absent) and it does not include the newest
/// revision, which is what makes the counter-offer row say something.
fn supported() -> SupportedVersions {
    SupportedVersions::new(vec![
        ProtocolVersion::V2024_11_05,
        ProtocolVersion::V2025_03_26,
        ProtocolVersion::V2025_11_25,
    ])
    .unwrap()
}

fn every_offer() -> Vec<ProtocolVersion> {
    let mut offered = ProtocolVersion::KNOWN.to_vec();
    offered.push(future());
    offered
}

#[test]
fn the_server_negotiation_table_is_golden() {
    let supported = supported();
    let mut table = String::new();
    table.push_str("server supports: 2024-11-05, 2025-03-26, 2025-11-25\n\n");
    for offer in every_offer() {
        let cell = match negotiate_server(offer, &supported) {
            ServerDecision::Echoed(v) => format!("echo     {v}"),
            ServerDecision::CounterOffered { offered, answer } => {
                format!("counter  {offered} -> {answer}")
            }
        };
        let _ = writeln!(table, "  offered {offer:<12} {cell}");
    }
    table.push('\n');
    insta::assert_snapshot!("server_negotiation", table);
}

#[test]
fn the_client_negotiation_table_is_golden() {
    let mut table = String::new();
    table.push_str("the client accepts exactly when the server's answer is in its own set\n\n");
    for client_slice in [
        vec![ProtocolVersion::V2024_11_05],
        vec![ProtocolVersion::V2025_06_18, ProtocolVersion::V2025_11_25],
        ProtocolVersion::KNOWN.to_vec(),
    ] {
        let client = SupportedVersions::new(client_slice.clone()).unwrap();
        let _ = writeln!(table, "  client speaks {client_slice:?}");
        for answer in every_offer() {
            let cell = match negotiate_client(ProtocolVersion::V2026_07_28, answer, &client) {
                Ok(v) => format!("accept  {v}"),
                // The rendered error, because what the operator actually sees
                // is the thing worth snapshotting.
                Err(e) => format!("reject  {e}"),
            };
            let _ = writeln!(table, "    server answers {answer:<12} {cell}");
        }
        table.push('\n');
    }
    insta::assert_snapshot!("client_negotiation", table);
}
