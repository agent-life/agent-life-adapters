use std::fs;
use std::path::PathBuf;
use std::process::Command;
use chrono::Utc;

#[test]
fn test_synthetic_data_validation() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let mut fixture_path = PathBuf::from(manifest_dir);
    fixture_path.push("fixtures/synthetic-agent.alf");

    assert!(
        fixture_path.exists(),
        "Synthetic test data not found. Please run `python3 scripts/generate_synthetic_data.py` first."
    );

    let bin_path = env!("CARGO_BIN_EXE_alf");
    
    let output = Command::new(bin_path)
        .arg("validate")
        .arg(&fixture_path)
        .output()
        .expect("Failed to execute alf validate");

    let mut schema_version = "unknown".to_string();
    let mut version_path = PathBuf::from(manifest_dir);
    version_path.push("fixtures/schema_version.txt");
    if version_path.exists() {
        if let Ok(v) = fs::read_to_string(&version_path) {
            schema_version = v.trim().to_string();
        }
    }

    if let Ok(_report_dir) = std::env::var("ALF_TEST_REPORT_DIR") {
        let mut report_path = PathBuf::from(manifest_dir);
        report_path.push("fixtures/reports");
        let _ = fs::create_dir_all(&report_path);
        report_path.push(format!("integration_test_report_{}.md", schema_version));

        let stdout_str = String::from_utf8_lossy(&output.stdout);
        let stderr_str = String::from_utf8_lossy(&output.stderr);
        let status_str = if output.status.success() { "SUCCESS" } else { "FAILED" };
        let timestamp = Utc::now().to_rfc3339();

        let report_content = format!(
            "# ALF CLI Integration Test Report\n\n\
            **Schema Version:** {}\n\
            **Timestamp:** {}\n\
            **Status:** {}\n\
            \n\
            ## `alf validate` Output\n\
            ```\n{}\n```\n\
            \n\
            ## `alf validate` Errors\n\
            ```\n{}\n```\n",
            schema_version, timestamp, status_str, stdout_str, stderr_str
        );

        let _ = fs::write(report_path, report_content);
    }

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        panic!(
            "Validation failed.\nSTDOUT:\n{}\nSTDERR:\n{}",
            stdout, stderr
        );
    }
}
