use super::*;

pub(crate) async fn characterization_provider_sources() -> [(&'static str, String); 2] {
    [
        (
            "chutes_envelope",
            chutes::crypto::tests::characterization_source().await,
        ),
        (
            "zero_g_envelope",
            zero_g::tests::characterization_source().await,
        ),
    ]
}

pub(crate) use super::zero_g::tests::{Fixture as ZeroGTerminalFixture, Mode as ZeroGTerminalMode};
