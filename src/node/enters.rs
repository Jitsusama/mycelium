//! Whether the account a fence's launcher runs what the node starts as
//! can enter the folder it begins in. A worker machine's node runs as one
//! account and its sessions and runs as another (`worker`), so a folder
//! the node finds, such as its own home, may be one that account can't
//! reach: begun there, pi can't start its shell, and fails each minute
//! with nothing saying why. The node judges each such folder for that
//! account first, as the kernel would, and refuses it plainly.

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Where the machine keeps its accounts, and its groups with their
/// members.
const ACCOUNTS: &str = "/etc/passwd";
const GROUPS: &str = "/etc/group";

/// What a machine, named before it, answers a start or a run it won't
/// begin in a folder its launcher's account can't enter, the folder
/// after it ([`Account::refusal`]).
pub(super) const CANT_BEGIN_IN: &str = " can't begin this in ";

/// An account, as the kernel judges it entering a folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Account {
    /// Its number.
    uid: u32,
    /// Its name, where the machine's records name it.
    name: Option<String>,
    /// Its groups: its primary group and every group naming it a member.
    groups: Vec<u32>,
}

/// A folder, as an account entering it is judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Folder {
    /// Its permission bits.
    mode: u32,
    /// The account that owns it.
    owner: u32,
    /// Its group.
    group: u32,
}

impl Folder {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            mode: metadata.mode(),
            owner: metadata.uid(),
            group: metadata.gid(),
        }
    }
}

impl Account {
    /// Account `uid`, with its groups: the node's own process's where it
    /// is the node's own account, and otherwise as the machine's records
    /// say, its primary group and every group naming it. None where those
    /// records can't be read or don't hold it, since its groups are then
    /// unknown and it may enter by one of them.
    pub(super) fn of(uid: u32) -> Option<Self> {
        let accounts = fs::read_to_string(ACCOUNTS);
        if uid == rustix::process::geteuid().as_raw() {
            let name = accounts
                .ok()
                .and_then(|accounts| Self::recorded(uid, &accounts, "").and_then(|own| own.name));
            let mut groups = rustix::process::getgroups().ok()?;
            groups.push(rustix::process::getegid());
            return Some(Self {
                uid,
                name,
                groups: groups.into_iter().map(rustix::fs::Gid::as_raw).collect(),
            });
        }
        Self::recorded(uid, &accounts.ok()?, &fs::read_to_string(GROUPS).ok()?)
    }

