# bybit-get

Read Bybit Card transaction history through the official HMAC API. `bybit-get`
is a read-only Rust CLI. It makes no trading or account changes and keeps no
transaction cache or notification state.

## Commands

```bash
bybit-get                 # Local help; no credentials or network needed
bybit-get --help
bybit-get --version
bybit-get --all           # All available transaction events, newest first
bybit-get 0012            # All available events for the card ending in 0012
bybit-get --cards         # Distinct card endings found in transaction history
bybit-get --config ./config.toml --all
```

Use exactly four digits for a card filter. Leading zeros are preserved.
`--cards` prints a sorted JSON array of last-four strings. It cannot discover
cards with no transaction history.

Data commands print one complete JSON array to stdout. Errors go to stderr and
return a nonzero status. Transaction fields and numeric precision are preserved.
The output contains private financial data; send it to a controlled destination.

## Install

### Nix

Nix with flakes supports Linux on ARM64 and AMD64:

```bash
nix run github:bogorad/bybit-get -- --help
nix run github:bogorad/bybit-get -- --all
nix profile add github:bogorad/bybit-get
```

Pin a release when you need a stable version:

```bash
nix run github:bogorad/bybit-get/v0.1.0 -- --help
```

### Linux release executable

Download the archive for your architecture and `SHA256SUMS` from
[Releases](https://github.com/bogorad/bybit-get/releases). Verify the checksum
before you extract the archive. Then install its `bybit-get` executable:

```bash
sha256sum --check SHA256SUMS --ignore-missing
install -Dm755 bybit-get "$HOME/.local/bin/bybit-get"
bybit-get --version
```

Release executables use musl and static linking. Choose ARM64 (`aarch64`) or
AMD64 (`x86_64`) to match the machine. Ensure `~/.local/bin` is on your `PATH`.

### Cargo

With a Rust toolchain installed:

```bash
cargo install --git https://github.com/bogorad/bybit-get --tag v0.1.0 --locked
```

## Configuration

The default file is `~/.config/bybit-get/config.toml`. Use `--config PATH` to
select another file. A missing default config uses the existing activated
secret files:

- `/run/secrets/api_keys/bybit/api_key`
- `/run/secrets/api_keys/bybit/api_secret`

The API key needs Card access. The program reads credentials only when a data
command needs them. It never prints their values and does not call the SOPS
keyservice. Help and version work without a config file, credentials, or network.

### Read credentials from files

Copy [config.example.toml](config.example.toml), then adjust the paths:

```toml
api_key_file = "/run/secrets/api_keys/bybit/api_key"
api_secret_file = "/run/secrets/api_keys/bybit/api_secret"
base_url = "https://api.bybit.com"
```

Relative secret-file paths are resolved from the config file's directory.
Paths that start with `~/` use your home directory. Keep credential files
readable only by the account that runs the command, normally mode `0600`.

### Inline credentials

A private runtime TOML file can hold inline values instead:

```toml
api_key = "REPLACE_WITH_YOUR_API_KEY"
api_secret = "REPLACE_WITH_YOUR_API_SECRET"
```

Create the config directory with mode `0700` and the file with mode `0600`.
Do not commit credentials or put inline credentials in Nix configuration. Nix
store files are readable by other users. Do not set both `api_key` and
`api_key_file`, or both `api_secret` and `api_secret_file`; these combinations
are rejected. You can use an inline key with a secret-file reference, or the
reverse, provided that each credential has only one source.

### Precedence and API host

Each credential source is selected in this order:

1. `BYBIT_API_KEY_FILE` or `BYBIT_API_SECRET_FILE`, when nonempty.
2. The matching inline value or file reference in the selected TOML file.
3. The default `/run/secrets/api_keys/bybit/` file shown above.

Nonempty `BYBIT_BASE_URL` overrides TOML `base_url`. The default is
`https://api.bybit.com`. Only supported official HTTPS Bybit API origins are
accepted. Standard HTTP proxy environment variables apply to network requests.
There are no inline credential environment variables.

## Home Manager

Add this flake as an input, import its module, and enable the program. The
example uses an existing Home Manager flake configuration:

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    bybit-get = {
      url = "github:bogorad/bybit-get/v0.1.0";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = inputs@{ nixpkgs, home-manager, ... }: {
    homeConfigurations.alice = home-manager.lib.homeManagerConfiguration {
      pkgs = import nixpkgs { system = "aarch64-linux"; };
      modules = [
        inputs.bybit-get.homeManagerModules.default
        {
          home.username = "alice";
          home.homeDirectory = "/home/alice";
          home.stateVersion = "26.05";
          programs.bybit-get = {
            enable = true;
            apiKeyFile = "/run/secrets/api_keys/bybit/api_key";
            apiSecretFile = "/run/secrets/api_keys/bybit/api_secret";
            baseURL = "https://api.bybit.com";
            # package = inputs.bybit-get.packages.aarch64-linux.bybit-get;
          };
        }
      ];
    };
  };
}
```

The module installs the package and writes `bybit-get/config.toml` under your
Home Manager XDG config directory. `package` defaults to the static package
for your system. File options are strings, such as `"/run/secrets/..."`, so
Nix does not copy the referenced file into the store. The generated config
contains file paths and the API origin only; the module has no inline-secret
options. Your secret manager must make the files readable at runtime.

If you change `xdg.configHome` from `~/.config`, pass that generated file to
`--config` when you run the CLI.

## Retrieval contract

The query includes authorizations, authorization reversals, financial events,
and financial refunds. These are lifecycle events. An authorization and its
later clearing are separate records; do not sum them as separate purchases.

The CLI fixes one upper time boundary for the whole query and requests history
without a lower time filter. Bybit retention limits still apply. Page size is
50. Pagination follows `totalCount` even when a page is short. Changed totals,
malformed responses, repeated identical records within a category, and
incomplete pagination stop the query. The full result is buffered before JSON
is written, so a failed request does not print a partial transaction array.

Exact duplicate records across overlapping categories are removed. Different
records with the same transaction ID are retained. Card filtering is sent to
Bybit and checked again for every returned record.

Card requests are paced at least 1.05 seconds apart. A later rate-reset header
extends the wait. A successful response with zero remaining requests is not a
failed query. HTTP or business errors stop the query; resolve the error and
rerun the whole command.

Creation times may be JSON integers or integer strings. Sorting uses their
numeric millisecond values and retains the original JSON representation.

## Development and checks

```bash
nix develop --no-update-lock-file
just check
just build
cargo run --locked -- --help
```

`just check` runs formatting, Clippy with warnings denied, and the Rust tests.
Tests use synthetic credentials and local HTTP fixtures. They do not need a
Bybit account or a live API. A real data command does need readable credentials.

The flake exports `packages`, `apps.default`, `devShells.default`,
`homeManagerModules.default`, `overlays.default`, and `formatter` for
`aarch64-linux` and `x86_64-linux`. `packages.default` and
`packages.bybit-get-static` use static musl builds; `packages.bybit-get` is the
ordinary host package. Named static target outputs support release builds:

```bash
nix build --no-update-lock-file .#bybit-get-static-aarch64-linux
nix build --no-update-lock-file .#bybit-get-static-x86_64-linux
nix build --no-update-lock-file .#checks.aarch64-linux.home-manager
nix flake check --no-update-lock-file
```

The Home Manager check evaluates the real module and verifies default and
custom generated TOML with synthetic paths. Package builds run the focused
Rust tests when the target can execute on the build host. Cross-target release
executables must also be checked on their target architecture before use.
Both `flake.lock` and `Cargo.lock` pin build inputs.

## API references

- [Query Asset Records](https://bybit-exchange.github.io/docs/v5/bybit-card/asset-records)
- [API signing](https://bybit-exchange.github.io/docs/v5/guide)
- [Rate limits](https://bybit-exchange.github.io/docs/v5/rate-limit)
