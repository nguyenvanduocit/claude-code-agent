//! Force Code Mode (the `REPL` tool) on for SDK sessions.
//!
//! The stock CLI (verified 2.1.329) refuses Code Mode for every
//! non-interactive session (`-p` / stream-json), so no SDK gets it. With
//! `SessionOptions::code_mode` the SDK runs a patched copy of the CLI
//! instead; the installed binary is never modified.
//!
//! The patch touches only the Code Mode opt-in module of Bun's embedded
//! module graph:
//!   1. local eligibility check (`entrypoint`, `unattended`, provider, …) passes
//!   2. the one-time caveats dialog counts as accepted
//!   3. the server gate `tengu_rustling_blossom` counts as open
//!   4. the module's precompiled bytecode is dropped, so Bun compiles the
//!      patched source (the bytecode would otherwise run unchanged)
//!
//! Every edit keeps the byte length, so no offset in the binary moves. An
//! anchor that is missing or ambiguous (a CLI version this patch does not
//! know) is an error, never a silent partial patch.
//!
//! Layout of Bun's standalone module graph (Format B, Claude Code 2.1.280+):
//! trailer `"\n---- Bun! ----\n"` preceded by a 32-byte offsets struct
//! (`byte_count: u64, modules: (u32 offset, u32 len), …`); the module list
//! is 52-byte entries of six `(u32 offset, u32 len)` pointers (name,
//! contents, sourcemap, bytecode, module_info, bytecode_origin_path) plus
//! four flag bytes; offsets are relative to `trailer - 32 - byte_count`.
use std::path::{Path, PathBuf};

use regex::bytes::Regex;

const TRAILER: &[u8] = b"\n---- Bun! ----\n";
const OFFSETS_SIZE: usize = 32;
const ENTRY_SIZE: usize = 52;
/// Byte offset of the bytecode pointer inside a module entry.
const BYTECODE_PTR: usize = 24;
/// Present only in the Code Mode opt-in module.
const MODULE_MARKER: &[u8] = b"\"tengu_rustling_blossom\"";

