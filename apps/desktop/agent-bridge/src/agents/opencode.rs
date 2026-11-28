//! The OpenCode provider entry, in OpenCode 1's `provider` shape and OpenCode
//! 2's `providers` shape.

use super::*;
use desktop_core::agents::{is_opencode_executable, OPENCODE_EXECUTABLE_NAMES};
use std::{
    io::{Read, Seek},
    iter,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
pub(super) enum OpenCodeVersion {
    V1,
    V2,
}

impl OpenCodeVersion {
    fn detect(user_home: &Path, recorded: Option<&Path>) -> Option<Self> {
        let path = env::var_os("PATH").unwrap_or_default();
        let path_dirs = || env::split_paths(&path).filter(|dir| dir.is_absolute());
        let executable = recorded.map(Path::to_path_buf).or_else(|| {
            path_dirs()
                .chain(opencode_install_dirs(user_home))
                .flat_map(|dir| {
                    OPENCODE_EXECUTABLE_NAMES
                        .iter()
                        .map(move |name| dir.join(name))
                })
                .find_map(|candidate| validate_opencode_executable(&candidate).ok())
        })?;
        let child_path =
            env::join_paths(iter::once(executable.parent()?.to_path_buf()).chain(path_dirs()))
                .ok()?;

        // Both releases initialize OpenCode directories even for --version.
        let home = tempfile::Builder::new()
            .prefix("pap-opencode-")
            .tempdir()
            .ok()?;
        let mut stdout = tempfile::tempfile_in(home.path()).ok()?;
        let mut command = Command::new(executable);
        command
            .arg("--version")
            .current_dir(home.path())
            .env_clear()
            .env("PATH", child_path)
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env("XDG_CONFIG_HOME", home.path().join("config"))
            .env("XDG_DATA_HOME", home.path().join("data"))
            .env("XDG_STATE_HOME", home.path().join("state"))
            .env("XDG_CACHE_HOME", home.path().join("cache"))
            .stdin(Stdio::null())
            .stdout(stdout.try_clone().ok()?)
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            for key in ["SystemRoot", "PATHEXT", "ComSpec"] {
                if let Some(value) = env::var_os(key) {
                    command.env(key, value);
                }
            }
            command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        }

        let mut child = command.spawn().ok()?;
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => break,
                Ok(Some(_)) => return None,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                Ok(None) | Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }

        stdout.rewind().ok()?;
        let mut output = String::new();
        stdout.take(256).read_to_string(&mut output).ok()?;
        let version = output.trim();
        let version = version.strip_prefix("opencode v").unwrap_or(version);
        match semver::Version::parse(version).ok()?.major {
            1 => Some(Self::V1),
            2 => Some(Self::V2),
            _ => None,
        }
    }
}

fn validate_opencode_executable(path: &Path) -> io::Result<PathBuf> {
    let invalid = || io::Error::from(io::ErrorKind::PermissionDenied);
    if !path.is_absolute() {
        return Err(invalid());
    }
    let resolved = path.canonicalize()?;
    if !is_opencode_executable(path) {
        return Err(invalid());
    }
    let parent = path.parent().ok_or_else(invalid)?.canonicalize()?;
    let normalized = parent.join(path.file_name().ok_or_else(invalid)?);
    let paths = [path, resolved.as_path(), normalized.as_path()];
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions.
        let user = unsafe { libc::geteuid() };
        let file = fs::metadata(&resolved)?;
        if (file.uid() != user && file.uid() != 0) || file.mode() & 0o002 != 0 {
            return Err(invalid());
        }
        for dir in paths.into_iter().flat_map(|path| path.ancestors().skip(1)) {
            if fs::metadata(dir)?.mode() & 0o002 != 0 {
                return Err(invalid());
            }
        }
    }
    #[cfg(windows)]
    {
        use desktop_core::windows_acl;
        use windows_sys::Win32::{
            Foundation::{GENERIC_ALL, GENERIC_WRITE},
            Storage::FileSystem::{
                DELETE, FILE_ADD_FILE, FILE_APPEND_DATA, FILE_DELETE_CHILD, FILE_WRITE_DATA,
                WRITE_DAC, WRITE_OWNER,
            },
        };
        let acl = |path: &Path| {
            windows_acl::read(&windows_acl::open_for_inspection(path)?).map_err(|_| invalid())
        };
        let trusted = |acl: &windows_acl::Acl, sid: &str| {
            sid == acl.current_user_sid || windows_acl::TRUSTED_SIDS.contains(&sid)
        };
        let writes = |acl: &windows_acl::Acl, mask, check_sid: &dyn Fn(&str) -> bool| {
            // ACEs marked inherit-only do not apply to this entry.
            acl.aces.iter().any(|ace| {
                ace.kind == 0
                    && ace.flags & 0x08 == 0
                    && ace.mask & mask != 0
                    && check_sid(&ace.sid)
            })
        };
        const COMMON_WRITE: u32 = GENERIC_ALL | GENERIC_WRITE | DELETE | WRITE_DAC | WRITE_OWNER;
        let file = acl(&resolved)?;
        if !trusted(&file, &file.owner_sid)
            || writes(
                &file,
                COMMON_WRITE | FILE_WRITE_DATA | FILE_APPEND_DATA,
                &|sid| !trusted(&file, sid),
            )
        {
            return Err(invalid());
        }
        for dir in paths.into_iter().flat_map(|path| path.ancestors().skip(1)) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            if writes(
                &acl(dir)?,
                COMMON_WRITE | FILE_ADD_FILE | FILE_DELETE_CHILD,
                &|sid| {
                    matches!(
                        sid,
                        "S-1-1-0" | "S-1-5-11" | "S-1-5-32-545" | "S-1-5-32-546"
                    )
                },
            ) {
                return Err(invalid());
            }
        }
    }
    // Keep the command name and adjacent runtime, while dropping temporary
    // directory symlinks such as a version manager's per-shell bin directory.
    #[cfg(unix)]
    return Ok(normalized);
    #[cfg(windows)]
    Ok(path.to_path_buf())
}

