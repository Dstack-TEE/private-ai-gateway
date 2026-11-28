# Changelog

## [0.12.0-beta.1](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.11.1...desktop-v0.12.0-beta.1) (2026-11-28)


### Features

* **desktop:** publish signed APT and RPM package repositories ([c755477](https://github.com/Dstack-TEE/private-ai-gateway/commit/c7554773d029d8bb8aea601bf9cbab10ce10a4d8))
* **desktop:** publish signed APT and RPM package repositories ([0964924](https://github.com/Dstack-TEE/private-ai-gateway/commit/0964924a6c2a96a5d25e4a143a076c9548201fcf))
* **desktop:** publish stable Arch User Repository packages ([2e9d32a](https://github.com/Dstack-TEE/private-ai-gateway/commit/2e9d32a8ef8d86cbac0d13e208897f9ee5d17d87))
* **desktop:** publish stable Arch User Repository packages ([a5a2b8a](https://github.com/Dstack-TEE/private-ai-gateway/commit/a5a2b8a2fb342a011263cbb05ee7c53bb4ce4457))
* **desktop:** publish stable WinGet manifests ([1596a0b](https://github.com/Dstack-TEE/private-ai-gateway/commit/1596a0b07ee3cf955bad5afc7870ef0a587dc35b))
* **desktop:** publish stable WinGet manifests ([24f54bd](https://github.com/Dstack-TEE/private-ai-gateway/commit/24f54bdc095a8dfa2237e345402b1b08f268d9b6))


### Bug Fixes

* **desktop:** adapt the settings watcher to notify-debouncer-mini 0.8 ([2bf1c95](https://github.com/Dstack-TEE/private-ai-gateway/commit/2bf1c955d81de97a4af56dbc91a934971a898682))
* **desktop:** apply the client environment without delaying startup ([7ea3670](https://github.com/Dstack-TEE/private-ai-gateway/commit/7ea3670157b12fede19c950126961b6e4eceb854))
* **desktop:** bound silent NSIS CLI registration ([8faad71](https://github.com/Dstack-TEE/private-ai-gateway/commit/8faad71ae706a0035c97993de08b126d710e5890))
* **desktop:** check agent validation overrides across environments ([b9e5f8f](https://github.com/Dstack-TEE/private-ai-gateway/commit/b9e5f8fe3a2e4ac20c7e4da0156c124d5d6895af))
* **desktop:** declare WinGet runtime dependencies and portable scope ([c9d0286](https://github.com/Dstack-TEE/private-ai-gateway/commit/c9d0286151a7b3e6aa4cd0944eb5af14e68f9840))
* **desktop:** declare WinGet runtime dependencies and portable scope ([e2c10a0](https://github.com/Dstack-TEE/private-ai-gateway/commit/e2c10a062b9b5f87f640363cf09511b0b71379bd))
* **desktop:** detect the package manager that owns CLI installs ([f13e98d](https://github.com/Dstack-TEE/private-ai-gateway/commit/f13e98d803993d83db1e12a18d1c228e3ef2ff3e))
* **desktop:** detect the package manager that owns CLI installs ([b33f4de](https://github.com/Dstack-TEE/private-ai-gateway/commit/b33f4deb5e9b83b3cb8b3a0f62998030f866c7a9))
* **desktop:** find OpenCode outside the service PATH ([d9f2aaf](https://github.com/Dstack-TEE/private-ai-gateway/commit/d9f2aaffae8c4e5d2c39b70774618741e828fd80))
* **desktop:** finish NSIS install when CLI registration fails ([d76d7c0](https://github.com/Dstack-TEE/private-ai-gateway/commit/d76d7c03704491cb1e4321d1a9ee7e8152557211))
* **desktop:** honor the Homebrew formula cellar ([aadd4df](https://github.com/Dstack-TEE/private-ai-gateway/commit/aadd4df2724b3efbe82f6af592bd4d679af816b2))
* **desktop:** import the Linux signing key before RPM upgrades ([c4c4f41](https://github.com/Dstack-TEE/private-ai-gateway/commit/c4c4f4134893fb8b9482ecbb5e64dc9d8acc4f52))
* **desktop:** import the signing key before in-app RPM updates ([7f3b60a](https://github.com/Dstack-TEE/private-ai-gateway/commit/7f3b60ae5e93d7f153110c2c6e98976898affdc5))
* **desktop:** isolate package-manager probes to update checks ([4b27d5d](https://github.com/Dstack-TEE/private-ai-gateway/commit/4b27d5de16b17d54478fc1c6299449b910db5466))
* **desktop:** keep APT trust independent of installed packages ([e772364](https://github.com/Dstack-TEE/private-ai-gateway/commit/e77236407d7ec2ed3ad27871307865076f6e7891))
* **desktop:** let test backends bind OS-assigned ports ([83d5f85](https://github.com/Dstack-TEE/private-ai-gateway/commit/83d5f854358322d5ec23b3c67cdfe5cdf2bb948a))
* **desktop:** let test backends bind OS-assigned ports ([dc85b77](https://github.com/Dstack-TEE/private-ai-gateway/commit/dc85b7717a997a21e26e096b6d0099ab4c78b551))
* **desktop:** link the Windows CRT statically ([10c064f](https://github.com/Dstack-TEE/private-ai-gateway/commit/10c064f2e1e6c882a648054d9e36f5ba722b40fc))
* **desktop:** link the Windows CRT statically ([809b2d9](https://github.com/Dstack-TEE/private-ai-gateway/commit/809b2d97c88b85f86314e448d571c49a0d6cb382))
* **desktop:** link to the AUR package from update settings ([f01e927](https://github.com/Dstack-TEE/private-ai-gateway/commit/f01e92732105898b638eca16b5373d6f9d5536f6))
* **desktop:** make Windows helper ACL denial test deterministic ([98ddf97](https://github.com/Dstack-TEE/private-ai-gateway/commit/98ddf97825dfc09e74214f66f6a51c292a8ebb1a))
* **desktop:** pass AUR credentials and support package revisions ([f6aeb2a](https://github.com/Dstack-TEE/private-ai-gateway/commit/f6aeb2a64f35a1e78adc99de71d1393584dd2a3c))
* **desktop:** persist the OpenCode path from the client ([597a7dd](https://github.com/Dstack-TEE/private-ai-gateway/commit/597a7dd9b1de3bd0577be63bd49eee85495cb7d8))
* **desktop:** preserve login preference on package removal ([eab622d](https://github.com/Dstack-TEE/private-ai-gateway/commit/eab622daf7902fa6890c341b5ba157dce8bf7ecb))
* **desktop:** preserve script installations across updates ([c83bcfd](https://github.com/Dstack-TEE/private-ai-gateway/commit/c83bcfd994dfae9e8a9aadc38d7ae5a6776ab6ea))
* **desktop:** reject mismatched CLI agent config paths ([f2961b6](https://github.com/Dstack-TEE/private-ai-gateway/commit/f2961b632513b83d8f1df80635ed53e03ce506ac))
* **desktop:** remove races in shell and web UI tests ([26872ac](https://github.com/Dstack-TEE/private-ai-gateway/commit/26872ace7d93f3c11cd44ac199f88c16235ca618))
* **desktop:** remove races in shell and web UI tests ([2e1793c](https://github.com/Dstack-TEE/private-ai-gateway/commit/2e1793cde1266958a89df4ef92f6baf608905045))
* **desktop:** require a trusted Windows OpenClaw helper owner ([18f682c](https://github.com/Dstack-TEE/private-ai-gateway/commit/18f682cdfc83bba33edb3b9e2779b354f8587633))
* **desktop:** reserve the golden test's refused port ([21a66aa](https://github.com/Dstack-TEE/private-ai-gateway/commit/21a66aa49cdc7175ced78fc3c5acaa5799c4e33b))
* **desktop:** resolve the account shell when SHELL is unset ([8e1fa43](https://github.com/Dstack-TEE/private-ai-gateway/commit/8e1fa437bbb3cca770f73730acceb8fe9ea23deb))
* **desktop:** seed WinGet submission branches through GitHub ([7879a5f](https://github.com/Dstack-TEE/private-ai-gateway/commit/7879a5fad97b7a4952a125af514783f694931fef))
* **desktop:** select the shell resolver on supported platforms ([d553d4b](https://github.com/Dstack-TEE/private-ai-gateway/commit/d553d4b9c0e2dd782666f81b24439d0223f4fee6))
* **desktop:** separate login service state and clarify start hints ([945bae8](https://github.com/Dstack-TEE/private-ai-gateway/commit/945bae8684411baa2be00f617e81eb52d6cf5b49))
* **desktop:** smooth pap first-run setup ([2fd56f9](https://github.com/Dstack-TEE/private-ai-gateway/commit/2fd56f9e1c09e87c84056389a03ee640e9a40c41))
* **desktop:** smooth pap first-run setup ([66ce50a](https://github.com/Dstack-TEE/private-ai-gateway/commit/66ce50a054dbbd8a5349f1f5320e7f120aa6fa96))
* **desktop:** stabilize login service diagnostics ([4847a7d](https://github.com/Dstack-TEE/private-ai-gateway/commit/4847a7de64c05e39cf877470b45484a0a5e15a16))
* **desktop:** submit WinGet manifests from an organization fork ([6477e2c](https://github.com/Dstack-TEE/private-ai-gateway/commit/6477e2cbe267342f344c68bfbb63b6f4964a4ab8))
* **desktop:** update script installs and clean Linux removal ([7c39714](https://github.com/Dstack-TEE/private-ai-gateway/commit/7c39714924aad56b43228d76e6faa382f02a892f))
* **desktop:** update script installs and clean Linux removal ([617b80d](https://github.com/Dstack-TEE/private-ai-gateway/commit/617b80d18be9db9f61d5a40fc838d26adfe3bad8))
* **desktop:** use the client's agent environment ([3224de8](https://github.com/Dstack-TEE/private-ai-gateway/commit/3224de8fc1219ea3662e13814d7f91aa9bf45886))
* **desktop:** use the client's agent environment ([5e8c538](https://github.com/Dstack-TEE/private-ai-gateway/commit/5e8c538b2e948ec785560e5474ff9952458ba8d0))
* **desktop:** use the installed OpenCode config shape ([971aacb](https://github.com/Dstack-TEE/private-ai-gateway/commit/971aacbfd2e0248698f006f094afd69cdc7ae818))
* **desktop:** use the installed OpenCode config shape ([2962281](https://github.com/Dstack-TEE/private-ai-gateway/commit/296228161ba98dbed3fb0b55ede071cb4c91b6b0))
* **desktop:** use the WinGet locale manifest filename ([16ba329](https://github.com/Dstack-TEE/private-ai-gateway/commit/16ba32959c5c7003dfc31c9e92ca7c41c5b507fe))
* **desktop:** use the WinGet locale manifest filename ([7623c83](https://github.com/Dstack-TEE/private-ai-gateway/commit/7623c834c11ce9e3252a20fcea1423ecbf0e387c))
* **desktop:** validate WinGet retries without fetching blobs ([8ea7ea0](https://github.com/Dstack-TEE/private-ai-gateway/commit/8ea7ea012cb34474452265c9b6b8595ec72e0854))
* **desktop:** verify Linux updates with installed release keys ([ecf0357](https://github.com/Dstack-TEE/private-ai-gateway/commit/ecf0357c0348f5de9e1cc73ebc291c2dc69669a6))
* harden uninstall fallback cleanup ([1f04f1c](https://github.com/Dstack-TEE/private-ai-gateway/commit/1f04f1cf167478a84727cbf7af217986740d590f))
* purge PAP providers during uninstall cleanup ([9064b3b](https://github.com/Dstack-TEE/private-ai-gateway/commit/9064b3b52d010f1ef03d5a989f672eddb92826a0))
* purge PAP providers during uninstall cleanup ([b7cd0bb](https://github.com/Dstack-TEE/private-ai-gateway/commit/b7cd0bbeda7ceeaf9f4be0275453e67d091a5a4e))

## [0.11.1](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.11.0...desktop-v0.11.1) (2026-11-26)

Private AI Proxy 0.11.1 configures each coding agent with the reasoning efforts, limits and capabilities RedPill lists for every model.

### Bug Fixes

* **Reasoning efforts follow each model.** Codex, OpenCode 1 and 2, Pi, Oh My Pi, OpenClaw and DeepSeek Harness offer exactly the efforts RedPill lists for a model, with its default effort. GLM-5.3 can now use `max`, and Codex no longer offers only low, medium and high. A model without reasoning metadata keeps each agent's own controls ([#646](https://github.com/Dstack-TEE/private-ai-gateway/pull/646)).
* **Model limits, inputs, tool support and prices come from the model list.** Every agent gets each model's context and output limits, input types and prices in its own fields. Claude Code's Opus, Sonnet, Haiku and Fable aliases, context window and output limit follow the default model; Hermes gets each model's context length and image support ([#646](https://github.com/Dstack-TEE/private-ai-gateway/pull/646)).
* **Agents list only models they can use.** Models without tool support, such as OCR models, and models past their deprecation date are left out of agent pickers and each agent's model list ([#646](https://github.com/Dstack-TEE/private-ai-gateway/pull/646)).
* **Codex works with RedPill models as with a model it does not know.** It gets Codex's own base instructions instead of a GPT model's, its freeform `apply_patch` tool, and tool search for models that take tools ([#646](https://github.com/Dstack-TEE/private-ai-gateway/pull/646)).
* **Tuning Claude Code's model settings keeps the connection.** A changed alias or limit stays until the model list changes it, and disconnecting restores the value from before the connection ([#646](https://github.com/Dstack-TEE/private-ai-gateway/pull/646)).

### Verification

The live end-to-end test passed against RedPill with a Linux package built from #646: Claude Code, Codex, DeepSeek Harness, OpenCode 1 and 2, Pi, Oh My Pi, OpenClaw and Hermes. Choosing efforts was checked live in OpenCode and Codex. The fixes made in review after that run are covered by unit and golden tests; the live test runs again on the published package.

## [0.11.0](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.10.0...desktop-v0.11.0) (2026-11-26)

Private AI Proxy 0.11.0 adds RedPill subscription keys: an account with a RedPill plan can choose whether a profile uses the plan's weekly usage or the credit balance.

### Features

* **Choose Subscription or Balance when connecting RedPill.** When the RedPill account has an active plan, sign-in asks which billing method the profile uses (`pap profiles login --funding subscription|balance` on the CLI) and shows the plan's weekly usage and reset time. Accounts without a plan see no change and get a balance key as before ([#630](https://github.com/Dstack-TEE/private-ai-gateway/pull/630)).
* **Re-login keeps the saved billing method.** Reconnecting a profile without `--funding` keeps its current choice; changing it requires reconnecting RedPill ([#630](https://github.com/Dstack-TEE/private-ai-gateway/pull/630)).

### Compatibility

* Works with RedPill servers that do not offer plans yet: no billing prompt is shown, `--funding balance` retries without the field, and `--funding subscription` fails without saving a profile. Phala sign-in is unchanged; billing method selection is RedPill only.

### Verification

Checked with the CLI and the Linux desktop app (Xvfb) against both a RedPill backend with plans and the current backend without them: interactive and `--no-browser --yes` sign-in, both funding flags, re-login of a 0.10.0 profile after upgrade, and billed requests through the gateway. Not exercised: a signed macOS package and hardware-attested proxy inference.

## [0.10.0](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.9.0...desktop-v0.10.0) (2026-11-25)

Private AI Proxy 0.10.0 keeps protection on across logins, reboots and crashes, and keeps coding agents connected while you change their settings.

### Features

* **Protection survives logins, reboots and crashes.** CLI-only Linux packages, npm and script installs on Linux and macOS, the Linux desktop packages and the macOS app now run the backend under the user's service manager (a systemd user unit or a launchd agent that Private AI Proxy owns). It starts at login, restarts after a crash, and resumes the protection session without any command. The first `pap start` or app launch turns it on and says so; turn it off in Settings or with `pap service disable`. On Windows, Open at Login is turned on once and the app restarts a crashed backend while it runs; the Mac App Store build restarts a crashed backend while the app runs ([#591](https://github.com/Dstack-TEE/private-ai-gateway/pull/591), [#635](https://github.com/Dstack-TEE/private-ai-gateway/pull/635)).
* **`pap agents connect` starts protection.** With protection off it asks to start it (or starts it with `--yes`) instead of saving an inactive connection; `--dry-run` and `--non-interactive` never start anything ([#591](https://github.com/Dstack-TEE/private-ai-gateway/pull/591)).
* **OpenCode 2.** The connection writes OpenCode 2's native `providers` configuration when your file uses it, understands object-form `model` and `policies`, and accepts comments in `opencode.json` ([#631](https://github.com/Dstack-TEE/private-ai-gateway/pull/631), [#637](https://github.com/Dstack-TEE/private-ai-gateway/pull/637)).

### Bug Fixes

* **Changing the default model no longer disconnects an agent.** Picking another model inside Codex, Claude Code, OpenCode, Hermes, OpenClaw, Pi, Oh My Pi or DeepSeek Harness keeps the connection; picking another provider's model is shown as not routed through the proxy instead of revoking access. Claude Code's `/model` choice is now the selection the connection uses ([#631](https://github.com/Dstack-TEE/private-ai-gateway/pull/631)).
* **Agents can no longer be routed around the proxy while shown as connected.** Provider switches (`CLAUDE_CODE_USE_BEDROCK` and similar), OpenCode policies and per-model endpoints, Hermes fallback providers and profiles, Oh My Pi provider filters and OpenClaw per-agent models are checked ([#631](https://github.com/Dstack-TEE/private-ai-gateway/pull/631)).
* **Stop and Disconnect restore what you had.** The original default model comes back unless you chose another provider's model; comments in Codex and OpenCode configs are kept; edits made while protection was off are kept; tokens are revoked only on Disconnect, so a stop and start no longer breaks running agents ([#631](https://github.com/Dstack-TEE/private-ai-gateway/pull/631), [#637](https://github.com/Dstack-TEE/private-ai-gateway/pull/637), [#639](https://github.com/Dstack-TEE/private-ai-gateway/pull/639)).
* **App updates no longer disconnect agents.** Agent configurations reference a helper path that survives updates ([#631](https://github.com/Dstack-TEE/private-ai-gateway/pull/631)).
* **`pap start` says when it cannot keep protection on at login**, naming the environment variables that prevented it ([#634](https://github.com/Dstack-TEE/private-ai-gateway/pull/634)).
* **Gateway verification checks the dstack event log more strictly.** Every RTMR3 runtime event must match the quoted measurements, following dstack's own verifier, for both version 1 and version 2 event logs ([#531](https://github.com/Dstack-TEE/private-ai-gateway/pull/531)).
* **Verifier reads are bounded** for the remaining attestation and artifact responses ([#530](https://github.com/Dstack-TEE/private-ai-gateway/pull/530)).

### Upgrade notes

* If you ran `pap service enable` from the desktop app's CLI on 0.10.0-beta.1, run `pap service disable` before updating, or remove that registration afterwards (see the [CLI distribution guide](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/apps/desktop/docs/cli-distribution.md)).
* Run `pap service disable` before uninstalling a supervised CLI or app.
* A few formatting details of agent files are not kept when connecting, stopping and disconnecting: Claude Code's `env` object, Codex's Windows line endings and a one-line OpenCode `model` object (see [Configuration](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/apps/desktop/docs/configuration.md), [#642](https://github.com/Dstack-TEE/private-ai-gateway/pull/642)).

### Verification

Checked on the published 0.10.0-beta.4: the live end-to-end suite for every supported agent and OpenCode 2; the Mac App Store package validation; and, in Ubuntu 24.04 containers with a real systemd user manager, CLI and desktop packages through first start, `kill -9`, orderly restart, reboot with the app closed, opt-out, stop and disconnect restoration, and upgrade from the previous beta. Not exercised: a signed macOS DMG login item and a Windows reboot outside CI, and the signed in-app updater path.

## [0.10.0-beta.4](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.10.0-beta.3...desktop-v0.10.0-beta.4) (2026-11-25)


### Bug Fixes

* **desktop:** keep comments when editing Codex config ([#639](https://github.com/Dstack-TEE/private-ai-gateway/issues/639)) ([297f38a](https://github.com/Dstack-TEE/private-ai-gateway/commit/297f38a51cfbff2e58f4996e49b55b66de78d4dd))

## [0.10.0-beta.3](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.10.0-beta.2...desktop-v0.10.0-beta.3) (2026-11-25)


### Bug Fixes

* **desktop:** accept comments in OpenCode's opencode.json and keep external model choices ([#637](https://github.com/Dstack-TEE/private-ai-gateway/issues/637)) ([a0626a0](https://github.com/Dstack-TEE/private-ai-gateway/commit/a0626a04779a8fc76f0966f96b5ed9df7d47b2e1))

## [0.10.0-beta.2](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.10.0-beta.1...desktop-v0.10.0-beta.2) (2026-11-25)


### Features

* **desktop:** keep protection on across logins and crashes for desktop app installs ([#635](https://github.com/Dstack-TEE/private-ai-gateway/issues/635)) ([e8a0ca8](https://github.com/Dstack-TEE/private-ai-gateway/commit/e8a0ca8f21408f2fba0820abfde4a0e3c13e3bf2))


### Bug Fixes

* **desktop:** say when pap start skips login supervision ([#634](https://github.com/Dstack-TEE/private-ai-gateway/issues/634)) ([fc3e513](https://github.com/Dstack-TEE/private-ai-gateway/commit/fc3e513d72fbc273ce2f13be43fa7a0c6501d858))

## [0.10.0-beta.1](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.9.0...desktop-v0.10.0-beta.1) (2026-11-24)


### Features

* **desktop:** keep protection on across logins and crashes, and connect in one step ([#591](https://github.com/Dstack-TEE/private-ai-gateway/issues/591)) ([1c97aa4](https://github.com/Dstack-TEE/private-ai-gateway/commit/1c97aa43f7591548b7f31bd7a6f6754d7908a385))


### Bug Fixes

* **desktop:** harden agent routing, provider overrides, and selector recovery ([#631](https://github.com/Dstack-TEE/private-ai-gateway/issues/631)) ([d344817](https://github.com/Dstack-TEE/private-ai-gateway/commit/d344817f278a191199b9176fe4ec7eb49361abfe))
* **verifiers:** bound remaining attestation and artifact reads ([#530](https://github.com/Dstack-TEE/private-ai-gateway/issues/530)) ([6c6076e](https://github.com/Dstack-TEE/private-ai-gateway/commit/6c6076edc86104df7824f818cad7dbc8c04fd6a7))

## [0.9.0](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.8.5...desktop-v0.9.0) (2026-11-18)

Private AI Proxy 0.9.0 keeps protection running through gateway key rotations, network changes and routine commands that used to interrupt it, and adds `pap update` and `pap service restart`.

### ⚠ BREAKING CHANGES

* **`blocked` is gone from the state.** `status` and `protection.phase` in `pap status --json` and the management API no longer report `blocked`; a session that lost verification reports the error states and is retried ([#580](https://github.com/Dstack-TEE/private-ai-gateway/pull/580)).
* **`pap agents connect` fails when it did not route the agent.** With protection off it saves the connection and exits 1, saying the agent still uses its own provider, instead of reporting success ([#590](https://github.com/Dstack-TEE/private-ai-gateway/pull/590)).

### Features

* **`pap update`.** Installs the newer release of the saved update channel for package-manager installations (CLI-only DEB, RPM and Arch packages, the Arch desktop package, npm outside Windows). Connected agents stay on the proxy during the installation, and the installed release resumes protection; if the installation fails, the previous release does ([#581](https://github.com/Dstack-TEE/private-ai-gateway/pull/581), [#586](https://github.com/Dstack-TEE/private-ai-gateway/pull/586), [#592](https://github.com/Dstack-TEE/private-ai-gateway/pull/592), [#594](https://github.com/Dstack-TEE/private-ai-gateway/pull/594)).
* **`pap service restart`.** Restarts the backend and resumes the protection session ([#580](https://github.com/Dstack-TEE/private-ai-gateway/pull/580)).
* **Requests wait out a brief re-verification.** For the first 30 seconds after verification is lost, new requests wait for it instead of failing; after that they are refused with `503` and `Retry-After` ([#583](https://github.com/Dstack-TEE/private-ai-gateway/pull/583), [#594](https://github.com/Dstack-TEE/private-ai-gateway/pull/594)).
* **The model list stays current.** It is read again every 10 minutes while protected and when a request names a model it lacks, and connected agents' configurations follow ([#582](https://github.com/Dstack-TEE/private-ai-gateway/pull/582), [#584](https://github.com/Dstack-TEE/private-ai-gateway/pull/584)).
* **`pap profiles verify --replace-key`.** Replaces a saved API key at a hidden prompt ([#590](https://github.com/Dstack-TEE/private-ai-gateway/pull/590)).

### Bug Fixes

* **A session no longer stays stuck after a gateway key rotation.** After the service's key set changed, protection could stay in "verifying" until `pap stop` and `pap start`. Any lost verification now retires the verifier and is retried with a fresh verification on a backoff ([#580](https://github.com/Dstack-TEE/private-ai-gateway/pull/580)).
* **`pap profiles verify` no longer interrupts protection.** On the profile that is protecting it stopped and restarted the session, which cut requests in flight and returned `gateway_not_verified` meanwhile. It now attests the service again where it runs ([#596](https://github.com/Dstack-TEE/private-ai-gateway/pull/596)).
* **A network address change no longer restarts the session.** Containers, VPNs and temporary IPv6 addresses changed the address list and each change restarted protection. The session is now rebuilt only when the service can no longer be reached ([#588](https://github.com/Dstack-TEE/private-ai-gateway/pull/588), [#593](https://github.com/Dstack-TEE/private-ai-gateway/pull/593), [#594](https://github.com/Dstack-TEE/private-ai-gateway/pull/594)).
* **Agents are not sent to their own provider unasked.** A Local API address change before the first verified model list, and `pap update`, restored connected agents' own configuration for a while; they now stay pointed at the proxy ([#590](https://github.com/Dstack-TEE/private-ai-gateway/pull/590), [#592](https://github.com/Dstack-TEE/private-ai-gateway/pull/592)).
* **Errors say what to do.** A rejected API key is reported as the key saved in the proxy (`service_key_rejected`) rather than as the agent's token; failures to reach the service reach agents in the OpenAI or Anthropic error format; an interrupted request says its delivery is unknown instead of advising a retry; and `pap status`, `pap start` and `pap stop` name the next command when there is no backend, profile or key, or the Local API port is taken ([#587](https://github.com/Dstack-TEE/private-ai-gateway/pull/587), [#590](https://github.com/Dstack-TEE/private-ai-gateway/pull/590)).
* **Refused requests are logged.** The service log records each request the Local API refused or failed, with its client and reason, and why protection restarted ([#588](https://github.com/Dstack-TEE/private-ai-gateway/pull/588)).
* **Models are listed newest first**, then by id, in `GET /v1/models`, the app and agents' pickers ([#585](https://github.com/Dstack-TEE/private-ai-gateway/pull/585)).
* **Web UI sessions stay signed in**, and the web UI gains page basics ([#577](https://github.com/Dstack-TEE/private-ai-gateway/pull/577)).

### Verification

Checked on the published 0.9.0-beta.3 against the live service: the live end-to-end suite for every supported agent; streams kept open across repeated `pap profiles verify` and across address changes; unattended recovery after the service presented a TLS key outside its attested set; a soak across a periodic model list refresh. Not exercised: a gateway key rotation performed by the operator, and manual runs on macOS and Windows beyond their CI jobs.

## [0.9.0-beta.3](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.9.0-beta.2...desktop-v0.9.0-beta.3) (2026-11-18)


### Bug Fixes

* **desktop:** verify the protecting profile where it runs instead of stopping and restarting protection, which cut requests and returned `gateway_not_verified` on every `pap profiles verify` ([b0270b6](https://github.com/Dstack-TEE/private-ai-gateway/commit/b0270b669a145de7c4fc92ac69216412cc3d44d2))

## [0.9.0-beta.2](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.9.0-beta.1...desktop-v0.9.0-beta.2) (2026-11-18)


### Bug Fixes

* **desktop:** exit 1 from `pap profiles verify` when protection does not return ([08400f8](https://github.com/Dstack-TEE/private-ai-gateway/commit/08400f8ef74657ece8fe6f27c5321ceb3f75d2c4))
* **desktop:** make `pap update` hold the installation lock, resume with its own command, and not report a failure while the new backend resumes ([08400f8](https://github.com/Dstack-TEE/private-ai-gateway/commit/08400f8ef74657ece8fe6f27c5321ceb3f75d2c4))
* **desktop:** rebuild a session whose connection a network change left dead instead of leaving requests to time out ([08400f8](https://github.com/Dstack-TEE/private-ai-gateway/commit/08400f8ef74657ece8fe6f27c5321ceb3f75d2c4))
* **desktop:** send a request that waited through a restart instead of refusing it with `credentials_changed` ([08400f8](https://github.com/Dstack-TEE/private-ai-gateway/commit/08400f8ef74657ece8fe6f27c5321ceb3f75d2c4))
* **desktop:** stop making agents wait when a profile is saved with protection off ([08400f8](https://github.com/Dstack-TEE/private-ai-gateway/commit/08400f8ef74657ece8fe6f27c5321ceb3f75d2c4))

## [0.9.0-beta.1](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.8.5...desktop-v0.9.0-beta.1) (2026-11-18)


### ⚠ BREAKING CHANGES

* **desktop:** `status` in the state no longer reports `blocked`, and `protection.phase` no longer reports `blocked`; both report the error states instead.

### Features

* **desktop:** add `pap update` ([#581](https://github.com/Dstack-TEE/private-ai-gateway/issues/581)) ([52690d7](https://github.com/Dstack-TEE/private-ai-gateway/commit/52690d7c81be69702f17459c773fbf7509fc37ab))
* **desktop:** let requests wait out a brief re-verification ([#583](https://github.com/Dstack-TEE/private-ai-gateway/issues/583)) ([9e1a001](https://github.com/Dstack-TEE/private-ai-gateway/commit/9e1a0019cd417d36694ccdefe6c043249eeb8b01))
* **desktop:** read the catalog again for an unknown model ([#584](https://github.com/Dstack-TEE/private-ai-gateway/issues/584)) ([7c7f2c3](https://github.com/Dstack-TEE/private-ai-gateway/commit/7c7f2c3b9ec404d6b905b3948c3397b6bdb94460))
* **desktop:** refresh the model catalog while protected ([#582](https://github.com/Dstack-TEE/private-ai-gateway/issues/582)) ([5923da5](https://github.com/Dstack-TEE/private-ai-gateway/commit/5923da55828036cb16bab5a5a6cc81c68e26f5fb))
* **desktop:** start protection again after `pap update` ([#586](https://github.com/Dstack-TEE/private-ai-gateway/issues/586)) ([4fb2827](https://github.com/Dstack-TEE/private-ai-gateway/commit/4fb282707c8385af3d66d320d80f04f162406341))


### Bug Fixes

* **desktop:** finish the dead ends a user meets from the terminal ([#590](https://github.com/Dstack-TEE/private-ai-gateway/issues/590)) ([bb012b8](https://github.com/Dstack-TEE/private-ai-gateway/commit/bb012b8a45eb14f2ad06494aec2b0376305632e8))
* **desktop:** keep agents routed while `pap update` installs ([#592](https://github.com/Dstack-TEE/private-ai-gateway/issues/592)) ([251ccb4](https://github.com/Dstack-TEE/private-ai-gateway/commit/251ccb44ff233f62b0a5540cd6a8f6d24db9b2e0))
* **desktop:** keep the session when an address changes and the service answers ([#588](https://github.com/Dstack-TEE/private-ai-gateway/issues/588)) ([ef8ee4a](https://github.com/Dstack-TEE/private-ai-gateway/commit/ef8ee4a93881157853a7b2136a2cf8f69e4bdac1))
* **desktop:** keep web UI sessions signed in and add page basics ([#577](https://github.com/Dstack-TEE/private-ai-gateway/issues/577)) ([7584f49](https://github.com/Dstack-TEE/private-ai-gateway/commit/7584f49bd46d18f6c502d0c408f1e8328b611e08))
* **desktop:** list models newest first ([#585](https://github.com/Dstack-TEE/private-ai-gateway/issues/585)) ([c1bfcbe](https://github.com/Dstack-TEE/private-ai-gateway/commit/c1bfcbe93a39d83b7cc35ae3f8dfb63be04c1071))
* **desktop:** recover a blocked session and add `pap service restart` ([#580](https://github.com/Dstack-TEE/private-ai-gateway/issues/580)) ([679fc0f](https://github.com/Dstack-TEE/private-ai-gateway/commit/679fc0fbcfb088e147e037efa4167e41ea77e49b))
* **desktop:** say what to do in the errors users actually hit ([#587](https://github.com/Dstack-TEE/private-ai-gateway/issues/587)) ([5d22578](https://github.com/Dstack-TEE/private-ai-gateway/commit/5d22578e0908a178cd2bd210c511fdcaffda59ef))

## [0.8.5](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.8.4...desktop-v0.8.5) (2026-11-14)

Private AI Proxy 0.8.5 fixes the RedPill account and top-up links and updates dependencies.

### Bug Fixes

* **RedPill account links.** RedPill moved organization pages under `/orgs/{slug}`, so **Manage account** and **Top up** opened pages that returned 404. They now open `https://redpill.ai/orgs/{slug}` and `https://redpill.ai/orgs/{slug}/credits` ([#556](https://github.com/Dstack-TEE/private-ai-gateway/pull/556)).

### Maintenance

* Update desktop npm dependencies ([#551](https://github.com/Dstack-TEE/private-ai-gateway/pull/551), [#566](https://github.com/Dstack-TEE/private-ai-gateway/pull/566)).
* Update desktop Rust dependencies, including zbus 6 and dcap-qvl 0.7.1 ([#552](https://github.com/Dstack-TEE/private-ai-gateway/pull/552), [#553](https://github.com/Dstack-TEE/private-ai-gateway/pull/553), [#570](https://github.com/Dstack-TEE/private-ai-gateway/pull/570)).

## [0.8.4](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.8.3...desktop-v0.8.4) (2026-11-05)

Private AI Proxy 0.8.4 adds image generation and audio transcription examples to the Local API examples dialog.

### Features

* **Images and Transcription examples.** The Local API examples dialog has Images and Transcription tabs next to Chat and Decisions, with cURL, Python and JavaScript code using the OpenAI SDK. Image examples save the returned image with the right extension (PNG or JPEG); transcription examples upload `audio.wav`. A tab is disabled when the verified catalog has no model of that kind ([#541](https://github.com/Dstack-TEE/private-ai-gateway/pull/541)).
* **Tab icons.** Each example tab shows an icon for its kind (text, image, microphone, decisions), hidden in very narrow windows ([#543](https://github.com/Dstack-TEE/private-ai-gateway/pull/543)).

### Maintenance

* macOS packaging waits for Finder to list the DMG and stops retrying after it unmounts ([#539](https://github.com/Dstack-TEE/private-ai-gateway/pull/539)).

## [0.8.3](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.8.0...desktop-v0.8.3) (2026-11-05)

Private AI Proxy 0.8.3 shows every model the verified gateway lists and ships smaller binaries. Changes since stable 0.8.0; 0.8.1 and 0.8.2 were not published.

### Bug Fixes

* **New models appear right away.** On the RedPill and Phala presets, models missing from a hand-maintained compatibility list were hidden, so newly launched models such as `deepseek/deepseek-v4.1-flash-1017` never showed up in the app, the Local API model list, or agent pickers. The verified live catalog is now the only model list. Models that advertise no text output (decision, image, or transcription models) still stay out of coding agents ([#532](https://github.com/Dstack-TEE/private-ai-gateway/pull/532)).

### Performance Improvements

* **Smaller binaries.** Release builds use link-time optimization and strip symbols: the CLI shrinks by about 40% and the background service by about a third. The desktop shell drops a duplicate D-Bus library and the clipboard plugin; copying, including keeping secrets out of clipboard history, works as before ([#534](https://github.com/Dstack-TEE/private-ai-gateway/pull/534)).

### Maintenance

* macOS packages and the Mac App Store build are built on macOS 27 with Xcode 27.1 ([#534](https://github.com/Dstack-TEE/private-ai-gateway/pull/534)).
* Universal macOS binaries are checked with `lipo -archs`, which Xcode 27 requires ([#537](https://github.com/Dstack-TEE/private-ai-gateway/pull/537)).
* The macOS package check retries unmounting the DMG while Finder releases it ([#536](https://github.com/Dstack-TEE/private-ai-gateway/pull/536)).
* Update desktop npm dependencies ([#529](https://github.com/Dstack-TEE/private-ai-gateway/pull/529)).

## [0.8.0](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.8.0-beta.1...desktop-v0.8.0) (2026-11-03)

Private AI Proxy 0.8.0 relays image generation and audio transcription through the verified Local API, and refuses constrained requests it can't check. Changes since stable 0.7.0.

### ⚠ BREAKING CHANGES

* **Constrained requests must be JSON.** `pap serve` and `@phala/aci-verifier` refuse a constrained request whose body isn't sent as JSON. `curl -d` and `fetch` with a string body must set `Content-Type: application/json` ([#517](https://github.com/Dstack-TEE/private-ai-gateway/pull/517)).

### Features

* **Image generation.** The Local API relays `POST /v1/images/generations` to the verified gateway, with receipts like other requests ([#493](https://github.com/Dstack-TEE/private-ai-gateway/pull/493)).
* **Audio transcription.** The Local API relays `POST /v1/audio/transcriptions` multipart uploads. Provider constraints travel in the `X-ACI-Provider` header, and only to gateways that advertise multipart support ([#517](https://github.com/Dstack-TEE/private-ai-gateway/pull/517)).

### Bug Fixes

* **Unverifiable bodies are refused.** A constrained request with a non-JSON body (other than a supported multipart upload) is refused locally instead of being forwarded unchecked ([#517](https://github.com/Dstack-TEE/private-ai-gateway/pull/517)).

### Maintenance

* Simplify keyset-renewal state and its test seams, with no behavior change ([#520](https://github.com/Dstack-TEE/private-ai-gateway/pull/520)).
* Update desktop npm and Rust dependencies ([#505](https://github.com/Dstack-TEE/private-ai-gateway/pull/505), [#509](https://github.com/Dstack-TEE/private-ai-gateway/pull/509)).

## [0.8.0-beta.1](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.7.0...desktop-v0.8.0-beta.1) (2026-10-29)


### ⚠ BREAKING CHANGES

* **desktop:** pap serve and @phala/aci-verifier refuse constrained JSON requests without a JSON content type (curl -d / fetch string bodies must set Content-Type: application/json).

### Features

* **desktop:** fail closed on non-JSON constrained bodies and relay audio transcription ([#517](https://github.com/Dstack-TEE/private-ai-gateway/issues/517)) ([673efd0](https://github.com/Dstack-TEE/private-ai-gateway/commit/673efd09c2571f08049d180fa39f5e6b59462ba2))
* **desktop:** relay image generation through the Local API ([#493](https://github.com/Dstack-TEE/private-ai-gateway/issues/493)) ([dd333b2](https://github.com/Dstack-TEE/private-ai-gateway/commit/dd333b24324a96468bc19137fbcf6c45aeecc366))

## [0.7.0](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.7.0-beta.1...desktop-v0.7.0) (2026-10-28)

Changes since stable 0.6.2, including both intervening betas and the reviewed #510 cleanup.

### Features

* **cli:** Add optional `--max-tokens <N>` to `pap send` to bound generated output. Values must be positive integers; omitting the flag preserves the previous request behavior ([#500](https://github.com/Dstack-TEE/private-ai-gateway/pull/500)).

### Bug Fixes

* **desktop:** Keep transport, the delivery gate and the model catalog across same-family gateway keyset renewal, and recover blocked sessions after successful verification ([#458](https://github.com/Dstack-TEE/private-ai-gateway/pull/458)).
* **aci:** Audit receipts only against exactly attested members. Re-attest an unseen member without repeating inference; a same-family digest alone does not authorize a receipt ([#458](https://github.com/Dstack-TEE/private-ai-gateway/pull/458)).
* **aci:** After re-verification accepts a different identity family, refuse the pending request with a retryable 503 so callers can retry after verification recovers. Unknown digest hints require fresh verification ([#458](https://github.com/Dstack-TEE/private-ai-gateway/pull/458)).
* **aci:** Require integer receipt `served_at < not_after` and enforce the attested-session boundary `served_at < expires_at`. Historical receipts remain auditable at their serving time ([#458](https://github.com/Dstack-TEE/private-ai-gateway/pull/458), [#456](https://github.com/Dstack-TEE/private-ai-gateway/pull/456)).

### Compatibility

* **Lifecycle consumers:** `pap serve --json-events` adds the `identity_renewed` event for same-family renewal. Rust consumers must handle the new `VerifierEvent::IdentityRenewed { identity }` variant in exhaustive matches ([#458](https://github.com/Dstack-TEE/private-ai-gateway/pull/458)).
* **Rust source consumers:** Receipt custody verification now returns structured authenticated public identities. Replace the removed `verify_dstack_kms_receipt_chain(...)` call with `verify_dstack_kms_receipt_custody(...)?.root_public_key` when only the root is needed, and continue applying the accepted-root policy. The #510 cleanup preserves session/receipt wire formats and the reviewed acceptance policy ([#456](https://github.com/Dstack-TEE/private-ai-gateway/pull/456), [#510](https://github.com/Dstack-TEE/private-ai-gateway/pull/510)).

### Maintenance

* Simplify shared dstack custody verification and include the reviewed stable-session cleanup ([#510](https://github.com/Dstack-TEE/private-ai-gateway/pull/510)).
* Update desktop npm and Rust dependencies, including zbus 6 ([#483](https://github.com/Dstack-TEE/private-ai-gateway/pull/483), [#484](https://github.com/Dstack-TEE/private-ai-gateway/pull/484), [#488](https://github.com/Dstack-TEE/private-ai-gateway/pull/488)).
* Correct the documented Phala API-key dashboard path ([#490](https://github.com/Dstack-TEE/private-ai-gateway/pull/490)).
* Improve live-E2E diagnostics for hung/degenerate models and count only outage-period forwarding during fail-closed checks ([#486](https://github.com/Dstack-TEE/private-ai-gateway/pull/486), [#492](https://github.com/Dstack-TEE/private-ai-gateway/pull/492)).

## [0.7.0-beta.1](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.6.3-beta.1...desktop-v0.7.0-beta.1) (2026-10-27)


### Features

* **aggregator:** stable attested sessions across re-verification ([#403](https://github.com/Dstack-TEE/private-ai-gateway/issues/403)) ([#456](https://github.com/Dstack-TEE/private-ai-gateway/issues/456)) ([da7df70](https://github.com/Dstack-TEE/private-ai-gateway/commit/da7df70cd5a412ea58b69df3456f58c1db72b006))
* **cli:** add --max-tokens to pap aci send ([#500](https://github.com/Dstack-TEE/private-ai-gateway/issues/500)) ([6475d5c](https://github.com/Dstack-TEE/private-ai-gateway/commit/6475d5c6b63c8b25cdc492f52b624270c328ed81))

## [0.6.3-beta.1](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.6.2...desktop-v0.6.3-beta.1) (2026-10-27)


### Bug Fixes

* **aci:** keep client sessions working across keyset renewal ([#458](https://github.com/Dstack-TEE/private-ai-gateway/issues/458)) ([4c12bf6](https://github.com/Dstack-TEE/private-ai-gateway/commit/4c12bf642904eff55c1aee175211f5059c22ca61))

## [0.6.2](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.6.1...desktop-v0.6.2) (2026-10-21)


### Bug Fixes

* **desktop:** connect DeepSeek Harness 0.2.1, whose headless bundle disables the credentials service ([#478](https://github.com/Dstack-TEE/private-ai-gateway/issues/478)) ([2154f50](https://github.com/Dstack-TEE/private-ai-gateway/commit/2154f50cbbfefc44f2415a52389e3a20bf901054))
* **desktop:** keep Claude Code before 2.1.299 from sending the local agent token with feature-flag requests to api.anthropic.com ([#478](https://github.com/Dstack-TEE/private-ai-gateway/issues/478)) ([2154f50](https://github.com/Dstack-TEE/private-ai-gateway/commit/2154f50cbbfefc44f2415a52389e3a20bf901054))
* **desktop:** restore a setting the user already had when an agent disconnects ([#478](https://github.com/Dstack-TEE/private-ai-gateway/issues/478)) ([2154f50](https://github.com/Dstack-TEE/private-ai-gateway/commit/2154f50cbbfefc44f2415a52389e3a20bf901054))


### Miscellaneous Chores

* **desktop:** release 0.6.2 ([#479](https://github.com/Dstack-TEE/private-ai-gateway/issues/479)) ([0dfd36e](https://github.com/Dstack-TEE/private-ai-gateway/commit/0dfd36e8e407380f16b67849d9475e33ca64788d))

## [0.6.1](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.6.0...desktop-v0.6.1) (2026-10-21)


### Bug Fixes

* **cli:** harden pap verification and output paths ([#461](https://github.com/Dstack-TEE/private-ai-gateway/issues/461)) ([d5341af](https://github.com/Dstack-TEE/private-ai-gateway/commit/d5341affa189e3824442426e75bff826df21743a))
* **cli:** polish pap's interface and harden standalone commands ([#469](https://github.com/Dstack-TEE/private-ai-gateway/issues/469)) ([64b2ded](https://github.com/Dstack-TEE/private-ai-gateway/commit/64b2dede59c7c47a662cd6a511d5a9af129471bf))
* **core:** recover a lost management endpoint and harden private files ([#468](https://github.com/Dstack-TEE/private-ai-gateway/issues/468)) ([752c820](https://github.com/Dstack-TEE/private-ai-gateway/commit/752c820ce92684b342e728a3bc121d5e4cd790d5))
* **desktop:** keep the client reachable and its state current ([#460](https://github.com/Dstack-TEE/private-ai-gateway/issues/460)) ([389ba2f](https://github.com/Dstack-TEE/private-ai-gateway/commit/389ba2fa523c350d08b9cfc12bfc0ff7856424eb))
* **desktop:** open target=_blank links in the browser ([#472](https://github.com/Dstack-TEE/private-ai-gateway/issues/472)) ([2201348](https://github.com/Dstack-TEE/private-ai-gateway/commit/2201348660fce875e245c7b11d81dadceec35646))
* **desktop:** polish native shell behavior ([#470](https://github.com/Dstack-TEE/private-ai-gateway/issues/470)) ([976b924](https://github.com/Dstack-TEE/private-ai-gateway/commit/976b92422541f2648c5efac286c3be85e84fbfb3))
* **desktop:** polish UI readability, forms and accessibility ([#471](https://github.com/Dstack-TEE/private-ai-gateway/issues/471)) ([1594248](https://github.com/Dstack-TEE/private-ai-gateway/commit/159424810139c7dbce603eb063c3a086f603cedc))


### Miscellaneous Chores

* **desktop:** release 0.6.1 ([#474](https://github.com/Dstack-TEE/private-ai-gateway/issues/474)) ([2203435](https://github.com/Dstack-TEE/private-ai-gateway/commit/22034350268417cf2894b277cecc2cab02f5e013))

## [0.6.1-beta.1](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.6.0...desktop-v0.6.1-beta.1) (2026-10-21)


### Bug Fixes

* **cli:** harden pap verification and output paths ([#461](https://github.com/Dstack-TEE/private-ai-gateway/issues/461)) ([d5341af](https://github.com/Dstack-TEE/private-ai-gateway/commit/d5341affa189e3824442426e75bff826df21743a))
* **cli:** polish pap's interface and harden standalone commands ([#469](https://github.com/Dstack-TEE/private-ai-gateway/issues/469)) ([64b2ded](https://github.com/Dstack-TEE/private-ai-gateway/commit/64b2dede59c7c47a662cd6a511d5a9af129471bf))
* **core:** recover a lost management endpoint and harden private files ([#468](https://github.com/Dstack-TEE/private-ai-gateway/issues/468)) ([752c820](https://github.com/Dstack-TEE/private-ai-gateway/commit/752c820ce92684b342e728a3bc121d5e4cd790d5))
* **desktop:** keep the client reachable and its state current ([#460](https://github.com/Dstack-TEE/private-ai-gateway/issues/460)) ([389ba2f](https://github.com/Dstack-TEE/private-ai-gateway/commit/389ba2fa523c350d08b9cfc12bfc0ff7856424eb))
* **desktop:** open target=_blank links in the browser ([#472](https://github.com/Dstack-TEE/private-ai-gateway/issues/472)) ([2201348](https://github.com/Dstack-TEE/private-ai-gateway/commit/2201348660fce875e245c7b11d81dadceec35646))
* **desktop:** polish native shell behavior ([#470](https://github.com/Dstack-TEE/private-ai-gateway/issues/470)) ([976b924](https://github.com/Dstack-TEE/private-ai-gateway/commit/976b92422541f2648c5efac286c3be85e84fbfb3))
* **desktop:** polish UI readability, forms and accessibility ([#471](https://github.com/Dstack-TEE/private-ai-gateway/issues/471)) ([1594248](https://github.com/Dstack-TEE/private-ai-gateway/commit/159424810139c7dbce603eb063c3a086f603cedc))

## [0.6.0](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.5.1...desktop-v0.6.0) (2026-10-16)


### Features

* **desktop:** release Mac App Store versions automatically after approval ([#448](https://github.com/Dstack-TEE/private-ai-gateway/issues/448)) ([e20027c](https://github.com/Dstack-TEE/private-ai-gateway/commit/e20027cf8abd5c2b024707accf8e2900d9ad121a))
* **desktop:** remove the production OS policy ([#446](https://github.com/Dstack-TEE/private-ai-gateway/issues/446)) ([493cdc3](https://github.com/Dstack-TEE/private-ai-gateway/commit/493cdc3b3ef7b1b9a6f5390ba3ece0e8004c34f2))


### Miscellaneous Chores

* **desktop:** release 0.6.0 ([#450](https://github.com/Dstack-TEE/private-ai-gateway/issues/450)) ([d2b7f10](https://github.com/Dstack-TEE/private-ai-gateway/commit/d2b7f10ffb54f6b90552f17ef4adce01690f4ed8))

## [0.5.1](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.5.0...desktop-v0.5.1) (2026-10-14)


### Bug Fixes

* **ci:** include beta changes in App Store What's New ([#435](https://github.com/Dstack-TEE/private-ai-gateway/issues/435)) ([3d5aaa0](https://github.com/Dstack-TEE/private-ai-gateway/commit/3d5aaa06e2d534a37c82a850e2d9c47b21ce7b11))
* **desktop:** remove Intel PCS collateral fallback ([#437](https://github.com/Dstack-TEE/private-ai-gateway/issues/437)) ([45b1273](https://github.com/Dstack-TEE/private-ai-gateway/commit/45b1273083d4043680735ab7c2f5111385dc6d90))


### Miscellaneous Chores

* **desktop:** release 0.5.1 ([#438](https://github.com/Dstack-TEE/private-ai-gateway/issues/438)) ([b3b17af](https://github.com/Dstack-TEE/private-ai-gateway/commit/b3b17af3880c6a3be94185ed089439c94ef6bd89))

## [0.5.0](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.5.0-beta.4...desktop-v0.5.0) (2026-10-14)


### Bug Fixes

* **desktop:** clarify status during failed verification retries ([#432](https://github.com/Dstack-TEE/private-ai-gateway/issues/432)) ([1b8286f](https://github.com/Dstack-TEE/private-ai-gateway/commit/1b8286f3e796eea5a05f6dcc083be9ad5c9628a2))


### Miscellaneous Chores

* **desktop:** release 0.5.0 ([#434](https://github.com/Dstack-TEE/private-ai-gateway/issues/434)) ([2efdeda](https://github.com/Dstack-TEE/private-ai-gateway/commit/2efdeda11954706f6a9458f130124d6c4df4aed4))

## [0.5.0-beta.4](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.5.0-beta.3...desktop-v0.5.0-beta.4) (2026-10-14)


### Bug Fixes

* **desktop:** fetch DCAP collateral reliably and explain why it failed ([#428](https://github.com/Dstack-TEE/private-ai-gateway/issues/428)) ([1c48f3c](https://github.com/Dstack-TEE/private-ai-gateway/commit/1c48f3c98ef276db2dfa2dc4501ef1df353af6ac))

## [0.5.0-beta.3](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.5.0-beta.2...desktop-v0.5.0-beta.3) (2026-10-14)


### Features

* **desktop:** highlight the Local API examples with Shiki ([#421](https://github.com/Dstack-TEE/private-ai-gateway/issues/421)) ([3ba2c50](https://github.com/Dstack-TEE/private-ai-gateway/commit/3ba2c5030456423d80c86527b6ebd21f314cf816))
* **desktop:** use the TypeSafe SDKs in the Decisions examples ([#420](https://github.com/Dstack-TEE/private-ai-gateway/issues/420)) ([442a362](https://github.com/Dstack-TEE/private-ai-gateway/commit/442a3620e6f1397d293e1a460337e88b82b9d712))


### Bug Fixes

* **desktop:** explain service verification failures, still fail closed ([#425](https://github.com/Dstack-TEE/private-ai-gateway/issues/425)) ([8a8ce77](https://github.com/Dstack-TEE/private-ai-gateway/commit/8a8ce77d39f6c0b434141bd91c42de1d76cd4073))

## [0.5.0-beta.2](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.5.0-beta.1...desktop-v0.5.0-beta.2) (2026-10-13)


### Features

* **ci:** add guarded Mac App Store review workflow ([#418](https://github.com/Dstack-TEE/private-ai-gateway/issues/418)) ([d6a7fcc](https://github.com/Dstack-TEE/private-ai-gateway/commit/d6a7fcc16e2dc8626f7e70f7536edd19cffbbe1f))
* **desktop:** add a Decisions tab to the Local API examples ([#415](https://github.com/Dstack-TEE/private-ai-gateway/issues/415)) ([4a562b7](https://github.com/Dstack-TEE/private-ai-gateway/commit/4a562b7e001dca6086c2d8da16b3a41d6caa6fa6))


### Bug Fixes

* **desktop-e2e:** install Oh My Pi from npm ([#419](https://github.com/Dstack-TEE/private-ai-gateway/issues/419)) ([0337126](https://github.com/Dstack-TEE/private-ai-gateway/commit/03371260b10e98c1032667b65e7d8df10c0d70c6))

## [0.5.0-beta.1](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.4.0...desktop-v0.5.0-beta.1) (2026-10-13)


### Features

* **desktop:** relay System One through local API ([#411](https://github.com/Dstack-TEE/private-ai-gateway/issues/411)) ([ee735ac](https://github.com/Dstack-TEE/private-ai-gateway/commit/ee735ac64fcb701fe7ad707cff424ec220f339ba))


### Bug Fixes

* **desktop:** skip non-text models in endpoint probes ([#413](https://github.com/Dstack-TEE/private-ai-gateway/issues/413)) ([f3c2d06](https://github.com/Dstack-TEE/private-ai-gateway/commit/f3c2d06e7a8f47dda7eff9263bf624161a4b2c4c))

## [0.4.0](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.3.0...desktop-v0.4.0) (2026-10-07)

Private AI Proxy 0.4.0 signs in to RedPill with a device code, the way Phala sign-in already works.

### ⚠ BREAKING CHANGES

* **`pap profiles login --callback-stdin` is removed.** Run `pap profiles login` (add `--no-browser` on a remote machine) and approve the code it prints.

### Features

* **RedPill sign-in with a device code.** The app, the web UI and `pap profiles login` show a link and a short code. Approve it in any browser, even on another device. Nothing listens on a local port, so signing in to a remote or headless service no longer needs callback forwarding or a pasted link. The **Paste callback link** form is gone.

### Bug Fixes

* **Release notes.** Release pages list the installers, the standalone CLI archives and the integrity details again.

## [0.4.0-beta.1](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.3.0...desktop-v0.4.0-beta.1) (2026-10-07)


### ⚠ BREAKING CHANGES

* **desktop:** `pap profiles login --callback-stdin` is removed; approve the device code that `pap profiles login` prints instead.

### Features

* **desktop:** sign in to RedPill with a device code ([#395](https://github.com/Dstack-TEE/private-ai-gateway/issues/395)) ([5a0311e](https://github.com/Dstack-TEE/private-ai-gateway/commit/5a0311e684b9d2da52265f19abe46921bab43ed5))


### Bug Fixes

* **desktop:** release notes list the downloads again ([#380](https://github.com/Dstack-TEE/private-ai-gateway/issues/380)) ([06a6b20](https://github.com/Dstack-TEE/private-ai-gateway/commit/06a6b208416392e1bde3075a54fd4071f8c13246))

## [0.3.0](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.2.1...desktop-v0.3.0) (2026-09-29)

Private AI Proxy 0.3.0 adds DeepSeek Harness and makes the whole app leaner, with the same look and behaviour.

### Features

* **DeepSeek Harness (`dsh`).** Connect it with one switch like the other agents. New dsh sessions (web, desktop, `dsh headless` and editor sessions through `dsh acp`) use the verified Local API. DeepSeek's own web search is turned off while connected so search queries don't bypass the proxy. Disconnecting restores your dsh configuration byte for byte. The `sdk` and `sdk-minimal` profiles pick their provider in code and are not covered.

### Bug Fixes

* **Your config formatting is kept.** JSON settings for Claude Code, OpenCode and Pi are now edited in place, so indentation and spacing you chose survive connect and disconnect. A settings file with duplicate keys is refused instead of silently losing one.
* **Tray.** Agents no longer show a check mark before their status has loaded.
* **CLI.** `pap verify --nonce` values are URL-encoded, so a nonce with `&` or `#` no longer changes the request.
* **Local API example.** The copy-paste `curl` example shows the endpoint unquoted and the key in single quotes.

### Internal

* About 1,100 lines of product code removed across the backend, CLI, native shell, renderer, agent adapters and release tooling. Every config-like format is read and written through a library.
* The nightly live end-to-end test now covers all eight agents, OpenCode 2 and a real network outage.

### Upgrade notes

* A Local API address written in a non-canonical form (port 80, or an IPv6 address that isn't compressed) is rewritten once in canonical form after upgrading. Connections keep working.

## [0.3.0-beta.1](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.2.1...desktop-v0.3.0-beta.1) (2026-09-29)


### Features

* **desktop:** support DeepSeek Harness ([#368](https://github.com/Dstack-TEE/private-ai-gateway/issues/368)) ([df032ed](https://github.com/Dstack-TEE/private-ai-gateway/commit/df032ed046dfdbd558d54500d3c5466de946d667))

## [0.2.1](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.2.0...desktop-v0.2.1) (2026-09-28)

Private AI Proxy 0.2.1 hardens OpenCode 2 support, restores agents when the app is uninstalled, and cleans up the app's internals with no visible change.

### Bug Fixes

* **OpenCode 2.** OpenCode 2 reads the configuration Private AI Proxy writes, so connecting works as before. A native OpenCode 2 `providers.private-ai-proxy` entry could override the proxy's endpoint while the app still showed Connected; it is now reported as a conflict and the connection stays closed.
* **Uninstall restores agents.** `pap stop --offline` ends the protection session and restores every agent's own configuration without a running backend. The Windows uninstaller runs it (never during in-app updates); if the restore fails it explains why and lets you keep the restore data. On macOS and Linux, choose Stop All and Quit, or run `pap stop --offline`, before removing the app.
* **Reduce Motion.** With Reduce Motion on, every animation on the protection card stops.

### Internal

* The UI API request and response types are generated from the Rust definitions, so a mismatch fails the build.
* Renderer rows and notices are composed from the standard components; every screen renders the same as 0.2.0.
* A nightly live end-to-end test covers all seven agents, OpenCode 2, and a real network outage.

## [0.2.0](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.1.6...desktop-v0.2.0) (2026-09-28)

Private AI Proxy 0.2.0 is the first stable release since 0.1.6. It rebuilds the app around one local API, plain settings files and a lean, native-feeling window.

### Highlights

* **Settings you can read and edit.** `~/.config/private-ai-proxy/config.toml` holds settings and `credentials.toml` (mode 0600) holds API keys and the web UI password. The OS keychain is no longer used. On macOS App Store builds the files stay in the app container. `pap settings` edits them and keeps your comments.
* **Fail closed, everywhere.** Agents stay pointed at the proxy when verification fails, the network drops, or the app restarts for an update; the proxy refuses requests until the service verifies again. Only an explicit stop, disconnect, Stop All and Quit, or Reset settings restores an agent's original configuration.
* **Agents.** Codex 0.157.1 (TEE models only in `/model`; the app offers to stop Codex's background service so new settings apply), Claude Code 2.1.242+, OpenCode, Pi, Oh My Pi, OpenClaw and Hermes, with install-independent detection. Proxied models show as `name [TEE]`.
* **Receipts.** Every delivered response is audited after delivery; request details show the raw signed receipt, and a failed audit names the check that failed.
* **Web UI.** One setting, a generated rotatable password, and cookie sessions.
* **Clear answers.** A refusal says why: requests during an outage get `503 gateway_not_verified` and resume on their own, a missing key names the Local API key, and busy, unreachable or unavailable services are named.
* **Native feel.** Errors appear in a dialog, background events as system notifications; no web-style toasts. Dialogs keep a stable height and scroll inside; the default window fits every page.
* **CLI.** `pap curl` sends one request pinned to the verified TLS key; `--json` failures report the backend's error code in `error.code`.
* **Distribution.** Per-channel update feeds, npm platform binaries as versions of one package, Linux packages without maintainer scripts, SBOMs and build attestations for every release.

### Upgrade notes

* 0.1 settings and keychain credentials are imported once on first launch, then the keychain entries are removed. Don't sync `credentials.toml` to a public dotfiles repository.
* If the keychain can't be read during that import, the import is not retried; profiles that lost their key ask you to sign in again.
* The management API is HTTP on a new local endpoint; 0.1 `pap` clients cannot manage a 0.2 backend. `pap status --json` reports `backend.apiVersion`.
* Receipt audits report after delivery and never block a response.
* "Production OS image" is the service-reported RTMR3 `os-image-hash`; it is not bound to MRTD/RTMR0–2.
* Before uninstalling, use Stop All and Quit (or `pap service stop`) so agents are restored.
* Windows builds are not Authenticode-signed yet; in-app updates are signature-checked.

## [0.2.0-beta.11](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.2.0-beta.10...desktop-v0.2.0-beta.11) (2026-09-27)


### Bug Fixes

* **desktop:** fail closed on verification failure and harden the 0.1 upgrade path ([#347](https://github.com/Dstack-TEE/private-ai-gateway/issues/347)) ([f31fc7f](https://github.com/Dstack-TEE/private-ai-gateway/commit/f31fc7f009348b0d8e58da0cfe9e7a8ef1a6cf33))
* **desktop:** show failure reasons and report errors in dialogs ([#346](https://github.com/Dstack-TEE/private-ai-gateway/issues/346)) ([9402c4c](https://github.com/Dstack-TEE/private-ai-gateway/commit/9402c4cbe7b16636231cf487b9f7068bbff6af0b))
* **desktop:** wait for the api.sock endpoint in the App Store smoke test ([#348](https://github.com/Dstack-TEE/private-ai-gateway/issues/348)) ([89f7e2d](https://github.com/Dstack-TEE/private-ai-gateway/commit/89f7e2dc51a57d21f6f91995eb86b8e1d7129a3e))

## [0.2.0-beta.10](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.2.0-beta.9...desktop-v0.2.0-beta.10) (2026-09-27)


### Features

* **desktop:** offer to restart Codex's background service after connecting ([#342](https://github.com/Dstack-TEE/private-ai-gateway/issues/342)) ([74685e8](https://github.com/Dstack-TEE/private-ai-gateway/commit/74685e82b5f38df024d2271838764577d140b227))


### Bug Fixes

* **desktop:** native feedback instead of web toasts ([#340](https://github.com/Dstack-TEE/private-ai-gateway/issues/340)) ([eb200d4](https://github.com/Dstack-TEE/private-ai-gateway/commit/eb200d434ad132aea87ecf330f32fafef943aed9))
* **desktop:** support the Codex 0.157.1 baseline and explain its background server ([#339](https://github.com/Dstack-TEE/private-ai-gateway/issues/339)) ([d9d3f58](https://github.com/Dstack-TEE/private-ai-gateway/commit/d9d3f58dd849efb63feceabce6c0c33df16dafd6))

## [0.2.0-beta.9](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.2.0-beta.8...desktop-v0.2.0-beta.9) (2026-09-27)


### Features

* **desktop:** keep settings in ~/.config/private-ai-proxy on every platform ([#334](https://github.com/Dstack-TEE/private-ai-gateway/issues/334)) ([f0074a7](https://github.com/Dstack-TEE/private-ai-gateway/commit/f0074a7ebf7674b21f2304d3c41ac4da1f149479))


### Bug Fixes

* **desktop:** center the update button; align the update flow with the updater docs ([#335](https://github.com/Dstack-TEE/private-ai-gateway/issues/335)) ([86d5b46](https://github.com/Dstack-TEE/private-ai-gateway/commit/86d5b465f0ce7b4265f103e837146891fa240917))

## [0.2.0-beta.8](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.2.0-beta.7...desktop-v0.2.0-beta.8) (2026-09-27)


### Bug Fixes

* **desktop:** restore grouped lists and fit the default window ([#330](https://github.com/Dstack-TEE/private-ai-gateway/issues/330)) ([f7ea7e0](https://github.com/Dstack-TEE/private-ai-gateway/commit/f7ea7e09110683ae27f1b02116ea07b7e1b1937e))

## [0.2.0-beta.7](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.2.0-beta.6...desktop-v0.2.0-beta.7) (2026-09-26)


### Bug Fixes

* **desktop:** final audit fixes for beta.6 ([#325](https://github.com/Dstack-TEE/private-ai-gateway/issues/325)) ([6061f60](https://github.com/Dstack-TEE/private-ai-gateway/commit/6061f601f7294bd6686e670ad10222e0b87ef8f0))
* **desktop:** restore the shadcn default styles ([#324](https://github.com/Dstack-TEE/private-ai-gateway/issues/324)) ([720292f](https://github.com/Dstack-TEE/private-ai-gateway/commit/720292f21f1bd9a38bb0d52cb4fbb8faa618a476))
* **desktop:** show a signed receipt that is not valid JSON as returned ([1db5247](https://github.com/Dstack-TEE/private-ai-gateway/commit/1db52476442485c37cfe74c11d811ea08be6c243))
* **desktop:** starting or stopping protection can no longer run twice from a double click ([6457a68](https://github.com/Dstack-TEE/private-ai-gateway/commit/6457a68466f645266c92ffede730b7dea88078c4))

## [0.2.0-beta.6](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.2.0-beta.5...desktop-v0.2.0-beta.6) (2026-09-26)


### Features

* **desktop:** add pap curl for one request pinned to the verified TLS key ([#319](https://github.com/Dstack-TEE/private-ai-gateway/issues/319)) ([a346e7a](https://github.com/Dstack-TEE/private-ai-gateway/commit/a346e7adbef4e86532b2b8f0fcd636c8ffa8f5d8))
* **desktop:** native look and behaviour on macOS, Windows and Linux ([#317](https://github.com/Dstack-TEE/private-ai-gateway/issues/317)) ([7a90e89](https://github.com/Dstack-TEE/private-ai-gateway/commit/7a90e89cc04ccb143969a125e02718e290f6a77b))


### Bug Fixes

* **desktop:** a late command result no longer overwrites newer state ([32f1bd6](https://github.com/Dstack-TEE/private-ai-gateway/commit/32f1bd6dae6ad95e913f5197c407a3d08a4e2965))
* **desktop:** cancelling the web UI import picker no longer leaves the dialog stuck ([32f1bd6](https://github.com/Dstack-TEE/private-ai-gateway/commit/32f1bd6dae6ad95e913f5197c407a3d08a4e2965))
* **desktop:** screen readers read usage token counts ([32f1bd6](https://github.com/Dstack-TEE/private-ai-gateway/commit/32f1bd6dae6ad95e913f5197c407a3d08a4e2965))
* **desktop:** Settings from the macOS menu or tray opens once ([32f1bd6](https://github.com/Dstack-TEE/private-ai-gateway/commit/32f1bd6dae6ad95e913f5197c407a3d08a4e2965))
* **desktop:** tray and menu navigation waits for an open dialog instead of being dropped ([32f1bd6](https://github.com/Dstack-TEE/private-ai-gateway/commit/32f1bd6dae6ad95e913f5197c407a3d08a4e2965))
* **desktop:** update checks continue while the window is hidden ([32f1bd6](https://github.com/Dstack-TEE/private-ai-gateway/commit/32f1bd6dae6ad95e913f5197c407a3d08a4e2965))
* **desktop:** web UI opens with a generated, rotatable password ([#316](https://github.com/Dstack-TEE/private-ai-gateway/issues/316)) ([548f265](https://github.com/Dstack-TEE/private-ai-gateway/commit/548f265c8be5446188076de939d097e6f4e977d0))

## [0.2.0-beta.5](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.2.0-beta.4...desktop-v0.2.0-beta.5) (2026-09-26)


### Features

* **desktop:** show proxied model names with a [TEE] suffix ([#312](https://github.com/Dstack-TEE/private-ai-gateway/issues/312)) ([f98a0b6](https://github.com/Dstack-TEE/private-ai-gateway/commit/f98a0b6763d36032a3300a4b7fe8be40a97660d9))

## [0.2.0-beta.4](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.2.0-beta.3...desktop-v0.2.0-beta.4) (2026-09-26)


### Features

* **desktop:** raw receipts in request details; single-entry password forms ([#308](https://github.com/Dstack-TEE/private-ai-gateway/issues/308)) ([81ef1f4](https://github.com/Dstack-TEE/private-ai-gateway/commit/81ef1f4be755355bfd7f4358d2b9b1f0ba5f7bae))


### Bug Fixes

* **desktop:** dialog exit animations and content lifecycle ([#309](https://github.com/Dstack-TEE/private-ai-gateway/issues/309)) ([b23f6fb](https://github.com/Dstack-TEE/private-ai-gateway/commit/b23f6fb693a03c0deff83dd7b0308df52e58ac50))
* **desktop:** install-independent agent detection and Oh My Pi connect ([#310](https://github.com/Dstack-TEE/private-ai-gateway/issues/310)) ([24fda44](https://github.com/Dstack-TEE/private-ai-gateway/commit/24fda440999475e992c985fb58bc36357ab3a2a4))
* **desktop:** no focus ring on programmatic focus ([#306](https://github.com/Dstack-TEE/private-ai-gateway/issues/306)) ([364037f](https://github.com/Dstack-TEE/private-ai-gateway/commit/364037fad4ee98a3f1c9cc8cd84708445f3926cc))

## [0.2.0-beta.3](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.2.0-beta.2...desktop-v0.2.0-beta.3) (2026-09-25)


### Features

* **desktop:** check key custody under a configured policy ([3bf5e44](https://github.com/Dstack-TEE/private-ai-gateway/commit/3bf5e44ec73747843c63d1d7cb198f5a09ef5ad7))


### Bug Fixes

* **desktop:** final backend audit fixes ([#287](https://github.com/Dstack-TEE/private-ai-gateway/issues/287)) ([4cdd927](https://github.com/Dstack-TEE/private-ai-gateway/commit/4cdd92743a359605fbc71c9a0ff5d4d683c30c18))
* **desktop:** keep the tray agents menu and protection text current ([ee16e3e](https://github.com/Dstack-TEE/private-ai-gateway/commit/ee16e3ee4e5d51f6ea70121a945e0c360765ae1f))
* **desktop:** new profiles default to RedPill, the default provider ([ee16e3e](https://github.com/Dstack-TEE/private-ai-gateway/commit/ee16e3ee4e5d51f6ea70121a945e0c360765ae1f))
* **desktop:** open the window in the saved appearance before its first paint ([ee16e3e](https://github.com/Dstack-TEE/private-ai-gateway/commit/ee16e3ee4e5d51f6ea70121a945e0c360765ae1f))
* **desktop:** pin only the TLS entry the report declares ([3bf5e44](https://github.com/Dstack-TEE/private-ai-gateway/commit/3bf5e44ec73747843c63d1d7cb198f5a09ef5ad7))
* **desktop:** save "Allow development OS" so the window, tray and web UI agree ([ee16e3e](https://github.com/Dstack-TEE/private-ai-gateway/commit/ee16e3ee4e5d51f6ea70121a945e0c360765ae1f))

## [0.2.0-beta.2](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.2.0-beta.1...desktop-v0.2.0-beta.2) (2026-09-25)


### Bug Fixes

* **desktop:** heal a stale TLS pin through the rotation path instead of wedging in 502 ([#285](https://github.com/Dstack-TEE/private-ai-gateway/issues/285)) ([b8159fb](https://github.com/Dstack-TEE/private-ai-gateway/commit/b8159fba35c6a4360e85ef4ddc753a569f221eb3)), closes [#241](https://github.com/Dstack-TEE/private-ai-gateway/issues/241)
* **desktop:** publish the feed and npm after a skipped App Store job ([#282](https://github.com/Dstack-TEE/private-ai-gateway/issues/282)) ([dd770e9](https://github.com/Dstack-TEE/private-ai-gateway/commit/dd770e9462670a4f7964f8b0323243fca68a0022))

## [0.2.0-beta.1](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.1.7-beta.4...desktop-v0.2.0-beta.1) (2026-09-25)


### ⚠ BREAKING CHANGES

* **desktop:** `--json` failures report the backend's error code in `error.code` with the message alone in `error.message`; previously the code was always `command_failed` and the message was prefixed with `CODE: `.
* **desktop:** The management API is HTTP on a new local endpoint (Unix `api.sock`, Windows pipe `<id>-<hash>-api`); 0.1 clients cannot manage a 0.2 backend, and 0.2 clients stop 0.1.4+ backends on update. `pap status --json` and `pap service start --json` report `backend.apiVersion` instead of `protocolVersion`. Failed commands report the backend's error codes: `pap usage show` of an unknown record says `not_found: Usage record not found`, and the `encoding_failed` and `incompatible_protocol` codes are gone. Web UI RPC paths are the snake_case command names and errors use each code's HTTP status.

### Features

* **desktop:** config.toml and credentials.toml replace the JSON settings and the OS keychain ([#265](https://github.com/Dstack-TEE/private-ai-gateway/issues/265)) ([1dfda3b](https://github.com/Dstack-TEE/private-ai-gateway/commit/1dfda3b0366205eeaa1e53867a2fefae418c4acb))
* **desktop:** release-please versions, changelog and tag-triggered releases ([#272](https://github.com/Dstack-TEE/private-ai-gateway/issues/272)) ([42b5a60](https://github.com/Dstack-TEE/private-ai-gateway/commit/42b5a60369d85687eb683d215b650c5913885533))
* **desktop:** route renderer pages with TanStack Router ([#261](https://github.com/Dstack-TEE/private-ai-gateway/issues/261)) ([333609a](https://github.com/Dstack-TEE/private-ai-gateway/commit/333609a26f69f300488696d1e1962bc5d6b7cfa2))
* **desktop:** single Web UI setting with password sign-in and cookie sessions ([#262](https://github.com/Dstack-TEE/private-ai-gateway/issues/262)) ([a9644d2](https://github.com/Dstack-TEE/private-ai-gateway/commit/a9644d2e019c92669929e3490c59ef192d71d77c))


### Bug Fixes

* **desktop:** backend review follow-ups ([#281](https://github.com/Dstack-TEE/private-ai-gateway/issues/281)) ([03eb86c](https://github.com/Dstack-TEE/private-ai-gateway/commit/03eb86c0b90af835252405a3d6f168d187debae7))
* **desktop:** nfpm-built Linux packages without maintainer scripts; web UI and helper precedents ([#269](https://github.com/Dstack-TEE/private-ai-gateway/issues/269)) ([6d9661a](https://github.com/Dstack-TEE/private-ai-gateway/commit/6d9661a116a39a1f35e6adfe319df589cee96739))
* **desktop:** pre-release backend audit fixes ([#280](https://github.com/Dstack-TEE/private-ai-gateway/issues/280)) ([5a09074](https://github.com/Dstack-TEE/private-ai-gateway/commit/5a09074ef65194263a5539838c66ee3d69a892e3))
* **desktop:** pre-release UI and CLI audit fixes ([#279](https://github.com/Dstack-TEE/private-ai-gateway/issues/279)) ([ab8e5fc](https://github.com/Dstack-TEE/private-ai-gateway/commit/ab8e5fc6c3a3d70039cf1cda0f9cd58ef2d0fb6f))
* **desktop:** publish npm platform binaries as versions of one package, like Codex ([#264](https://github.com/Dstack-TEE/private-ai-gateway/issues/264)) ([83989e0](https://github.com/Dstack-TEE/private-ai-gateway/commit/83989e0f427c030c0136d22cc3adc65993d09150))
* **desktop:** read one static latest.json per update channel ([#271](https://github.com/Dstack-TEE/private-ai-gateway/issues/271)) ([84dfcc6](https://github.com/Dstack-TEE/private-ai-gateway/commit/84dfcc6ffb52a47a372c608344d318dd45bbf572))
* **desktop:** settings file contract and a device-local, retryable 0.1 import ([#266](https://github.com/Dstack-TEE/private-ai-gateway/issues/266)) ([e5c4e81](https://github.com/Dstack-TEE/private-ai-gateway/commit/e5c4e81055ba9308c399cf84301348c99d25673a))


### Miscellaneous Chores

* **desktop:** release 0.2.0-beta.1 ([a540870](https://github.com/Dstack-TEE/private-ai-gateway/commit/a540870d2028cea239c5de2d4de6a23d1c2f2204))


### Code Refactoring

* **desktop:** one HTTP API for the local endpoint and the web UI ([#276](https://github.com/Dstack-TEE/private-ai-gateway/issues/276)) ([a56af02](https://github.com/Dstack-TEE/private-ai-gateway/commit/a56af02b2a4b546c12d678b5c8c569feb04fa24d))
