use clap::{Arg, ArgAction, Command};
use llm_json::{RepairOptions, repair_json};
use std::fs;
use std::io::{self, Read};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let matches = Command::new("json_repair")
        .version("0.1.0")
        .about("Repair and parse JSON files")
        .arg(
            Arg::new("filename")
                .help("The JSON file to repair (if omitted, reads from stdin)")
                .index(1),
        )
        .arg(
            Arg::new("inline")
                .short('i')
                .long("inline")
                .help("Replace the file inline instead of returning the output to stdout")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("output")
                .short('o')
                .long("output")
                .value_name("TARGET")
                .help(
                    "If specified, the output will be written to TARGET filename instead of stdout",
                ),
        )
        .arg(
            Arg::new("ensure_ascii")
                .long("ensure_ascii")
                .help("Ensure ASCII output (escape Unicode characters)")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("indent")
                .long("indent")
                .value_name("INDENT")
                .help("Number of spaces for indentation (Default 2)")
                .default_value("2"),
        )
        .arg(
            Arg::new("skip_json_loads")
                .long("skip-validation")
                .help("Skip JSON validation for performance")
                .action(ArgAction::SetTrue),
        )
        .get_matches();

    let input_content = if let Some(filename) = matches.get_one::<String>("filename") {
        fs::read_to_string(filename)
            .map_err(|e| format!("Failed to read file '{}': {}", filename, e))?
    } else {
        let mut buffer = String::new();
        io::stdin().read_to_string(&mut buffer)?;
        buffer
    };

    let options = RepairOptions {
        skip_json_loads: matches.get_flag("skip_json_loads"),
        return_objects: false,
        ensure_ascii: matches.get_flag("ensure_ascii"),
        stream_stable: false,
    };

    let repaired = repair_json(&input_content, &options)?;

    // Pretty print the JSON
    let indent: usize = matches
        .get_one::<String>("indent")
        .unwrap()
        .parse()
        .unwrap_or(2);

    let parsed: serde_json::Value = serde_json::from_str(&repaired)?;
    let pretty = if indent > 0 {
        serde_json::to_string_pretty(&parsed)?
    } else {
        serde_json::to_string(&parsed)?
    };

    // Handle output
    if matches.get_flag("inline") {
        if let Some(filename) = matches.get_one::<String>("filename") {
            fs::write(filename, &pretty)?;
            println!("File '{}' repaired in place", filename);
        } else {
            return Err("Cannot use --inline without specifying a filename".into());
        }
    } else if let Some(output_file) = matches.get_one::<String>("output") {
        fs::write(output_file, &pretty)?;
        println!("Output written to '{}'", output_file);
    } else {
        println!("{}", pretty);
    }

    Ok(())
}
