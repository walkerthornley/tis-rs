//! Runs steps as chosen and parameterised by a JSON or YAML config. See
//! README.md.
//!
//! A step is a struct whose fields are its parameters, as a config gives
//! them, implementing [`Step`]. A [`Tis`] holds the steps a config can name:
//! it lists them, writes an example config, reads and checks a config, and
//! runs it.
//!
//! ```ignore
//! let mut tis = Tis::new();
//! tis.register::<SignUp>().register::<SendTokens>();
//! let config = tis.read_config(path, &defaults)?;
//! let report = tis.run(config).await;
//! ```

mod config;
mod runner;
mod step;

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use serde_json::{Map, Value};

pub use crate::config::parse_defaults;
pub use crate::runner::Report;
pub use crate::step::Step;
use crate::step::{Prepare, example_entry, prepare, to_text};

/// The steps a config can name, by name, and how to read and run a config of
/// them.
#[derive(Default)]
pub struct Tis {
    steps: BTreeMap<String, Registered>,
    /// What [`Tis::example_config`] defines as `defaults`.
    example_defaults: Map<String, Value>,
}

/// A [`Step`] as the runner sees it, whatever its parameters.
struct Registered {
    about: String,
    /// The example parameters, as one line of JSON or as YAML.
    example: fn(yaml: bool) -> String,
    /// The example as an entry of a config's `steps`: one line of JSON, or a
    /// YAML list item.
    example_entry: fn(yaml: bool) -> String,
    /// The fields the example run outputs.
    outputs: Vec<String>,
    /// Checked on every run's parameters before any step starts, except
    /// where they use outputs of earlier runs.
    prepare: Prepare,
}

/// A config that [`Tis`] read and checked, ready to [run](Tis::run).
pub struct Config {
    /// Each run, with how to prepare it once earlier runs' outputs are in.
    runs: Vec<(config::Run, Prepare)>,
}

impl Tis {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the `defaults` that [`Tis::example_config`] defines, which steps'
    /// [examples](Step::example) can name as `defaults.<name>`.
    pub fn with_example_defaults<K: Into<String>, V: Into<Value>>(
        mut self,
        defaults: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        self.example_defaults = defaults
            .into_iter()
            .map(|(name, value)| (name.into(), value.into()))
            .collect();
        self
    }

    /// Adds `T` to the steps a config can name, as [`Step::name`]. Panics if
    /// a step registered earlier has the same name.
    pub fn register<T: Step>(&mut self) -> &mut Self {
        let registered = Registered {
            about: T::about(),
            example: |yaml| to_text(&T::example(), yaml),
            example_entry: example_entry::<T>,
            outputs: T::example().outputs(),
            prepare: prepare::<T>,
        };
        let name = T::name();
        assert!(
            !self.steps.contains_key(&name),
            "two steps are named {name}"
        );
        self.steps.insert(name, registered);
        self
    }

    /// Every step, by name, with what it does, its example parameters as JSON
    /// or YAML, and what the example outputs.
    pub fn list(&self, yaml: bool) -> String {
        self.steps
            .iter()
            .map(|(name, step)| {
                let mut entry = format!("{name}\n    {}\n", step.about);
                for line in (step.example)(yaml).lines() {
                    entry += &format!("    {line}\n");
                }
                if !step.outputs.is_empty() {
                    entry += &format!("    outputs {}\n", step.outputs.join(", "));
                }
                entry
            })
            .collect()
    }

    /// A config that runs every step once, with its example parameters,
    /// listing any outputs as not stored. As JSON, a step to a line, or as
    /// YAML.
    pub fn example_config(&self, yaml: bool) -> String {
        let defaults = &self.example_defaults;
        let steps = self.steps.values().map(|step| (step.example_entry)(yaml));

        if yaml {
            // Each step is a one-item list, so together they're the whole list.
            let defaults = to_text(&BTreeMap::from([("defaults", defaults)]), true);
            format!("{defaults}steps:\n{}", steps.collect::<String>())
        } else {
            let steps: Vec<String> = steps.map(|step| format!("        {step}")).collect();
            format!(
                "{{\n    \"defaults\": {},\n    \"steps\": [\n{}\n    ]\n}}\n",
                to_text(defaults, false),
                steps.join(",\n")
            )
        }
    }

