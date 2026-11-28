# The `mycelium` command, built from the crate with its tests run as it
# builds.
{
  lib,
  stdenv,
  rustPlatform,
  git,
  util-linux,
  # Whether the tests run as it builds on a Mac (see `doCheck`).
  macTests ? false,
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
      # Cargo.toml names the benches, so they must be there to build.
      ../benches
      ../tests
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;
  # The tests make a git clone, as a machine does of one another holds,
  # and on Linux lay a folder another owns under one their user owns, in a
  # user namespace `unshare` makes and `mount` binds the folder in.
  nativeCheckInputs = [ git ] ++ lib.optional stdenv.hostPlatform.isLinux util-linux;
  # Nix's sandbox on a Mac lets a build start no FSEvents stream, which
  # notify watches files through there, and bind no listener, even on the
  # loopback, so a Mac that builds its system sandboxed fails every test
  # that hears a file change or binds a listener: over 80 of them, and
  # most of tests/. No build can tell the sandbox from inside it. A skip
  # list cut by a refusal to list /Users missed on 20 November, as one
  # Mac's sandbox let `ls /Users` through that another's refused, and two
  # tests added since weren't on it; that Mac's system didn't build. So a
  # Mac's build runs no tests, and the Mac's gate check, built outside the
  # sandbox at the same commit, runs them all (nix/checks.nix), as Linux's
  # build does.
  doCheck = !stdenv.hostPlatform.isDarwin || macTests;
  meta = {
    description = "A nimble core for agent networks";
    license = lib.licenses.mit;
    mainProgram = "mycelium";
  };
}
