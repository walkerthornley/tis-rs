//! A run's logged failures never quote a default given at run time.
//!
//! A test binary of its own, as it reads logs: tracing caches whether anyone
//! listens at each log site for the whole process, so a subscriber set for one
//! test can miss events another test's thread reached first without one.

use std::io::Write;
use std::sync::{Arc, Mutex};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tis_rs::{Step, Tis};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Give {
    value: Value,
}

#[derive(Serialize, JsonSchema)]
struct Given {
    value: Value,
}

impl Step for Give {
    type Output = Given;

    fn name() -> String {
        "give".to_owned()
    }

    fn about() -> String {
        "Outputs `value`".to_owned()
    }

    fn example() -> Self {
        Give { value: 1.into() }
    }

    async fn run(self) -> Given {
        Given { value: self.value }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Expect {
    value: Value,
    equals: Value,
}

impl Step for Expect {
    type Output = ();

    fn name() -> String {
        "expect".to_owned()
    }

    fn about() -> String {
        "`value` equals `equals`, failing with both quoted if not".to_owned()
    }

    fn example() -> Self {
        Expect {
            value: 1.into(),
            equals: 1.into(),
        }
    }

    async fn run(self) {
        assert_eq!(self.value, self.equals);
    }
}

#[tokio::test]
async fn a_failure_reads_a_default_by_its_name() {
    let mut tis = Tis::new();
    tis.register::<Give>().register::<Expect>();
    let defaults = Map::from_iter([("secret".to_owned(), Value::from("0xab \"cd\""))]);
    // `expect` uses an output, so it is only checked once `give` has run.
    let config = tis
        .parse_config(
            r#"{"steps": [
                {"give": {"value": 3}, "outputs": {"value": "three"}},
                {"expect": {"value": "outputs.three", "equals": "defaults.secret"}}
            ]}"#,
            &defaults,
        )
        .expect("parses");

    let logs = Logs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_ansi(false)
        .finish();
    let report = {
        // The test's runtime runs every task on this thread, so the steps'
        // logs land here too.
        let _logging = tracing::subscriber::set_default(subscriber);
        tis.run(config).await
    };

    assert_eq!(report.failed, ["expect"]);
    let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("failed") && logs.contains("defaults.secret") && !logs.contains("0xab"),
        "{logs}"
    );
}

/// Where the test's logs are written.
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logs {
    type Writer = Logs;

    fn make_writer(&'a self) -> Logs {
        self.clone()
    }
}
