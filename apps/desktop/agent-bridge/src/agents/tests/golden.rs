//! Golden transcripts. Each scenario acts on a sandbox home through the
//! projector, and after every step, refusals included, records on one line
//! what became observable: the digest of every file that changed (and the
//! exact mode of a private one), tokens issued or revoked, parked secrets,
//! statuses and errors. Input files are recorded by digest when written, so
//! a restore shows the same digest again. Each agent's connected config is
//! also shown once in full. `PAP_UPDATE_GOLDEN=1` rewrites the transcripts.
//! Linux only: helper commands, data directories and the OpenClaw helper
//! environment differ per platform.
use super::*;
use std::fmt::Write as _;

const UPDATE: &str = "PAP_UPDATE_GOLDEN";
const MODEL: &str = "openai/gpt-oss-20b";

struct Golden {
    sandbox: Sandbox,
    out: String,
    files: BTreeMap<String, String>,
    tokens: BTreeMap<&'static str, String>,
    /// Random values and what the transcript calls them.
    names: Vec<(String, String)>,
    secrets: Vec<(String, String)>,
    /// The step being recorded; every step is one line.
    line: String,
}

impl Golden {
    fn new(name: &str) -> Self {
        let mut golden = Self {
            sandbox: sandbox(&format!(
                "golden-{}",
                name.replace(|c: char| !c.is_ascii_alphanumeric(), "-")
            )),
            out: format!("# {name}\n"),
            files: BTreeMap::new(),
            tokens: BTreeMap::new(),
            names: Vec::new(),
            secrets: Vec::new(),
            line: String::new(),
        };
        golden.files = golden.snapshot();
        golden
    }

    fn projector(&mut self) -> &mut Projector {
        &mut self.sandbox.projector
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.sandbox.home.join(relative)
    }

    fn log(&mut self, line: impl std::fmt::Display) {
        self.part(line);
        self.end();
    }

    /// Adds to the step being recorded.
    fn part(&mut self, text: impl std::fmt::Display) {
        let text = self.normalize(&text.to_string());
        self.line.push_str(&text);
    }

    fn end(&mut self) {
        let line = std::mem::take(&mut self.line);
        let _ = writeln!(self.out, "{line}");
    }

    /// Names the sandbox home and random values.
    fn normalize(&self, text: &str) -> String {
        let home = self.sandbox.home.display().to_string();
        let canonical = fs::canonicalize(&self.sandbox.home)
            .unwrap()
            .display()
            .to_string();
        let mut text = text.replace(&canonical, "$HOME").replace(&home, "$HOME");
        for (value, name) in &self.names {
            text = text.replace(value, name);
        }
        text
    }

    /// Writes an input file; it is part of the scenario, not an outcome.
    fn write(&mut self, relative: &str, text: &str) {
        write(&self.path(relative), text);
        self.wrote(relative);
    }

