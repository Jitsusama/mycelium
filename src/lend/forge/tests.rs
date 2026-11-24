use std::process::Command;

use super::*;

/// The fixture's commit before it was signed, as git asks it signed.
const UNSIGNED: &str = "tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
                        author Wren <wren@example.org> 1700000000 +0000\n\
                        committer Wren <wren@example.org> 1700000000 +0000\n\
                        \n\
                        a signed fixture\n";

/// The signature a throwaway key made of [`UNSIGNED`].
const SIGNATURE: &str = "-----BEGIN SSH SIGNATURE-----\n\
    U1NIU0lHAAAAAQAAADMAAAALc3NoLWVkMjU1MTkAAAAgMhO4udBm2U5ujjrO+4DkeWB6aJ\n\
    2NrVZmnuPYJ2xAYScAAAADZ2l0AAAAAAAAAAZzaGE1MTIAAABTAAAAC3NzaC1lZDI1NTE5\n\
    AAAAQA2Etbb1fzwAqzu6wiCUo1jDAJKe1pcTiqZLdaW1f3kNTwm2v74XDYyNPJ5B26AU8N\n\
    7RI1hNXs0ZNU3iYVqZ9Ao=\n\
    -----END SSH SIGNATURE-----\n";

/// The commit git made of [`UNSIGNED`] signed with [`SIGNATURE`].
const SIGNED_COMMIT: &str = "78bddd5fe114f23734480da23e26db56e927e293";

/// Where git is, as the forge is started with it: by its whole path,
/// since it runs with no `PATH` of its own.
fn git_path() -> PathBuf {
    std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .map(|folder| folder.join("git"))
        .find(|git| git.is_file())
        .expect("git is on the tests' PATH")
}

/// A forge in `dir`, signing with a stand-in `ssh-keygen` that keeps what
/// it was asked and answers [`SIGNATURE`]. It runs with no `PATH`, as
/// the forge runs it, so it uses the shell's own words alone.
fn forge_in(dir: &Path) -> Forge {
    let ssh_keygen = dir.join("ssh-keygen");
    crate::stand_in::program(
        &ssh_keygen,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" \"$SSH_AUTH_SOCK\" \"${{HOME-unset}}\" > '{asked}'\n\
             while IFS= read -r line; do printf '%s\\n' \"$line\"; done > '{payload}'\n\
             printf '%s' '{SIGNATURE}'\n",
            asked = dir.join("asked").display(),
            payload = dir.join("payload").display(),
        ),
    );
    Forge {
        key: dir.join("key.pub"),
        agent: dir.join("agent.sock"),
        repos: BTreeMap::new(),
        branches: vec!["agents/".to_owned()],
        ssh_keygen,
        git: git_path(),
        folder: dir.join("forge"),
    }
}

/// What the forge answers `request`: its status and its body, unchunked.
async fn asked_of(forge: &Forge, request: &[u8]) -> (u16, Vec<u8>) {
    let (mut near, far) = UnixStream::pair().expect("a pair of sockets");
    let serving = tokio::spawn(connection(far, Arc::new(forge.clone())));
    near.write_all(request)
        .await
        .expect("the request is written");
    let mut answer = Vec::new();
    near.read_to_end(&mut answer)
        .await
        .expect("the answer is read");
    serving.await.expect("the connection is answered");
    let end = answer
        .windows(4)
        .position(|four| four == b"\r\n\r\n")
        .expect("the answer has a head");
    let head = String::from_utf8_lossy(&answer[..end]).into_owned();
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|status| status.parse().ok())
        .expect("the answer has a status");
    let body = answer[end + 4..].to_vec();
    let body = if head.contains("transfer-encoding: chunked") {
        unchunked(&body)
    } else {
        body
    };
    (status, body)
}

/// `body` read as HTTP's chunks.
fn unchunked(mut body: &[u8]) -> Vec<u8> {
    let mut whole = Vec::new();
    loop {
        let line = body
            .windows(2)
            .position(|pair| pair == b"\r\n")
            .expect("a chunk's size");
        let size = usize::from_str_radix(&String::from_utf8_lossy(&body[..line]), 16)
            .expect("a chunk's size in hex");
        if size == 0 {
            return whole;
        }
        let begins = line + 2;
        whole.extend_from_slice(&body[begins..begins + size]);
        body = &body[begins + size + 2..];
    }
}

