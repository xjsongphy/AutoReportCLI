use std::process::Command;

#[test]
fn help_lists_the_primary_cli_options() {
    let output = Command::new(env!("CARGO_BIN_EXE_autoreport"))
        .arg("--help")
        .output()
        .expect("run autoreport --help");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 help output");
    assert!(stdout.contains("--workspace"));
    assert!(stdout.contains("--sync-presets"));
    assert!(stdout.contains("--no-sync"));
}
