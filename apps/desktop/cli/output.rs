//! Output shared by the ACI commands.

use serde_json::Value;

/// Pretty JSON on stdout, the form the ACI commands print.
pub fn print_json(value: &Value) -> Result<(), String> {
    let text =
        serde_json::to_string_pretty(value).map_err(|e| format!("failed to serialize: {e}"))?;
    println!("{text}");
    Ok(())
}
