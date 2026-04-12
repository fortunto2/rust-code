# LLM_JSON

A Rust library to repair broken JSON strings, particularly useful for handling malformed JSON output from Large Language Models.

This is a porting of the Python library [json_repair](https://github.com/mangiucugna/json_repair), written by [Stefano Baccianella](https://github.com/mangiucugna) and published under the MIT license.

All credits go to the original author for the amazing work.

## Programmatic Usage

Install `llm_json` in your project: 

```sh
cargo add llm_json
```

Then use it to repair your broken JSON strings:

```rust
use llm_json::{repair_json, loads, JsonRepairError};

fn main() {
  // Basic repair
  let broken_json = r#"{name: 'John', age: 30,}"#;
  let repaired = repair_json(broken_json, &Default::default())?;
  println!("{}", repaired); // {"name": "John", "age": 30}
  
  // Parse directly to Value
  let value = loads(broken_json, &Default::default())?;
}
```

## CLI Usage

Install `llm_json` locally:

```sh
cargo install llm_json
```

Then use it to repair your broken JSON strings and files:

```sh
# Repair JSON from stdin
echo '{name: "John", age: 30,}' | llm_json

# Repair a file
llm_json broken.json

# Save to new file
llm_json input.json -o fixed.json

# Fix file in-place
llm_json broken.json --inline
```

## License

[MIT](/LICENSE.md)