/// A request of `path` from `borrower`, carrying `body`.
fn request(path: &str, borrower: Option<&str>, body: &serde_json::Value) -> Vec<u8> {
    let body = body.to_string();
    let named = borrower.map_or_else(String::new, |name| format!("{BORROWER}: {name}\r\n"));
    format!(
        "POST {path} HTTP/1.1\r\ncontent-type: application/json\r\n{named}\
         content-length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn why(body: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|said| said.get("why")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned())
}

#[test]
fn a_signature_goes_in_as_git_puts_one() {
    let object = with_signature(UNSIGNED.as_bytes(), SIGNATURE);
    let expected = format!(
        "{}gpgsig {}\n{}",
        &UNSIGNED[..UNSIGNED.find("\n\n").expect("a message") + 1],
        SIGNATURE.trim_end().replace('\n', "\n "),
        &UNSIGNED[UNSIGNED.find("\n\n").expect("a message") + 1..],
    );
    assert_eq!(
        String::from_utf8(object).expect("text"),
        expected,
        "a gpgsig header after the committer, each line past its first begun with a space"
    );
}

#[test]
fn only_a_commit_not_yet_signed_is_signed() {
    assert!(
        commit_like(UNSIGNED.as_bytes()),
        "a commit as git asks one signed"
    );
    let signed = with_signature(UNSIGNED.as_bytes(), SIGNATURE);
    assert!(
        !commit_like(&signed),
        "a commit signed already isn't signed again"
    );
    let tag = "object 4b825dc642cb6eb9a060e54bf8d69288fbee4904\ntype commit\ntag v1\n\
               tagger Wren <wren@example.org> 1700000000 +0000\n\na tag\n";
    assert!(!commit_like(tag.as_bytes()), "a tag isn't a commit");
    let sha256 = UNSIGNED.replace("4b825dc642cb6eb9a060e54bf8d69288fbee4904", &"a".repeat(64));
    assert!(
        !commit_like(sha256.as_bytes()),
        "a repository whose ids aren't SHA-1 isn't one the forge signs for"
    );
    let no_committer = "tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\nno one\n";
    assert!(
        !commit_like(no_committer.as_bytes()),
        "a commit says who committed it"
    );
    let message_says_it = "tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\ncommitter in words\n";
    assert!(
        !commit_like(message_says_it.as_bytes()),
        "a committer only in the message is none"
    );
}

#[test]
fn a_branch_is_pushed_only_within_what_the_forge_allows() {
    let allowed = ["agents/".to_owned()];
    assert!(branch_allowed("agents/kestrel/fix-1", &allowed));
    for refused in [
        "agents/",
        "main",
        "agents/../main",
        "agents/a//b",
        "agents/a/",
        "agents/a.lock",
        "agents/.hidden",
        "agents/a b",
        "agents/a~1",
        "agents/a:b",
        "other/agents/a",
    ] {
        assert!(!branch_allowed(refused, &allowed), "{refused} isn't pushed");
    }
    assert!(
        !branch_allowed("anything", &[String::new()]),
        "an empty start allows nothing"
    );
}

#[test]
fn a_bundle_is_read_for_its_one_commit_and_what_it_needs() {
    let (head, need) = ("a".repeat(40), "b".repeat(40));
    let bundle = |header: &str| format!("{header}\n\nPACK").into_bytes();
    assert_eq!(
        bundled(&bundle(&format!(
            "# v2 git bundle\n-{need} the base\n{head} HEAD\n{head} refs/heads/x"
        ))),
        Ok((head.clone(), vec![need.clone()])),
        "one commit, though two refs name it, and what it needs"
    );
    assert_eq!(
        bundled(&bundle(&format!(
            "# v3 git bundle\n@object-format=sha1\n{head} HEAD"
        ))),
        Ok((head.clone(), Vec::new())),
    );
    for (header, why) in [
        (
            format!("# v3 git bundle\n@object-format=sha256\n{head} HEAD"),
            "has a capability the forge doesn't take",
        ),
        (
            format!("# v2 git bundle\n{head} HEAD\n{need} refs/heads/y"),
            "holds more than one commit to push",
        ),
        (
            format!("# v2 git bundle\n-{need} the base"),
            "holds no commit to push",
        ),
        (
            format!("# v2 git bundle\n-nope the base\n{head} HEAD"),
            "needs what isn't a commit",
        ),
        (
            format!("# v4 git bundle\n{head} HEAD"),
            "isn't a git bundle of version 2 or 3",
        ),
    ] {
        assert_eq!(bundled(&bundle(&header)), Err(why), "{header}");
    }
    assert_eq!(bundled(b"PACK"), Err("isn't a git bundle"));
}

#[tokio::test]
async fn a_commit_is_signed_with_the_forges_key_and_named_as_git_names_it() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let forge = forge_in(dir.path());
    let asking = json!({
        "cell": "KP0L6A",
        "payload": STANDARD.encode(UNSIGNED),
    });

    let (status, body) = asked_of(&forge, &request("/sign", Some("kestrel"), &asking)).await;

    assert_eq!(status, 200, "signed: {}", why(&body));
    let signed: Signed = serde_json::from_slice(&body).expect("a signature");
    assert_eq!(
        signed,
        Signed {
            signature: SIGNATURE.to_owned(),
            commit: SIGNED_COMMIT.to_owned(),
        },
        "the commit's id is the one git gives it signed"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("payload")).expect("what was signed"),
        UNSIGNED,
        "the commit itself is what the key signs"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("asked")).expect("what ssh-keygen was asked"),
        format!(
            "-Y sign -n git -U -f {}\n{}\nunset\n",
            forge.key.display(),
            forge.agent.display()
        ),
        "it signs as git with the key's agent alone, and nothing else of this machine's"
    );
}

