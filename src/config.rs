//! The config: which steps to run, in order, and their parameters. JSON if it
//! starts with `{`, and YAML otherwise.
//!
//! ```json
//! {
//!     "defaults": { "user_a": "+12345678901" },
//!     "steps": [
//!         { "sign_up": {}, "outputs": { "msisdn": "new_user" } },
//!         { "send": { "from": "defaults.user_a", "to": "outputs.new_user", "amount": 2 } },
//!         { "send": { "from": "outputs.new_user", "to": "defaults.user_a", "amount": 1 } }
//!     ]
//! }
//! ```
//!
//! Each entry in `steps` has a key that is the step's name, holding its
//! parameters. Any string `"defaults.<name>"` in them stands for that entry of
//! `defaults`, which the command line can add to or override.
//!
//! An entry can also have `outputs`, which stores fields of what the run
//! outputs for later runs, as `{ "<field>": "<name>" }`, or `null` not to
//! store that field. Any string `"outputs.<name>"` in a later run's parameters
//! stands for what was stored as `name`. Outputs are kept apart from
//! `defaults`, so `outputs.user_a` and `defaults.user_a` can differ.

use std::collections::{BTreeMap, HashMap};

use serde::Deserialize;
use serde_json::{Map, Value};

/// The config as written. Unknown keys are errors, to catch typos.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    defaults: Map<String, Value>,
    /// Each `{ "<step name>": <params> }`, with any `"outputs"`.
    steps: Vec<Map<String, Value>>,
}

/// One run of a step.
pub struct Run {
    /// The step name, indexed when the config runs it more than once.
    pub label: String,
    pub step: String,
    /// With every `defaults.<name>` reference replaced. `outputs.<name>`
    /// references are replaced when the run's turn comes, by
    /// [`resolve_outputs`].
    pub params: Value,
    /// Fields of the run's output, each with the name to store it as for
    /// later runs, or `None` not to store it.
    pub outputs: BTreeMap<String, Option<String>>,
}

/// `defaults` from the command line are added to the config's, overriding
/// any it names too.
pub fn parse(text: &str, defaults: &Map<String, Value>) -> Result<Vec<Run>, String> {
    let mut file: File = if text.trim_start().starts_with('{') {
        serde_json::from_str(text).map_err(|err| err.to_string())?
    } else {
        serde_saphyr::from_str(text).map_err(|err| err.to_string())?
    };
    file.defaults.extend(defaults.clone());

    let entries = file
        .steps
        .into_iter()
        .enumerate()
        .map(|(i, mut entry)| {
            let outputs = match entry.remove("outputs") {
                Some(outputs) => serde_json::from_value(outputs)
                    .map_err(|err| format!("steps[{i}].outputs: {err}"))?,
                None => BTreeMap::new(),
            };
            let mut entry = entry.into_iter();
            match (entry.next(), entry.next()) {
                (Some((step, params)), None) => Ok((step, params, outputs)),
                _ => Err(format!(
                    "steps[{i}] must have exactly one key besides `outputs`, the step's name"
                )),
            }
        })
        .collect::<Result<Vec<(String, Value, BTreeMap<String, Option<String>>)>, String>>()?;

    let mut runs_of = HashMap::<&str, usize>::new();
    for (step, ..) in &entries {
        *runs_of.entry(step).or_default() += 1;
    }

    let mut seen = HashMap::<&str, usize>::new();
    // The run that stores each output name.
    let mut stored = HashMap::<&str, String>::new();
    let mut runs = Vec::new();
    for (step, params, outputs) in &entries {
        let label = if runs_of[step.as_str()] > 1 {
            let nth = seen.entry(step).or_default();
            *nth += 1;
            format!("{step}[{}]", *nth - 1)
        } else {
            step.clone()
        };
        let params = resolve(params.clone(), "defaults.", &file.defaults)
            .map_err(|name| format!("{label}: no default named `{name}`"))?;
        if let Some(name) = outputs_used(&params)
            .into_iter()
            .find(|name| !stored.contains_key(*name))
        {
            return Err(format!(
                "{label}: no earlier step stores an output as `{name}`"
            ));
        }
        for name in outputs.values().flatten() {
            if let Some(earlier) = stored.insert(name, label.clone()) {
                return Err(format!(
                    "{label}: {earlier} already stores an output as `{name}`"
                ));
            }
        }
        runs.push(Run {
            label,
            step: step.clone(),
            params,
            outputs: outputs.clone(),
        });
    }
    Ok(runs)
}

