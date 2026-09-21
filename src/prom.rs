use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;

fn get_prom_dir() -> Result<PathBuf, String> {
    let home = env::var("HOME")
        .or_else(|_| env::var("USERPROFILE"))
        .map_err(|_| "Could not determine home directory (HOME or USERPROFILE not set)".to_string())?;
    Ok(PathBuf::from(home).join(".orchestrate"))
}

fn get_registry_path() -> Result<PathBuf, String> {
    let dir = get_prom_dir()?;
    if !dir.exists() {
        fs::create_dir_all(&dir).map_err(|e| format!("Failed to create prom directory {:?}: {}", dir, e))?;
    }
    let registry = dir.join("registry.toml");
    if !registry.exists() {
        fs::write(&registry, "[modules]\n").map_err(|e| format!("Failed to create registry.toml: {}", e))?;
    }
    Ok(registry)
}

pub fn read_registry() -> Result<HashMap<String, String>, String> {
    let path = get_registry_path()?;
    let content = fs::read_to_string(&path).map_err(|e| format!("Failed to read registry: {}", e))?;
    let mut map = HashMap::new();
    let mut in_modules = false;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_modules = line == "[modules]";
            continue;
        }
        if in_modules {
            if let Some(idx) = line.find('=') {
                let key = line[..idx].trim().to_string();
                let mut val = line[idx + 1..].trim();
                if val.starts_with('"') && val.ends_with('"') {
                    val = &val[1..val.len() - 1];
                }
                map.insert(key, val.replace("\\\\", "\\").replace("\\\"", "\""));
            }
        }
    }
    Ok(map)
}

pub fn write_registry(map: &HashMap<String, String>) -> Result<(), String> {
    let path = get_registry_path()?;
    let mut content = String::from("[modules]\n");
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort();
    for k in keys {
        content.push_str(&format!(
            "{} = \"{}\"\n",
            k,
            map[k].replace('\\', "\\\\").replace('"', "\\\"")
        ));
    }
    fs::write(&path, content).map_err(|e| format!("Failed to write registry: {}", e))?;
    Ok(())
}

pub fn prom_add(name: &str, path: &str) -> Result<(), String> {
    let path_buf = PathBuf::from(path);
    let canonical = path_buf
        .canonicalize()
        .map_err(|_| format!("Error: '{}' does not exist", path))?;

    if !canonical.join("module.orch").exists() {
        return Err(format!("Error: '{}' does not contain a module.orch file", path));
    }

    let mut map = read_registry()?;
    let canonical_str = canonical.to_string_lossy().into_owned();
    if let Some(old) = map.get(name) {
        println!("Warning: overwriting existing entry for '{}' (was: {})", name, old);
    }
    map.insert(name.to_string(), canonical_str.clone());
    write_registry(&map)?;
    println!("Successfully registered '{}' -> {}", name, canonical_str);
    Ok(())
}

pub fn prom_remove(name: &str) -> Result<(), String> {
    let mut map = read_registry()?;
    if map.remove(name).is_some() {
        write_registry(&map)?;
        println!("Successfully removed '{}' from PROM.", name);
        Ok(())
    } else {
        Err(format!("Error: no PROM entry named '{}'", name))
    }
}

pub fn prom_list() -> Result<(), String> {
    let map = read_registry()?;
    if map.is_empty() {
        println!("No PROM entries registered.");
    } else {
        let mut keys: Vec<&String> = map.keys().collect();
        keys.sort();
        for k in keys {
            println!("{} -> {}", k, map[k]);
        }
    }
    Ok(())
}

pub fn resolve_module(name: &str) -> Result<Option<PathBuf>, String> {
    // Check if it's a bare name. If it contains separators, it's not a bare name.
    if name.contains('/') || name.contains('\\') || name.starts_with('.') {
        return Ok(None);
    }
    // Built-in stdlib modules are resolved first
    if let Some(stdlib_path) = resolve_stdlib_module(name)? {
        return Ok(Some(stdlib_path));
    }
    // Then check PROM registry
    let map = read_registry()?;
    if let Some(path_str) = map.get(name) {
        Ok(Some(PathBuf::from(path_str)))
    } else {
        Ok(None)
    }
}

fn resolve_stdlib_module(name: &str) -> Result<Option<PathBuf>, String> {
    // 1. Relative to the running executable (installed mode)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let candidate = parent.join("stdlib").join(name);
            if candidate.is_dir() && candidate.join("module.orch").exists() {
                return Ok(Some(candidate));
            }
        }
    }
    // 2. Relative to the current working directory (dev mode: running from project root)
    if let Ok(cwd) = std::env::current_dir() {
        let candidate = cwd.join("stdlib").join(name);
        if candidate.is_dir() && candidate.join("module.orch").exists() {
            return Ok(Some(candidate));
        }
    }
    let Some((_, files)) = EMBEDDED_STDLIB.iter().find(|(module, _)| *module == name) else {
        return Ok(None);
    };
    // One copy per compiler build, shared by every process that needs it. The name
    // carries the version and a hash of the embedded sources, so a copy is never stale
    // and there is nothing to clean up; each process used to write its own copy and
    // leave it behind. A copy appears whole or not at all: it is written beside its
    // final name and renamed into place, and a process that loses that race uses the
    // winner's copy, which is the same bytes.
    static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    let root = ROOT.get_or_init(|| {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        EMBEDDED_STDLIB.hash(&mut hasher);
        std::env::temp_dir().join(format!("orchestrate_stdlib_{}_{:016x}", env!("CARGO_PKG_VERSION"), hasher.finish()))
    });
    let directory = root.join(name);
    if !directory.is_dir() {
        let staging = root.join(format!(".{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
        for (file, source) in *files {
            std::fs::write(staging.join(file), source).map_err(|e| e.to_string())?;
        }
        if std::fs::rename(&staging, &directory).is_err() {
            let _ = std::fs::remove_dir_all(&staging);
            if !directory.is_dir() {
                return Err(format!("could not unpack the embedded stdlib module '{}' into {}", name, root.display()));
            }
        }
    }
    Ok(Some(directory))
}

/// The standard library compiled into the binary, for an install that has no `stdlib/`
/// directory beside it.
const EMBEDDED_STDLIB: &[(&str, &[(&str, &str)])] = &[
    ("lists", &[("module.orch", include_str!("../stdlib/lists/module.orch")), ("impl.rs", include_str!("../stdlib/lists/impl.rs")), ("impl.orch_ffi", include_str!("../stdlib/lists/impl.orch_ffi"))]),
    ("strings", &[("module.orch", include_str!("../stdlib/strings/module.orch")), ("impl.rs", include_str!("../stdlib/strings/impl.rs")), ("impl.orch_ffi", include_str!("../stdlib/strings/impl.orch_ffi"))]),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prom_bare_name_detection() {
        // These are not bare names
        assert_eq!(resolve_module("./mydb").unwrap(), None);
        assert_eq!(resolve_module("../mydb").unwrap(), None);
        assert_eq!(resolve_module("C:\\mydb").unwrap(), None);
        assert_eq!(resolve_module("/usr/lib/mydb").unwrap(), None);

        // A bare name lookup (will be None because registry is empty, but won't return early)
        // Note: this test depends on the local machine state, so we won't assert the result
        // is None in case the developer has PROM entries, but we check it doesn't panic.
        let _ = resolve_module("mydb");
    }
}