fn opencode_install_dirs(home: &Path) -> Vec<PathBuf> {
    let configured = |key| env_path(key).map(|dir| home.join(dir));
    // Both current curl installers use ~/.opencode/bin. The v1 README and
    // older v2 README also document OPENCODE_INSTALL_DIR, XDG_BIN_DIR and ~/bin.
    let mut dirs: Vec<_> = ["OPENCODE_INSTALL_DIR", "XDG_BIN_DIR", "XDG_BIN_HOME"]
        .into_iter()
        .filter_map(configured)
        .collect();
    dirs.extend(["bin", ".opencode/bin", ".local/bin"].map(|dir| home.join(dir)));

    // npm links global commands into its prefix on Windows and prefix/bin on Unix.
    if let Some(prefix) =
        configured("npm_config_prefix").or_else(|| configured("NPM_CONFIG_PREFIX"))
    {
        dirs.push(if cfg!(windows) {
            prefix
        } else {
            prefix.join("bin")
        });
    }
    dirs.extend(configured("BUN_INSTALL").map(|dir| dir.join("bin")));
    dirs.push(home.join(".bun/bin"));

    #[cfg(target_os = "macos")]
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    #[cfg(not(windows))]
    dirs.push(PathBuf::from("/usr/local/bin"));
    #[cfg(target_os = "linux")]
    dirs.push(PathBuf::from("/home/linuxbrew/.linuxbrew/bin"));
    #[cfg(windows)]
    {
        let app_data = configured("APPDATA").unwrap_or_else(|| home.join("AppData/Roaming"));
        let local_app_data =
            configured("LOCALAPPDATA").unwrap_or_else(|| home.join("AppData/Local"));
        dirs.push(app_data.join("npm"));
        dirs.push(local_app_data.join("Microsoft/WinGet/Links"));
        dirs.extend(configured("ProgramFiles").map(|dir| dir.join("WinGet/Links")));
        dirs.push(
            configured("SCOOP")
                .unwrap_or_else(|| home.join("scoop"))
                .join("shims"),
        );
        dirs.extend(
            configured("ChocolateyInstall")
                .or_else(|| configured("ProgramData").map(|dir| dir.join("chocolatey")))
                .map(|dir| dir.join("bin")),
        );
    }
    dirs
}

#[derive(Clone, Copy)]
pub(super) struct ConnectionCatalog<'a> {
    pub(super) catalog: &'a Catalog,
    pub(super) opencode_version: Option<OpenCodeVersion>,
}

