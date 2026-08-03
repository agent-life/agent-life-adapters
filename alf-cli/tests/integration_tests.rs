use chrono::Utc;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn fixture_path() -> PathBuf {
    std::env::var_os("ALF_INTEGRATION_FIXTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            path.push("fixtures/synthetic-agent.alf");
            path
        })
}

fn schema_revision() -> String {
    if let Ok(revision) = std::env::var("ALF_INTEGRATION_SCHEMA_REVISION") {
        return revision;
    }
    let mut version_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    version_path.push("fixtures/schema_version.txt");
    fs::read_to_string(version_path)
        .map(|value| value.trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

fn report_value(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| "unknown".to_string())
}

fn report_label(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

#[test]
fn test_synthetic_data_validation() {
    let fixture_path = fixture_path();
    assert!(
        fixture_path.exists(),
        "Synthetic test data not found at {}. Run ./scripts/run_integration_tests.sh.",
        fixture_path.display()
    );

    let output = Command::new(env!("CARGO_BIN_EXE_alf"))
        .arg("validate")
        .arg(&fixture_path)
        .output()
        .expect("Failed to execute alf validate");
    let schema_revision = schema_revision();

    if let Ok(report_dir) = std::env::var("ALF_INTEGRATION_REPORT_DIR") {
        let report_dir = PathBuf::from(report_dir);
        fs::create_dir_all(&report_dir).expect("Failed to create integration report directory");
        let report_path = report_dir.join(format!(
            "integration_test_report_{}.md",
            report_label(&schema_revision)
        ));
        let status = if output.status.success() {
            "SUCCESS"
        } else {
            "FAILED"
        };
        let report = format!(
            "# ALF CLI Integration Test Report\n\n            **Adapter Commit:** {}\n            **Schema Commit:** {}\n            **Fixture ALF Format Version:** {}\n            **Fixture SHA-256:** {}\n            **Generator Python:** {}\n            **Generator Requirements Lock SHA-256:** {}\n            **JSF Version:** {}\n            **Faker Version:** {}\n            **Fixture Path:** {}\n            **Timestamp:** {}\n            **Status:** {}\n\n            ## alf validate Output\n\n{}\n\n            ## alf validate Errors\n\n{}\n",
            report_value("ALF_INTEGRATION_ADAPTER_COMMIT"),
            schema_revision,
            report_value("ALF_INTEGRATION_FIXTURE_ALF_VERSION"),
            report_value("ALF_INTEGRATION_FIXTURE_SHA256"),
            report_value("ALF_INTEGRATION_PYTHON_VERSION"),
            report_value("ALF_INTEGRATION_REQUIREMENTS_LOCK_SHA256"),
            report_value("ALF_INTEGRATION_JSF_VERSION"),
            report_value("ALF_INTEGRATION_FAKER_VERSION"),
            fixture_path.display(),
            Utc::now().to_rfc3339(),
            status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        fs::write(report_path, report).expect("Failed to write integration report");
    }

    if !output.status.success() {
        panic!(
            "Validation failed.\nSTDOUT:\n{}\nSTDERR:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}
