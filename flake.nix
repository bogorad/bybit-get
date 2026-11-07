{
  description = "Read-only Bybit Card transaction history CLI";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/6dadaf4375be35033e1c84395adbd53ff5fded96";
    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      home-manager,
    }:
    let
      systems = [
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      pkgsFor = system: import nixpkgs { inherit system; };
    in
    {
      overlays.default = final: _prev: {
        bybit-get = final.callPackage ./nix/package.nix { };
      };

      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          staticPackage = pkgs.pkgsStatic.callPackage ./nix/package.nix { };
        in
        {
          bybit-get = pkgs.callPackage ./nix/package.nix { };
          bybit-get-static = staticPackage;
          bybit-get-static-x86_64-linux = pkgs.pkgsCross.musl64.pkgsStatic.callPackage ./nix/package.nix { };
          bybit-get-static-aarch64-linux =
            pkgs.pkgsCross.aarch64-multiplatform-musl.pkgsStatic.callPackage ./nix/package.nix
              { };
          default = staticPackage;
        }
      );

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/bybit-get";
          meta.description = "Read Bybit Card transaction history";
        };
      });

      homeManagerModules.default = import ./nix/home-manager.nix self;

      checks = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          package = self.packages.${system}.default;
          home-manager = import ./nix/check-home-manager.nix {
            inherit self home-manager pkgs;
          };
          packaged-help = pkgs.runCommand "bybit-get-packaged-help-check" { } ''
            export HOME="$TMPDIR/missing-home"
            export BYBIT_API_KEY_FILE="$TMPDIR/missing-api-key"
            export BYBIT_API_SECRET_FILE="$TMPDIR/missing-api-secret"
            export BYBIT_BASE_URL="http://invalid.invalid"
            program=${self.packages.${system}.default}/bin/bybit-get
            "$program" > no-args.txt
            "$program" --help > help.txt
            cmp no-args.txt help.txt
            "$program" --config "$TMPDIR/missing-config.toml" --help > explicit-help.txt
            cmp help.txt explicit-help.txt
            "$program" --version > version.txt
            grep -Fx 'bybit-get 0.1.0' version.txt
            mkdir -p "$out"
            cp help.txt version.txt "$out/"
          '';
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              rustc
              cargo
              rustfmt
              clippy
              just
              nixfmt
              file
              binutils
            ];
            RUST_BACKTRACE = "1";
          };
        }
      );

      formatter = forAllSystems (system: (pkgsFor system).nixfmt);
    };
}
