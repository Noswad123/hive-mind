use super::*;

fn fixture() -> (tempfile::TempDir, Remote) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().canonicalize().unwrap().join("remote.git");
    fs::create_dir(&path).unwrap();
    plumbing(&path, &["init", "--bare", "--template=", "."], b"").unwrap();
    let remote = Remote::initialize(&path, "synthetic", &value(0)).unwrap();
    (temp, remote)
}
fn value(n: usize) -> Value {
    json!({"origin":"Loopback","namespace":"synthetic","n":n})
}
fn fresh(exchange: Exchange) -> Receipt {
    match exchange {
        Exchange::Fresh(r) => r,
        _ => panic!("expected fresh receipt"),
    }
}

#[test]
fn stale_lease_rejects_even_when_stale_proposal_would_be_fast_forward() {
    let (_temp, remote) = fixture();
    let stale = remote.observe().unwrap();
    let second = remote.reopen();
    fresh(
        second
            .exchange(
                &second.observe().unwrap(),
                &value(1),
                "one",
                "worker",
                false,
            )
            .unwrap(),
    );
    // This proposal is a descendant of the server's *new* tip, so plain FF
    // would succeed. The lease must still compare the old observed root.
    let latest = remote.observe().unwrap();
    let descendant = commit(latest.stage.path(), Some(&latest.oid), &value(2)).unwrap();
    let (ok, out) = git(
        latest.stage.path(),
        &[
            "push",
            "--porcelain",
            &format!("--force-with-lease={REF}:{}", stale.oid),
            "--receive-pack=/usr/bin/git -c core.hooksPath=/dev/null receive-pack",
            "--",
            remote.path.to_str().unwrap(),
            &format!("{descendant}:{REF}"),
        ],
        b"",
    )
    .unwrap();
    assert!(!ok);
    assert!(out.contains("[rejected] (stale info)"));
    assert_eq!(remote.observe().unwrap().oid, latest.oid);
    assert!(matches!(
        remote
            .exchange(&stale, &value(3), "three", "worker", false)
            .unwrap(),
        Exchange::Rejected
    ));
}

#[test]
fn up_to_date_is_never_a_fresh_receipt_and_matching_readback_never_upgrades_unknown() {
    let (_temp, remote) = fixture();
    let view = remote.observe().unwrap();
    let new = commit(view.stage.path(), Some(&view.oid), &value(1)).unwrap();
    assert!(matches!(
        remote
            .push(view.stage.path(), &view.oid, &new, "one", "worker", true)
            .unwrap(),
        Exchange::Unknown
    ));
    assert_eq!(remote.observe().unwrap().oid, new);
    // Git accepts an already-current ref without evaluating a stale lease.
    // Its '=' acknowledgement is deliberately NOT a fresh update receipt.
    assert!(matches!(
        remote
            .push(view.stage.path(), &view.oid, &new, "one", "worker", false)
            .unwrap(),
        Exchange::Unknown
    ));
}

#[test]
fn deleted_ref_never_reinitializes_and_expected_inception_cannot_be_rebound() {
    let (_temp, remote) = fixture();
    let view = remote.observe().unwrap();
    fresh(
        remote
            .exchange(&view, &value(1), "one", "worker", false)
            .unwrap(),
    );
    let other = Remote {
        path: remote.path.clone(),
        namespace: remote.namespace.clone(),
        root: "a".repeat(40),
    };
    assert!(other.observe().is_err());
    let stage = stage().unwrap();
    plumbing(
        stage.path(),
        &[
            "push",
            "--porcelain",
            "--receive-pack=/usr/bin/git -c core.hooksPath=/dev/null receive-pack",
            "--",
            remote.path.to_str().unwrap(),
            &format!(":{REF}"),
        ],
        b"",
    )
    .unwrap();
    assert!(remote.observe().is_err());
    assert!(matches!(
        remote
            .exchange(&view, &value(2), "two", "worker", false)
            .unwrap(),
        Exchange::Rejected
    ));
    assert!(remote.observe().is_err());
}

#[test]
fn explicit_genesis_race_only_one_fresh_inception_no_implicit_genesis() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().canonicalize().unwrap().join("remote.git");
    fs::create_dir(&path).unwrap();
    plumbing(&path, &["init", "--bare", "--template=", "."], b"").unwrap();
    let barrier = std::sync::Barrier::new(2);
    let winners = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..2)
            .map(|n| {
                let path = &path;
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    Remote::initialize(path, "synthetic", &value(n)).is_ok()
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|w| usize::from(w.join().unwrap()))
            .sum::<usize>()
    });
    assert_eq!(winners, 1);
    assert!(Remote::initialize(&path, "synthetic", &value(5)).is_err());
}

#[test]
fn proposal_wrong_parent_and_test_origin_promotion_are_refused() {
    let (_temp, remote) = fixture();
    let view = remote.observe().unwrap();
    let orphan = commit(view.stage.path(), None, &value(4)).unwrap();
    assert!(
        remote
            .push(
                view.stage.path(),
                &view.oid,
                &orphan,
                "one",
                "worker",
                false
            )
            .is_err()
    );
    let mut promoted = value(2);
    promoted["origin"] = json!("Google");
    assert!(
        remote
            .exchange(&view, &promoted, "one", "worker", false)
            .is_err()
    );
    // Bypass the proposal schema in a fixture, as if an administrator edited raw
    // state claims. The independent inception/origin floor still rejects it.
    let edited = commit(view.stage.path(), Some(&view.oid), &promoted).unwrap();
    fresh(
        remote
            .push(
                view.stage.path(),
                &view.oid,
                &edited,
                "one",
                "worker",
                false,
            )
            .unwrap(),
    );
    assert!(remote.observe().is_err());
}