    /// Account `uid` as `accounts`, read as `/etc/passwd` is, and
    /// `groups`, read as `/etc/group` is, hold it; none where `accounts`
    /// has no line for it.
    fn recorded(uid: u32, accounts: &str, groups: &str) -> Option<Self> {
        let (name, primary) = accounts.lines().find_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            let said = fields.nth(1)?.parse::<u32>().ok()?;
            let primary = fields.next()?.parse::<u32>().ok()?;
            (said == uid).then(|| (name.to_owned(), primary))
        })?;
        let mut held = vec![primary];
        held.extend(groups.lines().filter_map(|line| {
            let mut fields = line.split(':');
            let gid = fields.nth(2)?.parse::<u32>().ok()?;
            let members = fields.next()?;
            members
                .split(',')
                .any(|member| member == name)
                .then_some(gid)
        }));
        Some(Self {
            uid,
            name: Some(name),
            groups: held,
        })
    }

    /// Whether it may search `folder`, so pass through it or begin there:
    /// by its owner's bits where it owns it, else by its group's where
    /// the folder's group is one of its own, else by everyone's. The
    /// system's account searches any.
    fn searches(&self, folder: Folder) -> bool {
        let bit = if folder.owner == self.uid {
            0o100
        } else if self.groups.contains(&folder.group) {
            0o010
        } else {
            0o001
        };
        self.uid == 0 || folder.mode & bit != 0
    }

    /// The first folder from `/` down to `folder` itself, as its path
    /// resolves now, that it can't search, so can't begin in `folder`;
    /// none where it can, or where the node can't look at one of them,
    /// since then nothing says it can't.
    pub(super) fn first_barred(&self, folder: &Path) -> Option<PathBuf> {
        let resolved = folder.canonicalize().ok()?;
        self.barred_on(&resolved, |each| {
            fs::metadata(each).map(|kept| Folder::of(&kept))
        })
    }

    /// As [`Self::first_barred`], for `folder` resolved already, each
    /// folder on its way as `look` finds it.
    fn barred_on(
        &self,
        folder: &Path,
        look: impl Fn(&Path) -> io::Result<Folder>,
    ) -> Option<PathBuf> {
        let mut way: Vec<&Path> = folder.ancestors().collect();
        way.reverse();
        for each in way {
            if !self.searches(look(each).ok()?) {
                return Some(each.to_owned());
            }
        }
        None
    }

    /// What `machine` answers a start or a run it won't begin in `folder`,
    /// since `barred` stops this account, which its launcher runs it as,
    /// on its way there.
    pub(super) fn refusal(&self, machine: &str, folder: &Path, barred: &Path) -> String {
        let account = match &self.name {
            Some(name) => format!("{} ({name})", self.uid),
            None => self.uid.to_string(),
        };
        format!(
            "{machine}{CANT_BEGIN_IN}{}, since its launcher runs it as account {account}, which \
             can't enter {}; ask for `--folder /` or a folder that account owns",
            folder.display(),
            barred.display()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;

    /// A worker's account, 1001, whose own group is 1001 and which the
    /// mesh's group, 1500, names.
    fn worker() -> Account {
        Account {
            uid: 1001,
            name: Some("worker".to_owned()),
            groups: vec![1001, 1500],
        }
    }

    /// A machine's folders, each with its mode, owner and group.
    fn machine(folders: &[(&str, u32, u32, u32)]) -> BTreeMap<PathBuf, Folder> {
        folders
            .iter()
            .map(|&(path, mode, owner, group)| {
                let folder = Folder { mode, owner, group };
                (PathBuf::from(path), folder)
            })
            .collect()
    }

    /// The first folder on the way to `folder` on `machine` that
    /// `account` can't search.
    fn barred(account: &Account, on: &BTreeMap<PathBuf, Folder>, folder: &str) -> Option<PathBuf> {
        account.barred_on(Path::new(folder), |each| {
            on.get(each)
                .copied()
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        })
    }

    #[test]
    fn a_folder_under_another_accounts_closed_home_is_refused_naming_that_home() {
        let skiff = machine(&[
            ("/", 0o755, 0, 0),
            ("/home", 0o755, 0, 0),
            ("/home/kestrel", 0o700, 1000, 100),
            ("/home/kestrel/src", 0o755, 1000, 100),
        ]);
        assert_eq!(
            barred(&worker(), &skiff, "/home/kestrel/src"),
            Some(PathBuf::from("/home/kestrel")),
            "the home stops it, however open the folder below"
        );
        assert_eq!(
            barred(&worker(), &skiff, "/home/kestrel"),
            Some(PathBuf::from("/home/kestrel")),
            "and so does the home itself"
        );
        assert_eq!(barred(&worker(), &skiff, "/home"), None, "but not above it");
    }

    #[test]
    fn a_folder_reached_through_a_group_folder_of_the_account_passes() {
        let skiff = machine(&[
            ("/", 0o755, 0, 0),
            ("/srv", 0o755, 0, 0),
            ("/srv/mesh", 0o750, 1000, 1500),
            ("/srv/mesh/work", 0o755, 1000, 1500),
        ]);
        assert_eq!(
            barred(&worker(), &skiff, "/srv/mesh/work"),
            None,
            "the mesh's group, among its others, lets it through"
        );
        let outside = Account {
            groups: vec![1001],
            ..worker()
        };
        assert_eq!(
            barred(&outside, &skiff, "/srv/mesh/work"),
            Some(PathBuf::from("/srv/mesh")),
            "an account outside that group is judged by everyone's bits"
        );
    }

    #[test]
    fn the_root_folder_passes() {
        let skiff = machine(&[("/", 0o755, 0, 0)]);
        assert_eq!(barred(&worker(), &skiff, "/"), None);
    }

    #[test]
    fn an_account_is_judged_by_the_first_of_owner_group_and_everyone_it_is() {
        let skiff = machine(&[
            ("/", 0o755, 0, 0),
            ("/owned", 0o071, 1001, 1500),
            ("/grouped", 0o701, 1000, 1500),
        ]);
        assert_eq!(
            barred(&worker(), &skiff, "/owned"),
            Some(PathBuf::from("/owned")),
            "owning it, its owner's bits alone count"
        );
        assert_eq!(
            barred(&worker(), &skiff, "/grouped"),
            Some(PathBuf::from("/grouped")),
            "in its group, its group's bits alone count"
        );
        let system = Account {
            uid: 0,
            name: None,
            groups: vec![0],
        };
        assert_eq!(
            barred(&system, &skiff, "/grouped"),
            None,
            "the system's account searches any"
        );
    }

    #[test]
    fn a_folder_the_node_cant_look_at_refuses_nothing() {
        let skiff = machine(&[("/", 0o755, 0, 0)]);
        assert_eq!(barred(&worker(), &skiff, "/gone/work"), None);
    }

    #[test]
    fn an_accounts_groups_are_its_primary_and_those_naming_it() {
        let accounts = "root:x:0:0::/root:/bin/sh\n\
                        kestrel:x:1000:100::/home/kestrel:/bin/sh\n\
                        worker:x:1001:1001::/home/worker:/bin/sh\n";
        let groups = "users:x:100:kestrel\n\
                      worker:x:1001:\n\
                      mesh:x:1500:kestrel,worker\n\
                      wheel:x:1:kestrel\n\
                      workers:x:1600:workerish\n";
        assert_eq!(
            Account::recorded(1001, accounts, groups),
            Some(worker()),
            "its own group, and the mesh's, which names it"
        );
        assert_eq!(
            Account::recorded(1002, accounts, groups),
            None,
            "an account with no record isn't known"
        );
    }

    #[test]
    fn a_real_folder_is_judged_as_the_kernel_would() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let closed = dir.path().join("closed");
        fs::create_dir(&closed).expect("made");
        fs::set_permissions(&closed, fs::Permissions::from_mode(0o600)).expect("closed");
        let own = Account::of(rustix::process::geteuid().as_raw()).expect("its own account");
        assert_eq!(
            own.first_barred(dir.path()),
            None,
            "its own open folder passes"
        );
        let resolved = closed.canonicalize().expect("it resolves");
        assert_eq!(
            own.first_barred(&closed),
            (own.uid != 0).then_some(resolved),
            "but not one it can't search, even its own"
        );
    }

    #[test]
    fn a_refusal_names_the_folder_the_account_and_what_stops_it() {
        let said = worker().refusal(
            "skiff",
            Path::new("/home/kestrel/src"),
            Path::new("/home/kestrel"),
        );
        assert_eq!(
            said,
            "skiff can't begin this in /home/kestrel/src, since its launcher runs it as account \
             1001 (worker), which can't enter /home/kestrel; ask for `--folder /` or a folder \
             that account owns"
        );
    }
}
