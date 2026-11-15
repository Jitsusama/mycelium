use std::os::unix::fs::PermissionsExt;

use tokio::io::AsyncWriteExt;

use super::*;

/// How long a test waits for the builders to be heard.
const PATIENCE: Duration = Duration::from_secs(10);

fn file(name: &str, path: &Path, lent: bool) -> File {
    File {
        name: name.to_owned(),
        path: path.to_owned(),
        lent,
    }
}

#[test]
fn a_file_is_said_as_a_name_and_a_full_path() {
    assert_eq!(
        File::parse("standing=/etc/nix/machines", false),
        Ok(file("standing", Path::new("/etc/nix/machines"), false))
    );
    for wrong in ["standing", "=/etc/nix/machines", "standing=machines"] {
        assert!(File::parse(wrong, true).is_err(), "{wrong} is refused");
    }
}

#[test]
fn a_builder_several_files_name_is_one_saying_what_the_first_says() {
    let (standing, lent) = (
        file("standing", Path::new("/standing"), false),
        file("lent", Path::new("/lent"), true),
    );
    let read = [
        (
            &standing,
            lines("ssh-ng://builder@kestrel.local x86_64-linux - 4 2 kvm\nskiff aarch64-linux"),
        ),
        (
            &lent,
            lines("ssh://kestrel x86_64-linux - 16 1 - big-parallel\nheron x86_64-linux"),
        ),
    ];
    let builders = merged(&read);
    let said: Vec<(&str, u32, &[String], bool)> = builders
        .iter()
        .map(|one| (one.name.as_str(), one.jobs, one.lines.as_slice(), one.lent))
        .collect();
    let named =
        |names: &[&str]| -> Vec<String> { names.iter().map(|&one| one.to_owned()).collect() };
    let (both, standing_only, lent_only) = (
        named(&["standing", "lent"]),
        named(&["standing"]),
        named(&["lent"]),
    );
    assert_eq!(
        said,
        vec![
            ("kestrel", 4, both.as_slice(), true),
            ("skiff", 1, standing_only.as_slice(), false),
            ("heron", 1, lent_only.as_slice(), true),
        ],
        "kestrel says what standing says, and is lent since a lent file names it"
    );
    assert!(
        builders.iter().all(|one| one.state == "unheard"),
        "none is heard before it is"
    );
}

#[test]
fn a_builder_says_nothing_of_its_keys_or_its_address() {
    let standing = file("standing", Path::new("/standing"), false);
    let read = [(
        &standing,
        lines(
            "ssh-ng://builder@kestrel?ssh-key=/etc/nix/key x86_64-linux /etc/nix/key 1 1 - - \
             c3NoLWVkMjU1MTkgQUFBQQ==",
        ),
    )];
    let said = serde_json::to_string(&merged(&read)).expect("it is said");
    for unsaid in ["/etc/nix/key", "c3NoLWVk", "builder@", "ssh-ng"] {
        assert!(!said.contains(unsaid), "{unsaid} is said: {said}");
    }
}

/// A stand-in for SSH, in `folder`, that keeps each host it is asked of in
/// `asked` and says each is at `port` here.
fn ssh_keeping(folder: &Path, port: u16) -> (String, PathBuf) {
    let (program, asked) = (folder.join("ssh"), folder.join("asked"));
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\nfor host; do :; done\necho \"$host\" >> {}\nprintf 'hostname 127.0.0.1\\nport {port}\\n'\n",
            asked.display()
        ),
    )
    .expect("it is written");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("it runs");
    (program.display().to_string(), asked)
}

/// Writes `text` to `path` whole, as a program keeping a machines file
/// does: beside it, then renamed into place.
fn write(path: &Path, text: &str) {
    let beside = path.with_extension("new");
    std::fs::write(&beside, text).expect("it is written");
    std::fs::rename(&beside, path).expect("it is put in place");
}

/// The builders `kept` holds once `wanted` says they're as wanted.
async fn until(kept: &Builders, wanted: impl Fn(&[Builder]) -> bool) -> Vec<Builder> {
    let mut told = kept.told();
    let waiting = async {
        loop {
            let builders = kept.builders();
            if wanted(&builders) {
                return builders;
            }
            told.changed().await.expect("the builders are kept");
        }
    };
    tokio::time::timeout(PATIENCE, waiting)
        .await
        .unwrap_or_else(|_| panic!("never as wanted: {:?}", kept.builders()))
}

