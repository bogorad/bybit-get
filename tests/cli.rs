use std::process::Command;

#[test]
fn no_args_help_and_version_succeed_with_inaccessible_configuration() {
    for args in [
        vec![],
        vec!["--help"],
        vec!["-h"],
        vec!["--version"],
        vec!["--config", "/does-not-exist/bybit.toml"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_bybit-get"))
            .args(&args)
            .env("HOME", "/does-not-exist")
            .env("BYBIT_API_KEY_FILE", "/does-not-exist/key")
            .env("BYBIT_API_SECRET_FILE", "/does-not-exist/secret")
            .env("BYBIT_BASE_URL", "http://127.0.0.1:1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}");
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(if args == vec!["--version"] {
            text == "bybit-get 0.1.0\n"
        } else {
            text.contains("Usage:")
        });
    }
}

#[test]
fn argument_and_runtime_errors_use_distinct_exit_codes_and_no_data() {
    for (args, code) in [
        (vec!["--all", "--cards"], 2),
        (vec!["12x4"], 2),
        (vec!["--all", "--config", "/does-not-exist/config"], 1),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_bybit-get"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(code));
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
}
