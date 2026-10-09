use std::{env, fs, path::Path};

fn main() {
    let manifest_dir = env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets manifest dir");
    let source = Path::new(&manifest_dir).join("web/dist/public");
    println!("cargo:rerun-if-changed={}", source.display());

    let mut assets = Vec::new();
    if source.is_dir() {
        collect_assets(&source, &source, &mut assets);
        assets.sort_by(|a, b| a.0.cmp(&b.0));
    }

    let mut generated = String::from("pub static UI_ASSETS: &[(&str, &[u8])] = &[\n");
    for (name, path) in assets {
        generated.push_str(&format!(
            "    ({name:?}, include_bytes!({path:?}) as &[u8]),\n"
        ));
    }
    generated.push_str("];\n");

    let out_dir = env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo");
    fs::write(Path::new(&out_dir).join("ui_assets.rs"), generated)
        .expect("write generated UI asset index");
}

fn collect_assets(root: &Path, current: &Path, assets: &mut Vec<(String, String)>) {
    let Ok(entries) = fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_assets(root, &path, assets);
        } else if path.is_file() {
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let name = relative.to_string_lossy().replace('\\', "/");
            assets.push((name, path.to_string_lossy().into_owned()));
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}