    /// Writes an owner-only input file.
    fn private(&mut self, relative: &str, text: &str) {
        write(&self.path(relative), text);
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(self.path(relative), fs::Permissions::from_mode(0o600)).unwrap();
        }
        self.wrote(relative);
    }

    /// Records the digest of an input file, so a restore shows it again.
    fn wrote(&mut self, relative: &str) {
        self.files = self.snapshot();
        let shown = self.files.get(relative).cloned().unwrap_or_default();
        self.log(format!("write {relative}{shown}"));
    }

    /// Records a file's content in full.
    fn show(&mut self, relative: &str) {
        let text = self.read(relative);
        self.log(format!("show {relative}"));
        for line in text.lines() {
            self.log(format!("    | {line}"));
        }
    }

    fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.path(relative)).unwrap()
    }

    fn replace(&mut self, relative: &str, from: &str, to: &str) {
        let text = self.read(relative).replacen(from, to, 1);
        self.write(relative, &text);
    }

    fn edit(&mut self, relative: &str, format: Format, change: impl FnOnce(&mut ConfigDoc)) {
        let mut doc = ConfigDoc::parse(format, &self.read(relative)).unwrap();
        change(&mut doc);
        self.write(relative, &doc.render().unwrap());
    }

    fn remove(&mut self, relative: &str) {
        let path = self.path(relative);
        if path.is_dir() {
            fs::remove_dir_all(path).unwrap();
        } else {
            fs::remove_file(path).unwrap();
        }
        self.files = self.snapshot();
    }

    fn mkdir(&mut self, relative: &str) {
        fs::create_dir_all(self.path(relative)).unwrap();
    }

    /// Changes the connection record through the projector's own save.
    fn edit_store(&mut self, change: impl FnOnce(&mut Store)) {
        let mut store = self.sandbox.projector.load_store().unwrap();
        change(&mut store);
        self.sandbox.projector.save_store(&store).unwrap();
        self.files = self.snapshot();
    }

    /// Writes the connection record as is, without the projector's checks.
    fn write_store(&mut self, change: impl FnOnce(&mut Store)) {
        let mut store = self.sandbox.projector.load_store().unwrap();
        change(&mut store);
        let path = self.sandbox.projector.store_path();
        write(&path, &serde_json::to_string(&store).unwrap());
        self.files = self.snapshot();
    }

    fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.sandbox.home)
            .unwrap()
            .display()
            .to_string()
    }

    /// Points the projector at another home, as another profile would.
    fn set_home(&mut self, relative: &str) {
        self.sandbox.projector.home = self.path(relative);
        self.log(format!("use the home {relative}"));
    }

    /// OpenClaw runs the helper staged in the app data directory.
    fn stage_helper(&mut self) {
        let projector = self.projector();
        projector.helper_exe = projector
            .data_dir
            .join("helpers")
            .join(helper_binary_name());
    }

    fn home_credentials(&mut self) {
        let sandbox = &mut self.sandbox;
        sandbox.projector = Projector::at(
            sandbox.home.clone(),
            sandbox.projector.data_dir.clone(),
            sandbox.home.join("missing-helper"),
            ENDPOINT,
            false,
            sandbox.secrets.clone(),
        )
        .with_home_credentials();
        self.log("use home credentials");
    }

    fn move_endpoint(&mut self, endpoint: &str) {
        let projector = &self.sandbox.projector;
        self.sandbox.projector = Projector::at(
            projector.home.clone(),
            projector.data_dir.clone(),
            projector.helper_exe.clone(),
            endpoint,
            false,
            self.sandbox.secrets.clone(),
        );
        self.log(format!("move the Local API to {endpoint}"));
    }

    fn preview(
        &mut self,
        agent: Agent,
        connect: bool,
        catalog: Option<&Catalog>,
        model: Option<&str>,
    ) -> Option<String> {
        let revision = self.preview_part(agent, connect, catalog, model);
        self.observe();
        revision
    }

    fn preview_part(
        &mut self,
        agent: Agent,
        connect: bool,
        catalog: Option<&Catalog>,
        model: Option<&str>,
    ) -> Option<String> {
        let options = options(model);
        let action = if connect { "connect" } else { "disconnect" };
        self.part(format!(
            "{action} {}{}: ",
            agent.id(),
            label(catalog, model)
        ));
        match self.projector().preview(agent, connect, catalog, &options) {
            Ok(preview) => {
                let keys: Vec<_> = preview
                    .changes
                    .iter()
                    .map(|change| {
                        format!("{}{}", if change.sensitive { "*" } else { "" }, change.key)
                    })
                    .collect();
                let changes = serde_json::to_string(&preview.changes).unwrap();
                let note = if preview.note.starts_with("Access will be revoked") {
                    preview.note.clone()
                } else {
                    digest(preview.note.as_bytes())
                };
                let changes = digest(self.normalize(&changes).as_bytes());
                self.part(format!(
                    "changes {changes} [{}], note {note}",
                    keys.join(", ")
                ));
                Some(preview.revision)
            }
            Err(error) => {
                self.part(format!("refused {:?}: {error}", error.code()));
                None
            }
        }
    }

    fn apply(
        &mut self,
        agent: Agent,
        connect: bool,
        revision: &str,
        catalog: Option<&Catalog>,
        model: Option<&str>,
    ) {
        let result = self
            .projector()
            .apply(agent, connect, revision, catalog, &options(model));
        match result {
            Ok(status) => self.part(format!("; applied: {}", status_line(&status))),
            Err(error) => self.part(format!("; apply refused {:?}: {error}", error.code())),
        }
        self.observe();
    }

    fn connect(&mut self, agent: Agent, catalog: Option<&Catalog>, model: Option<&str>) {
        match self.preview_part(agent, true, catalog, model) {
            Some(revision) => self.apply(agent, true, &revision, catalog, model),
            // A refusal changes nothing, which the observation proves.
            None => self.observe(),
        }
    }

    fn disconnect(&mut self, agent: Agent) {
        match self.preview_part(agent, false, None, None) {
            Some(revision) => self.apply(agent, false, &revision, None, None),
            None => self.observe(),
        }
    }

    fn reconcile(&mut self, catalog: Option<&Catalog>) {
        self.part(format!("reconcile{}", label(catalog, None)));
        let failures = self.projector().reconcile(catalog).unwrap();
        self.failures(failures);
    }

    fn retarget(&mut self, catalog: &Catalog) {
        self.part(format!("retarget{}", label(Some(catalog), None)));
        let failures = self.projector().retarget(catalog).unwrap();
        self.failures(failures);
    }

    fn disconnect_all(&mut self) {
        self.part("disconnect all");
        let failures = self.projector().disconnect_all().unwrap();
        self.failures(failures);
    }

    fn initialize(&mut self) {
        self.part("initialize");
        self.projector().initialize_store().unwrap();
        self.observe();
    }

    fn failures(&mut self, failures: Vec<(String, String)>) {
        for (agent, failure) in failures {
            self.part(format!("; failed {agent}: {failure}"));
        }
        self.observe();
    }

    fn scan(&mut self, agent: Agent, catalog: Option<&Catalog>) {
        let (statuses, tokens) = self.projector().scan(catalog).unwrap();
        let authorized: Vec<_> = Agent::ALL
            .iter()
            .filter_map(|agent| {
                let token = self.sandbox.projector.tokens.read(agent.id()).ok()??;
                tokens.agent_for(&token).map(|_| agent.id())
            })
            .collect();
        self.part(format!(
            "scan{}: {} | proxy accepts {authorized:?}",
            label(catalog, None),
            status_line(agent_status(&statuses, agent))
        ));
        self.observe();
    }

    /// Records what changed since the last step.
    fn observe(&mut self) {
        let mut changes = Vec::new();
        for agent in Agent::ALL {
            let now = self
                .sandbox
                .projector
                .tokens
                .read(agent.id())
                .ok()
                .flatten();
            let before = self.tokens.get(agent.id()).cloned();
            let change = match (&before, &now) {
                (None, Some(_)) => "issued",
                (Some(_), None) => "revoked",
                (Some(before), Some(now)) if before != now => "rotated",
                _ => continue,
            };
            changes.push(format!("token {}: {change}", agent.id()));
            match now {
                Some(token) => {
                    let name = format!("<token:{}:{}>", agent.id(), self.names.len() / 2);
                    self.names.push((
                        value_digest(&ConfigValue::Str(token.clone())),
                        format!("{name}-digest"),
                    ));
                    self.names.push((token.clone(), name));
                    self.tokens.insert(agent.id(), token);
                }
                None => {
                    self.tokens.remove(agent.id());
                }
            }
        }
        let files = self.snapshot();
        for (path, shown) in &files {
            match self.files.get(path) {
                Some(before) if before == shown => {}
                Some(_) => changes.push(format!("~ {path}{shown}")),
                None => changes.push(format!("+ {path}{shown}")),
            }
        }
        for path in self.files.keys().filter(|path| !files.contains_key(*path)) {
            changes.push(format!("- {path}"));
        }
        self.files = files;
        let mut secrets = self.sandbox.secrets.entries();
        secrets.sort();
        if secrets != self.secrets {
            changes.push(format!("parked secrets: {secrets:?}"));
            self.secrets = secrets;
        }
        if !changes.is_empty() {
            self.part(format!(" => {}", changes.join("; ")));
        }
        self.end();
    }

    /// Every file under the home but the helpers and token files, as the
    /// transcript shows it.
    fn snapshot(&self) -> BTreeMap<String, String> {
        let mut paths = Vec::new();
        collect(&self.sandbox.home, &mut paths);
        let mut files = BTreeMap::new();
        for path in paths {
            let relative = path
                .strip_prefix(&self.sandbox.home)
                .unwrap()
                .display()
                .to_string();
            let parent = path.parent().and_then(Path::file_name);
            if relative.starts_with("Private AI Proxy.app")
                || parent.is_some_and(|name| name == "helpers" || name == "agent-tokens")
            {
                continue;
            }
            // The exact mode of a private file; a shared one's follows the umask.
            let access = {
                use std::os::unix::fs::PermissionsExt;
                match fs::metadata(&path).unwrap().permissions().mode() & 0o777 {
                    mode if mode & 0o077 == 0 => format!(" ({mode:o})"),
                    _ => String::new(),
                }
            };
            let text = String::from_utf8_lossy(&fs::read(&path).unwrap()).into_owned();
            let shown = digest(self.normalize(&text).as_bytes());
            files.insert(relative, format!("{access} {shown}"));
        }
        files
    }

    fn finish(self) -> String {
        self.out
    }
}

fn collect(dir: &Path, paths: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect(&path, paths);
        } else {
            paths.push(path);
        }
    }
    paths.sort();
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", &hex::encode(Sha256::digest(bytes))[..12])
}

fn options(model: Option<&str>) -> ConnectOptions {
    ConnectOptions {
        default_model: model.map(str::to_string),
    }
}

fn label(catalog: Option<&Catalog>, model: Option<&str>) -> String {
    let mut label = String::new();
    if let Some(model) = model {
        label.push_str(&format!(" model={model:?}"));
    }
    label.push_str(&match catalog {
        Some(catalog) => format!(" catalog={}", &catalog.revision[..8]),
        None => " no-catalog".to_string(),
    });
    label
}

fn status_line(status: &AgentStatus) -> String {
    let mut line: Vec<String> = [
        ("installed", status.installed),
        ("recorded", status.recorded),
        ("connected", status.connected),
        ("authorized", status.authorized),
    ]
    .into_iter()
    .filter(|(_, set)| *set)
    .map(|(name, _)| name.to_string())
    .collect();
    if let Some(attention) = &status.attention {
        line.push(format!("attention: {attention}"));
    }
    if let Some(error) = &status.error {
        line.push(format!("error: {error}"));
    }
    if let Some(repair) = status.repair_action {
        line.push(format!("repair: {repair:?}"));
    }
    line.join(", ")
}