/// Defaults given on the command line, as a JSON object like a config's
/// `defaults`. Fits clap's `value_parser`. Its error doesn't quote the text,
/// which is where a secret goes.
pub fn parse_defaults(json: &str) -> Result<Map<String, Value>, String> {
    // Read as any JSON first: serde_json's syntax errors say where the text
    // went wrong, but its type errors quote it.
    let not =
        match serde_json::from_str(json).map_err(|err| format!("expected a JSON object: {err}"))? {
            Value::Object(defaults) => return Ok(defaults),
            Value::Null => "null",
            Value::Bool(_) => "a boolean",
            Value::Number(_) => "a number",
            Value::String(_) => "a string",
            Value::Array(_) => "an array",
        };
    Err(format!("expected a JSON object, not {not}"))
}

/// Keeps the values of defaults given at run time out of what's reported,
/// putting `defaults.<name>` where one was. Only those, as that's where a
/// secret goes: the config's own are in the config for anyone to read.
pub struct Redact {
    /// Each string in a default, as written and as `{:?}` escapes it, with
    /// what replaces it. Longest first, so a value inside another goes last.
    values: Vec<(String, String)>,
}

impl Redact {
    pub fn new(defaults: &Map<String, Value>) -> Self {
        let mut values = Vec::new();
        for (name, value) in defaults {
            for raw in strings(value) {
                // Serde's errors quote a string escaped, as `{:?}` does.
                let escaped = format!("{raw:?}");
                let escaped = &escaped[1..escaped.len() - 1];
                for form in [raw, escaped] {
                    if !form.is_empty() {
                        values.push((form.to_owned(), format!("defaults.{name}")));
                    }
                }
            }
        }
        values.sort_by_key(|(value, _)| std::cmp::Reverse(value.len()));
        Self { values }
    }

    pub fn apply(&self, mut message: String) -> String {
        for (value, reference) in &self.values {
            message = message.replace(value, reference);
        }
        message
    }
}

/// Every string in `value`, however deep.
fn strings(value: &Value) -> Vec<&str> {
    match value {
        Value::String(s) => vec![s],
        Value::Array(items) => items.iter().flat_map(strings).collect(),
        Value::Object(fields) => fields.values().flat_map(strings).collect(),
        _ => Vec::new(),
    }
}

/// The names of the outputs `params` use, as `"outputs.<name>"`.
pub fn outputs_used(params: &Value) -> Vec<&str> {
    match params {
        Value::String(s) => s.strip_prefix("outputs.").into_iter().collect(),
        Value::Array(items) => items.iter().flat_map(outputs_used).collect(),
        Value::Object(fields) => fields.values().flat_map(outputs_used).collect(),
        _ => Vec::new(),
    }
}

/// Replaces every `outputs.<name>` reference in a run's parameters with what
/// an earlier run stored as `name`, from `stored`.
pub fn resolve_outputs(params: Value, stored: &Map<String, Value>) -> Result<Value, String> {
    // `parse` checked an earlier run stores each, so a missing one failed.
    resolve(params, "outputs.", stored)
        .map_err(|name| format!("`outputs.{name}` wasn't stored, as the step outputting it failed"))
}

