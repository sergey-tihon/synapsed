use std::{error::Error, ffi::OsString, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let output: OsString = args.next().ok_or("usage: export-schema OUTPUT_DIR")?;
    if args.next().is_some() {
        return Err("usage: export-schema OUTPUT_DIR".into());
    }
    let output = PathBuf::from(output);
    fs::create_dir_all(&output)?;
    for (name, schema) in synapsed_api::schema::roots() {
        let mut bytes = serde_json::to_vec_pretty(&schema)?;
        bytes.push(b'\n');
        fs::write(output.join(format!("{name}.json")), bytes)?;
    }
    Ok(())
}