/// Compares the transcript with its fixture, or rewrites the fixture.
fn check(name: &str, scenarios: Vec<Golden>) {
    let actual: String = scenarios.into_iter().map(Golden::finish).collect();
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/agents/tests/golden")
        .join(format!("{name}.txt"));
    if env::var_os(UPDATE).is_some() {
        write(&path, &actual);
        return;
    }
    let expected = fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing {}; run with {UPDATE}=1", path.display()));
    if let Some(line) = expected
        .lines()
        .zip(actual.lines())
        .position(|(a, b)| a != b)
        .or((expected != actual).then(|| expected.lines().count().min(actual.lines().count())))
    {
        panic!(
            "{name} differs from its golden transcript at line {}:\n expected: {:?}\n   actual: {:?}",
            line + 1,
            expected.lines().nth(line),
            actual.lines().nth(line)
        );
    }
}

fn config(agent: Agent) -> String {
    agent
        .config_path(Path::new(""), false)
        .display()
        .to_string()
}

/// Models with every projected attribute, and Chat Completions models without
/// any: no text or image input, and zero limits.
fn rich_catalog(completion: &str) -> Catalog {
    let mut catalog = Catalog::from_remote(
        &json!({"data": [
            {"id": "openai/gpt-oss-120b", "name": "GPT OSS 120B", "is_tee": true,
             "context_length": 131072, "max_output_length": 32768,
             "input_modalities": ["text", "image", "audio"],
             "supported_features": ["reasoning", "verbosity", "web_search"],
             "description": "Open weights",
             "pricing": {"prompt": "0.0000001", "completion": completion, "input_cache_read": "0.00000001"}},
            {"id": "gpt-5.4", "name": "GPT 5.4", "context_length": 400000},
            {"id": "phala/qwen", "input_modalities": ["text"]},
            {"id": "vendor/audio", "context_length": 0, "max_output_length": 0,
             "input_modalities": ["audio"]}
        ]}),
        1,
    )
    .unwrap();
    for model in &mut catalog.models[2..] {
        model.agent_surfaces = Some(vec![Surface::ChatCompletions]);
    }
    catalog
}

/// A user's own config and companion files, with comments where the format
/// keeps them.
fn users_files(agent: Agent) -> Vec<(String, &'static str)> {
    let beside = |name: &str| {
        agent
            .config_path(Path::new(""), false)
            .with_file_name(name)
            .display()
            .to_string()
    };
    let mut files = vec![(config(agent), match agent {
        Agent::Codex => "# user settings\nmodel_provider = 'original' # mine\nmodel = 'native'\n\n[model_providers.original]\nname = 'Original'\nbase_url = 'https://original.invalid/v1'\n",
        Agent::ClaudeCode => "{\n  \"env\": {\"ANTHROPIC_BASE_URL\": \"https://original.invalid\", \"ANTHROPIC_MODEL\": \"native\", \"ANTHROPIC_AUTH_TOKEN\": \"old-secret\"},\n  \"apiKeyHelper\": \"old-helper\",\n  \"model\": \"native\"\n}\n",
        Agent::OpenCode => "{\n  \"$schema\": \"https://opencode.ai/config.json\",\n  \"model\": \"original/native\",\n  \"provider\": {\"original\": {\"name\": \"User provider\"}}\n}\n",
        Agent::Pi => "{\n  \"providers\": {\"user\": {\"baseUrl\": \"https://user.invalid\", \"models\": []}}\n}\n",
        Agent::Hermes => "# hermes\nmodel:\n  provider: original # mine\n  default: native\nproviders:\n  other:\n    api: https://other.invalid/v1\ntheme: dark\n",
        Agent::OpenClaw => "{\n  // user routing\n  agents: {defaults: {model: {primary: 'original/native', fallbacks: ['original/fallback']}}},\n  models: {providers: {other: {baseUrl: 'https://other.invalid'}}},\n}\n",
        Agent::OhMyPi => "# omp models\nproviders:\n  other: {apiKey: 'user-owned', models: []} # keep\n",
        Agent::Dsh => "# user patches\n- id: session-telemetry-otel\n  disabled: true\n- id: agent-default-model # chosen in the web app\n  config:\n    provider: 'deepseek-official'\n    model: \"deepseek-flash\"\n",
    })];
    match agent {
        Agent::Pi => files.push((
            beside("settings.json"),
            "{\"defaultProvider\":\"original\",\"defaultModel\":\"native\",\"theme\":\"dark\"}\n",
        )),
        Agent::OhMyPi => files.push((
            beside("config.yml"),
            "modelRoles:\n  default: original/native # mine\n  smol: original/small\ntheme: dark\n",
        )),
        Agent::Dsh => files.push((
            beside(dsh::CREDENTIALS_FILE),
            "version: 1\n# my keys\nrefs:\n  DEEPSEEK_API_KEY: sk-user\n",
        )),
        _ => {}
    }
    files
}

/// Every agent through its whole life: a fresh install, the user's own
/// config, model and catalog changes, a moved Local API, protection stopping
/// and resuming, disconnecting, and restoring everything.
#[test]
fn agent_lifecycles_match_their_golden_transcripts() {
    let mut scenarios = Vec::new();
    for agent in Agent::ALL {
        let mut g = Golden::new(&format!("{} lifecycle", agent.id()));
        if agent == Agent::OpenClaw {
            g.stage_helper();
        }
        let first = catalog();
        let rich = rich_catalog("0.0000005");
        let repriced = rich_catalog("0.0000007");
        g.mkdir(config(agent).rsplit_once('/').unwrap().0);
        g.connect(agent, Some(&first), None);
        g.scan(agent, Some(&first));
        g.disconnect(agent);
        for (path, text) in users_files(agent) {
            g.private(&path, text);
        }
        g.connect(agent, Some(&rich), Some("openai/gpt-oss-120b"));
        for (path, _) in users_files(agent) {
            g.show(&path);
        }
        g.connect(agent, Some(&rich), Some("gpt-5.4"));
        g.reconcile(Some(&repriced));
        g.move_endpoint("http://127.0.0.1:5190");
        g.retarget(&repriced);
        g.reconcile(None);
        g.scan(agent, None);
        g.reconcile(Some(&repriced));
        g.scan(agent, Some(&repriced));
        g.disconnect_all();
        scenarios.push(g);

        // Home credentials: configs read the token file, never the helper.
        let mut g = Golden::new(&format!("{} home credentials", agent.id()));
        g.home_credentials();
        for (path, text) in users_files(agent) {
            g.private(&path, text);
        }
        g.connect(agent, Some(&rich), Some("openai/gpt-oss-120b"));
        g.disconnect(agent);
        scenarios.push(g);

        // A record written before the endpoint was recorded.
        let mut g = Golden::new(&format!("{} record without an endpoint", agent.id()));
        if agent == Agent::OpenClaw {
            g.stage_helper();
        }
        for (path, text) in users_files(agent) {
            g.private(&path, text);
        }
        g.connect(agent, Some(&rich), Some("openai/gpt-oss-120b"));
        g.edit_store(|store| store.get_mut(agent.id()).unwrap().endpoint = None);
        g.scan(agent, Some(&rich));
        g.reconcile(Some(&rich));
        g.disconnect(agent);
        scenarios.push(g);
    }
    check("lifecycles", scenarios);
}

