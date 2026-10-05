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
    /// The connection created the file: the SHA-256 of the text it created
    /// it with, before adding anything. A restore that returns the file to
    /// exactly that text removes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created: Option<String>,
}

impl Journal {
    pub(super) fn validate(&self) -> Result<(), String> {
        if self.fields.iter().any(|field| {
            let path = refs(&field.path);
            let string = matches!(field.value, Some(ConfigValue::Str(_)));
            let valid = match self.file {
                File::Pi => string && (path == ["defaultProvider"] || path == ["defaultModel"]),
                File::OmpYml | File::OmpYaml => string && path == ["modelRoles", "default"],
                // The token only as a digest, and the `refs` mapping it needed.
                File::DshCredentials => {
                    field.exact
                        && field.previous.is_none()
                        && if field.container {
                            path == ["refs"] && field.value.is_none()
                        } else {
                            field.hashed && string && path == ["refs", dsh::TOKEN_REF]
                        }
                }
            };
            !valid
                || !matches!(
                    field.previous,
                    None | Some(Previous::Plain(ConfigValue::Str(_)))
                )
        }) {
            return Err("Invalid companion file journal".into());
        }
        Ok(())
    }

    /// The journal as the record `project` and `restore` read.
    fn record(&self) -> Connection {
        Connection {
            fields: self.fields.clone(),
            ..Connection::default()
        }
    }

    /// Whether a dsh credential journal wrote `token`.
    pub(super) fn wrote_token(&self, token: &str) -> bool {
        let token = ConfigValue::Str(token.to_string());
        self.file == File::DshCredentials
            && self
                .fields
                .iter()
                .any(|field| !field.container && field.holds(Some(&token)))
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
    let prior = prior.map(Journal::record);
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

fn text_digest(text: &str) -> String {
    hex::encode(Sha256::digest(text))
}

/// Store the connection's token in dsh's credential store. A store the
/// connection creates is owned whole; in the user's store the token is an
/// exact field, journaled only as its digest.
pub(super) fn prepare_token(
    config: &Path,
    prior: Option<&Journal>,
    token: &str,
) -> Result<Edit, String> {
    let (file, path) =
        location(Agent::Dsh, config, prior)?.ok_or("Missing dsh credential store path")?;
    let before = read(&path)?;
    let (mut doc, created) = match &before {
        // dsh reads a store with entries only when it declares version 1.
        None => {
            let mut doc = ConfigDoc::new_yaml_mapping();
            doc.set_value(&["version"], &ConfigValue::Number(1))?;
            let base = text_digest(&doc.render()?);
            (doc, Some(base))
        }
        Some(text) => (
            ConfigDoc::parse(file.format(), text)?,
            prior.and_then(|journal| journal.created.clone()),
        ),
    };
    let field = set(&["refs", dsh::TOKEN_REF], token).exact();
    let prior = prior.map(Journal::record);
    let edit = project(&mut doc, &[field], prior.as_ref(), Agent::Dsh)?;
    let after = doc.render()?;
    let mut fields = edit.record.ok_or("Missing credential journal")?.fields;
    for field in fields
        .iter_mut()
        .filter(|field| !field.hashed && !field.container)
    {
        field.value = field
            .value
            .as_ref()
            .map(|value| ConfigValue::Str(value_digest(value)));
        field.hashed = true;
    }
    let journal = Journal {
        file,
        fields,
        created,
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
    let mut doc = ConfigDoc::parse(journal.file.format(), &text)?;
    let edit = restore(&mut doc, &journal.record(), secrets).map_err(|error| error.to_string())?;
    let after = doc.render()?;
    Ok(Some(Edit {
        path,
        before: Some(text),
        after: (journal.created != Some(text_digest(&after))).then_some(after),
        journal: journal.clone(),
        changes: edit.changes,
    }))
}
