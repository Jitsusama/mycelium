# The `mycelium` command, built from the crate with its tests run as it
# builds.
{
  lib,
  rustPlatform,
  git,
}:
rustPlatform.buildRustPackage {
  pname = "mycelium";
  version = "0.1.0";
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;
  # The tests make a git clone, as a machine does of one another holds.
  nativeCheckInputs = [ git ];
  meta = {
    description = "A nimble core for agent networks";
    license = lib.licenses.mit;
    mainProgram = "mycelium";
  };
}
