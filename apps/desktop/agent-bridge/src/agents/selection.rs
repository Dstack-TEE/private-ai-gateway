//! Companion files beside an agent's config, with their own journal. Pi's
//! settings.json defaultProvider/defaultModel and omp's config.yml
//! modelRoles.default (config.yaml is its supported fallback filename) hold the
//! native default model; dsh's `.credentials.yaml` holds the connection token.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum File {
    Pi,
    OmpYml,
    OmpYaml,
    DshCredentials,
}

impl File {
    fn name(self) -> &'static str {
        match self {
            Self::Pi => "settings.json",
            Self::OmpYml => "config.yml",
            Self::OmpYaml => "config.yaml",
            Self::DshCredentials => dsh::CREDENTIALS_FILE,
        }
    }
    fn format(self) -> Format {
        match self {
            Self::Pi => Format::Json,
            _ => Format::Yaml,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Journal {
    file: File,
    fields: Vec<OwnedField>,
    /// The connection created the file: the SHA-256 of the text it wrote,
    /// which may hold a credential. While the file still holds exactly that
    /// text, restoring removes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created: Option<String>,
}

impl Journal {
    pub(super) fn validate(&self) -> Result<(), String> {
        if self.fields.iter().any(|field| {
            let path = refs(&field.path);
            let valid = match self.file {
                File::Pi => path == ["defaultProvider"] || path == ["defaultModel"],
                File::OmpYml | File::OmpYaml => path == ["modelRoles", "default"],
                // Only digests of the token are journaled.
                File::DshCredentials => {
                    field.hashed
                        && field.previous.is_none()
                        && (path == ["refs", dsh::TOKEN_REF] || path == ["refs"])
                }
            };
            !valid
                || !matches!(
                    field.previous,
                    None | Some(Previous::Plain(ConfigValue::Str(_)))
                )
                || !matches!(field.value, Some(ConfigValue::Str(_)))
        }) {
            return Err("Invalid companion file journal".into());
        }
        Ok(())
    }

    /// Whether a dsh credential journal wrote `token`.
    pub(super) fn wrote_token(&self, token: &str) -> bool {
        let token = ConfigValue::Str(token.to_string());
        let refs = ConfigValue::Json(serde_json::json!({ dsh::TOKEN_REF: token.to_json() }));
        self.file == File::DshCredentials
            && self
                .fields
                .iter()
                .any(|field| field.holds(Some(&token)) || field.holds(Some(&refs)))
    }
}

#[derive(Clone)]
pub(super) struct Edit {
    pub path: PathBuf,
    pub before: Option<String>,
    /// `None` removes the file.
    pub after: Option<String>,
    pub journal: Journal,
    pub changes: Vec<ConfigChange>,
}

impl Edit {
    /// The edit that puts back what this one replaces.
    pub(super) fn reversed(&self) -> Self {
        Self {
            before: self.after.clone(),
            after: self.before.clone(),
            changes: Vec::new(),
            ..self.clone()
        }
    }
}

/// Write the edit if the file still holds `before`. dsh's credential store is
/// owner-only and written under dsh's own writer lock.
pub(super) fn commit(edit: &Edit) -> io::Result<()> {
    let write = || match &edit.after {
        Some(after) if edit.journal.file == File::DshCredentials => {
            private_fs::write_private_atomic(&edit.path, after, Some(edit.before.as_deref()))
        }
        Some(after) => write_atomic(&edit.path, after, Some(edit.before.as_deref())),
        None => {
            if read(&edit.path).ok().flatten() != edit.before {
                return Err(io::Error::other(
                    "The file changed while it was being restored",
                ));
            }
            fs::remove_file(&edit.path)
        }
    };
    if edit.journal.file == File::DshCredentials {
        dsh::with_lock(&edit.path, write)
    } else {
        write()
    }
}

fn location(
    agent: Agent,
    config: &Path,
    prior: Option<&Journal>,
) -> Result<Option<(File, PathBuf)>, String> {
    let dir = config.parent().ok_or("Missing agent directory")?;
    let file = match (agent, prior) {
        (Agent::Pi, None) => File::Pi,
        (Agent::Dsh, None) => File::DshCredentials,
        (Agent::OhMyPi, None) => {
            if !dir.join("config.yml").exists() && dir.join("config.yaml").exists() {
                File::OmpYaml
            } else {
                File::OmpYml
            }
        }
        (Agent::Pi, Some(journal)) if matches!(journal.file, File::Pi) => journal.file,
        (Agent::OhMyPi, Some(journal)) if matches!(journal.file, File::OmpYml | File::OmpYaml) => {
            journal.file
        }
        (Agent::Dsh, Some(journal)) if journal.file == File::DshCredentials => journal.file,
        (_, None) => return Ok(None),
        _ => return Err("Native selection journal belongs to a different agent".into()),
    };
    Ok(Some((file, dir.join(file.name()))))
}

fn read(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(format!(
            "Cannot read native model settings at {}",
            path.display()
        )),
    }
}

pub(super) fn fingerprint(
    agent: Agent,
    config: &Path,
    prior: Option<&Journal>,
) -> Result<String, String> {
    let Some((_, path)) = location(agent, config, prior)? else {
        return Ok(String::new());
    };
    serde_json::to_string(&(path.clone(), read(&path)?))
        .map_err(|_| "Cannot fingerprint native settings".into())
}

pub(super) fn prepare(
    agent: Agent,
    config: &Path,
    prior: Option<&Journal>,
    model: &str,
) -> Result<Option<Edit>, String> {
    let Some((file, path)) = location(agent, config, prior)? else {
        return Ok(None);
    };
    if matches!(file, File::OmpYaml) && path.with_file_name("config.yml").exists() {
        return Err(
            "Oh My Pi model settings moved to config.yml; Disconnect before reconnecting.".into(),
        );
    }
    let before = read(&path)?;
    let mut doc = ConfigDoc::parse(file.format(), before.as_deref().unwrap_or_default())?;
    let fields = match file {
        File::Pi => vec![
            set(&["defaultProvider"], "private-ai-proxy"),
            set(&["defaultModel"], model),
        ],
        File::OmpYml | File::OmpYaml => vec![set(
            &["modelRoles", "default"],
            format!("private-ai-proxy/{model}"),
        )],
        File::DshCredentials => return Ok(None),
    };
    let prior = prior.map(|journal| Connection {
        fields: journal.fields.clone(),
        ..Connection::default()
    });
    let edit = project(&mut doc, &fields, prior.as_ref(), agent)?;
    let journal = Journal {
        file,
        fields: edit.record.ok_or("Missing model selection journal")?.fields,
        created: None,
    };
    journal.validate()?;
    Ok(Some(Edit {
        path,
        before,
        after: Some(doc.render()?),
        journal,
        changes: edit.changes,
    }))
}

/// Store the connection's token in dsh's credential store. A store the
/// connection creates is owned whole; in the user's store the token is an
/// exact field.
fn text_digest(text: &str) -> String {
    hex::encode(Sha256::digest(text))
}

pub(super) fn prepare_token(
    config: &Path,
    prior: Option<&Journal>,
    token: &str,
) -> Result<Edit, String> {
    let (file, path) =
        location(Agent::Dsh, config, prior)?.ok_or("Missing dsh credential store path")?;
    let before = read(&path)?;
    let mut doc = ConfigDoc::parse(file.format(), before.as_deref().unwrap_or_default())?;
    let created = match &before {
        None => {
            doc.set_value(&["version"], &ConfigValue::Number(1))?;
            true
        }
        Some(text) => prior.is_some_and(|journal| journal.created == Some(text_digest(text))),
    };
    // A store the connection created stays owned whole, even once dsh adds to it.
    let owned = before.is_none()
        || prior.is_some_and(|journal| journal.fields.iter().any(|field| !field.exact));
    let field = set(&["refs", dsh::TOKEN_REF], token);
    let field = if owned { field } else { field.exact() };
    let prior = prior.map(|journal| Connection {
        fields: journal.fields.clone(),
        ..Connection::default()
    });
    let edit = project(&mut doc, &[field], prior.as_ref(), Agent::Dsh)?;
    let after = doc.render()?;
    let mut fields = edit.record.ok_or("Missing credential journal")?.fields;
    for field in fields.iter_mut().filter(|field| !field.hashed) {
        field.value = field
            .value
            .as_ref()
            .map(|value| ConfigValue::Str(value_digest(value)));
        field.hashed = true;
    }
    let journal = Journal {
        file,
        fields,
        created: created.then(|| text_digest(&after)),
    };
    journal.validate()?;
    Ok(Edit {
        path,
        before,
        after: Some(after),
        journal,
        changes: edit.changes,
    })
}

pub(super) fn restoration(
    agent: Agent,
    config: &Path,
    journal: &Journal,
    secrets: &dyn SecretStore,
) -> Result<Option<Edit>, String> {
    journal.validate()?;
    let (_, path) =
        location(agent, config, Some(journal))?.ok_or("Missing native selection path")?;
    let Some(text) = read(&path)? else {
        return Ok(None);
    };
    if journal.created == Some(text_digest(&text)) {
        return Ok(Some(Edit {
            path,
            before: Some(text),
            after: None,
            journal: journal.clone(),
            changes: vec![ConfigChange {
                key: journal.file.name().to_string(),
                before: Some("Created by the connection".to_string()),
                after: None,
                sensitive: false,
            }],
        }));
    }
    let mut doc = ConfigDoc::parse(journal.file.format(), &text)?;
    let record = Connection {
        fields: journal.fields.clone(),
        ..Connection::default()
    };
    let edit = restore(&mut doc, &record, secrets).map_err(|error| error.to_string())?;
    Ok(Some(Edit {
        path,
        before: Some(text),
        after: Some(doc.render()?),
        journal: journal.clone(),
        changes: edit.changes,
    }))
}
