use super::*;

#[test]
fn capture_failure_is_checkpointed_once_without_retry() {
    struct Failing {
        path: std::path::PathBuf,
        runs: usize,
    }
    impl Tools for Failing {
        fn tool(&mut self, name: &str, _: Value) -> Result<Value> {
            match name {
                "fieldkit_investigation_create" => Ok(json!({"id":"inv"})),
                "fieldkit_investigation_plan_step" => Ok(json!({"id":"step"})),
                "fieldkit_investigation_run_step" => {
                    self.runs += 1;
                    let receipt = read_json(&self.path)?;
                    ensure!(
                        receipt["inputs"][0]["status"] == "attempting"
                            && receipt["inputs"][0]["step_id"] == "step",
                        "Missing attempt checkpoint"
                    );
                    anyhow::bail!("Synthetic timeout")
                }
                _ => anyhow::bail!("Unexpected tool"),
            }
        }
    }
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("receipt.json");
    let mut output = Output::new(&path).unwrap();
    let mut client = Failing {
        path: path.clone(),
        runs: 0,
    };
    let task = json!({"id":"synthetic","track":"replay","question":"synthetic","mode":"organization","inputs":[{"id":"seed","url":"https://example.invalid","sha256":"0".repeat(64)}]});
    assert!(capture(&mut client, &task, &mut output).is_err());
    assert_eq!(client.runs, 1);
    let receipt = read_json(&path).unwrap();
    assert_eq!(receipt["status"], "incomplete");
    assert_eq!(receipt["inputs"][0]["status"], "attempting");
    assert!(Output::new(&path).is_err());
}