#[test]
fn config_helper_includes_and_foreign_objects_refused_before_transport() {
    for injection in [
        "[credential]\n\thelper = !touch SHOULD_NOT_EXIST\n",
        "[include]\n\tpath = /dev/null\n",
        "[url \"file:///\"]\n\tinsteadOf = https://github.com/\n",
    ] {
        let (_temp, remote) = fixture();
        let mut config = fs::read_to_string(remote.path.join("config")).unwrap();
        config.push_str(injection);
        fs::write(remote.path.join("config"), config).unwrap();
        assert!(remote.observe().is_err());
        assert!(!remote.path.join("SHOULD_NOT_EXIST").exists());
    }
}

#[test]
fn malformed_tree_merge_and_wrong_ref_cannot_be_observed_as_state() {
    for case in 0..3 {
        let (_temp, remote) = fixture();
        let view = remote.observe().unwrap();
        let blob = plumbing(view.stage.path(), &["hash-object", "-w", "--stdin"], b"{}").unwrap();
        let tree = plumbing(
            view.stage.path(),
            &["mktree"],
            format!("100644 blob {blob}\twrong.json\n").as_bytes(),
        )
        .unwrap();
        let bad = plumbing(
            view.stage.path(),
            &["commit-tree", &tree, "-p", &view.oid],
            b"bad state",
        )
        .unwrap();
        if case == 0 {
            fresh(
                remote
                    .push(view.stage.path(), &view.oid, &bad, "bad", "worker", false)
                    .unwrap(),
            );
            assert!(remote.observe().is_err());
        } else if case == 1 {
            let sibling = commit(view.stage.path(), Some(&view.oid), &value(3)).unwrap();
            let merge = plumbing(
                view.stage.path(),
                &["commit-tree", &tree, "-p", &view.oid, "-p", &sibling],
                b"merge",
            )
            .unwrap();
            assert!(
                remote
                    .push(
                        view.stage.path(),
                        &view.oid,
                        &merge,
                        "merge",
                        "worker",
                        false
                    )
                    .is_err()
            );
        } else {
            // An unrelated branch never substitutes for a deleted fixed ref.
            plumbing(
                view.stage.path(),
                &[
                    "push",
                    "--porcelain",
                    "--receive-pack=/usr/bin/git -c core.hooksPath=/dev/null receive-pack",
                    "--",
                    remote.path.to_str().unwrap(),
                    &format!("{bad}:refs/heads/other"),
                    &format!(":{REF}"),
                ],
                b"",
            )
            .unwrap();
            assert!(remote.observe().is_err());
        }
    }
}

#[test]
fn isolated_git_process_has_no_ambient_secret_or_trace_and_kills_descendants() {
    let stage = stage().unwrap();
    let check = "!test -z \"${GITHUB_TOKEN+x}${HIVE_HOSTED_TASKS_WRITE_RECORD+x}${GOOGLE_ACCESS_TOKEN+x}${SSH_AUTH_SOCK+x}${GIT_TRACE+x}\"";
    plumbing(
        stage.path(),
        &["-c", &format!("alias.isolated={check}"), "isolated"],
        b"",
    )
    .unwrap();
    let marker = stage.path().join("escaped-child");
    let background = format!(
        "alias.background=!( /bin/sleep 1; /usr/bin/touch '{}' ) &",
        marker.display()
    );
    plumbing(stage.path(), &["-c", &background, "background"], b"").unwrap();
    std::thread::sleep(Duration::from_millis(1200));
    assert!(
        !marker.exists(),
        "descendant escaped successful-parent group cleanup"
    );
    // Output overflow fails closed and runs the same RAII cleanup path.
    assert!(
        git(
            stage.path(),
            &[
                "-c",
                "alias.overflow=!/usr/bin/yes synthetic-output",
                "overflow"
            ],
            b""
        )
        .is_err()
    );
}

#[test]
fn porcelain_ack_must_be_complete_exact_fresh_success_for_only_fixed_ref() {
    let (_temp, remote) = fixture();
    let old = "a".repeat(40);
    let new = "b".repeat(40);
    let valid = format!(
        "To {}\n \t{new}:{REF}\t{old}..{new}\nDone\n",
        remote.path.display()
    );
    let receipt = fresh(
        remote
            .acknowledgement(&old, &new, "operation", "invocation", true, &valid)
            .unwrap(),
    );
    assert!(receipt.binds(&remote, &old, &new, "operation", "invocation"));
    assert!(!receipt.binds(&remote, &old, &new, "operation", "other-worker"));
    assert!(!receipt.binds(&remote, &old, &new, "other-operation", "invocation"));
    for bad in [
        String::new(),
        valid.replace("Done\n", ""),
        valid.replace(REF, "refs/heads/main"),
        valid
            .replace(&format!("{old}..{new}"), "[up to date]")
            .replace("\n \t", "\n=\t"),
        valid.replace("\n \t", "\n+\t"),
        valid.replace(&old, "c".repeat(40).as_str()),
        format!("{valid} \t{new}:refs/heads/other\t{old}..{new}\n"),
        valid.replace(&format!("{old}..{new}"), "aaaaaaa..bbbbbbb"),
    ] {
        assert!(matches!(
            remote
                .acknowledgement(&old, &new, "op", "worker", true, &bad)
                .unwrap(),
            Exchange::Unknown
        ));
    }
    assert!(matches!(
        remote
            .acknowledgement(&old, &new, "op", "worker", false, &valid)
            .unwrap(),
        Exchange::Unknown
    ));
}
