//! What a step is, and how a run of one is prepared from a config's
//! parameters.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use serde_json::Value;

/// A step a config can name. Its fields are its parameters, as a config gives
/// them. What a run returns is its output, whose fields a config can store for
/// later runs' parameters.
pub trait Step: Serialize + DeserializeOwned + Send + 'static {
    /// What a run returns for later runs to use: a struct, whose fields a
    /// config can store by name, or `()` for nothing.
    type Output: Serialize + JsonSchema;

    /// The key a config runs the step by.
    fn name() -> String;

    /// What the step does, for [`Tis::list`](crate::Tis::list).
    fn about() -> String;

    /// Parameters for one run, for [`Tis::list`](crate::Tis::list) and
    /// [`Tis::example_config`](crate::Tis::example_config). They can name the
    /// example defaults as `defaults.<name>`; see
    /// [`Tis::with_example_defaults`](crate::Tis::with_example_defaults).
    fn example() -> Self;

    /// Checks what the parameters' types can't, such as an amount against
    /// its currency. The whole config is checked before any step starts.
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }

    /// The fields of a run's output a config can store: by default, every
    /// field of [`Step::Output`]. A step whose parameters decide which fields
    /// it outputs says which.
    fn outputs(&self) -> Vec<String> {
        schemars::schema_for!(Self::Output)
            .get("properties")
            .and_then(Value::as_object)
            .map(|fields| fields.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Runs the step, which fails by panicking.
    fn run(self) -> impl Future<Output = Self::Output> + Send;
}

/// A run, which gives its output as JSON.
pub type Run = Pin<Box<dyn Future<Output = Value> + Send>>;

/// A run whose parameters were checked, ready to start.
pub struct Prepared {
    pub run: Run,
    /// The fields of its output a config can store.
    pub outputs: Vec<String>,
}

/// Checks a run's parameters and prepares the run.
pub type Prepare = fn(Value) -> Result<Prepared, String>;

pub fn prepare<T: Step>(params: Value) -> Result<Prepared, String> {
    let params = match params {
        Value::Null => Value::Object(Default::default()),
        params => params,
    };
    let step: T = serde_json::from_value(params).map_err(|err| err.to_string())?;
    step.validate()?;
    Ok(Prepared {
        outputs: step.outputs(),
        run: Box::pin(
            async move { serde_json::to_value(step.run().await).expect("output serializes") },
        ),
    })
}

pub fn example_entry<T: Step>(yaml: bool) -> String {
    let example = T::example();
    let entry = ExampleEntry {
        name: T::name(),
        outputs: example.outputs(),
        params: example,
    };
    if yaml {
        // A one-item list, which reads as the item `steps` would list.
        to_text(&[entry], true)
    } else {
        to_text(&entry, false)
    }
}

/// A step's example as `steps` lists it, `{ "<name>": <params> }`, with any
/// outputs listed as not stored.
struct ExampleEntry<T> {
    name: String,
    params: T,
    outputs: Vec<String>,
}

impl<T: Serialize> Serialize for ExampleEntry<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut entry = serializer.serialize_map(None)?;
        entry.serialize_entry(&self.name, &self.params)?;
        if !self.outputs.is_empty() {
            let unstored: BTreeMap<&str, Option<&str>> = self
                .outputs
                .iter()
                .map(|field| (field.as_str(), None))
                .collect();
            entry.serialize_entry("outputs", &unstored)?;
        }
        entry.end()
    }
}

/// `value` as one line of JSON, or as YAML. Serialized straight from its
/// type, which keeps its fields' order where a `serde_json::Value` wouldn't.
pub fn to_text(value: &impl Serialize, yaml: bool) -> String {
    if yaml {
        serde_saphyr::to_string(value).expect("example serializes")
    } else {
        serde_json::to_string(value).expect("example serializes")
    }
}
