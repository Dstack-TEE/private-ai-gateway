//! Native default-model files are separate from provider catalogs for Pi and omp.
//! Pi uses settings.json defaultProvider/defaultModel; omp uses config.yml's
//! modelRoles.default (config.yaml is its supported fallback filename).
use super::*;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(super) enum File {
    Pi,
    OmpYml,
    OmpYaml,
}

impl File {
    fn name(self) -> &'static str {
        match self {
            Self::Pi => "settings.json",
            Self::OmpYml => "config.yml",
            Self::OmpYaml => "config.yaml",
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
}

impl Journal {
    pub(super) fn validate(&self) -> Result<(), String> {
        if self.fields.iter().any(|field| {
            let path = refs(&field.path);
            let allowed = match self.file {
                File::Pi => path == ["defaultProvider"] || path == ["defaultModel"],
                _ => path == ["modelRoles", "default"],
            };
            !allowed
                || !matches!(
                    field.previous,
                    None | Some(Previous::Plain(ConfigValue::Str(_)))
                )
                || !matches!(field.value, Some(ConfigValue::Str(_)))
        }) {
            return Err("Invalid native model selection journal".into());
        }
        Ok(())
    }
}

pub(super) struct Edit {
    pub path: PathBuf,
    pub before: Option<String>,
    pub after: String,
    pub journal: Journal,
    pub changes: Vec<ConfigChange>,
}

fn location(
    agent: Agent,
    config: &Path,
    prior: Option<&Journal>,
) -> Result<Option<(File, PathBuf)>, String> {
    let dir = config.parent().ok_or("Missing agent directory")?;
    let file = match (agent, prior) {
        (Agent::Pi, None) => File::Pi,
        (Agent::OhMyPi, None) => {
            if !dir.join("config.yml").exists() && dir.join("config.yaml").exists() {
                File::OmpYaml
            } else {
                File::OmpYml
            }
        }
        (Agent::Pi, Some(journal)) if matches!(journal.file, File::Pi) => journal.file,
        (Agent::OhMyPi, Some(journal)) if !matches!(journal.file, File::Pi) => journal.file,
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
        _ => vec![set(
            &["modelRoles", "default"],
            format!("private-ai-proxy/{model}"),
        )],
    };
    let prior = prior.map(|journal| Connection {
        fields: journal.fields.clone(),
        ..Connection::default()
    });
    let edit = project(&mut doc, &fields, prior.as_ref(), agent)?;
    let journal = Journal {
        file,
        fields: edit.record.ok_or("Missing model selection journal")?.fields,
    };
    journal.validate()?;
    Ok(Some(Edit {
        path,
        before,
        after: doc.render()?,
        journal,
        changes: edit.changes,
    }))
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
    let record = Connection {
        fields: journal.fields.clone(),
        ..Connection::default()
    };
    let edit = restore(&mut doc, &record, secrets)?;
    Ok(Some(Edit {
        path,
        before: Some(text),
        after: doc.render()?,
        journal: journal.clone(),
        changes: edit.changes,
    }))
}
