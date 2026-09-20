use std::process::Command;

fn bin() -> Command {
    #[allow(
        clippy::disallowed_methods,
        reason = "integration test executes the capobara binary"
    )]
    Command::new(env!("CARGO_BIN_EXE_capobara"))
}

#[test]
fn help_lists_every_subcommand() {
    let out = bin().arg("--help").output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    for name in [
        "catalog",
        "plan",
        "apply",
        "verify",
        "check",
        "prepare",
        "preflight",
        "publish",
        "run",
    ] {
        assert!(text.contains(name), "missing subcommand {name}");
    }
}