/// An agent, what its home holds, and the files that hold it.
type Refusal = (Agent, &'static str, &'static [(&'static str, &'static str)]);

/// Configurations each agent refuses to connect over, untouched.
#[test]
fn refusals_match_their_golden_transcript() {
    let cases: &[Refusal] = &[
        (Agent::Codex, "invalid TOML", &[(".codex/config.toml", "model = \n")]),
        (Agent::Codex, "AWS authentication", &[(".codex/config.toml", "[model_providers.private_ai_proxy.aws]\nregion = 'x'\n")]),
        (Agent::ClaudeCode, "invalid JSON", &[(".claude/settings.json", "{\"env\": ")]),
        (Agent::ClaudeCode, "an unmanaged model picker", &[(".claude/settings.json", "{\"modelPicker\":{\"options\":[]}}")]),
        (Agent::OpenCode, "an unmanaged null provider", &[(".config/opencode/opencode.json", "{\"provider\":{\"private-ai-proxy\":null}}")]),
        (Agent::OpenCode, "an unmanaged provider with credentials", &[(".config/opencode/opencode.json", "{\"provider\":{\"private-ai-proxy\":{\"apiKey\":\"sk-test-hidden\",\"options\":{\"headers\":{\"Authorization\":\"sk-test-hidden\"}}}}}")]),
        (Agent::OpenCode, "an OpenCode 2 provider", &[(".config/opencode/opencode.json", "{\"providers\":{\"private-ai-proxy\":{\"settings\":{\"baseURL\":\"http://127.0.0.1:1/v1\"}}}}")]),
        (Agent::OpenCode, "an OpenCode 2 provider in another layer", &[(".config/opencode/opencode.jsonc", "{/* v2 */\"providers\":{\"private-ai-proxy\":{}}}")]),
        (Agent::OpenCode, "disabled providers", &[(".config/opencode/opencode.json", "{\"disabled_providers\":[\"private-ai-proxy\"]}")]),
        (Agent::OpenCode, "an invalid layer", &[(".config/opencode/config.json", "{invalid")]),
        (Agent::Pi, "an unmanaged null provider", &[(".pi/agent/models.json", "{\"providers\":{\"private-ai-proxy\":null}}")]),
        (Agent::Pi, "an unmanaged provider with credentials", &[(".pi/agent/models.json", "{\"providers\":{\"private-ai-proxy\":{\"apiKey\":\"sk-test-hidden\",\"options\":{\"headers\":{\"Authorization\":\"sk-test-hidden\"}}}}}")]),
        (Agent::Pi, "a stored key", &[(".pi/agent/auth.json", "{\"private-ai-proxy\":{\"type\":\"api_key\",\"key\":\"sk-test-hidden\"}}")]),
        (Agent::Pi, "an invalid auth file", &[(".pi/agent/auth.json", "[]")]),
        (Agent::Hermes, "an unmanaged provider", &[(".hermes/config.yaml", "providers:\n  private-ai-proxy:\n    api: https://x.invalid\n")]),
        (Agent::Hermes, "an explicit key", &[(".hermes/config.yaml", "model:\n  api_key: sk-test-hidden\n")]),
        (Agent::Hermes, "a fallback model", &[(".hermes/config.yaml", "fallback_model:\n  provider: other\n")]),
        (Agent::Hermes, "a named credential pool", &[(".hermes/auth.json", "{\"credential_pool\":{\"custom:private-ai-proxy\":[{}]}}")]),
        (Agent::OpenClaw, "an include", &[(".openclaw/openclaw.json", "{models: {$include: 'models.json'}}")]),
        (Agent::OpenClaw, "a nested include", &[(".openclaw/openclaw.json", "{\"models\":{\"$include\":\"models.json\"}}")]),
        (Agent::OpenClaw, "a remote gateway", &[(".openclaw/openclaw.json", "{\"gateway\":{\"mode\":\"remote\"}}")]),
        (Agent::OpenClaw, "an existing provider", &[(".openclaw/openclaw.json", "{\"models\":{\"providers\":{\"private-ai-proxy\":{\"apiKey\":\"synthetic-existing\"}}}}")]),
        (Agent::OpenClaw, "a null secret provider", &[(".openclaw/openclaw.json", "{\"secrets\":{\"providers\":{\"private-ai-proxy\":null}}}")]),
        (Agent::OpenClaw, "a provider alias", &[(".openclaw/openclaw.json", "{\"models\":{\"providers\":{\" PRIVATE-AI-PROXY \":{}}}}")]),
        (Agent::OpenClaw, "another exec SecretRef", &[(".openclaw/openclaw.json", "{\"env\":{\"KEY\":{\"source\":\"exec\",\"provider\":\"private-ai-proxy\",\"id\":\"another\"}}}")]),
        (Agent::OpenClaw, "another file SecretRef", &[(".openclaw/openclaw.json", "{\"env\":{\"KEY\":{\"source\":\"file\",\"provider\":\"private-ai-proxy\",\"id\":\"openclaw\"}}}")]),
        (Agent::OpenClaw, "a scalar default model", &[(".openclaw/openclaw.json", "{\"agents\":{\"defaults\":{\"model\":\"external/model\"}}}")]),
        (Agent::OpenClaw, "a legacy config", &[(".openclaw/clawdbot.json", "{}")]),
        (Agent::OhMyPi, "a legacy models.json", &[(".omp/agent/models.json", "{\"providers\":{\"private-ai-proxy\":{\"apiKey\":\"legacy-secret\"}}}")]),
        (Agent::OhMyPi, "an unmanaged provider", &[(".omp/agent/models.yml", "providers:\n  private-ai-proxy: {apiKey: secret}\n")]),
        (Agent::OhMyPi, "a null provider", &[(".omp/agent/models.yml", "providers:\n  private-ai-proxy: null\n")]),
        (Agent::OhMyPi, "a provider list", &[(".omp/agent/models.yml", "providers: []\n")]),
        (Agent::OhMyPi, "duplicate keys", &[(".omp/agent/models.yml", "providers: {other: {}, other: {}}\n")]),
        (Agent::OhMyPi, "a merge key", &[(".omp/agent/models.yml", "base: &base {other: {}}\nproviders: {<<: *base}\n")]),
        (Agent::OhMyPi, "two documents", &[(".omp/agent/models.yml", "providers: {}\n---\nproviders: {}\n")]),
        (Agent::Dsh, "a profile's own providers", &[(".dsh/profiles/web/cordis.patch.yml", "- id: llm-pi-ai\n  config: {providers: {kimi: {}}}\n")]),
        (Agent::Dsh, "a profile disabling credentials", &[(".dsh/profiles/web/cordis.patch.yml", "- id: credentials\n  disabled: true\n")]),
        (Agent::Dsh, "a profile moving the store", &[(".dsh/profiles/web/cordis.patch.yml", "- id: credentials\n  config: {path: /tmp/elsewhere.yaml}\n")]),
        (Agent::Dsh, "an inserted default model", &[(".dsh/cordis.patch.yml", "- insert:\n    - {id: agent-default-model, name: x}\n")]),
        (Agent::Dsh, "duplicate ids", &[(".dsh/cordis.patch.yml", "- id: llm-pi-ai\n- id: llm-pi-ai\n")]),
        (Agent::Dsh, "a flow list", &[(".dsh/cordis.patch.yml", "[{id: tools}]\n")]),
        (Agent::Dsh, "a flow item", &[(".dsh/cordis.patch.yml", "- {id: agent-default-model, config: {model: m}}\n")]),
        (Agent::Dsh, "no final newline", &[(".dsh/cordis.patch.yml", "- id: tools")]),
        (Agent::Dsh, "a reasoning effort", &[(".dsh/cordis.patch.yml", "- id: agent-default-model\n  config:\n    reasoningEffort: max\n")]),
        (Agent::Dsh, "an existing provider", &[(".dsh/cordis.patch.yml", "- id: llm-pi-ai\n  config:\n    providers:\n      private-ai-proxy: {api: x}\n")]),
        (Agent::Dsh, "a foreign token", &[(".dsh/.credentials.yaml", "version: 1\nrefs:\n  PRIVATE_AI_PROXY_DSH_TOKEN: someone-else\n")]),
        (Agent::Dsh, "an indented list", &[(".dsh/cordis.patch.yml", "    - id: tools\n")]),
        (Agent::Dsh, "search under another id", &[(".dsh/profiles/web/cordis.patch.yml", "- insert:\n    - {id: my-search, name: '@deepseek-ai/dsh-web-search-deepseek'}\n")]),
        (Agent::Dsh, "acp disabled", &[(".dsh/profiles/web/cordis.patch.yml", "- id: acp\n  disabled: true\n")]),
        (Agent::Dsh, "acp under another id", &[(".dsh/profiles/web/cordis.patch.yml", "- insert:\n    - {id: my-acp, name: '@deepseek-ai/dsh-acp', config: {provider: deepseek-official, model: m}}\n")]),
        (Agent::Dsh, "an inserted acp", &[(".dsh/cordis.patch.yml", "- insert:\n    - {id: acp, name: '@deepseek-ai/dsh-acp'}\n")]),
        (Agent::Dsh, "legacy settings", &[(".dsh/settings.yaml", "llm-pi-ai: {}\n")]),
        (Agent::Dsh, "a store version", &[(".dsh/.credentials.yaml", "version: 2\n")]),
    ];
    let mut scenarios = Vec::new();
    for (agent, case, files) in cases {
        let mut g = Golden::new(&format!("{} refuses {case}", agent.id()));
        if *agent == Agent::OpenClaw {
            g.stage_helper();
        }
        for (path, text) in *files {
            g.private(path, text);
        }
        let model = if *agent == Agent::Dsh {
            "phala/qwen"
        } else {
            MODEL
        };
        g.connect(*agent, Some(&catalog()), Some(model));
        scenarios.push(g);
    }
    // A store others can read, and models the catalog does not offer.
    let mut g = Golden::new("dsh refuses a readable store");
    g.write(".dsh/.credentials.yaml", "version: 1\nrefs: {}\n");
    g.connect(Agent::Dsh, Some(&catalog()), Some("phala/qwen"));
    scenarios.push(g);
    let mut g = Golden::new("unknown models are refused");
    for (agent, model) in [
        (Agent::ClaudeCode, "claude-sonnet-4-6"),
        (Agent::Codex, "missing/model"),
        (Agent::Hermes, " "),
    ] {
        g.preview(agent, true, Some(&catalog()), Some(model));
    }
    scenarios.push(g);
    check("refusals", scenarios);
}

