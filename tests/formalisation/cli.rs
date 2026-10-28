// SPDX-License-Identifier: MPL-2.0

use std::process::Command;

#[test]
fn all_help_forms_succeed_without_starting_a_server() {
    for arguments in [vec![], vec!["help"], vec!["--help"], vec!["-h"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_puggle"))
            .args(&arguments)
            .output()
            .unwrap();
        assert!(output.status.success(), "arguments={arguments:?}");
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("puggle serve"));
        assert!(text.contains("POST /v1/freeze"));
        assert!(text.contains("PUGGLE_ADDR"));
    }
}

#[test]
fn unknown_command_reports_failure_on_stderr() {
    let output = Command::new(env!("CARGO_BIN_EXE_puggle"))
        .arg("unknown-command")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let message = String::from_utf8(output.stderr).unwrap();
    assert!(message.contains("puggle: unknown command \"unknown-command\""));
    assert!(message.contains("puggle help"));
}