/// Replaces every string `"<prefix><name>"` in `value` with entry `name` of
/// `entries`, failing with the first `name` that has no entry.
fn resolve(value: Value, prefix: &str, entries: &Map<String, Value>) -> Result<Value, String> {
    Ok(match value {
        Value::String(s) => match s.strip_prefix(prefix) {
            Some(name) => entries.get(name).cloned().ok_or_else(|| name.to_owned())?,
            None => Value::String(s),
        },
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| resolve(item, prefix, entries))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(key, value)| Ok((key, resolve(value, prefix, entries)?)))
                .collect::<Result<_, String>>()?,
        ),
        other => other,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn later_runs_use_stored_outputs() {
        let runs = parse(
            r#"{"steps": [
                {"a": {}, "outputs": {"msisdn": "user"}},
                {"b": {"users": ["outputs.user", "+1"]}}
            ]}"#,
            &Map::new(),
        )
        .expect("parses");
        assert_eq!(runs[0].outputs["msisdn"].as_deref(), Some("user"));

        let stored = Map::from_iter([("user".to_owned(), json!("+889"))]);
        let params = resolve_outputs(runs[1].params.clone(), &stored).expect("resolves");
        assert_eq!(params, json!({"users": ["+889", "+1"]}));
        assert!(resolve_outputs(runs[1].params.clone(), &Map::new()).is_err());
    }

    #[test]
    fn an_output_is_stored_once_before_it_is_used() {
        for config in [
            r#"{"steps": [{"b": {"user": "outputs.user"}}, {"a": {}, "outputs": {"msisdn": "user"}}]}"#,
            r#"{"steps": [{"a": {}, "outputs": {"msisdn": "user"}}, {"a": {}, "outputs": {"msisdn": "user"}}]}"#,
        ] {
            assert!(parse(config, &Map::new()).is_err(), "{config}");
        }
    }

    #[test]
    fn yaml_configs_read_as_json_ones_do() {
        let json = r#"{
            "defaults": {"user_a": "+1"},
            "steps": [{"a": {"user": "defaults.user_a", "amount": 2.5}, "outputs": {"msisdn": "user"}}]
        }"#;
        let yaml = "
            defaults:
              user_a: '+1'
            steps:
            - a:
                user: defaults.user_a
                amount: 2.5
              outputs:
                msisdn: user
        ";
        let json = parse(json, &Map::new()).expect("JSON parses");
        let yaml = parse(yaml, &Map::new()).expect("YAML parses");
        assert_eq!(yaml[0].params, json!({"user": "+1", "amount": 2.5}));
        assert_eq!(yaml[0].params, json[0].params);
        assert_eq!(yaml[0].outputs, json[0].outputs);
    }

    #[test]
    fn command_line_defaults_override_the_configs() {
        let config = r#"{
            "defaults": {"user_a": "+1", "user_b": "+2"},
            "steps": [{"a": {"from": "defaults.user_a", "to": "defaults.user_b", "via": "defaults.user_c"}}]
        }"#;
        let defaults = parse_defaults(r#"{"user_a": "+3", "user_c": "+4"}"#).expect("parses");
        let runs = parse(config, &defaults).expect("parses");
        assert_eq!(
            runs[0].params,
            json!({"from": "+3", "to": "+2", "via": "+4"})
        );
        assert!(parse_defaults(r#"["+3"]"#).is_err());
    }

    #[test]
    fn defaults_given_at_run_time_are_not_quoted_back() {
        let err = parse_defaults(r#""0xab""#).expect_err("not an object");
        assert_eq!(err, "expected a JSON object, not a string");

        let defaults =
            Map::from_iter([("key".to_owned(), json!(["0xab", r#"a "quoted" one"#, ""]))]);
        assert_eq!(
            Redact::new(&defaults).apply(
                r#"saw 0xab, then string "a \"quoted\" one", then `a "quoted" one`"#.to_owned()
            ),
            r#"saw defaults.key, then string "defaults.key", then `defaults.key`"#
        );
    }
}