#[tokio::test]
async fn what_the_forge_wont_sign_is_refused_saying_why() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let forge = forge_in(dir.path());
    let signing = |payload: &[u8]| json!({"payload": STANDARD.encode(payload)});
    let signed = with_signature(UNSIGNED.as_bytes(), SIGNATURE);
    for (request, status, words) in [
        (
            request("/sign", None, &signing(UNSIGNED.as_bytes())),
            400,
            "named no machine",
        ),
        (
            request("/sign", Some("../kestrel"), &signing(UNSIGNED.as_bytes())),
            400,
            "named no machine",
        ),
        (
            request("/sign", Some("kestrel"), &signing(&signed)),
            422,
            "signs commits alone",
        ),
        (
            request("/sign", Some("kestrel"), &json!({"payload": "not base64!"})),
            400,
            "isn't base64",
        ),
        (
            request("/sign", Some("kestrel"), &json!(["a list"])),
            400,
            "isn't a commit to sign",
        ),
        (
            request("/keys", Some("kestrel"), &json!({})),
            404,
            "answers no POST /keys",
        ),
        (
            b"POST /sign HTTP/1.1\r\ntransfer-encoding: chunked\r\n\r\n0\r\n\r\n".to_vec(),
            501,
            "a body of a length alone",
        ),
        (b"POST /sign\r\n\r\n".to_vec(), 400, "isn't HTTP"),
    ] {
        let (answered, body) = asked_of(&forge, &request).await;
        assert_eq!(
            (answered, why(&body).contains(words)),
            (status, true),
            "{}: {}",
            String::from_utf8_lossy(&request),
            why(&body)
        );
    }
    assert!(
        !dir.path().join("payload").exists(),
        "nothing refused reached the key"
    );
}

/// Runs git in `repo`, as someone, and says what it wrote.
fn git(repo: &Path, args: &[&str]) -> String {
    let ran = Command::new(git_path())
        .args(["-c", "user.name=Wren", "-c", "user.email=wren@example.org"])
        .args([
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
        ])
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        ran.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&ran.stderr)
    );
    String::from_utf8_lossy(&ran.stdout).trim().to_owned()
}

/// An origin holding one commit on `main`, the forge's clone of it, and a
/// borrower's: the origin's folder, and the commit.
fn repos_in(dir: &Path) -> (PathBuf, String) {
    let origin = dir.join("origin.git");
    fs::create_dir_all(&origin).expect("made");
    git(&origin, &["init", "--quiet", "--bare"]);
    for clone in ["forge-repo", "borrower"] {
        git(
            dir,
            &["clone", "--quiet", origin.to_str().expect("utf-8"), clone],
        );
    }
    let borrower = dir.join("borrower");
    git(
        &borrower,
        &["commit", "--quiet", "--allow-empty", "-m", "base"],
    );
    git(&borrower, &["push", "--quiet", "origin", "main"]);
    git(&dir.join("forge-repo"), &["fetch", "--quiet", "origin"]);
    (origin, git(&borrower, &["rev-parse", "HEAD"]))
}

/// What a push answered, line by line.
fn lines_of(body: &[u8]) -> Vec<Pushed> {
    String::from_utf8_lossy(body)
        .lines()
        .map(|line| serde_json::from_str(line).expect("a line of a push"))
        .collect()
}

