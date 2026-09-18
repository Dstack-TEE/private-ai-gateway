//! The `aci` CLI must not grow a second verifier.
//!
//! Verification steps belong in `src/aci`, where the gateway's own verifier
//! consumes them; the CLI maps their outcomes to transcript lines. When both
//! sides implement a step, they drift — and both sides keep passing their own
//! tests while disagreeing about the same service.

use std::fs;
use std::path::Path;

/// Primitives that decide whether evidence is genuine. Calling one from the
/// CLI means a verification step was implemented there instead of shared.
const VERIFICATION_PRIMITIVES: &[&str] = &[
    "dcap_qvl::verify",
    "dcap_qvl::collateral",
    "ed25519_dalek",
    "k256",
    "p256",
];

fn shipped_code(source: &str) -> &str {
    match source.find("\n#[cfg(test)]\n") {
        Some(i) => &source[..i],
        None => source,
    }
}

fn test_only_modules(main_rs: &str) -> Vec<String> {
    main_rs
        .lines()
        .zip(main_rs.lines().skip(1))
        .filter(|(attr, _)| attr.trim() == "#[cfg(test)]")
        .filter_map(|(_, decl)| {
            decl.trim()
                .strip_prefix("mod ")
                .and_then(|rest| rest.strip_suffix(';'))
                .map(str::to_string)
        })
        .collect()
}

#[test]
fn the_cli_calls_no_verification_primitive_directly() {
    let dir = Path::new("src/bin/aci");
    let main_rs = fs::read_to_string(dir.join("main.rs")).expect("CLI entry point");
    let test_only = test_only_modules(&main_rs);
    assert!(
        test_only.iter().any(|m| m == "spec_fixtures"),
        "expected the fixture module to be test-only; the scan below would skip shipped code"
    );

    let mut offenders = Vec::new();
    for entry in fs::read_dir(dir).expect("CLI source directory") {
        let path = entry.expect("directory entry").path();
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        if test_only.iter().any(|m| *m == stem) {
            continue;
        }
        let source = fs::read_to_string(&path).expect("readable source");
        for (n, line) in shipped_code(&source).lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for primitive in VERIFICATION_PRIMITIVES {
                if line.contains(primitive) {
                    offenders.push(format!(
                        "{}:{}: {primitive} — put this step in src/aci and call it from both \
                         verifiers\n    {}",
                        path.display(),
                        n + 1,
                        line.trim()
                    ));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the CLI verifies evidence directly instead of sharing the step:\n{}",
        offenders.join("\n")
    );
}