#[tokio::test]
async fn builders_are_heard_as_they_stand_and_a_lent_one_is_never_dialled() {
    let folder = tempfile::tempdir().expect("a temporary folder");
    let listening = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a listener binds");
    let port = listening.local_addr().expect("an address").port();
    let dialled = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counting = Arc::clone(&dialled);
    tokio::spawn(async move {
        while let Ok((mut ssh, _)) = listening.accept().await {
            counting.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ = ssh.write_all(b"SSH-2.0-x\r\n").await;
        }
    });
    let (ssh, asked) = ssh_keeping(folder.path(), port);
    let (standing, lent) = (folder.path().join("standing"), folder.path().join("lent"));
    write(&standing, "skiff x86_64-linux\nkestrel x86_64-linux");
    write(
        &lent,
        "ssh-ng://hook@kestrel x86_64-linux\nheron x86_64-linux",
    );
    std::fs::write(
        folder.path().join("heron.json"),
        format!(
            r#"{{"at": {}, "state": "passing"}}"#,
            jiff::Timestamp::now().as_second()
        ),
    )
    .expect("heron's lender says it runs");
    let kept = Builders::start(Given {
        files: vec![
            file("standing", &standing, false),
            file("lent", &lent, true),
        ],
        lent_health: Some(format!("{}/{{host}}.json", folder.path().display())),
        ssh,
    });
    let heard = until(&kept, |builders| {
        builders.len() == 3 && builders.iter().all(|one| one.state != "unheard")
    })
    .await;
    let stood: Vec<(&str, &str, Option<&str>)> = heard
        .iter()
        .map(|one| (one.name.as_str(), one.state.as_str(), one.said.as_deref()))
        .collect();
    assert_eq!(
        stood,
        vec![
            ("skiff", "answering", None),
            (
                "kestrel",
                "not running",
                Some("its lender says nothing of it")
            ),
            ("heron", "running", Some("passing")),
        ]
    );
    assert_eq!(
        std::fs::read_to_string(&asked).expect("SSH was asked"),
        "skiff\n",
        "only skiff is dialled: kestrel and heron are lent"
    );
    assert_eq!(dialled.load(std::sync::atomic::Ordering::SeqCst), 1);

    // A change to the files is heard at once, and skiff, dialled lately,
    // isn't dialled again for it.
    write(&standing, "skiff x86_64-linux - 8");
    let changed = until(&kept, |builders| {
        builders
            .iter()
            .any(|one| one.name == "skiff" && one.jobs == 8)
    })
    .await;
    let names: Vec<&str> = changed.iter().map(|one| one.name.as_str()).collect();
    assert_eq!(names, vec!["skiff", "kestrel", "heron"]);
    assert_eq!(
        changed[0].state, "answering",
        "skiff stands as it was heard"
    );
    assert_eq!(
        std::fs::read_to_string(&asked).expect("SSH was asked"),
        "skiff\n"
    );

    // A file taken away takes its builders with it.
    std::fs::remove_file(&lent).expect("the lent file goes");
    let gone = until(&kept, |builders| builders.len() == 1).await;
    assert_eq!(gone[0].name, "skiff");
    assert_eq!(
        kept.health(),
        vec![
            json!({"name": "builders standing", "file": standing.display().to_string(), "state": "answering"}),
            json!({"name": "builders lent", "file": lent.display().to_string(), "state": "answering"}),
        ],
        "a file not there is no trouble"
    );
}

#[tokio::test]
async fn a_file_that_cant_be_read_is_said_in_health() {
    let folder = tempfile::tempdir().expect("a temporary folder");
    let standing = folder.path().join("standing");
    std::fs::create_dir(&standing).expect("a folder where the file should be");
    let kept = Builders::start(Given {
        files: vec![file("standing", &standing, false)],
        lent_health: None,
        ssh: "ssh".to_owned(),
    });
    let mut told = kept.told();
    let waiting = async {
        loop {
            if kept.health()[0]["state"] == "refused" {
                return;
            }
            told.changed().await.expect("the builders are kept");
        }
    };
    tokio::time::timeout(PATIENCE, waiting)
        .await
        .unwrap_or_else(|_| panic!("never refused: {:?}", kept.health()));
}
