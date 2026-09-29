#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import os
import sys
import time
from pathlib import Path
from typing import Any

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from live_e2e.cases.embeddings import run_embeddings_case  # noqa: E402
from live_e2e.cases.fidelity_structured_outputs import (  # noqa: E402
    run_structured_outputs_case,
)
from live_e2e.cases.lifecycle import run_lifecycle_case  # noqa: E402
from live_e2e.common import (  # noqa: E402
    DEFAULT_ARTIFACT_DIR,
    DEFAULT_DSTACK_ENDPOINT,
    DEFAULT_ENV_FILE,
    ROOT,
    find_free_port,
    load_dotenv,
    load_providers,
    merged_env,
    write_json,
)
from live_e2e.launch_aggregator import AggregatorProcess  # noqa: E402
from live_e2e.preflight import preflight  # noqa: E402
from live_e2e.provider_verify import verify_provider  # noqa: E402


def main() -> None:
    parser = argparse.ArgumentParser(description="Run live E2E tests for the ACI aggregator.")
    parser.add_argument("--profile", choices=["quick", "full", "strict-release"], default="quick")
    parser.add_argument("--providers-file", type=Path, default=ROOT / "scripts/live_e2e/providers.json")
    parser.add_argument("--provider", action="append", default=[])
    parser.add_argument("--env-file", type=Path, default=DEFAULT_ENV_FILE)
    parser.add_argument("--port", type=int, default=18086)
    parser.add_argument("--dstack-endpoint", default=DEFAULT_DSTACK_ENDPOINT)
    parser.add_argument("--artifacts-dir", type=Path, default=DEFAULT_ARTIFACT_DIR)
    parser.add_argument("--skip-provider-verify", action="store_true")
    parser.add_argument("--no-build", action="store_true")
    args = parser.parse_args()

    if args.port == 0:
        args.port = find_free_port()
    artifact_dir = args.artifacts_dir / time.strftime("%Y%m%d-%H%M%S")
    artifact_dir.mkdir(parents=True, exist_ok=True)
    load_dotenv(args.env_file)
    providers = load_providers(args.providers_file, args.provider)
    summary: dict[str, Any] = {
        "profile": args.profile,
        "providers": [provider.name for provider in providers],
        "artifact_dir": str(artifact_dir),
        "phases": {},
    }

    try:
        summary["phases"]["preflight"] = preflight(
            providers,
            port=args.port,
            dstack_endpoint=args.dstack_endpoint,
            env_file=args.env_file,
            check_build=not args.no_build,
        )
        if args.skip_provider_verify:
            summary["phases"]["provider_verify"] = {"status": "skipped"}
        else:
            provider_results = {}
            for provider in providers:
                provider_results[provider.name] = verify_provider(
                    provider,
                    env=merged_env(),
                    strict=args.profile == "strict-release",
                    artifact_dir=artifact_dir,
                    timeout=360,
                )
            summary["phases"]["provider_verify"] = provider_results

        with AggregatorProcess(
            providers,
            port=args.port,
            dstack_endpoint=args.dstack_endpoint,
            env=merged_env(),
            artifact_dir=artifact_dir,
        ) as aggregator:
            summary["phases"]["aggregator"] = {
                "base_url": aggregator.base_url,
                "log_path": str(aggregator.log_path),
            }
            lifecycle = []
            for provider in providers:
                if not provider.has_capability("chat"):
                    continue
                lifecycle.append(
                    run_lifecycle_case(
                        base_url=aggregator.base_url,
                        provider=provider,
                        artifact_dir=artifact_dir,
                    )
                )
            summary["phases"]["lifecycle"] = lifecycle

            embeddings = []
            for provider in providers:
                if not provider.has_capability("embeddings"):
                    continue
                embeddings.append(
                    run_embeddings_case(
                        base_url=aggregator.base_url,
                        provider=provider,
                        artifact_dir=artifact_dir,
                    )
                )
            summary["phases"]["embeddings"] = embeddings

            if args.profile in ("full", "strict-release"):
                structured = []
                for provider in providers:
                    structured.append(
                        run_structured_outputs_case(
                            base_url=aggregator.base_url,
                            provider=provider,
                            artifact_dir=artifact_dir,
                        )
                    )
                summary["phases"]["structured_outputs"] = structured

        summary["ok"] = True
        write_json(artifact_dir / "summary.json", summary)
        print(json.dumps(summary, indent=2, sort_keys=True))
    except Exception as exc:  # noqa: BLE001 - test runner should summarize all fatal failures.
        summary["ok"] = False
        summary["error"] = str(exc)
        write_json(artifact_dir / "summary.json", summary)
        print(json.dumps(summary, indent=2, sort_keys=True), file=sys.stderr)
        raise SystemExit(1) from exc


if __name__ == "__main__":
    main()
