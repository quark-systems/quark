//! The native resolver against firstmate's, on recorded cases.
//!
//! Each `tests/parity/*.json` holds a rules file, the classifier's HTTP
//! response, a `quota-axi --json` snapshot, and what firstmate's
//! `fm-dispatch-resolve.sh` decided from them (recorded by running the
//! script with stub `curl` and `quota-axi`). The native resolver must make
//! the same decision: status, selected profile, and each candidate's
//! eligibility.

use std::path::Path;

use quark_dispatch::classify::{parse_answer, Classification};
use quark_dispatch::shadow::decision;
use quark_dispatch::{
    resolve, ClassifierSettings, DispatchConfig, ProviderFamilies, QuotaSnapshot,
};
use serde_json::Value;

#[test]
fn matches_firstmate_on_every_recorded_case() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity");
    let mut cases = 0;
    let mut failures = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let case: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let config = DispatchConfig::parse(&case["rules"].to_string()).unwrap();
        let settings = ClassifierSettings::from_config(&config, true);
        let classification = if case["http_code"] == 200 {
            match parse_answer(&case["classifier_response"], config.rules.len()) {
                Ok(answer) => Classification::Answered { answer },
                Err(reason) => Classification::Failed { reason },
            }
        } else {
            Classification::Failed {
                reason: format!("http {}", case["http_code"]),
            }
        };
        let quota = QuotaSnapshot::parse_checked(&case["quota"].to_string())
            .map_err(|_| "quota-axi --json returned an invalid snapshot".to_string());
        let native = resolve(
            &config,
            &settings,
            Some(&classification),
            quota.as_ref().map_err(String::as_str),
            &ProviderFamilies::builtin(),
        );
        let mut want = case["firstmate"].clone();
        // An error carries no candidates or profile in either.
        if want["status"] == "error" {
            want["candidates"] = Value::Array(Vec::new());
        }
        let got = decision(&native);
        if got != want {
            failures.push(format!(
                "{}:\n  firstmate {want}\n  native    {got}\n  native reason {:?}",
                path.file_name().unwrap().to_string_lossy(),
                native.reason
            ));
        }
        cases += 1;
    }
    assert!(cases >= 20, "only {cases} cases");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