    /// Reads and checks a whole config, JSON if it starts with `{` and YAML
    /// otherwise, so a mistake in it fails before any step starts. `defaults`,
    /// such as from the command line, are added to the config's, overriding
    /// any it names too.
    pub fn parse_config(
        &self,
        text: &str,
        defaults: &Map<String, Value>,
    ) -> Result<Config, String> {
        let runs = config::parse(text, defaults)?;
        if runs.is_empty() {
            return Err("it names no steps".to_owned());
        }
        let runs = runs
            .into_iter()
            .map(|run| {
                let step = self
                    .steps
                    .get(run.step.as_str())
                    .ok_or_else(|| format!("no step named `{}`; see `list`", run.step))?;
                // Parameters that use outputs can only be checked once those are in.
                if config::outputs_used(&run.params).is_empty()
                    && let Err(err) = runner::prepare_run(&run, &Map::new(), step.prepare)
                {
                    return Err(format!("{}: {err}", run.label));
                }
                Ok((run, step.prepare))
            })
            .collect::<Result<_, _>>()?;
        Ok(Config { runs })
    }

    /// [`Tis::parse_config`] on the file at `path`, or on stdin if `path` is
    /// `-`.
    pub fn read_config(
        &self,
        path: &Path,
        defaults: &Map<String, Value>,
    ) -> Result<Config, String> {
        let text = if path.as_os_str() == "-" {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(|err| format!("stdin: {err}"))?;
            text
        } else {
            std::fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?
        };
        self.parse_config(&text, defaults)
    }

    /// Runs `config`'s runs one at a time, in order, each in a `step{name=…}`
    /// span, logging each one's result and then the totals. A step that
    /// panics fails, and its panic is logged with its result rather than
    /// printed. Runs that use the outputs of a failed run fail too.
    pub async fn run(&self, config: Config) -> Report {
        runner::run(config.runs).await
    }
}

#[cfg(test)]
mod tests {
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Add {
        a: i64,
        b: i64,
    }

    #[derive(Serialize, JsonSchema)]
    struct Sum {
        sum: i64,
    }

    impl Step for Add {
        type Output = Sum;

        fn name() -> String {
            "add".to_owned()
        }

        fn about() -> String {
            "Adds `a` and `b`".to_owned()
        }

        fn example() -> Self {
            Add { a: 1, b: 2 }
        }

        async fn run(self) -> Sum {
            Sum {
                sum: self.a.checked_add(self.b).expect("sum overflows"),
            }
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
            "`value` equals `equals`".to_owned()
        }

        fn example() -> Self {
            Expect {
                value: "defaults.three".into(),
                equals: 3.into(),
            }
        }

        fn validate(&self) -> Result<(), String> {
            match self.equals {
                Value::Null => Err("`equals` can't be null".to_owned()),
                _ => Ok(()),
            }
        }

        async fn run(self) {
            assert_eq!(self.value, self.equals);
        }
    }

    fn tis() -> Tis {
        let mut tis = Tis::new().with_example_defaults([("three", 3)]);
        tis.register::<Add>().register::<Expect>();
        tis
    }

    #[test]
    fn example_config_is_valid() {
        let tis = tis();
        for yaml in [false, true] {
            if let Err(err) = tis.parse_config(&tis.example_config(yaml), &Map::new()) {
                panic!("{err} (yaml: {yaml})");
            }
        }
    }

    #[test]
    #[should_panic(expected = "two steps are named add")]
    fn step_names_are_unique() {
        tis().register::<Add>();
    }

    #[test]
    fn mistakes_fail_before_any_step_starts() {
        let tis = tis();
        for config in [
            r#"{"steps": []}"#,
            r#"{"steps": [{"subtract": {}}]}"#,
            r#"{"steps": [{"add": {"a": 1}}]}"#,
            r#"{"steps": [{"add": {"a": 1, "b": 2, "c": 3}}]}"#,
            r#"{"steps": [{"expect": {"value": 1, "equals": null}}]}"#,
            r#"{"steps": [{"expect": {"value": 1, "equals": 1}, "outputs": {"sum": "x"}}]}"#,
        ] {
            assert!(tis.parse_config(config, &Map::new()).is_err(), "{config}");
        }
    }

    #[tokio::test]
    async fn later_runs_use_outputs_of_earlier_ones_that_passed() {
        let tis = tis();
        let config = tis
            .parse_config(
                r#"{"steps": [
                    {"add": {"a": 1, "b": 2}, "outputs": {"sum": "three"}},
                    {"expect": {"value": "outputs.three", "equals": 3}},
                    {"expect": {"value": "outputs.three", "equals": 4}},
                    {"add": {"a": 9223372036854775807, "b": 1}, "outputs": {"sum": "big"}},
                    {"expect": {"value": "outputs.big", "equals": 0}}
                ]}"#,
                &Map::new(),
            )
            .expect("parses");
        let report = tis.run(config).await;
        assert_eq!(report.total, 5);
        assert_eq!(report.failed, ["expect[1]", "add[1]", "expect[2]"]);
        assert!(!report.passed());
    }
}
