{
  lib,
  rustPlatform,
  stdenv,
}:

rustPlatform.buildRustPackage {
  pname = "bybit-get";
  version = "0.1.0";
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
      (lib.fileset.maybeMissing ../tests)
      ../LICENSE
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;
  doCheck = stdenv.buildPlatform.canExecute stdenv.hostPlatform;
  meta = {
    description = "Read-only Bybit Card transaction history CLI";
    homepage = "https://github.com/bogorad/bybit-get";
    license = lib.licenses.mit;
    mainProgram = "bybit-get";
    platforms = lib.platforms.linux;
  };
}