impl Projector {
    pub(super) fn connection_catalog<'a>(
        &self,
        agent: Agent,
        connect: bool,
        catalog: Option<&'a Catalog>,
        executable: Option<&Path>,
    ) -> Option<ConnectionCatalog<'a>> {
        catalog.map(|catalog| ConnectionCatalog {
            catalog,
            opencode_version: (agent == Agent::OpenCode && connect && self.tool_env)
                .then(|| OpenCodeVersion::detect(&self.home, executable))
                .flatten(),
        })
    }

    pub(super) fn opencode_executable(
        &self,
        agent: Agent,
        connect: bool,
        requested: &ConnectOptions,
        prior: Option<&Connection>,
    ) -> Result<Option<PathBuf>, AgentError> {
        if agent != Agent::OpenCode || !connect || !self.tool_env {
            return Ok(None);
        }
        if let Some(path) = &requested.opencode_executable {
            match validate_opencode_executable(path) {
                Ok(path) => return Ok(Some(path)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(_) => return Err(AgentError::ConfigurationConflict(
                    "The OpenCode executable must use an absolute path, be owned by you or the system, and be protected from public writes".into(),
                )),
            }
        }
        Ok(prior
            .and_then(|record| record.options.opencode_executable.as_ref())
            .and_then(|path| validate_opencode_executable(path).ok()))
    }
}

pub(super) fn project_opencode_shape(
    fields: &mut Vec<Field>,
    doc: &ConfigDoc,
    prior: Option<&Connection>,
    connection: ConnectionCatalog<'_>,
) {
    const LEGACY: &[&str] = &["provider", "private-ai-proxy"];
    const NATIVE: &[&str] = &["providers", "private-ai-proxy"];
    let v2 = match connection.opencode_version {
        Some(OpenCodeVersion::V1) => false,
        Some(OpenCodeVersion::V2) => true,
        None => doc.contains(&["providers"]) || doc.contains(&["policies"]),
    };
    if v2 {
        if let Some(field) = fields.iter_mut().find(|field| field.path == LEGACY) {
            if let Some(ConfigValue::Json(provider)) = &field.value {
                field.path = owned(NATIVE);
                field.value = Some(ConfigValue::Json(opencode_provider_v2(
                    provider,
                    connection.catalog,
                )));
            }
        }
    }
    let other = if v2 { LEGACY } else { NATIVE };
    if prior.is_some_and(|record| {
        record.fields.iter().any(|field| {
            field.path == other
                && (!doc.contains(other) || field.holds(doc.get_value(other).as_ref()))
        })
    }) {
        fields.push(absent(other));
    }
}

pub(super) fn opencode_provider(
    catalog: &Catalog,
    api: &str,
    token_path: &Path,
) -> Result<serde_json::Value, String> {
    use serde_json::json;
    let models = catalog
        .models
        .iter()
        .map(|model| {
            let facts = OpenCodeModel::of(model);
            let mut config = json!({"name": model.display_name()});
            // Without its own variants OpenCode offers low, medium and high
            // for every custom model, its controls for a model whose efforts
            // the service does not state.
            if let Some(efforts) = &facts.efforts {
                config["variants"] = efforts
                    .iter()
                    .map(|effort| (effort.clone(), json!({"reasoningEffort": effort})))
                    .collect::<serde_json::Map<_, _>>()
                    .into();
            }
            if let Some(tools) = facts.tools {
                config["tool_call"] = json!(tools);
            }
            if let Some((input, output)) = &facts.modalities {
                config["modalities"] = json!({"input": input, "output": output});
            }
            if let Some(limit) = facts.limit {
                config["limit"] = limit;
            }
            if let Some(cost) = &facts.cost {
                let mut v1 = json!({"input": cost.input, "output": cost.output});
                if let Some(read) = cost.cache_read {
                    v1["cache_read"] = json!(read);
                }
                if let Some(write) = cost.cache_write {
                    v1["cache_write"] = json!(write);
                }
                config["cost"] = v1;
            }
            (model.id().to_string(), config)
        })
        .collect();
    Ok(json!({
        "npm": "@ai-sdk/openai-compatible",
        "name": PRODUCT_NAME,
        "options": {
            "baseURL": api,
            "apiKey": opencode_file_reference(token_path)?,
        },
        "models": serde_json::Value::Object(models),
    }))
}