/// Ports of catalog and projection tests: the same inputs and actions,
/// with everything observable recorded.
#[test]
fn catalog_scenarios_match_their_golden_transcript() {
    let cat = catalog();
    let codex = config(Agent::Codex);
    let mut scenarios = Vec::new();

    let mut g = Golden::new("codex model changes keep credentials, endpoint changes revoke them");
    g.mkdir(".codex");
    g.connect(Agent::Codex, Some(&cat), Some(MODEL));
    g.edit(&codex, Format::Toml, |doc| {
        doc.set_str(&["model"], "phala/qwen").unwrap()
    });
    g.reconcile(Some(&cat));
    g.scan(Agent::Codex, Some(&cat));
    g.reconcile(None);
    g.edit_store(|store| {
        let record = store.get_mut("codex").unwrap();
        record.options = options(Some(MODEL));
        record.attention = Some("Configuration requires explicit repair".into());
    });
    g.connect(Agent::Codex, Some(&cat), None);
    g.reconcile(None);
    g.reconcile(Some(&cat));
    g.edit(&codex, Format::Toml, |doc| {
        doc.set_str(
            &["model_providers", "private_ai_proxy", "base_url"],
            "https://example.com/v1",
        )
        .unwrap()
    });
    g.reconcile(Some(&cat));
    scenarios.push(g);

    let mut g = Golden::new("codex takes over provider credentials; opencode connects");
    g.write(&codex, "[model_providers.private_ai_proxy]\nenv_key = 'OLD_KEY'\nexperimental_bearer_token = 'old-synthetic-token'\nrequires_openai_auth = true\n");
    g.connect(Agent::Codex, Some(&cat), Some(MODEL));
    g.connect(Agent::Codex, Some(&cat), Some(MODEL));
    let catalog_file = g.relative(&g.sandbox.projector.codex_catalog_path());
    g.remove(&catalog_file);
    g.connect(Agent::Codex, Some(&cat), Some(MODEL));
    g.disconnect(Agent::Codex);
    g.connect(Agent::Codex, Some(&cat), Some(MODEL));
    g.disconnect(Agent::Codex);
    g.connect(Agent::OpenCode, Some(&cat), Some(MODEL));
    g.disconnect(Agent::OpenCode);
    scenarios.push(g);

    let mut g = Golden::new("startup repairs only an invalid codex provider name");
    g.write(&codex, "# keep this comment\n[model_providers.private_ai_proxy]\nbase_url = 'https://example.com/v1'\n");
    g.initialize();
    g.edit(&codex, Format::Toml, |doc| {
        doc.set_str(
            &["model_providers", "private_ai_proxy", "name"],
            "Existing Name",
        )
        .unwrap()
    });
    g.initialize();
    scenarios.push(g);

    let mut g = Golden::new("codex overlay keeps native templates and unique slugs");
    let overlay = Catalog::from_remote(&json!({"data": [
        {"id": "gpt-5.4", "context_length": 32768,
         "supported_features": ["reasoning", "verbosity"], "input_modalities": ["text", "image"]},
        {"id": "openai/gpt-5.4", "context_length": 65536}
    ]}), 1).unwrap();
    g.connect(Agent::Codex, Some(&overlay), Some("openai/gpt-5.4"));
    scenarios.push(g);

    let mut g = Golden::new("an inventory refresh updates the catalog without rotating the token");
    g.write(&config(Agent::Pi), r#"{"custom":true}"#);
    let inventory = crate::catalog::EndpointInventory::bundled().unwrap();
    let value = serde_json::to_value(&inventory).unwrap();
    let model = value["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| {
            entry["endpoint"].as_str() == Some("/v1/chat/completions")
                && entry["status"].as_str() == Some("supported")
                && entry["checks"].as_object().is_some_and(|checks| {
                    checks.values().all(|check| check["status"] == "supported")
                })
        })
        .and_then(|entry| entry["model"].as_str())
        .unwrap();
    let mut probed = Catalog::from_remote(
        &json!({"data": [{"id": model}, {"id": "unprobed/model"}]}),
        1,
    )
    .unwrap();
    g.connect(Agent::Pi, Some(&probed), None);
    probed
        .apply_endpoint_inventory("https://tee.redpill.ai", &inventory)
        .unwrap();
    g.reconcile(Some(&probed));
    g.reconcile(Some(&probed));
    g.disconnect(Agent::Pi);
    scenarios.push(g);

    let mut g = Golden::new("claude selects a Messages model");
    g.write(
        &config(Agent::ClaudeCode),
        r#"{"model":"opus","env":{"KEEP":"value"}}"#,
    );
    let mut surfaces = catalog();
    surfaces.models[0].supported_surfaces = Some(vec![Surface::Responses]);
    surfaces.models[1].supported_surfaces = Some(vec![Surface::Messages]);
    g.connect(Agent::ClaudeCode, Some(&surfaces), None);
    g.disconnect(Agent::ClaudeCode);
    scenarios.push(g);

    let mut g = Golden::new("agents require a verified model and the helper");
    g.preview(Agent::ClaudeCode, true, Some(&cat), None);
    g.preview(
        Agent::ClaudeCode,
        true,
        Some(&cat),
        Some("claude-sonnet-4-6"),
    );
    {
        use std::os::unix::fs::PermissionsExt;
        let helper = g.sandbox.projector.helper_exe.clone();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o600)).unwrap();
        g.log("make the helper not executable");
    }
    g.preview(Agent::Codex, true, Some(&cat), Some(MODEL));
    fs::remove_file(&g.sandbox.projector.helper_exe).unwrap();
    g.log("remove the helper");
    g.scan(Agent::ClaudeCode, Some(&cat));
    g.preview(Agent::ClaudeCode, true, Some(&cat), Some(MODEL));
    scenarios.push(g);

    let mut g = Golden::new("a record projected to another endpoint is projected again");
    g.mkdir(".claude");
    g.connect(Agent::ClaudeCode, Some(&cat), Some(MODEL));
    g.move_endpoint("http://127.0.0.1:5190");
    g.reconcile(Some(&cat));
    g.scan(Agent::ClaudeCode, Some(&cat));
    scenarios.push(g);

    let mut g = Golden::new("projections and codex defaults use the same endpoint filter");
    g.mkdir(".codex");
    let mut filtered = catalog();
    filtered.models[0].supported_surfaces = Some(vec![Surface::ChatCompletions]);
    filtered.models[1].supported_surfaces = Some(vec![Surface::Responses]);
    g.connect(Agent::Codex, Some(&filtered), None);
    g.connect(Agent::Pi, Some(&filtered), None);
    g.preview(Agent::ClaudeCode, true, Some(&filtered), None);
    g.preview(Agent::Codex, true, Some(&filtered), Some(MODEL));
    filtered.models[0].supported_surfaces = Some(vec![Surface::Responses]);
    filtered.models[1].supported_surfaces = Some(vec![]);
    filtered.revision = "responses-model-changed".into();
    g.reconcile(Some(&filtered));
    scenarios.push(g);

    let mut g = Golden::new("connect creates the official config from scratch");
    g.scan(Agent::ClaudeCode, None);
    g.connect(Agent::ClaudeCode, Some(&cat), Some(MODEL));
    scenarios.push(g);

    // Prices decimals: older float prices still match, long prices are rounded.
    const OLDER: &str = "0.049999999999999996";
    let priced = Catalog::from_remote(
        &json!({"data": [{
            "id": "openai/gpt-5-nano",
            "pricing": {"prompt": "0.00000005", "completion": "0.0000004"}
        }]}),
        1,
    )
    .unwrap();
    let long = Catalog::from_remote(
        &json!({"data": [{
            "id": "openai/gpt-5-nano",
            "pricing": {"prompt": "0.000000049999999999999996", "completion": "0.0000004"}
        }]}),
        1,
    )
    .unwrap();
    for agent in [Agent::Pi, Agent::OhMyPi, Agent::OpenClaw] {
        for saved_again in [true, false] {
            let mut g = Golden::new(&format!(
                "{} older float prices, saved again: {saved_again}",
                agent.id()
            ));
            g.connect(agent, Some(&priced), None);
            g.replace(&config(agent), "0.05", OLDER);
            if !saved_again {
                let store = g.relative(&g.sandbox.projector.store_path());
                g.replace(&store, "0.05", OLDER);
            }
            g.scan(agent, Some(&priced));
            g.connect(agent, Some(&priced), None);
            scenarios.push(g);
        }
        let mut g = Golden::new(&format!("{} long prices", agent.id()));
        g.connect(agent, Some(&long), None);
        scenarios.push(g);
    }

    // Older builds wrote names without `[TEE]`, and no Claude Code labels,
    // under a revision that did not cover display names.
    fn older(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::String(text) => *text = text.replace(" [TEE]", ""),
            serde_json::Value::Array(items) => items.iter_mut().for_each(older),
            serde_json::Value::Object(object) => {
                if object.contains_key("model") {
                    object.remove("label");
                }
                object.values_mut().for_each(older);
            }
            _ => {}
        }
    }
    let tee = Catalog::from_remote(
        &json!({"data": [
            {"id": "openai/gpt-oss-120b", "name": "OpenAI: GPT OSS 120B", "is_tee": true},
            {"id": "phala/qwen", "is_tee": true}
        ]}),
        1,
    )
    .unwrap();
    let mut hasher = Sha256::new();
    for model in &tee.models {
        let canonical = serde_json::to_vec(&model.remote).unwrap();
        hasher.update((canonical.len() as u64).to_be_bytes());
        hasher.update(&canonical);
    }
    let older_revision = hex::encode(hasher.finalize());
    for agent in [
        Agent::Codex,
        Agent::ClaudeCode,
        Agent::OpenCode,
        Agent::Pi,
        Agent::OhMyPi,
        Agent::OpenClaw,
    ] {
        let mut g = Golden::new(&format!("{} older names without [TEE]", agent.id()));
        g.mkdir(config(agent).rsplit_once('/').unwrap().0);
        g.connect(agent, Some(&tee), Some("openai/gpt-oss-120b"));
        let path = if agent == Agent::Codex {
            g.relative(&g.sandbox.projector.codex_catalog_path())
        } else {
            config(agent)
        };
        let connected = g.read(&path);
        let text = match serde_json::from_str::<serde_json::Value>(&connected) {
            Ok(mut value) => {
                older(&mut value);
                serde_json::to_string_pretty(&value).unwrap()
            }
            Err(_) => connected.replace(" [TEE]", ""),
        };
        g.write(&path, &text);
        let store = g.relative(&g.sandbox.projector.store_path());
        let mut record: serde_json::Value = serde_json::from_str(&g.read(&store)).unwrap();
        older(&mut record);
        record[agent.id()]["catalog_revision"] = json!(older_revision);
        g.write(&store, &serde_json::to_string(&record).unwrap());
        g.scan(agent, Some(&tee));
        g.reconcile(Some(&tee));
        g.scan(agent, Some(&tee));
        g.connect(agent, Some(&tee), Some("openai/gpt-oss-120b"));
        scenarios.push(g);
    }
    check("catalogs", scenarios);
}

