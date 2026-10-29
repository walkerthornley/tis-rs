//! Runs a config's runs, one at a time, in order.

use std::sync::{Arc, Mutex, Once};
use std::time::Instant;

use serde_json::{Map, Value};
use tracing::instrument::Instrumented;
use tracing::{Instrument, debug, info, info_span};

use crate::config;
use crate::step::{Prepare, Run};

/// How a config's runs went.
pub struct Report {
    /// How many runs the config had.
    pub total: usize,
    /// The labels of the runs that failed, in order, e.g. `swap[1]`.
    pub failed: Vec<String>,
}

impl Report {
    /// Whether every run passed.
    pub fn passed(&self) -> bool {
        self.failed.is_empty()
    }
}

pub async fn run(runs: Vec<(config::Run, Prepare)>) -> Report {
    static CAPTURE_STEP_PANICS: Once = Once::new();
    CAPTURE_STEP_PANICS.call_once(capture_step_panics);

    let total = runs.len();
    let mut failed = Vec::new();
    // Outputs that runs so far stored for later runs, by name.
    let mut stored = Map::new();
    for (run, prepare) in runs {
        // Everything logged during the run carries the step's name.
        let span = info_span!("step", name = %run.label);
        info!(parent: &span, "running");
        let started = Instant::now();
        let result = match prepare_run(&run, &stored, prepare) {
            Ok(step) => run_one(step.instrument(span.clone())).await,
            Err(err) => Err(err),
        };
        let elapsed = format!("{:.1?}", started.elapsed());
        match result {
            Ok(output) => {
                for (field, name) in run.outputs {
                    let Some(name) = name else { continue };
                    let value = output.get(&field).cloned().unwrap_or(Value::Null);
                    debug!(parent: &span, output = name, %value, "stored");
                    stored.insert(name, value);
                }
                info!(parent: &span, %elapsed, "passed");
            }
            Err(failure) => {
                info!(parent: &span, %elapsed, %failure, "failed");
                failed.push(run.label);
            }
        }
    }

    if failed.is_empty() {
        info!("all {total} steps passed");
    } else {
        info!(failed = %failed.join(", "), "{} of {total} steps failed", failed.len());
    }
    Report { total, failed }
}

/// Prepares `run` with any outputs it uses from `stored`, checking it outputs
/// every field the config stores.
pub fn prepare_run(
    run: &config::Run,
    stored: &Map<String, Value>,
    prepare: Prepare,
) -> Result<Run, String> {
    let params = config::resolve_outputs(run.params.clone(), stored)?;
    let prepared = prepare(params)?;
    if let Some(field) = run
        .outputs
        .keys()
        .find(|field| !prepared.outputs.contains(field))
    {
        return Err(format!(
            "`{}` has no output `{field}` with these parameters; see `list`",
            run.step
        ));
    }
    Ok(prepared.run)
}

tokio::task_local! {
    /// Where a step's panic is kept, set only in the step's own task.
    static FAILURE: Arc<Mutex<Option<String>>>;
}

/// Steps fail by panicking. Keeps a step's panic message and location to log
/// with its result rather than printing them as it happens. Any other panic
/// prints as usual.
fn capture_step_panics() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let kept = FAILURE.try_with(|failure| {
            let message = info.payload_as_str().unwrap_or("panicked");
            let location = info
                .location()
                .map(|location| format!(" (at {location})"))
                .unwrap_or_default();
            *failure
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                Some(format!("{message}{location}"));
        });
        if kept.is_err() {
            default_hook(info);
        }
    }));
}

async fn run_one(run: Instrumented<Run>) -> Result<Value, String> {
    let failure = Arc::new(Mutex::new(None));
    let result = tokio::spawn(FAILURE.scope(Arc::clone(&failure), run)).await;
    result.map_err(|err| {
        failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .unwrap_or_else(|| err.to_string())
    })
}