/// The OpenCode 2 `providers` entry for a projected OpenCode 1 `provider`
/// entry, its models written from the catalog in v2's own fields.
pub(super) fn opencode_provider_v2(
    provider: &serde_json::Value,
    catalog: &Catalog,
) -> serde_json::Value {
    use serde_json::json;
    let mut native = serde_json::Map::new();
    native.insert(
        "package".into(),
        json!("@opencode/ai/providers/openai-compatible"),
    );
    if let Some(name) = provider.get("name") {
        native.insert("name".into(), name.clone());
    }
    if let Some(options) = provider
        .get("options")
        .and_then(serde_json::Value::as_object)
    {
        let mut settings = serde_json::Map::new();
        for key in ["baseURL", "apiKey", "headers", "body"] {
            if let Some(value) = options.get(key) {
                settings.insert(key.into(), value.clone());
            }
        }
        native.insert("settings".into(), serde_json::Value::Object(settings));
    }
    if let Some(models) = provider
        .get("models")
        .and_then(serde_json::Value::as_object)
    {
        let models = models
            .keys()
            .filter_map(|id| catalog.get(id))
            .map(|model| {
                let facts = OpenCodeModel::of(model);
                // OpenCode 2 builds variants from the listed reasoning efforts.
                let mut reasoning = json!({"supported": model.reasons()});
                if let Some(efforts) = facts.efforts.as_ref().filter(|efforts| !efforts.is_empty())
                {
                    reasoning["effort"] = json!(efforts);
                }
                let mut config = json!({
                    "name": model.display_name(),
                    "capabilities": {"reasoning": reasoning},
                });
                if let Some(tools) = facts.tools {
                    config["capabilities"]["tools"] = json!(tools);
                }
                if let Some((input, output)) = &facts.modalities {
                    config["capabilities"]["input"] = json!(input);
                    config["capabilities"]["output"] = json!(output);
                }
                if let Some(limit) = facts.limit {
                    config["limit"] = limit;
                }
                if let Some(cost) = &facts.cost {
                    let mut v2 = json!({"input": cost.input, "output": cost.output});
                    let mut cache = serde_json::Map::new();
                    if let Some(read) = cost.cache_read {
                        cache.insert("read".into(), json!(read));
                    }
                    if let Some(write) = cost.cache_write {
                        cache.insert("write".into(), json!(write));
                    }
                    if !cache.is_empty() {
                        v2["cache"] = serde_json::Value::Object(cache);
                    }
                    config["cost"] = json!([v2]);
                }
                (model.id().to_string(), config)
            })
            .collect();
        native.insert("models".into(), serde_json::Value::Object(models));
    }
    serde_json::Value::Object(native)
}

/// What OpenCode 1 and 2 both describe for a model, in their own fields.
struct OpenCodeModel {
    tools: Option<bool>,
    /// Input and output modalities, when the service lists them.
    modalities: Option<(Vec<String>, Vec<String>)>,
    /// The service's efforts, as it orders them for selectors, when it
    /// states them.
    efforts: Option<Vec<String>>,
    limit: Option<serde_json::Value>,
    cost: Option<OpenCodeCost>,
}

struct OpenCodeCost {
    input: f64,
    output: f64,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
}

impl OpenCodeModel {
    fn of(model: &CatalogModel) -> Self {
        // OpenCode's own modality names. Without a list, OpenCode 1 strips
        // images and OpenCode 2 sends them to every model.
        const MODALITIES: &[&str] = &["text", "audio", "image", "video", "pdf"];
        let known = |field: &str| {
            model
                .string_array(field)
                .into_iter()
                .filter(|value| MODALITIES.contains(&value.as_str()))
                .collect::<Vec<_>>()
        };
        // Text when the service lists only the inputs, as for any chat model.
        let modalities = model
            .remote
            .extra
            .contains_key("input_modalities")
            .then(|| {
                let output = if model.remote.extra.contains_key("output_modalities") {
                    known("output_modalities")
                } else {
                    vec!["text".to_string()]
                };
                (known("input_modalities"), output)
            });
        // In the service's order, which it gives for selectors.
        let efforts = model.reasoning().map(|contract| contract.efforts);
        let price = |name| model.price_per_million(name);
        Self {
            tools: model.supports_tools(),
            modalities,
            efforts,
            limit: model
                .remote
                .context_length
                .zip(model.remote.max_output_length)
                .map(|(context, output)| {
                    serde_json::json!({"context": context, "output": output.min(context)})
                }),
            cost: (price("prompt").is_some() || price("completion").is_some()).then(|| {
                OpenCodeCost {
                    input: price("prompt").unwrap_or(0.0),
                    output: price("completion").unwrap_or(0.0),
                    cache_read: price("input_cache_read"),
                    cache_write: price("input_cache_write"),
                }
            }),
        }
    }
}

/// OpenCode's `{file:<path>}` substitution, which no library writes. It ends
/// at the first `}`, so a path with a brace cannot be written safely.
fn opencode_file_reference(token_path: &Path) -> Result<String, String> {
    let path = token_path
        .to_str()
        .ok_or("The Agent token path is not valid Unicode")?;
    if path.contains(['{', '}']) {
        return Err("OpenCode cannot read an Agent token path that contains { or }".into());
    }
    Ok(format!("{{file:{path}}}"))
}