/// Ports of lifecycle, drift and recovery tests.
#[test]
fn recovery_scenarios_match_their_golden_transcript() {
    let cat = catalog();
    let claude = config(Agent::ClaudeCode);
    let mut scenarios = Vec::new();

    let mut g = Golden::new("links survive stop, restart and uninstall");
    g.write(
        &claude,
        r#"{"model":"original","env":{"ANTHROPIC_AUTH_TOKEN":"original-secret"}}"#,
    );
    g.connect(Agent::ClaudeCode, None, Some(MODEL));
    g.reconcile(Some(&cat));
    g.scan(Agent::ClaudeCode, None);
    g.reconcile(None);
    g.reconcile(None);
    g.scan(Agent::ClaudeCode, None);
    g.reconcile(Some(&cat));
    g.remove(".claude");
    g.reconcile(Some(&cat));
    g.scan(Agent::ClaudeCode, None);
    g.mkdir(".claude");
    g.reconcile(Some(&cat));
    g.scan(Agent::ClaudeCode, None);
    g.disconnect(Agent::ClaudeCode);
    scenarios.push(g);

    let mut g = Golden::new("a temporary failure retries without clearing user conflicts");
    let opencode = config(Agent::OpenCode);
    g.mkdir(opencode.rsplit_once('/').unwrap().0);
    g.connect(Agent::OpenCode, None, None);
    let token = g.relative(&g.sandbox.projector.tokens.path("opencode"));
    g.mkdir(&token);
    g.log(format!("make {token} a directory"));
    g.reconcile(Some(&cat));
    g.remove(&token);
    g.reconcile(Some(&cat));
    g.scan(Agent::OpenCode, Some(&cat));
    g.reconcile(None);
    g.write(
        &opencode,
        r#"{"provider":{"private-ai-proxy":{"name":"User-owned"}}}"#,
    );
    g.reconcile(Some(&cat));
    g.reconcile(Some(&cat));
    scenarios.push(g);

    let cases: &[(Agent, &str, &str, &str)] = &[
        (
            Agent::Codex,
            "aws",
            "model_providers.private_ai_proxy.aws.region",
            "test-region",
        ),
        (
            Agent::Pi,
            "stored-key",
            ".pi/agent/auth.json",
            r#"{"private-ai-proxy":{"type":"api_key","key":"sk-test-hidden"}}"#,
        ),
        (
            Agent::OpenCode,
            "disabled",
            "disabled_providers",
            "private-ai-proxy",
        ),
        (Agent::OpenCode, "allowlist", "enabled_providers", "other"),
        (
            Agent::Hermes,
            "api_mode",
            "providers.private-ai-proxy.api_mode",
            "codex_responses",
        ),
        (
            Agent::Hermes,
            "disabled",
            "providers.private-ai-proxy.enabled",
            "false",
        ),
        (
            Agent::Hermes,
            "explicit-key",
            "model.api_key",
            "sk-test-hidden",
        ),
        (
            Agent::Hermes,
            "pool",
            ".hermes/auth.json",
            r#"{"credential_pool":{"private-ai-proxy":[{"access_token":"sk-test-hidden"}]}}"#,
        ),
        (
            Agent::Hermes,
            "fallback",
            "fallback_model.provider",
            "other",
        ),
    ];
    for (agent, case, key, value) in cases {
        let agent = *agent;
        let mut g = Golden::new(&format!(
            "{} conflicts after connecting: {case}",
            agent.id()
        ));
        g.connect(agent, Some(&cat), Some(MODEL));
        let revision = g.preview(agent, true, Some(&cat), Some(MODEL)).unwrap();
        if key.starts_with('.') {
            g.write(key, value);
        } else {
            let path: Vec<&str> = key.split('.').collect();
            g.edit(&config(agent), agent.format(), |doc| {
                let value = match *value {
                    "false" => ConfigValue::Bool(false),
                    _ if agent == Agent::OpenCode => ConfigValue::List(vec![value.to_string()]),
                    _ => ConfigValue::Str(value.to_string()),
                };
                doc.set_value(&path, &value).unwrap();
            });
        }
        g.scan(agent, Some(&cat));
        g.preview(agent, true, Some(&cat), Some(MODEL));
        g.apply(agent, true, &revision, Some(&cat), Some(MODEL));
        g.disconnect(agent);
        scenarios.push(g);
    }

    let mut g = Golden::new("drifted or broken configs deauthorize but stay recoverable");
    g.write(
        &claude,
        r#"{"model": "opus", "env": {"ANTHROPIC_AUTH_TOKEN": "sk-old-secret"}}"#,
    );
    g.connect(Agent::ClaudeCode, Some(&cat), Some(MODEL));
    g.scan(Agent::ClaudeCode, None);
    g.edit(&claude, Format::Json, |doc| {
        doc.set_str(&["env", "ANTHROPIC_MODEL"], "somewhere/else")
            .unwrap()
    });
    g.scan(Agent::ClaudeCode, None);
    g.write(&claude, "{ not json");
    g.preview(Agent::ClaudeCode, true, Some(&cat), Some(MODEL));
    g.scan(Agent::ClaudeCode, None);
    g.disconnect(Agent::ClaudeCode);
    scenarios.push(g);

    let mut g = Golden::new("an empty OpenClaw helper deauthorizes without blocking disconnect");
    g.stage_helper();
    g.connect(Agent::OpenClaw, Some(&cat), Some(MODEL));
    fs::write(&g.sandbox.projector.helper_exe, "").unwrap();
    g.log("empty the staged helper");
    g.scan(Agent::OpenClaw, None);
    g.preview(Agent::OpenClaw, true, Some(&cat), Some(MODEL));
    g.disconnect(Agent::OpenClaw);
    scenarios.push(g);

    let mut g =
        Golden::new("native model settings edits invalidate a preview and survive disconnect");
    let settings = ".pi/agent/settings.json";
    g.write(&config(Agent::Pi), "{}");
    g.write(
        settings,
        r#"{"defaultProvider":"original","defaultModel":"native"}"#,
    );
    let revision = g.preview(Agent::Pi, true, Some(&cat), None).unwrap();
    g.write(
        settings,
        r#"{"defaultProvider":"edited","defaultModel":"edited-model"}"#,
    );
    g.apply(Agent::Pi, true, &revision, Some(&cat), None);
    g.connect(Agent::Pi, Some(&cat), None);
    g.write(
        settings,
        r#"{"defaultProvider":"user-choice","defaultModel":"user-model"}"#,
    );
    g.disconnect(Agent::Pi);
    scenarios.push(g);

    let mut g = Golden::new("OpenCode merge conflicts revoke without writes");
    let jsonc = opencode.replace(".json", ".jsonc");
    g.write(
        &opencode,
        r#"{"model":"other/original","provider":{"other":{"name":"User provider"}}}"#,
    );
    let benign =
        "{/* user's comment */\"provider\":{\"other\":{\"name\":\"JSONC user provider\"}},}";
    g.write(&jsonc, benign);
    g.connect(Agent::OpenCode, Some(&cat), Some(MODEL));
    let revision = g
        .preview(Agent::OpenCode, true, Some(&cat), Some(MODEL))
        .unwrap();
    for conflict in [
        "{\"model\":\"other/override\",}",
        "{/* keep */\"provider\":{\"private-ai-proxy\":{\"options\":{\"baseURL\":\"http://127.0.0.1:1/v1\"}}}}",
        "{\"provider\":{\"private-ai-proxy\":{\"options\":{\"apiKey\":\"synthetic-never-log-me\"}}}}",
        "{\"providers\":{\"private-ai-proxy\":{\"settings\":{\"baseURL\":\"http://127.0.0.1:1/v1\"}}}}",
        "{\"provider\":null}",
        "{/* broken",
    ] {
        g.write(&jsonc, conflict);
        g.scan(Agent::OpenCode, None);
        g.preview(Agent::OpenCode, true, Some(&cat), Some(MODEL));
        g.apply(Agent::OpenCode, true, &revision, Some(&cat), Some(MODEL));
    }
    g.remove(&jsonc);
    g.mkdir(&jsonc);
    g.log(format!("make {jsonc} a directory"));
    g.scan(Agent::OpenCode, None);
    g.remove(&jsonc);
    g.write(&jsonc, benign);
    g.scan(Agent::OpenCode, None);
    g.write(
        &jsonc,
        "{/* keep on disconnect */\"model\":\"other/override\"}",
    );
    g.edit(&opencode, Format::Json, |doc| {
        doc.set_str(&["provider", "other", "name"], "Edited outside the app")
            .unwrap()
    });
    g.disconnect(Agent::OpenCode);
    scenarios.push(g);

    let mut g = Golden::new("credential restore revokes before refusing a changed route");
    g.write(
        &claude,
        r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"sk-test-parked"}}"#,
    );
    g.connect(Agent::ClaudeCode, Some(&cat), Some(MODEL));
    let projection = g.read(&claude);
    g.edit(&claude, Format::Json, |doc| {
        doc.set_str(&["env", "ANTHROPIC_BASE_URL"], "http://127.0.0.1:1")
            .unwrap()
    });
    g.disconnect(Agent::ClaudeCode);
    g.write(&claude, &projection);
    g.disconnect(Agent::ClaudeCode);
    scenarios.push(g);

    for agent in [
        Agent::Codex,
        Agent::OpenCode,
        Agent::Pi,
        Agent::Hermes,
        Agent::OpenClaw,
        Agent::OhMyPi,
    ] {
        let mut g = Golden::new(&format!(
            "{} ownership does not follow a new config path",
            agent.id()
        ));
        g.connect(agent, Some(&cat), None);
        g.disconnect(agent);
        let retained = g.read(&config(agent));
        g.set_home("new-home");
        let target = format!("new-home/{}", config(agent));
        g.write(&target, &retained);
        g.scan(agent, None);
        if agent == Agent::OpenCode {
            g.preview(agent, true, Some(&cat), None);
            g.connect(agent, None, None);
            g.preview(agent, true, Some(&cat), None);
            g.disconnect(agent);
        }
        g.remove(&target);
        g.connect(agent, Some(&cat), None);
        g.disconnect(agent);
        scenarios.push(g);
    }

    let mut g = Golden::new("disconnect all restores every agent");
    g.write(&claude, r#"{"model": "opus"}"#);
    g.connect(Agent::ClaudeCode, Some(&cat), Some(MODEL));
    g.write(
        &config(Agent::Codex),
        "model_provider = \"private_ai_proxy\"\n",
    );
    g.sandbox.projector.tokens.ensure("codex").unwrap();
    let codex_path = g.path(&config(Agent::Codex));
    g.edit_store(|store| {
        store.insert(
            "codex".into(),
            Connection {
                config_path: codex_path,
                fields: vec![OwnedField {
                    path: owned(&["model_provider"]),
                    value: Some(ConfigValue::Str("private_ai_proxy".into())),
                    previous: None,
                    entry: None,
                    exact: false,
                    source: None,
                    hashed: false,
                    container: false,
                }],
                disabled: true,
                cleanup_pending: false,
                ..Connection::default()
            },
        );
    });
    g.disconnect_all();
    scenarios.push(g);

    for structured in [false, true] {
        let mut g = Golden::new(&format!(
            "invalid recovery never guesses paths, structured: {structured}"
        ));
        g.connect(Agent::ClaudeCode, Some(&cat), Some(MODEL));
        g.write_store(|store| {
            let record = store.get_mut("claude-code").unwrap();
            record.config_path = PathBuf::new();
            if structured {
                record.fields[0].previous = Some(Previous::Plain(ConfigValue::Json(
                    json!({"apiKey":"sk-test-hidden"}),
                )));
            }
        });
        g.scan(Agent::ClaudeCode, None);
        g.disconnect(Agent::ClaudeCode);
        scenarios.push(g);
    }

    check("recovery", scenarios);
}

