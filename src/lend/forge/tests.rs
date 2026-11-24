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
        &UNSIGNED[..=UNSIGNED.find("\n\n").expect("a message")],
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
fn what_git_says_goes_back_with_no_credentials_in_its_urls() {
    assert_eq!(
        unlocked(
            "fatal: unable to access 'https://x-access-token:s3cret@example.org/wren/repo.git/': \
             403, then ssh://git@example.org/wren and https://example.org/kestrel"
        ),
        "fatal: unable to access 'https://***@example.org/wren/repo.git/': 403, then \
         ssh://***@example.org/wren and https://example.org/kestrel",
        "each URL's credentials go, and a URL with none stays whole"
    );
    assert_eq!(
        unlocked("https://a@b@example.org/x"),
        "https://***@example.org/x",
        "up to the host's last @"
    );
    assert_eq!(
        unlocked("see https://example.org/x?by=wren@example.org"),
        "see https://example.org/x?by=wren@example.org",
        "an @ past the host's end is the path's"
    );
}

#[test]
fn a_command_running_past_its_time_is_ended() {
    let began = Instant::now();
    ran_within(Command::new("sleep").arg("30"), Duration::from_millis(200));
    assert!(
        began.elapsed() < Duration::from_secs(10),
        "ended near its time, not at its own end"
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
    assert_eq!(
        bundled(&bundle(&format!(
            "# v2 git bundle\n-{} the base\n{} HEAD",
            need.to_ascii_uppercase(),
            head.to_ascii_uppercase()
        ))),
        Ok((head.clone(), vec![need.clone()])),
        "each named as git names it, so a head in capitals is still the one signed"
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

#[test]
fn a_commit_comes_back_unsigned_with_its_parents_as_signed() {
    let signed = with_signature(UNSIGNED.as_bytes(), SIGNATURE);
    assert_eq!(
        unsigned(&signed, &BTreeMap::new()),
        Some(UNSIGNED.as_bytes().to_vec()),
        "a signature it had is taken out, whole"
    );
    let (was, now) = ("a".repeat(40), "b".repeat(40));
    let child = UNSIGNED.replacen("\nauthor", &format!("\nparent {was}\nauthor"), 1);
    assert_eq!(
        unsigned(child.as_bytes(), &[(was.clone(), now.clone())].into()),
        Some(child.replace(&was, &now).into_bytes()),
        "a parent signed in this push is named as signed"
    );
    let message = format!("{UNSIGNED}parent {was}\n");
    assert_eq!(
        unsigned(message.as_bytes(), &[(was.clone(), now)].into()),
        Some(message.clone().into_bytes()),
        "a parent only in the message is left as written"
    );
    assert_eq!(
        unsigned(b"tree x\n\nno committer\n", &BTreeMap::new()),
        None,
        "what isn't a commit the forge signs isn't readied"
    );
}

#[test]
fn a_forge_starts_only_with_branch_starts_and_paths_named_whole() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let forge = forge_in(dir.path());
    assert_eq!(forge.unstartable(), None, "as the tests start it");
    for (branches, said) in [
        (vec![], "no branch without a start"),
        (vec!["agents".to_owned()], "so not agents"),
        (vec![String::new()], "ends in /"),
        (vec!["agents/../".to_owned()], "ends in /"),
    ] {
        let starting = Forge {
            branches: branches.clone(),
            ..forge.clone()
        };
        let why = starting.unstartable().unwrap_or_default();
        assert!(why.contains(said), "{branches:?}: {why}");
    }
    let relative = Forge {
        folder: PathBuf::from("forge"),
        ..forge.clone()
    };
    assert!(
        relative
            .unstartable()
            .is_some_and(|why| why.contains("isn't named from the root")),
        "a folder named from where it starts"
    );
    let mut repo = forge;
    repo.repos.insert("core".to_owned(), PathBuf::from("core"));
    assert!(
        repo.unstartable()
            .is_some_and(|why| why.starts_with("core isn't named")),
        "and a repository too"
    );
}

#[tokio::test]
async fn what_the_forge_wont_answer_is_refused_saying_why() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let forge = forge_in(dir.path());
    let pushing = json!({"repo": "core", "branch": "agents/x", "bundle": ""});
    for (request, status, words) in [
        (request("/push", None, &pushing), 400, "named no machine"),
        (
            request("/push", Some("../kestrel"), &pushing),
            400,
            "named no machine",
        ),
        (
            request("/keys", Some("kestrel"), &json!({})),
            404,
            "answers no POST /keys",
        ),
        (
            request(
                "/sign",
                Some("kestrel"),
                &json!({"payload": STANDARD.encode(UNSIGNED)}),
            ),
            404,
            "answers no POST /sign",
        ),
        (
            b"POST /push HTTP/1.1\r\ntransfer-encoding: chunked\r\n\r\n0\r\n\r\n".to_vec(),
            501,
            "a body of a length alone",
        ),
        (b"POST /push\r\n\r\n".to_vec(), 400, "isn't HTTP"),
        (
            {
                let mut twice = request("/push", Some("kestrel"), &pushing);
                let named = format!("{BORROWER}: osprey\r\n");
                let at = twice
                    .windows(2)
                    .position(|pair| pair == b"\r\n")
                    .expect("a line")
                    + 2;
                twice.splice(at..at, named.into_bytes());
                twice
            },
            400,
            "the machine asking is named twice",
        ),
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
        "nothing reached the key: it signs only what it pushes"
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
#[expect(
    clippy::too_many_lines,
    reason = "one push followed from its bundle to its origin and its tidying"
)]
async fn a_branch_bundled_is_pushed_to_its_origin_and_tidied_after() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let (origin, base) = repos_in(dir.path());
    let mut forge = forge_in(dir.path());
    forge
        .repos
        .insert("core".to_owned(), dir.path().join("forge-repo"));
    let borrower = dir.path().join("borrower");
    for message in ["a change", "another on it"] {
        git(
            &borrower,
            &["commit", "--quiet", "--allow-empty", "-m", message],
        );
    }
    let (first, head) = (
        git(&borrower, &["rev-parse", "HEAD^"]),
        git(&borrower, &["rev-parse", "HEAD"]),
    );
    // As `mycelium forge push` bundles it: the commit its head names.
    let was = borrow::resolved(&forge.git, &borrower, "HEAD").expect("resolved");
    assert_eq!(was, head, "the head named by its commit");
    let bundle = borrow::bundle(&forge.git, &borrower, &base, &was).expect("bundled");
    assert_eq!(
        git(&borrower, &["for-each-ref", borrow::BUNDLED]),
        "",
        "what named it to bundle is let go"
    );
    let pushing = Pushing {
        cell: Some("KP0L6A".to_owned()),
        repo: "core".to_owned(),
        branch: "agents/kestrel-fix".to_owned(),
        bundle: STANDARD.encode(&bundle),
    };

    let (status, body) =
        asked_of(&forge, &request("/push", Some("kestrel"), &json!(pushing))).await;

    assert_eq!(status, 200, "begun: {}", why(&body));
    let answered = lines_of(&body);
    let [
        Pushed::Pushed {
            head: now,
            commits,
            signed,
            bundle: Some(_),
        },
    ] = answered.as_slice()
    else {
        panic!("one line saying it pushed, with a bundle back: {answered:?}");
    };
    let first_signed = signed.get(&first).expect("the first commit signed").clone();
    assert_eq!(
        (signed.len(), signed.get(&head)),
        (2, Some(now)),
        "each commit past the origin signed"
    );
    assert_eq!(
        commits,
        &[now.clone(), first_signed.clone()],
        "and said newest first"
    );
    assert_ne!(now, &head, "signed, the head is another commit");
    assert_eq!(
        git(&origin, &["rev-parse", "refs/heads/agents/kestrel-fix"]),
        *now,
        "the signed branch is on the origin"
    );
    assert_eq!(
        git(&origin, &["rev-parse", &format!("{now}^")]),
        first_signed,
        "each signed commit's parent is its parent as signed"
    );
    assert!(
        git(&origin, &["cat-file", "commit", now.as_str()])
            .contains("\ngpgsig -----BEGIN SSH SIGNATURE"),
        "it holds the forge's signature"
    );
    assert!(
        fs::read_to_string(dir.path().join("payload"))
            .expect("what was signed")
            .contains(&format!("\nparent {first_signed}\n")),
        "the key signed the commit as it points at its parent signed"
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
    assert_eq!(
        borrow::arrive(&forge.git, &borrower, "main", &head, &answered[0]).expect("taken back"),
        Some("refs/heads/main".to_owned()),
        "the borrower's branch moves"
    );
    assert_eq!(
        git(&borrower, &["rev-parse", "main"]),
        *now,
        "to the signed head"
    );
    assert_eq!(
        git(&dir.path().join("forge-repo"), &["for-each-ref", FETCHED]),
        "",
        "what was fetched and signed to push is let go"
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
    let was = borrow::resolved(&forge.git, &borrower, "HEAD").expect("resolved");
    let bundle = borrow::bundle(&forge.git, &borrower, &base, &was).expect("bundled");
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
        *now,
        "the branch stands as it was"
    );
}

#[tokio::test]
async fn a_bundle_whose_head_isnt_a_commit_is_refused_before_anything_is_signed() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let (origin, base) = repos_in(dir.path());
    let mut forge = forge_in(dir.path());
    forge
        .repos
        .insert("core".to_owned(), dir.path().join("forge-repo"));
    let borrower = dir.path().join("borrower");
    git(
        &borrower,
        &["commit", "--quiet", "--allow-empty", "-m", "tagged"],
    );
    git(
        &borrower,
        &["-c", "tag.gpgSign=false", "tag", "-a", "-m", "a tag", "v1"],
    );
    let file = dir.path().join("tag.bundle");
    git(
        &borrower,
        &[
            "bundle",
            "create",
            "--quiet",
            file.to_str().expect("utf-8"),
            "v1",
            &format!("^{base}"),
        ],
    );
    let pushing = Pushing {
        cell: None,
        repo: "core".to_owned(),
        branch: "agents/kestrel-fix".to_owned(),
        bundle: STANDARD.encode(fs::read(&file).expect("the bundle")),
    };
    let (status, body) =
        asked_of(&forge, &request("/push", Some("kestrel"), &json!(pushing))).await;
    assert_eq!(status, 200, "begun: {}", why(&body));
    assert!(
        matches!(
            lines_of(&body).as_slice(),
            [Pushed::Refused { why }] if why == "the bundle's head isn't a commit"
        ),
        "a tag's head is refused: {}",
        String::from_utf8_lossy(&body)
    );
    assert!(
        !dir.path().join("payload").exists(),
        "and nothing was signed for it"
    );
    assert_eq!(
        git(&origin, &["for-each-ref", "refs/heads/agents"]),
        "",
        "nor pushed"
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