#[tokio::test]
async fn a_branch_bundled_is_pushed_to_its_origin_and_tidied_after() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let (origin, base) = repos_in(dir.path());
    let mut forge = forge_in(dir.path());
    forge
        .repos
        .insert("core".to_owned(), dir.path().join("forge-repo"));
    let borrower = dir.path().join("borrower");
    git(
        &borrower,
        &["commit", "--quiet", "--allow-empty", "-m", "a change"],
    );
    let head = git(&borrower, &["rev-parse", "HEAD"]);
    let bundle = borrow::bundle(&forge.git, &borrower, &base, "HEAD").expect("bundled");
    let pushing = Pushing {
        cell: Some("KP0L6A".to_owned()),
        repo: "core".to_owned(),
        branch: "agents/kestrel-fix".to_owned(),
        bundle: STANDARD.encode(&bundle),
    };

    let (status, body) =
        asked_of(&forge, &request("/push", Some("kestrel"), &json!(pushing))).await;

    assert_eq!(status, 200, "begun: {}", why(&body));
    assert_eq!(
        lines_of(&body),
        [Pushed::Pushed {
            head: head.clone(),
            commits: vec![head.clone()],
        }],
        "it says what it pushed: the commits past the bundle's base"
    );
    assert_eq!(
        git(&origin, &["rev-parse", "refs/heads/agents/kestrel-fix"]),
        head,
        "the branch is on the origin"
    );
    assert_eq!(
        git(&dir.path().join("forge-repo"), &["for-each-ref", FETCHED]),
        "",
        "what was fetched to push is let go"
    );
    assert_eq!(
        fs::read_dir(&forge.folder).expect("the folder").count(),
        0,
        "and so is the bundle"
    );

    // Another change, not on top of the one pushed, would rewrite it.
    git(&borrower, &["reset", "--quiet", "--hard", base.as_str()]);
    git(
        &borrower,
        &["commit", "--quiet", "--allow-empty", "-m", "another"],
    );
    let bundle = borrow::bundle(&forge.git, &borrower, &base, "HEAD").expect("bundled");
    let forcing = Pushing {
        bundle: STANDARD.encode(&bundle),
        ..pushing
    };
    let (status, body) =
        asked_of(&forge, &request("/push", Some("kestrel"), &json!(forcing))).await;
    assert_eq!(status, 200, "begun: {}", why(&body));
    assert!(
        matches!(
            lines_of(&body).as_slice(),
            [Pushed::Refused { why }] if why.starts_with("the push was refused")
        ),
        "a push is never forced: {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(
        git(&origin, &["rev-parse", "refs/heads/agents/kestrel-fix"]),
        head,
        "the branch stands as it was"
    );
}

#[tokio::test]
async fn a_push_the_forge_doesnt_allow_is_refused_before_git_is_asked() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let mut forge = forge_in(dir.path());
    forge
        .repos
        .insert("core".to_owned(), dir.path().join("forge-repo"));
    let bundle = STANDARD.encode(format!("# v2 git bundle\n{} HEAD\n\nPACK", "a".repeat(40)));
    let pushing = |repo: &str, branch: &str, bundle: &str| {
        json!(Pushing {
            cell: None,
            repo: repo.to_owned(),
            branch: branch.to_owned(),
            bundle: bundle.to_owned(),
        })
    };
    for (asked, status, words) in [
        (
            pushing("core", "main", &bundle),
            403,
            "beginning agents/, so not main",
        ),
        (
            pushing("world", "agents/x", &bundle),
            404,
            "no repository named world",
        ),
        (
            pushing("core", "agents/x", "%%"),
            400,
            "the bundle isn't base64",
        ),
        (
            pushing("core", "agents/x", &STANDARD.encode("PACK")),
            400,
            "the bundle isn't a git bundle",
        ),
    ] {
        let (answered, body) = asked_of(&forge, &request("/push", Some("kestrel"), &asked)).await;
        assert_eq!(
            (answered, why(&body).contains(words)),
            (status, true),
            "{asked}: {}",
            why(&body)
        );
    }
    assert!(!forge.folder.exists(), "nothing was kept to fetch");
}

#[test]
fn git_asking_its_signing_program_to_sign_is_told_from_anything_else() {
    let words = |words: &[&str]| {
        words
            .iter()
            .map(|word| (*word).to_owned())
            .collect::<Vec<_>>()
    };
    let signing = words(&["-Y", "sign", "-n", "git", "-f", "key.pub", "/tmp/x"]);
    assert_eq!(borrow::asked(&signing), borrow::Asked::Sign("/tmp/x"));
    let verifying = words(&[
        "-Y", "verify", "-n", "git", "-f", "allowed", "-I", "a", "-s", "x",
    ]);
    assert_eq!(borrow::asked(&verifying), borrow::Asked::Other);
    assert_eq!(
        borrow::asked(&words(&["-Y", "sign", "-n", "git", "-U"])),
        borrow::Asked::Other,
        "a sign with no file named last is ssh-keygen's own"
    );
}
