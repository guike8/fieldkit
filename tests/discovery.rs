use fieldkit::{Config, Workbench, discovery::DiscoveryOptions, model::text, store::Store};
use serde_json::json;
use std::sync::atomic::AtomicBool;

fn workspace() -> (tempfile::TempDir, Workbench) {
    let temp = tempfile::tempdir().unwrap();
    let wb = Workbench {
        store: Store::open(temp.path()).unwrap(),
        config: Config::default(),
    };
    (temp, wb)
}

#[test]
fn a_requested_stop_preserves_pending_frontier_without_collecting() {
    let (temp, wb) = workspace();
    let session = wb
        .create_discovery(DiscoveryOptions {
            seed: "sample_handle".into(),
            ..Default::default()
        })
        .unwrap();
    let session_id = text(&session, "id");
    let stopped = wb
        .run_discovery(session_id, &AtomicBool::new(true), |_| {})
        .unwrap();
    assert_eq!(stopped["status"], "paused");
    assert_eq!(stopped["attempted"], 0);
    assert_eq!(wb.store.reports(10).unwrap().len(), 0);
    let reopened = Workbench {
        store: Store::open(temp.path()).unwrap(),
        config: Config::default(),
    };
    assert_eq!(
        reopened.discovery_jobs(session_id).unwrap()[0]["status"],
        "pending"
    );
}

#[test]
fn invalid_scope_and_budget_are_rejected_before_jobs_are_created() {
    let (_temp, wb) = workspace();
    for options in [
        DiscoveryOptions {
            seed: "alice".into(),
            max_jobs: 0,
            ..Default::default()
        },
        DiscoveryOptions {
            seed: "alice".into(),
            max_depth: 5,
            ..Default::default()
        },
        DiscoveryOptions {
            seed: "alice".into(),
            domains: vec!["*.example.org".into()],
            ..Default::default()
        },
        DiscoveryOptions {
            kind: "url".into(),
            seed: "https://outside.example/".into(),
            domains: vec!["example.org".into()],
            ..Default::default()
        },
        DiscoveryOptions {
            kind: "url".into(),
            seed: "https://user:password@example.org/".into(),
            ..Default::default()
        },
    ] {
        assert!(wb.create_discovery(options).is_err());
    }
    assert!(wb.discoveries().unwrap().is_empty());
}

#[test]
fn budget_and_failed_collector_results_remain_persistent_evidence() {
    let (_temp, wb) = workspace();
    // The normal collector rejects this address before any network operation.
    let session = wb
        .create_discovery(DiscoveryOptions {
            kind: "url".into(),
            seed: "http://127.0.0.1/".into(),
            max_jobs: 1,
            ..Default::default()
        })
        .unwrap();
    let session_id = text(&session, "id");
    let finished = wb
        .run_discovery(session_id, &AtomicBool::new(false), |_| {})
        .unwrap();
    assert_eq!(finished["attempted"], 1);
    assert_eq!(finished["status"], "budget_exhausted");
    let jobs = wb.discovery_jobs(session_id).unwrap();
    assert_eq!(jobs[0]["status"], "failed");
    assert!(!text(&jobs[0], "run_id").is_empty());
    assert_eq!(wb.store.reports(10).unwrap()[0]["status"], "invalid");
    wb.run_discovery(session_id, &AtomicBool::new(false), |_| {})
        .unwrap();
    assert_eq!(wb.store.reports(10).unwrap().len(), 1);
    assert_eq!(wb.store.verify().unwrap()["ok"], true);
}

#[test]
fn crashed_running_job_is_not_silently_collected_twice() {
    let (_temp, wb) = workspace();
    let session = wb
        .create_discovery(DiscoveryOptions {
            seed: "alice".into(),
            max_jobs: 1,
            ..Default::default()
        })
        .unwrap();
    let session_id = text(&session, "id");
    let mut job = wb.discovery_jobs(session_id).unwrap().remove(0);
    job["status"] = json!("running");
    job["attempts"] = json!(1);
    wb.store.put("discovery_jobs", &job).unwrap();
    let resumed = wb
        .run_discovery(session_id, &AtomicBool::new(false), |_| {})
        .unwrap();
    assert_eq!(resumed["attempted"], 1);
    assert_eq!(
        wb.discovery_jobs(session_id).unwrap()[0]["status"],
        "interrupted"
    );
    assert!(wb.store.reports(10).unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn discovery_lock_rejects_symlinks_and_hardlinks_before_collection() {
    use std::os::unix::fs::symlink;
    let (temp, wb) = workspace();
    let session = wb
        .create_discovery(DiscoveryOptions {
            seed: "alice".into(),
            ..Default::default()
        })
        .unwrap();
    let other = temp.path().join("other-file");
    std::fs::write(&other, b"unchanged").unwrap();
    let lock = temp.path().join("discovery.lock");
    symlink(&other, &lock).unwrap();
    assert!(
        wb.run_discovery(text(&session, "id"), &AtomicBool::new(false), |_| {})
            .is_err()
    );
    std::fs::remove_file(&lock).unwrap();
    std::fs::hard_link(&other, &lock).unwrap();
    assert!(
        wb.run_discovery(text(&session, "id"), &AtomicBool::new(false), |_| {})
            .is_err()
    );
    assert!(wb.store.reports(10).unwrap().is_empty());
    assert_eq!(std::fs::read(&other).unwrap(), b"unchanged");
}
