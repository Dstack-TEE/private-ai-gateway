# Changelog

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