#[derive(Debug, thiserror::Error)]
pub enum CodeModeError {
    #[error("not a Bun standalone binary: {0}")]
    NotBunBinary(&'static str),
    #[error("Code Mode module not found in the CLI binary")]
    ModuleNotFound,
    #[error("patch anchor `{0}` found {1} times (expected 1); this CLI version is not supported")]
    Anchor(&'static str, usize),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("re-signing the patched CLI failed: {0}")]
    Sign(String),
    #[error("`settings` must be a JSON object (with an object `env`) to add the Code Mode env var")]
    Settings,
    #[error("Code Mode is supported on macOS and Linux only")]
    UnsupportedPlatform,
}

// ─── Functional core ─────────────────────────────────────────────────────────

/// Return `binary` with Code Mode forced on.
pub fn patch(binary: &[u8]) -> Result<Vec<u8>, CodeModeError> {
    let mut out = binary.to_vec();
    let (entry, contents) = find_module(&out)?;

    let module = &out[contents.clone()];
    let gate_var = Regex::new(r#"var ([\w$]+)="tengu_rustling_blossom""#)
        .ok()
        .and_then(|re| re.captures(module))
        .and_then(|c| c.get(1))
        .map(|m| regex::escape(&String::from_utf8_lossy(m.as_bytes())))
        .ok_or(CodeModeError::Anchor("gate variable", 0))?;

    let edits: [(&'static str, String, &[u8]); 3] = [
        ("eligibility", r#"(?:\{)(if\(![\w$]+\(\)\)return"runtime";)"#.into(), b"return;"),
        (
            "caveats",
            r"([\w$]+\.codeModeCaveatsAcceptedV1===!0&&[\w$]+\.codeModeAutoModeCaveatAcceptedV1===!0)".into(),
            b"!0",
        ),
        ("gate", format!(r"(return [\w$]+\({gate_var},!0\)!==!0)"), b"return!1"),
    ];
    for (name, pattern, replacement) in edits {
        let span = unique_match(&out[contents.clone()], name, &pattern)?;
        if span.len() < replacement.len() {
            return Err(CodeModeError::Anchor(name, 1));
        }
        let start = contents.start + span.start;
        overwrite_padded(&mut out[start..start + span.len()], replacement);
    }

    // Without bytecode Bun compiles the (patched) source of this module.
    out.get_mut(entry + BYTECODE_PTR..entry + BYTECODE_PTR + 8)
        .ok_or(CodeModeError::NotBunBinary("module table"))?
        .fill(0);
    Ok(out)
}

/// (entry offset, contents range) of the Code Mode opt-in module.
fn find_module(data: &[u8]) -> Result<(usize, std::ops::Range<usize>), CodeModeError> {
    let trailer = rfind(data, TRAILER).ok_or(CodeModeError::NotBunBinary("no trailer"))?;
    let offsets = trailer.checked_sub(OFFSETS_SIZE).ok_or(CodeModeError::NotBunBinary("short file"))?;
    let byte_count = usize::try_from(read_u64(data, offsets)?).map_err(|_| CodeModeError::NotBunBinary("size"))?;
    let modules_off = read_u32(data, offsets + 8)?;
    let modules_len = read_u32(data, offsets + 12)?;
    let base = offsets.checked_sub(byte_count).ok_or(CodeModeError::NotBunBinary("graph size"))?;

    for i in 0..modules_len / ENTRY_SIZE {
        let entry = base + modules_off + i * ENTRY_SIZE;
        let start = base + read_u32(data, entry + 8)?;
        let end = start + read_u32(data, entry + 12)?;
        let contents = data.get(start..end).ok_or(CodeModeError::NotBunBinary("module bounds"))?;
        if contents.windows(MODULE_MARKER.len()).any(|w| w == MODULE_MARKER) {
            return Ok((entry, start..end));
        }
    }
    Err(CodeModeError::ModuleNotFound)
}

/// Range (within `haystack`) of capture group 1 of the only match.
fn unique_match(haystack: &[u8], name: &'static str, pattern: &str) -> Result<std::ops::Range<usize>, CodeModeError> {
    let re = Regex::new(pattern).map_err(|_| CodeModeError::Anchor(name, 0))?;
    let found: Vec<_> = re.captures_iter(haystack).filter_map(|c| c.get(1)).map(|m| m.range()).collect();
    match found.as_slice() {
        [one] => Ok(one.clone()),
        _ => Err(CodeModeError::Anchor(name, found.len())),
    }
}

/// Write `with` into `slot`, padding with spaces (valid JS whitespace).
fn overwrite_padded(slot: &mut [u8], with: &[u8]) {
    slot.fill(b' ');
    for (dst, src) in slot.iter_mut().zip(with) {
        *dst = *src;
    }
}

fn rfind(data: &[u8], needle: &[u8]) -> Option<usize> {
    data.windows(needle.len()).rposition(|w| w == needle)
}

fn read_u32(data: &[u8], at: usize) -> Result<usize, CodeModeError> {
    let bytes = data.get(at..at + 4).and_then(|b| <[u8; 4]>::try_from(b).ok());
    bytes.map(|b| u32::from_le_bytes(b) as usize).ok_or(CodeModeError::NotBunBinary("truncated"))
}

fn read_u64(data: &[u8], at: usize) -> Result<u64, CodeModeError> {
    let bytes = data.get(at..at + 8).and_then(|b| <[u8; 8]>::try_from(b).ok());
    bytes.map(u64::from_le_bytes).ok_or(CodeModeError::NotBunBinary("truncated"))
}

// ─── Imperative shell ────────────────────────────────────────────────────────

/// Serializes builds: a pool warming N sessions builds the copy once.
static BUILD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Path of a patched copy of `binary` (resolved through `PATH`), built on
/// first use and cached under `<cache>/claude-sdk/code-mode/`. The cache key
/// is the installed binary's name, size and mtime plus this SDK's version,
/// so a CLI update or a new patch builds a new copy.
pub async fn patched_binary(binary: &Path) -> Result<PathBuf, CodeModeError> {
    if cfg!(windows) {
        return Err(CodeModeError::UnsupportedPlatform);
    }
    let source = tokio::fs::canonicalize(resolve(binary)).await?;
    let key = source_key(&source).await?;
    let target = cache_dir().join(format!("{key}-sdk{}", env!("CARGO_PKG_VERSION"))).join("claude");

    let _guard = BUILD.lock().await;
    if tokio::fs::try_exists(&target).await? {
        return Ok(target);
    }
    let dir = target.parent().ok_or(CodeModeError::NotBunBinary("cache path"))?;
    tokio::fs::create_dir_all(dir).await?;
    // Read by prune_stale_builds in any process: the build is stale once
    // this source changes or disappears.
    tokio::fs::write(dir.join(SOURCE_FILE), source.to_string_lossy().as_bytes()).await?;

    let original = tokio::fs::read(&source).await?;
    let patched = tokio::task::spawn_blocking(move || patch(&original))
        .await
        .map_err(|e| CodeModeError::Io(std::io::Error::other(e)))??;
    // Write to a name unique to this build, then rename: neither a crash nor
    // another process building at the same time leaves a half copy.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    let partial = dir.join(format!("claude.{}.{nanos}.partial", std::process::id()));
    tokio::fs::write(&partial, patched).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o755)).await?;
    }
    if let Err(e) = sign(&partial, &source).await {
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(e);
    }
    tokio::fs::rename(&partial, &target).await?;
    prune_stale_builds(dir).await;
    tracing::info!(source = %source.display(), patched = %target.display(), "built Code Mode CLI");
    Ok(target)
}

/// Name of the file in each build directory holding the source CLI's path.
const SOURCE_FILE: &str = "source";

/// `<name>-<size>-<mtime>` of the installed CLI.
async fn source_key(source: &Path) -> std::io::Result<String> {
    let meta = tokio::fs::metadata(source).await?;
    let mtime = meta
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let name = source.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "claude".into());
    Ok(format!("{name}-{}-{mtime}", meta.len()))
}

/// Delete copies (about 230 MB each) whose source CLI was updated or
/// removed. Copies of CLIs still installed stay, so programs using other
/// CLI installs or SDK versions keep theirs. A session still running a
/// deleted copy keeps working: unix unlinks open files lazily.
async fn prune_stale_builds(current: &Path) {
    let Some(root) = current.parent() else { return };
    let Ok(mut entries) = tokio::fs::read_dir(root).await else { return };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path != current && is_stale_build(&path).await && tokio::fs::remove_dir_all(&path).await.is_ok() {
            tracing::debug!(path = %path.display(), "removed stale Code Mode CLI");
        }
    }
}

