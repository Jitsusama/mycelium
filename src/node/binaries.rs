//! Which of the system's own binaries a process runs, for telling the
//! person's terminal from any other ([`super::terminal`]).
//!
//! A binary is judged by the file the kernel says a process runs, never by
//! its name, which any process can change for itself, nor by its owner,
//! since any account can put a root-owned file of any name in the Nix
//! store. It counts as the system's only where it is in the folder one of
//! the system's generations names for it ([`System`]): only root adds a
//! generation, so no account can plant one.

use std::fs;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

/// Who owns the system's generations: root.
pub const ROOT: u32 = 0;

/// Where NixOS keeps the system's generations, each a `system-N-link`.
pub const SYSTEM_PROFILES: &str = "/nix/var/nix/profiles";

/// A binary of the system's this tells apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binary {
    /// The terminal, kitty.
    Kitty,
    /// mycelium, whose holder keeps a held session's terminal.
    Mycelium,
    /// The launcher, Quickshell.
    Quickshell,
}

impl Binary {
    const ALL: [Self; 3] = [Self::Kitty, Self::Mycelium, Self::Quickshell];

    /// The name a profile gives it.
    fn file(self) -> &'static str {
        match self {
            Self::Kitty => "kitty",
            Self::Mycelium => "mycelium",
            Self::Quickshell => "quickshell",
        }
    }

    /// The binary a file of this name is, as the store names each and the
    /// wrapper Nix puts round one renames it.
    fn named(name: &str) -> Option<Self> {
        match name
            .strip_prefix('.')
            .and_then(|name| name.strip_suffix("-wrapped"))
        {
            Some(wrapped) => Self::bare(wrapped),
            None => Self::bare(name),
        }
    }

    fn bare(name: &str) -> Option<Self> {
        match name {
            "kitty" => Some(Self::Kitty),
            "mycelium" => Some(Self::Mycelium),
            "quickshell" => Some(Self::Quickshell),
            _ => None,
        }
    }
}

/// The folders the system's own kitty, mycelium and Quickshell are in, as
/// the system's generations name them, each generation's system profile
/// and every account's profile in it alike, since root declares them all.
/// A kitty from an older generation still counts until that generation is
/// collected, so a terminal opened before a switch stays the person's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct System {
    folders: Vec<(Binary, PathBuf)>,
}

impl System {
    /// The folders the generations in `profiles` name, where `profiles`
    /// and each generation's link are owned by `owner`, as by [`ROOT`],
    /// and `profiles` is open to no other account's writes; none where
    /// they aren't, so a machine set up otherwise tells no binary the
    /// system's.
    ///
    /// # Errors
    ///
    /// Where `profiles` can't be read or trusted, saying why.
    pub fn read(profiles: &Path, owner: u32) -> Result<Self, String> {
        let untrusted = |why: &str| {
            format!(
                "the system's generations in {} {why}, so no binary is told the system's",
                profiles.display()
            )
        };
        let meta = fs::metadata(profiles).map_err(|_| untrusted("can't be read"))?;
        if meta.uid() != owner || meta.mode() & 0o022 != 0 {
            return Err(untrusted("may be changed by an account other than root"));
        }
        let entries = fs::read_dir(profiles).map_err(|_| untrusted("can't be listed"))?;
        let mut folders = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !(name.starts_with("system-") && name.ends_with("-link")) {
                continue;
            }
            let generation = entry.path();
            if !fs::symlink_metadata(&generation).is_ok_and(|link| link.uid() == owner) {
                continue;
            }
            let mut profiles = vec![generation.join("sw")];
            if let Ok(accounts) = fs::read_dir(generation.join("etc/profiles/per-user")) {
                profiles.extend(accounts.flatten().map(|account| account.path()));
            }
            for profile in profiles {
                for binary in Binary::ALL {
                    let named = profile.join("bin").join(binary.file());
                    if let Some(folder) = fs::canonicalize(named)
                        .ok()
                        .and_then(|file| file.parent().map(Path::to_owned))
                        && !folders.contains(&(binary, folder.clone()))
                    {
                        folders.push((binary, folder));
                    }
                }
            }
        }
        Ok(Self { folders })
    }

    /// Those `profiles` names, or none, said on the node's standard error,
    /// where they can't be trusted.
    #[must_use]
    pub fn read_or_none(profiles: &Path) -> Self {
        Self::read(profiles, ROOT).unwrap_or_else(|why| {
            eprintln!("mycelium: {why}");
            Self::default()
        })
    }

    /// Also `binary` in `folder`, as the node's own mycelium is: the
    /// holders it starts run it.
    #[must_use]
    pub fn with(mut self, binary: Binary, folder: PathBuf) -> Self {
        if !self.folders.contains(&(binary, folder.clone())) {
            self.folders.push((binary, folder));
        }
        self
    }

    /// This machine's: those its generations in [`SYSTEM_PROFILES`] name,
    /// with the folder of the node's own mycelium, whose holders it starts.
    #[must_use]
    pub fn here() -> Self {
        let system = Self::read_or_none(Path::new(SYSTEM_PROFILES));
        match std::env::current_exe()
            .ok()
            .and_then(|own| own.parent().map(Path::to_owned))
        {
            Some(folder) => system.with(Binary::Mycelium, folder),
            None => system,
        }
    }

    fn holds(&self, binary: Binary, folder: &Path) -> bool {
        self.folders
            .iter()
            .any(|(one, held)| *one == binary && held == folder)
    }
}

/// Which of the system's binaries process `pid` runs, as `procs` tells
/// it: by the file its `exe` names, a binary of that name in a folder
/// `system` names for it; none for a file of another name or folder, one
/// since removed, or one that can't be read.
#[must_use]
pub fn binary(procs: &Path, pid: i32, system: &System) -> Option<Binary> {
    let file = fs::read_link(procs.join(pid.to_string()).join("exe")).ok()?;
    let name = file.file_name()?.to_str()?;
    // The kernel says a file since removed with this after its path, and
    // one removed may have been anything.
    if name.ends_with(" (deleted)") {
        return None;
    }
    let binary = Binary::named(name)?;
    system.holds(binary, file.parent()?).then_some(binary)
}
