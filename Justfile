set shell := ["bash", "-c"]

default:
    @just --list

check:
    cargo fmt --check
    cargo clippy --all-targets --locked -- -D warnings
    cargo test --locked

build:
    cargo build --release --locked

nix-check:
    nix flake check --no-update-lock-file

static:
    nix build --no-update-lock-file .#bybit-get-static

help:
    cargo run --locked -- --help