async fn is_stale_build(dir: &Path) -> bool {
    match tokio::fs::read(dir.join(SOURCE_FILE)).await {
        Ok(bytes) => {
            let source = PathBuf::from(String::from_utf8_lossy(&bytes).into_owned());
            let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            match source_key(&source).await {
                Ok(key) => !name.starts_with(&format!("{key}-")),
                Err(_) => true,
            }
        }
        // No source file: a build being set up right now (no `claude` yet)
        // is kept; a finished one without it predates source tracking.
        Err(_) => tokio::fs::try_exists(dir.join("claude")).await.unwrap_or(false),
    }
}

/// `binary` as given if it has a directory part, else the first match on `PATH`.
fn resolve(binary: &Path) -> PathBuf {
    if binary.components().count() > 1 {
        return binary.to_path_buf();
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|dir| dir.join(binary))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| binary.to_path_buf())
}

fn cache_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("claude-sdk").join("code-mode")
}

/// macOS kills a modified binary unless it is re-signed; the stock
/// entitlements (JIT, unsigned executable memory) must carry over.
#[cfg(target_os = "macos")]
async fn sign(patched: &Path, source: &Path) -> Result<(), CodeModeError> {
    use tokio::process::Command;
    let ents = Command::new("codesign").args(["-d", "--entitlements", "-", "--xml"]).arg(source).output().await?;
    if !ents.status.success() || !ents.stdout.starts_with(b"<?xml") {
        return Err(CodeModeError::Sign(format!(
            "could not read entitlements of {}: {}",
            source.display(),
            String::from_utf8_lossy(&ents.stderr).trim()
        )));
    }
    let plist = patched.with_extension("entitlements.plist");
    tokio::fs::write(&plist, &ents.stdout).await?;
    let out = Command::new("codesign")
        .args(["--force", "--sign", "-", "--timestamp=none", "--entitlements"])
        .arg(&plist)
        .arg(patched)
        .output()
        .await?;
    let _ = tokio::fs::remove_file(&plist).await;
    if out.status.success() {
        Ok(())
    } else {
        Err(CodeModeError::Sign(String::from_utf8_lossy(&out.stderr).trim().to_string()))
    }
}

#[cfg(not(target_os = "macos"))]
async fn sign(_patched: &Path, _source: &Path) -> Result<(), CodeModeError> {
    Ok(())
}
