# tis-rs

Runs a suite of steps chosen and parameterised by a JSON or YAML config. A
suite defines its steps, registers them with a `Tis`, and hands it a config.
`Tis` checks the whole config before anything starts, runs the steps one at a
time, passes outputs from earlier steps to later ones, and reports which
failed.

## Steps

A step is a struct whose fields are its parameters, as a config gives them,
implementing `Step`:

```rust
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tis_rs::Step;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignUp {
    /// Where to sign up; anywhere if omitted.
    country: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct SignedUp {
    msisdn: String,
}

impl Step for SignUp {
    /// What a run returns for later runs, or `()` for nothing.
    type Output = SignedUp;

    fn name() -> String {
        "sign_up".to_owned()
    }

    fn about() -> String {
        "A new user signs up, in `country` if given".to_owned()
    }

    fn example() -> Self {
        SignUp { country: Some("PH".to_owned()) }
    }

    async fn run(self) -> SignedUp {
        // ... fail by panicking, e.g. with assert!
    }
}
```

`name` is the key configs use, and `about` and `example` feed `Tis::list` and
`Tis::example_config`. A step can also implement:

- `validate`, to check what its parameters' types can't before any step runs.
- `outputs`, when its parameters decide which fields of `Output` it outputs.
  By default it outputs every field.

A step fails by panicking. `Tis` keeps the panic message and location to log
with the step's result, instead of printing them when the panic happens.

## Running

```rust
use tis_rs::Tis;

let mut tis = Tis::new().with_example_defaults([("user_a", "+12345678901")]);
tis.register::<SignUp>().register::<SendTokens>();

let config = tis.read_config(&path, &defaults)?; // or `tis.parse_config(&text, &defaults)`
let report = tis.run(config).await;
std::process::exit(if report.passed() { 0 } else { 1 });
```

`run` needs a Tokio runtime. Each step runs in its own task.

`read_config` reads stdin when the path is `-`. It returns an error, without
running anything, if the config is malformed, names a step that isn't
registered, or gives a step parameters that fail to deserialize or validate.
Parameters that use an earlier step's outputs are checked when that step's
turn comes.

`list` and `example_config` return text for a CLI to print, so that someone
writing a config can see the steps and their parameters. Errors suggest
running `list`, so expose it under that name. `parse_defaults` parses defaults
given on the command line and works as a clap `value_parser`.

## Config

```yaml
defaults:
  user_a: "+12345678901"
steps:
  - sign_up: { country: PH }
    outputs: { msisdn: new_user }
  - send: { from: defaults.user_a, to: outputs.new_user, amount: 2 }
  - send: { from: outputs.new_user, to: defaults.user_a, amount: 1 }
```

- A config is read as JSON if it starts with `{`, and as YAML otherwise.
- `steps` lists the runs, in order. Each run is an object whose one key is a
  step's name, holding that step's parameters. A step listed more than once
  is reported as `send[0]`, `send[1]`.
- Any string `"defaults.<name>"` in the parameters stands for that entry of
  `defaults`. Defaults passed to `parse_config` or `read_config` override the
  config's.
- `outputs` stores fields of a run's output for later runs, as
  `{ "<field>": "<name>" }`, or `null` not to store a field. Any string
  `"outputs.<name>"` in a later run's parameters stands for the stored value.
  Each name is stored by one run, before any run uses it. If the run storing
  an output fails, runs that use it fail too.
- Parameters that are `null` deserialize as `{}`, so a step whose parameters
  are all optional can be written with `null` or `{}`.

## Logs

Run results are logged with `tracing` under the `tis_rs` target. `info`
covers each step starting, passing or failing, and the totals. `debug` adds
the name of each output stored, but not its value. Each step runs inside a `step{name=…}` span, so the
suite's own logs from inside a step carry the step's name.
