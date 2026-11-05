# Changelog

## [0.8.2](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.8.1...desktop-v0.8.2) (2026-11-05)


### ⚠ BREAKING CHANGES

* **desktop:** pap serve and @phala/aci-verifier refuse constrained JSON requests without a JSON content type (curl -d / fetch string bodies must set Content-Type: application/json).
* **desktop:** `pap profiles login --callback-stdin` is removed; approve the device code that `pap profiles login` prints instead.
* **desktop:** `--json` failures report the backend's error code in `error.code` with the message alone in `error.message`; previously the code was always `command_failed` and the message was prefixed with `CODE: `.
* **desktop:** The management API is HTTP on a new local endpoint (Unix `api.sock`, Windows pipe `<id>-<hash>-api`); 0.1 clients cannot manage a 0.2 backend, and 0.2 clients stop 0.1.4+ backends on update. `pap status --json` and `pap service start --json` report `backend.apiVersion` instead of `protocolVersion`. Failed commands report the backend's error codes: `pap usage show` of an unknown record says `not_found: Usage record not found`, and the `encoding_failed` and `incompatible_protocol` codes are gone. Web UI RPC paths are the snake_case command names and errors use each code's HTTP status.

### Features

* **aggregator:** stable attested sessions across re-verification ([#403](https://github.com/Dstack-TEE/private-ai-gateway/issues/403)) ([#456](https://github.com/Dstack-TEE/private-ai-gateway/issues/456)) ([da7df70](https://github.com/Dstack-TEE/private-ai-gateway/commit/da7df70cd5a412ea58b69df3456f58c1db72b006))
* **ci:** add guarded Mac App Store review workflow ([#418](https://github.com/Dstack-TEE/private-ai-gateway/issues/418)) ([d6a7fcc](https://github.com/Dstack-TEE/private-ai-gateway/commit/d6a7fcc16e2dc8626f7e70f7536edd19cffbbe1f))
* **cli:** add --max-tokens to pap aci send ([#500](https://github.com/Dstack-TEE/private-ai-gateway/issues/500)) ([6475d5c](https://github.com/Dstack-TEE/private-ai-gateway/commit/6475d5c6b63c8b25cdc492f52b624270c328ed81))
* **desktop:** add a Decisions tab to the Local API examples ([#415](https://github.com/Dstack-TEE/private-ai-gateway/issues/415)) ([4a562b7](https://github.com/Dstack-TEE/private-ai-gateway/commit/4a562b7e001dca6086c2d8da16b3a41d6caa6fa6))
* **desktop:** add Arch Linux packages ([#200](https://github.com/Dstack-TEE/private-ai-gateway/issues/200)) ([db90905](https://github.com/Dstack-TEE/private-ai-gateway/commit/db90905046baf65e8f12c4e949bcd94153a1ce0a))
* **desktop:** add Mac App Store distribution foundation ([#216](https://github.com/Dstack-TEE/private-ai-gateway/issues/216)) ([d010980](https://github.com/Dstack-TEE/private-ai-gateway/commit/d0109800a8200d714dff2d8b8a2a22209dfa1e67))
* **desktop:** add pap curl for one request pinned to the verified TLS key ([#319](https://github.com/Dstack-TEE/private-ai-gateway/issues/319)) ([a346e7a](https://github.com/Dstack-TEE/private-ai-gateway/commit/a346e7adbef4e86532b2b8f0fcd636c8ffa8f5d8))
* **desktop:** check key custody under a configured policy ([3bf5e44](https://github.com/Dstack-TEE/private-ai-gateway/commit/3bf5e44ec73747843c63d1d7cb198f5a09ef5ad7))
* **desktop:** config.toml and credentials.toml replace the JSON settings and the OS keychain ([#265](https://github.com/Dstack-TEE/private-ai-gateway/issues/265)) ([1dfda3b](https://github.com/Dstack-TEE/private-ai-gateway/commit/1dfda3b0366205eeaa1e53867a2fefae418c4acb))
* **desktop:** fail closed on non-JSON constrained bodies and relay audio transcription ([#517](https://github.com/Dstack-TEE/private-ai-gateway/issues/517)) ([673efd0](https://github.com/Dstack-TEE/private-ai-gateway/commit/673efd09c2571f08049d180fa39f5e6b59462ba2))
* **desktop:** highlight the Local API examples with Shiki ([#421](https://github.com/Dstack-TEE/private-ai-gateway/issues/421)) ([3ba2c50](https://github.com/Dstack-TEE/private-ai-gateway/commit/3ba2c5030456423d80c86527b6ebd21f314cf816))
* **desktop:** keep settings in ~/.config/private-ai-proxy on every platform ([#334](https://github.com/Dstack-TEE/private-ai-gateway/issues/334)) ([f0074a7](https://github.com/Dstack-TEE/private-ai-gateway/commit/f0074a7ebf7674b21f2304d3c41ac4da1f149479))
* **desktop:** let the web UI listen on a network address ([#249](https://github.com/Dstack-TEE/private-ai-gateway/issues/249)) ([4522851](https://github.com/Dstack-TEE/private-ai-gateway/commit/4522851907056dd5425fa5ea2b50895a05638397))
* **desktop:** native look and behaviour on macOS, Windows and Linux ([#317](https://github.com/Dstack-TEE/private-ai-gateway/issues/317)) ([7a90e89](https://github.com/Dstack-TEE/private-ai-gateway/commit/7a90e89cc04ccb143969a125e02718e290f6a77b))
* **desktop:** offer to restart Codex's background service after connecting ([#342](https://github.com/Dstack-TEE/private-ai-gateway/issues/342)) ([74685e8](https://github.com/Dstack-TEE/private-ai-gateway/commit/74685e82b5f38df024d2271838764577d140b227))
* **desktop:** package private ai proxy for npm ([#199](https://github.com/Dstack-TEE/private-ai-gateway/issues/199)) ([91f1496](https://github.com/Dstack-TEE/private-ai-gateway/commit/91f14960e52fd5032aa73631329c06fa1e097b87))
* **desktop:** raw receipts in request details; single-entry password forms ([#308](https://github.com/Dstack-TEE/private-ai-gateway/issues/308)) ([81ef1f4](https://github.com/Dstack-TEE/private-ai-gateway/commit/81ef1f4be755355bfd7f4358d2b9b1f0ba5f7bae))
* **desktop:** relay image generation through the Local API ([#493](https://github.com/Dstack-TEE/private-ai-gateway/issues/493)) ([dd333b2](https://github.com/Dstack-TEE/private-ai-gateway/commit/dd333b24324a96468bc19137fbcf6c45aeecc366))
* **desktop:** relay System One through local API ([#411](https://github.com/Dstack-TEE/private-ai-gateway/issues/411)) ([ee735ac](https://github.com/Dstack-TEE/private-ai-gateway/commit/ee735ac64fcb701fe7ad707cff424ec220f339ba))
* **desktop:** release Mac App Store versions automatically after approval ([#448](https://github.com/Dstack-TEE/private-ai-gateway/issues/448)) ([e20027c](https://github.com/Dstack-TEE/private-ai-gateway/commit/e20027cf8abd5c2b024707accf8e2900d9ad121a))
* **desktop:** release-please versions, changelog and tag-triggered releases ([#272](https://github.com/Dstack-TEE/private-ai-gateway/issues/272)) ([42b5a60](https://github.com/Dstack-TEE/private-ai-gateway/commit/42b5a60369d85687eb683d215b650c5913885533))
* **desktop:** remove the production OS policy ([#446](https://github.com/Dstack-TEE/private-ai-gateway/issues/446)) ([493cdc3](https://github.com/Dstack-TEE/private-ai-gateway/commit/493cdc3b3ef7b1b9a6f5390ba3ece0e8004c34f2))
* **desktop:** route renderer pages with TanStack Router ([#261](https://github.com/Dstack-TEE/private-ai-gateway/issues/261)) ([333609a](https://github.com/Dstack-TEE/private-ai-gateway/commit/333609a26f69f300488696d1e1962bc5d6b7cfa2))
* **desktop:** serve an optional web UI from the backend service ([#246](https://github.com/Dstack-TEE/private-ai-gateway/issues/246)) ([0d879b8](https://github.com/Dstack-TEE/private-ai-gateway/commit/0d879b81e2d49a8f8fb221734a960e5e82b89243))
* **desktop:** show proxied model names with a [TEE] suffix ([#312](https://github.com/Dstack-TEE/private-ai-gateway/issues/312)) ([f98a0b6](https://github.com/Dstack-TEE/private-ai-gateway/commit/f98a0b6763d36032a3300a4b7fe8be40a97660d9))
* **desktop:** sign in to RedPill with a device code ([#395](https://github.com/Dstack-TEE/private-ai-gateway/issues/395)) ([5a0311e](https://github.com/Dstack-TEE/private-ai-gateway/commit/5a0311e684b9d2da52265f19abe46921bab43ed5))
* **desktop:** single Web UI setting with password sign-in and cookie sessions ([#262](https://github.com/Dstack-TEE/private-ai-gateway/issues/262)) ([a9644d2](https://github.com/Dstack-TEE/private-ai-gateway/commit/a9644d2e019c92669929e3490c59ef192d71d77c))
* **desktop:** support DeepSeek Harness ([#368](https://github.com/Dstack-TEE/private-ai-gateway/issues/368)) ([df032ed](https://github.com/Dstack-TEE/private-ai-gateway/commit/df032ed046dfdbd558d54500d3c5466de946d667))
* **desktop:** use the TypeSafe SDKs in the Decisions examples ([#420](https://github.com/Dstack-TEE/private-ai-gateway/issues/420)) ([442a362](https://github.com/Dstack-TEE/private-ai-gateway/commit/442a3620e6f1397d293e1a460337e88b82b9d712))


### Bug Fixes

* **aci:** keep client sessions working across keyset renewal ([#458](https://github.com/Dstack-TEE/private-ai-gateway/issues/458)) ([4c12bf6](https://github.com/Dstack-TEE/private-ai-gateway/commit/4c12bf642904eff55c1aee175211f5059c22ca61))
* **ci:** include beta changes in App Store What's New ([#435](https://github.com/Dstack-TEE/private-ai-gateway/issues/435)) ([3d5aaa0](https://github.com/Dstack-TEE/private-ai-gateway/commit/3d5aaa06e2d534a37c82a850e2d9c47b21ce7b11))
* **cli:** harden pap verification and output paths ([#461](https://github.com/Dstack-TEE/private-ai-gateway/issues/461)) ([d5341af](https://github.com/Dstack-TEE/private-ai-gateway/commit/d5341affa189e3824442426e75bff826df21743a))
* **cli:** polish pap's interface and harden standalone commands ([#469](https://github.com/Dstack-TEE/private-ai-gateway/issues/469)) ([64b2ded](https://github.com/Dstack-TEE/private-ai-gateway/commit/64b2dede59c7c47a662cd6a511d5a9af129471bf))
* **core:** recover a lost management endpoint and harden private files ([#468](https://github.com/Dstack-TEE/private-ai-gateway/issues/468)) ([752c820](https://github.com/Dstack-TEE/private-ai-gateway/commit/752c820ce92684b342e728a3bc121d5e4cd790d5))
* **desktop-e2e:** install Oh My Pi from npm ([#419](https://github.com/Dstack-TEE/private-ai-gateway/issues/419)) ([0337126](https://github.com/Dstack-TEE/private-ai-gateway/commit/03371260b10e98c1032667b65e7d8df10c0d70c6))
* **desktop:** a late command result no longer overwrites newer state ([32f1bd6](https://github.com/Dstack-TEE/private-ai-gateway/commit/32f1bd6dae6ad95e913f5197c407a3d08a4e2965))
* **desktop:** accept the Tauri DEB version when packaging Arch prereleases ([#251](https://github.com/Dstack-TEE/private-ai-gateway/issues/251)) ([0e764dd](https://github.com/Dstack-TEE/private-ai-gateway/commit/0e764ddb2ce2c74d313060018477c37de25a425d))
* **desktop:** accurate refusals, profile switches and agent status from live testing ([#353](https://github.com/Dstack-TEE/private-ai-gateway/issues/353)) ([d8c88c2](https://github.com/Dstack-TEE/private-ai-gateway/commit/d8c88c2042e2d8718dfe46ef850aa5461664d4ff))
* **desktop:** allow a slow first backend start after an update ([#258](https://github.com/Dstack-TEE/private-ai-gateway/issues/258)) ([56792ef](https://github.com/Dstack-TEE/private-ai-gateway/commit/56792ef7a52483c07a304061376eca2dbffc3406))
* **desktop:** backend review follow-ups ([#281](https://github.com/Dstack-TEE/private-ai-gateway/issues/281)) ([03eb86c](https://github.com/Dstack-TEE/private-ai-gateway/commit/03eb86c0b90af835252405a3d6f168d187debae7))
* **desktop:** cancelling the web UI import picker no longer leaves the dialog stuck ([32f1bd6](https://github.com/Dstack-TEE/private-ai-gateway/commit/32f1bd6dae6ad95e913f5197c407a3d08a4e2965))
* **desktop:** center the update button; align the update flow with the updater docs ([#335](https://github.com/Dstack-TEE/private-ai-gateway/issues/335)) ([86d5b46](https://github.com/Dstack-TEE/private-ai-gateway/commit/86d5b465f0ce7b4265f103e837146891fa240917))
* **desktop:** clarify status during failed verification retries ([#432](https://github.com/Dstack-TEE/private-ai-gateway/issues/432)) ([1b8286f](https://github.com/Dstack-TEE/private-ai-gateway/commit/1b8286f3e796eea5a05f6dcc083be9ad5c9628a2))
* **desktop:** detect MAS agents from authorized home ([ce214cf](https://github.com/Dstack-TEE/private-ai-gateway/commit/ce214cfaf17e1e1929a3c21ab0d2db7c21031c82))
* **desktop:** dialog exit animations and content lifecycle ([#309](https://github.com/Dstack-TEE/private-ai-gateway/issues/309)) ([b23f6fb](https://github.com/Dstack-TEE/private-ai-gateway/commit/b23f6fb693a03c0deff83dd7b0308df52e58ac50))
* **desktop:** dispatch trusted npm publisher ([#239](https://github.com/Dstack-TEE/private-ai-gateway/issues/239)) ([30e5efd](https://github.com/Dstack-TEE/private-ai-gateway/commit/30e5efd2d885e07aa6d88668112183a9048375b4))
* **desktop:** explain service verification failures, still fail closed ([#425](https://github.com/Dstack-TEE/private-ai-gateway/issues/425)) ([8a8ce77](https://github.com/Dstack-TEE/private-ai-gateway/commit/8a8ce77d39f6c0b434141bd91c42de1d76cd4073))
* **desktop:** expose backend startup failures ([0b2dcb3](https://github.com/Dstack-TEE/private-ai-gateway/commit/0b2dcb3cb51479954ac3ce5c0b59bb8a4c7fa930))
* **desktop:** fail closed on verification failure and harden the 0.1 upgrade path ([#347](https://github.com/Dstack-TEE/private-ai-gateway/issues/347)) ([f31fc7f](https://github.com/Dstack-TEE/private-ai-gateway/commit/f31fc7f009348b0d8e58da0cfe9e7a8ef1a6cf33))
* **desktop:** fetch DCAP collateral reliably and explain why it failed ([#428](https://github.com/Dstack-TEE/private-ai-gateway/issues/428)) ([1c48f3c](https://github.com/Dstack-TEE/private-ai-gateway/commit/1c48f3c98ef276db2dfa2dc4501ef1df353af6ac))
* **desktop:** final audit fixes for beta.6 ([#325](https://github.com/Dstack-TEE/private-ai-gateway/issues/325)) ([6061f60](https://github.com/Dstack-TEE/private-ai-gateway/commit/6061f601f7294bd6686e670ad10222e0b87ef8f0))
* **desktop:** final backend audit fixes ([#287](https://github.com/Dstack-TEE/private-ai-gateway/issues/287)) ([4cdd927](https://github.com/Dstack-TEE/private-ai-gateway/commit/4cdd92743a359605fbc71c9a0ff5d4d683c30c18))
* **desktop:** full-row hover and scrollable dialogs with a stable height ([#355](https://github.com/Dstack-TEE/private-ai-gateway/issues/355)) ([70ea7b7](https://github.com/Dstack-TEE/private-ai-gateway/commit/70ea7b73392c4edf4dedf937d3af9d279f5c7e94))
* **desktop:** guard OpenCode V2 provider overrides and test OpenCode 2 ([#363](https://github.com/Dstack-TEE/private-ai-gateway/issues/363)) ([d1fa727](https://github.com/Dstack-TEE/private-ai-gateway/commit/d1fa7273b0c10c58e51dcce44e6b10f9b9a55c9a))
* **desktop:** harden MAS agent access and IPC ([86105ec](https://github.com/Dstack-TEE/private-ai-gateway/commit/86105ec5c0999793083c3ab2f11e079bbededc61))
* **desktop:** harden MAS first launch ([#219](https://github.com/Dstack-TEE/private-ai-gateway/issues/219)) ([c7693a9](https://github.com/Dstack-TEE/private-ai-gateway/commit/c7693a95b174bcef8eb4bf988ba7b1b11235f714))
* **desktop:** heal a stale TLS pin through the rotation path instead of wedging in 502 ([#285](https://github.com/Dstack-TEE/private-ai-gateway/issues/285)) ([b8159fb](https://github.com/Dstack-TEE/private-ai-gateway/commit/b8159fba35c6a4360e85ef4ddc753a569f221eb3)), closes [#241](https://github.com/Dstack-TEE/private-ai-gateway/issues/241)
* **desktop:** install-independent agent detection and Oh My Pi connect ([#310](https://github.com/Dstack-TEE/private-ai-gateway/issues/310)) ([24fda44](https://github.com/Dstack-TEE/private-ai-gateway/commit/24fda440999475e992c985fb58bc36357ab3a2a4))
* **desktop:** keep stable feed non-prerelease ([#198](https://github.com/Dstack-TEE/private-ai-gateway/issues/198)) ([b642a45](https://github.com/Dstack-TEE/private-ai-gateway/commit/b642a455befcb2d99b45b2598f83ac132a9c1071))
* **desktop:** keep the client reachable and its state current ([#460](https://github.com/Dstack-TEE/private-ai-gateway/issues/460)) ([389ba2f](https://github.com/Dstack-TEE/private-ai-gateway/commit/389ba2fa523c350d08b9cfc12bfc0ff7856424eb))
* **desktop:** keep the tray agents menu and protection text current ([ee16e3e](https://github.com/Dstack-TEE/private-ai-gateway/commit/ee16e3ee4e5d51f6ea70121a945e0c360765ae1f))
* **desktop:** launch MAS smoke through Launch Services ([#220](https://github.com/Dstack-TEE/private-ai-gateway/issues/220)) ([b0ed5d7](https://github.com/Dstack-TEE/private-ai-gateway/commit/b0ed5d7a222f6001a19180421e778f0377f67f44))
* **desktop:** make embedded MAS profile readable ([#218](https://github.com/Dstack-TEE/private-ai-gateway/issues/218)) ([e0f4852](https://github.com/Dstack-TEE/private-ai-gateway/commit/e0f4852d3a3882759fbd111f42377de24fc6c5da))
* **desktop:** mark stable releases latest ([#201](https://github.com/Dstack-TEE/private-ai-gateway/issues/201)) ([09064d4](https://github.com/Dstack-TEE/private-ai-gateway/commit/09064d4a05e41f22bdbebaaa8c6afbc4ea969b67))
* **desktop:** native feedback instead of web toasts ([#340](https://github.com/Dstack-TEE/private-ai-gateway/issues/340)) ([eb200d4](https://github.com/Dstack-TEE/private-ai-gateway/commit/eb200d434ad132aea87ecf330f32fafef943aed9))
* **desktop:** new profiles default to RedPill, the default provider ([ee16e3e](https://github.com/Dstack-TEE/private-ai-gateway/commit/ee16e3ee4e5d51f6ea70121a945e0c360765ae1f))
* **desktop:** nfpm-built Linux packages without maintainer scripts; web UI and helper precedents ([#269](https://github.com/Dstack-TEE/private-ai-gateway/issues/269)) ([6d9661a](https://github.com/Dstack-TEE/private-ai-gateway/commit/6d9661a116a39a1f35e6adfe319df589cee96739))
* **desktop:** no focus ring on programmatic focus ([#306](https://github.com/Dstack-TEE/private-ai-gateway/issues/306)) ([364037f](https://github.com/Dstack-TEE/private-ai-gateway/commit/364037fad4ee98a3f1c9cc8cd84708445f3926cc))
* **desktop:** normalize MAS smoke process paths ([#223](https://github.com/Dstack-TEE/private-ai-gateway/issues/223)) ([25cfcac](https://github.com/Dstack-TEE/private-ai-gateway/commit/25cfcac40052eb5c1a3165bb0ef8cbf270ce90a7))
* **desktop:** open target=_blank links in the browser ([#472](https://github.com/Dstack-TEE/private-ai-gateway/issues/472)) ([2201348](https://github.com/Dstack-TEE/private-ai-gateway/commit/2201348660fce875e245c7b11d81dadceec35646))
* **desktop:** open the window in the saved appearance before its first paint ([ee16e3e](https://github.com/Dstack-TEE/private-ai-gateway/commit/ee16e3ee4e5d51f6ea70121a945e0c360765ae1f))
* **desktop:** pin only the TLS entry the report declares ([3bf5e44](https://github.com/Dstack-TEE/private-ai-gateway/commit/3bf5e44ec73747843c63d1d7cb198f5a09ef5ad7))
* **desktop:** polish native shell behavior ([#470](https://github.com/Dstack-TEE/private-ai-gateway/issues/470)) ([976b924](https://github.com/Dstack-TEE/private-ai-gateway/commit/976b92422541f2648c5efac286c3be85e84fbfb3))
* **desktop:** polish UI readability, forms and accessibility ([#471](https://github.com/Dstack-TEE/private-ai-gateway/issues/471)) ([1594248](https://github.com/Dstack-TEE/private-ai-gateway/commit/159424810139c7dbce603eb063c3a086f603cedc))
* **desktop:** pre-release backend audit fixes ([#280](https://github.com/Dstack-TEE/private-ai-gateway/issues/280)) ([5a09074](https://github.com/Dstack-TEE/private-ai-gateway/commit/5a09074ef65194263a5539838c66ee3d69a892e3))
* **desktop:** pre-release UI and CLI audit fixes ([#279](https://github.com/Dstack-TEE/private-ai-gateway/issues/279)) ([ab8e5fc](https://github.com/Dstack-TEE/private-ai-gateway/commit/ab8e5fc6c3a3d70039cf1cda0f9cd58ef2d0fb6f))
* **desktop:** pre-stable hardening for 0.1.7 (arm64 open flags, startup wait, web UI sessions) ([#259](https://github.com/Dstack-TEE/private-ai-gateway/issues/259)) ([e759a83](https://github.com/Dstack-TEE/private-ai-gateway/commit/e759a832f30eda801f44d24e52e95d4bd3a114ef))
* **desktop:** preserve receipt proofs after stream close ([#234](https://github.com/Dstack-TEE/private-ai-gateway/issues/234)) ([787111c](https://github.com/Dstack-TEE/private-ai-gateway/commit/787111c44d14017d3f87ed4954d2f8b13662050d))
* **desktop:** publish npm platform binaries as versions of one package, like Codex ([#264](https://github.com/Dstack-TEE/private-ai-gateway/issues/264)) ([83989e0](https://github.com/Dstack-TEE/private-ai-gateway/commit/83989e0f427c030c0136d22cc3adc65993d09150))
* **desktop:** publish the feed and npm after a skipped App Store job ([#282](https://github.com/Dstack-TEE/private-ai-gateway/issues/282)) ([dd770e9](https://github.com/Dstack-TEE/private-ai-gateway/commit/dd770e9462670a4f7964f8b0323243fca68a0022))
* **desktop:** publish the npm channel wrapper only after its platform versions resolve ([#257](https://github.com/Dstack-TEE/private-ai-gateway/issues/257)) ([8eba74f](https://github.com/Dstack-TEE/private-ai-gateway/commit/8eba74f648a37d771b745d07f23fe18cbe4b686f))
* **desktop:** read one static latest.json per update channel ([#271](https://github.com/Dstack-TEE/private-ai-gateway/issues/271)) ([84dfcc6](https://github.com/Dstack-TEE/private-ai-gateway/commit/84dfcc6ffb52a47a372c608344d318dd45bbf572))
* **desktop:** release notes list the downloads again ([#380](https://github.com/Dstack-TEE/private-ai-gateway/issues/380)) ([06a6b20](https://github.com/Dstack-TEE/private-ai-gateway/commit/06a6b208416392e1bde3075a54fd4071f8c13246))
* **desktop:** remove Intel PCS collateral fallback ([#437](https://github.com/Dstack-TEE/private-ai-gateway/issues/437)) ([45b1273](https://github.com/Dstack-TEE/private-ai-gateway/commit/45b1273083d4043680735ab7c2f5111385dc6d90))
* **desktop:** repair invalid Codex provider names ([#238](https://github.com/Dstack-TEE/private-ai-gateway/issues/238)) ([8e117e1](https://github.com/Dstack-TEE/private-ai-gateway/commit/8e117e1bbb46dc41a8c763820feb17243e955acf))
* **desktop:** repair Linux persistence, startup, and packaging ([#242](https://github.com/Dstack-TEE/private-ai-gateway/issues/242)) ([16dbdb9](https://github.com/Dstack-TEE/private-ai-gateway/commit/16dbdb9a94db79c0dceb908f67852cb16980b2d7))
* **desktop:** restart stale backend before agent scan ([5ef87b1](https://github.com/Dstack-TEE/private-ai-gateway/commit/5ef87b11b62405bc4b98247604db2699e1423094))
* **desktop:** restore agents offline when the app is uninstalled ([#359](https://github.com/Dstack-TEE/private-ai-gateway/issues/359)) ([0f6db38](https://github.com/Dstack-TEE/private-ai-gateway/commit/0f6db384e0a903923ddc81073be6b8a79d507cc2))
* **desktop:** restore grouped lists and fit the default window ([#330](https://github.com/Dstack-TEE/private-ai-gateway/issues/330)) ([f7ea7e0](https://github.com/Dstack-TEE/private-ai-gateway/commit/f7ea7e09110683ae27f1b02116ea07b7e1b1937e))
* **desktop:** restore Linux release compatibility ([#211](https://github.com/Dstack-TEE/private-ai-gateway/issues/211)) ([8f16144](https://github.com/Dstack-TEE/private-ai-gateway/commit/8f161449817234cc3cf60843bb135bb44b297b8f))
* **desktop:** restore MAS agent detection ([7200f16](https://github.com/Dstack-TEE/private-ai-gateway/commit/7200f16343627ad9a50a46a20443a40b5a15eb33))
* **desktop:** restore the shadcn default styles ([#324](https://github.com/Dstack-TEE/private-ai-gateway/issues/324)) ([720292f](https://github.com/Dstack-TEE/private-ai-gateway/commit/720292f21f1bd9a38bb0d52cb4fbb8faa618a476))
* **desktop:** resume protection after updates ([#208](https://github.com/Dstack-TEE/private-ai-gateway/issues/208)) ([843c531](https://github.com/Dstack-TEE/private-ai-gateway/commit/843c53118a279bf0a1927b4324c4a679c6895de7))
* **desktop:** retry the DMG unmount while Finder releases it ([#536](https://github.com/Dstack-TEE/private-ai-gateway/issues/536)) ([0cbfd92](https://github.com/Dstack-TEE/private-ai-gateway/commit/0cbfd92081e85e0a6e2700bdebe59f72038c0087))
* **desktop:** save "Allow development OS" so the window, tray and web UI agree ([ee16e3e](https://github.com/Dstack-TEE/private-ai-gateway/commit/ee16e3ee4e5d51f6ea70121a945e0c360765ae1f))
* **desktop:** say why a profile or protection change is refused while protection is busy ([#356](https://github.com/Dstack-TEE/private-ai-gateway/issues/356)) ([3dd451e](https://github.com/Dstack-TEE/private-ai-gateway/commit/3dd451e407a779bfaa7acc45b5aabaf989ecc268))
* **desktop:** screen readers read usage token counts ([32f1bd6](https://github.com/Dstack-TEE/private-ai-gateway/commit/32f1bd6dae6ad95e913f5197c407a3d08a4e2965))
* **desktop:** settings file contract and a device-local, retryable 0.1 import ([#266](https://github.com/Dstack-TEE/private-ai-gateway/issues/266)) ([e5c4e81](https://github.com/Dstack-TEE/private-ai-gateway/commit/e5c4e81055ba9308c399cf84301348c99d25673a))
* **desktop:** Settings from the macOS menu or tray opens once ([32f1bd6](https://github.com/Dstack-TEE/private-ai-gateway/commit/32f1bd6dae6ad95e913f5197c407a3d08a4e2965))
* **desktop:** show a signed receipt that is not valid JSON as returned ([1db5247](https://github.com/Dstack-TEE/private-ai-gateway/commit/1db52476442485c37cfe74c11d811ea08be6c243))
* **desktop:** show every verified catalog model ([#532](https://github.com/Dstack-TEE/private-ai-gateway/issues/532)) ([41b84f9](https://github.com/Dstack-TEE/private-ai-gateway/commit/41b84f9da69ab24119b53f0d8de0410caae94dde))
* **desktop:** show failure reasons and report errors in dialogs ([#346](https://github.com/Dstack-TEE/private-ai-gateway/issues/346)) ([9402c4c](https://github.com/Dstack-TEE/private-ai-gateway/commit/9402c4cbe7b16636231cf487b9f7068bbff6af0b))
* **desktop:** show MAS main window after page load ([#225](https://github.com/Dstack-TEE/private-ai-gateway/issues/225)) ([897b56d](https://github.com/Dstack-TEE/private-ai-gateway/commit/897b56d7b685f00d88417607b2eecbcaf4a40de9))
* **desktop:** skip non-text models in endpoint probes ([#413](https://github.com/Dstack-TEE/private-ai-gateway/issues/413)) ([f3c2d06](https://github.com/Dstack-TEE/private-ai-gateway/commit/f3c2d06e7a8f47dda7eff9263bf624161a4b2c4c))
* **desktop:** smoke test MAS app from Applications ([#221](https://github.com/Dstack-TEE/private-ai-gateway/issues/221)) ([15f632d](https://github.com/Dstack-TEE/private-ai-gateway/commit/15f632d6bb1eaf4638257cbd99d380bb4ab02efc))
* **desktop:** starting or stopping protection can no longer run twice from a double click ([6457a68](https://github.com/Dstack-TEE/private-ai-gateway/commit/6457a68466f645266c92ffede730b7dea88078c4))
* **desktop:** support MAS backend IPC sandbox ([f7168d0](https://github.com/Dstack-TEE/private-ai-gateway/commit/f7168d0531703f42320573e27152c25e7fe693b6))
* **desktop:** support MAS backend IPC sandbox ([e100a64](https://github.com/Dstack-TEE/private-ai-gateway/commit/e100a6452a76443bbc473810f2ddbb99ae709a22))
* **desktop:** support the Codex 0.157.1 baseline and explain its background server ([#339](https://github.com/Dstack-TEE/private-ai-gateway/issues/339)) ([d9d3f58](https://github.com/Dstack-TEE/private-ai-gateway/commit/d9d3f58dd849efb63feceabce6c0c33df16dafd6))
* **desktop:** tray and menu navigation waits for an open dialog instead of being dropped ([32f1bd6](https://github.com/Dstack-TEE/private-ai-gateway/commit/32f1bd6dae6ad95e913f5197c407a3d08a4e2965))
* **desktop:** unblock Windows packaging and harden listener rebinds ([#250](https://github.com/Dstack-TEE/private-ai-gateway/issues/250)) ([59dba10](https://github.com/Dstack-TEE/private-ai-gateway/commit/59dba103c64a752e50cb997b03862a38c90294dc))
* **desktop:** update checks continue while the window is hidden ([32f1bd6](https://github.com/Dstack-TEE/private-ai-gateway/commit/32f1bd6dae6ad95e913f5197c407a3d08a4e2965))
* **desktop:** validate MAS runtime before distribution signing ([#222](https://github.com/Dstack-TEE/private-ai-gateway/issues/222)) ([41a46f7](https://github.com/Dstack-TEE/private-ai-gateway/commit/41a46f7a690bd4a219aa0f8ca11cedb7795785b0))
* **desktop:** wait for the api.sock endpoint in the App Store smoke test ([#348](https://github.com/Dstack-TEE/private-ai-gateway/issues/348)) ([89f7e2d](https://github.com/Dstack-TEE/private-ai-gateway/commit/89f7e2dc51a57d21f6f91995eb86b8e1d7129a3e))
* **desktop:** web UI opens with a generated, rotatable password ([#316](https://github.com/Dstack-TEE/private-ai-gateway/issues/316)) ([548f265](https://github.com/Dstack-TEE/private-ai-gateway/commit/548f265c8be5446188076de939d097e6f4e977d0))
* **desktop:** working auto-update or upgrade notices for every Direct install ([#254](https://github.com/Dstack-TEE/private-ai-gateway/issues/254)) ([dced42a](https://github.com/Dstack-TEE/private-ai-gateway/commit/dced42a800243bb937ee25abd6270f9553aa8ab9))


### Performance Improvements

* **desktop:** build smaller release binaries and run macOS CI on xcode-27 ([c205233](https://github.com/Dstack-TEE/private-ai-gateway/commit/c2052334df96b4e9c0f90d793b5d37b30be8c6fe))


### Miscellaneous Chores

* **desktop:** release 0.2.0 ([#351](https://github.com/Dstack-TEE/private-ai-gateway/issues/351)) ([c59ea2d](https://github.com/Dstack-TEE/private-ai-gateway/commit/c59ea2db348e274d4253a6797433004687519eb8))
* **desktop:** release 0.2.0-beta.1 ([a540870](https://github.com/Dstack-TEE/private-ai-gateway/commit/a540870d2028cea239c5de2d4de6a23d1c2f2204))
* **desktop:** release 0.2.1 ([#365](https://github.com/Dstack-TEE/private-ai-gateway/issues/365)) ([7c1230b](https://github.com/Dstack-TEE/private-ai-gateway/commit/7c1230bb1866dcbb194b1eff7ba586cd8a2ede5f))
* **desktop:** release 0.3.0 ([#377](https://github.com/Dstack-TEE/private-ai-gateway/issues/377)) ([928e628](https://github.com/Dstack-TEE/private-ai-gateway/commit/928e6288d389959bae0026015765fde5b51b479c))
* **desktop:** release 0.4.0 ([#397](https://github.com/Dstack-TEE/private-ai-gateway/issues/397)) ([69898ed](https://github.com/Dstack-TEE/private-ai-gateway/commit/69898ede0e28cfd41af85d858422ae14d7603082))
* **desktop:** release 0.5.0 ([#434](https://github.com/Dstack-TEE/private-ai-gateway/issues/434)) ([2efdeda](https://github.com/Dstack-TEE/private-ai-gateway/commit/2efdeda11954706f6a9458f130124d6c4df4aed4))
* **desktop:** release 0.5.1 ([#438](https://github.com/Dstack-TEE/private-ai-gateway/issues/438)) ([b3b17af](https://github.com/Dstack-TEE/private-ai-gateway/commit/b3b17af3880c6a3be94185ed089439c94ef6bd89))
* **desktop:** release 0.6.0 ([#450](https://github.com/Dstack-TEE/private-ai-gateway/issues/450)) ([d2b7f10](https://github.com/Dstack-TEE/private-ai-gateway/commit/d2b7f10ffb54f6b90552f17ef4adce01690f4ed8))
* **desktop:** release 0.6.1 ([#474](https://github.com/Dstack-TEE/private-ai-gateway/issues/474)) ([2203435](https://github.com/Dstack-TEE/private-ai-gateway/commit/22034350268417cf2894b277cecc2cab02f5e013))
* **desktop:** release 0.6.2 ([#479](https://github.com/Dstack-TEE/private-ai-gateway/issues/479)) ([0dfd36e](https://github.com/Dstack-TEE/private-ai-gateway/commit/0dfd36e8e407380f16b67849d9475e33ca64788d))
* **desktop:** release 0.7.0 ([#511](https://github.com/Dstack-TEE/private-ai-gateway/issues/511)) ([9bea43e](https://github.com/Dstack-TEE/private-ai-gateway/commit/9bea43e110e48b04404773876dc3e13368a85daf))
* **desktop:** release 0.8.0 ([03ffa5d](https://github.com/Dstack-TEE/private-ai-gateway/commit/03ffa5d2bf6865556f886361b9263f9efc916134))
* **desktop:** release 0.8.0 ([#527](https://github.com/Dstack-TEE/private-ai-gateway/issues/527)) ([db1cce4](https://github.com/Dstack-TEE/private-ai-gateway/commit/db1cce42ef19eef876c240f99e2e4de01a6ca9e3))


### Code Refactoring

* **desktop:** one HTTP API for the local endpoint and the web UI ([#276](https://github.com/Dstack-TEE/private-ai-gateway/issues/276)) ([a56af02](https://github.com/Dstack-TEE/private-ai-gateway/commit/a56af02b2a4b546c12d678b5c8c569feb04fa24d))

## [0.8.1](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.8.0...desktop-v0.8.1) (2026-11-04)

Private AI Proxy 0.8.1 shows every model the verified gateway lists. Changes since stable 0.8.0.

### Bug Fixes

* **New models appear right away.** On the RedPill and Phala presets, models missing from a hand-maintained compatibility list were hidden, so newly launched models such as `deepseek/deepseek-v4.1-flash-1017` never showed up in the app, the Local API model list, or agent pickers. The verified live catalog is now the only model list. Models that advertise no text output (decision, image, or transcription models) still stay out of coding agents ([#532](https://github.com/Dstack-TEE/private-ai-gateway/pull/532)).

### Maintenance

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
