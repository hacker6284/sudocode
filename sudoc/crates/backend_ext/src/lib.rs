//! External backend adapter: spawn a process over the emit protocol
//! (spec/protocol.md).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use sudoc_ir::IrModule;
use sudoc_sdk::{GeneratedFile, TestRecipe};

/// Backend manifest (spec/protocol.md §1).
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub protocol: u32,
    pub name: String,
    pub emit: Vec<String>,
    pub recipe: ManifestRecipe,
}

/// Build/run recipe from a manifest.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestRecipe {
    pub build: Vec<Vec<String>>,
    pub run: Vec<String>,
}

/// A backend loaded from a `.sudoc-backend.json` manifest.
pub struct ExternalBackend {
    name: String,
    emit: Vec<String>,
    recipe: ManifestRecipe,
    /// Canonical directory containing the manifest (child cwd for `emit`).
    manifest_dir: PathBuf,
}

impl ExternalBackend {
    /// Load and validate a backend manifest.
    pub fn load(manifest_path: &Path) -> Result<ExternalBackend, String> {
        let text = std::fs::read_to_string(manifest_path)
            .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
        let manifest: Manifest = serde_json::from_str(&text)
            .map_err(|e| format!("{}: {e}", manifest_path.display()))?;

        if manifest.protocol != 1 {
            return Err(format!(
                "{}: unsupported protocol {} (expected 1)",
                manifest_path.display(),
                manifest.protocol
            ));
        }
        if !valid_backend_name(&manifest.name) {
            return Err(format!(
                "{}: invalid backend name '{}' (expected [a-z][a-z0-9_]*)",
                manifest_path.display(),
                manifest.name
            ));
        }
        if manifest.emit.is_empty() {
            return Err("emit must be a non-empty argv".into());
        }

        let manifest_dir = match manifest_path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        let manifest_dir = manifest_dir
            .canonicalize()
            .map_err(|e| format!("{}: {e}", manifest_path.display()))?;

        let mut emit = manifest.emit;
        emit[0] = resolve_emit0(&emit[0], &manifest_dir);

        Ok(ExternalBackend {
            name: manifest.name,
            emit,
            recipe: manifest.recipe,
            manifest_dir,
        })
    }
}

/// CLI name: `^[a-z][a-z0-9_]*$`.
fn valid_backend_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Resolve `emit[0]` against the manifest directory per protocol.md §1.
fn resolve_emit0(cmd: &str, manifest_dir: &Path) -> String {
    let path = Path::new(cmd);
    if path.is_absolute() {
        return cmd.to_string();
    }
    // Bare command name (PATH lookup) when there is no directory component.
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => {
            manifest_dir.join(path).to_string_lossy().into_owned()
        }
        _ => cmd.to_string(),
    }
}

fn validate_response_path(p: &str) -> Result<(), String> {
    let path = Path::new(p);
    for comp in path.components() {
        match comp {
            std::path::Component::ParentDir => {
                return Err(format!("response path '{p}' contains '..'"));
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err(format!("response path '{p}' is absolute"));
            }
            _ => {}
        }
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct EmitRequest<'a> {
    protocol: u32,
    cmd: &'static str,
    entry: &'a str,
    with_tests: bool,
    modules: &'a serde_json::value::RawValue,
}

impl sudoc_sdk::Backend for ExternalBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn runtime_files(&self) -> Vec<GeneratedFile> {
        // External backends return runtime support from emit_program (spec §2).
        vec![]
    }

    fn test_recipe(&self, entry: &str) -> TestRecipe {
        let subst = |s: &str| s.replace("{entry}", entry);
        TestRecipe {
            build: self
                .recipe
                .build
                .iter()
                .map(|step| step.iter().map(|a| subst(a)).collect())
                .collect(),
            run: self.recipe.run.iter().map(|a| subst(a)).collect(),
        }
    }

    fn emit_program(
        &self,
        modules: &[IrModule],
        with_tests: bool,
    ) -> Result<Vec<GeneratedFile>, String> {
        let entry = modules.last().expect("entry module");
        let modules_json = sudoc_ir::wire::to_wire_json(modules).map_err(|e| e.to_string())?;
        let raw = serde_json::value::RawValue::from_string(modules_json)
            .map_err(|e| format!("{}: {e}", self.name))?;
        let request = EmitRequest {
            protocol: 1,
            cmd: "emit",
            entry: &entry.name,
            with_tests,
            modules: &raw,
        };
        let request_bytes =
            serde_json::to_string(&request).map_err(|e| format!("{}: {e}", self.name))?;

        let mut child = Command::new(&self.emit[0])
            .args(&self.emit[1..])
            .current_dir(&self.manifest_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{}: failed to spawn '{}': {e}", self.name, self.emit[0]))?;

        let mut stdin = child.stdin.take().expect("piped stdin");
        let req = request_bytes;
        let writer = std::thread::spawn(move || {
            use std::io::Write;
            let _ = stdin.write_all(req.as_bytes());
            // stdin drops here, closing the pipe
        });
        let output = child
            .wait_with_output()
            .map_err(|e| format!("{}: {e}", self.name))?;
        let _ = writer.join();

        if !output.status.success() {
            return Err(format!(
                "{}: emit exited with {}: {}",
                self.name,
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let value: serde_json::Value = serde_json::from_str(&stdout).map_err(|e| {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.trim().is_empty() {
                format!("{}: malformed emit response: {e}", self.name)
            } else {
                format!(
                    "{}: malformed emit response: {e} (stderr: {})",
                    self.name,
                    stderr.trim()
                )
            }
        })?;

        let obj = value
            .as_object()
            .ok_or_else(|| format!("{}: malformed emit response: expected object", self.name))?;

        if let Some(err) = obj.get("error") {
            let msg = err.as_str().unwrap_or("non-string error field");
            return Err(format!("{}: {msg}", self.name));
        }

        let files_val = obj.get("files").ok_or_else(|| {
            format!(
                "{}: malformed emit response: missing 'files' or 'error'",
                self.name
            )
        })?;
        let files_arr = files_val.as_array().ok_or_else(|| {
            format!(
                "{}: malformed emit response: 'files' is not an array",
                self.name
            )
        })?;

        let mut files = Vec::with_capacity(files_arr.len());
        for item in files_arr {
            let path = item
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    format!("{}: malformed emit response: file missing path", self.name)
                })?;
            let contents = item
                .get("contents")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    format!(
                        "{}: malformed emit response: file missing contents",
                        self.name
                    )
                })?;
            validate_response_path(path).map_err(|e| format!("{}: {e}", self.name))?;
            files.push(GeneratedFile {
                path: path.to_string(),
                contents: contents.to_string(),
            });
        }
        Ok(files)
    }
}