/// Ports of the OpenClaw, Oh My Pi and dsh scenario tests.
#[test]
fn native_agent_scenarios_match_their_golden_transcript() {
    let cat = catalog();
    let mut scenarios = Vec::new();

    let mut g = Golden::new("omp external provider edit revokes without overwriting");
    g.connect(Agent::OhMyPi, Some(&cat), None);
    g.write(
        &config(Agent::OhMyPi),
        "# externally replaced\nproviders: {private-ai-proxy: {apiKey: external-secret}}\n",
    );
    g.scan(Agent::OhMyPi, None);
    g.disconnect(Agent::OhMyPi);
    scenarios.push(g);

    let dsh = ".dsh";
    let credentials = format!("{dsh}/{}", dsh::CREDENTIALS_FILE);
    let mut g = Golden::new("dsh pauses access when it drifts after connecting");
    g.mkdir(dsh);
    g.connect(Agent::Dsh, Some(&cat), Some("phala/qwen"));
    let stored = g.read(&credentials);
    let token = g.sandbox.projector.tokens.read("dsh").unwrap().unwrap();
    g.private(&credentials, &stored.replace(&token, "replaced"));
    g.scan(Agent::Dsh, Some(&cat));
    g.private(&credentials, &stored);
    g.scan(Agent::Dsh, Some(&cat));
    let profile = format!("{dsh}/profiles/work/cordis.patch.yml");
    g.write(
        &profile,
        "- id: llm-pi-ai\n  config: {providers: {kimi: {}}}\n",
    );
    g.scan(Agent::Dsh, Some(&cat));
    g.remove(&profile);
    g.scan(Agent::Dsh, Some(&cat));
    g.disconnect(Agent::Dsh);
    scenarios.push(g);

    for (name, original) in [
        ("created", None),
        ("existing", Some("version: 1\n# my records\nrecords: {}\n")),
    ] {
        let mut g = Golden::new(&format!("dsh keys added later keep a {name} store"));
        g.mkdir(dsh);
        if let Some(original) = original {
            g.private(&credentials, original);
        }
        g.connect(Agent::Dsh, Some(&cat), Some("phala/qwen"));
        let mut store = ConfigDoc::parse(Format::Yaml, &g.read(&credentials)).unwrap();
        store
            .set_str(&["refs", "DEEPSEEK_API_KEY"], "sk-user")
            .unwrap();
        g.private(&credentials, &store.render().unwrap());
        g.scan(Agent::Dsh, Some(&cat));
        g.disconnect(Agent::Dsh);
        g.connect(Agent::Dsh, Some(&cat), Some("phala/qwen"));
        g.disconnect(Agent::Dsh);
        scenarios.push(g);
    }
    check("native", scenarios);
}
