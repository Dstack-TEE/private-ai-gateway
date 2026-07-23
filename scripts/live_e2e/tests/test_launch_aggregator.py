from __future__ import annotations

import hashlib
import json
import sys
import unittest
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

from live_e2e.common import Provider  # noqa: E402
from live_e2e.launch_aggregator import (  # noqa: E402
    AggregatorProcess,
    build_gateway_config,
    build_upstream_config,
    redact_upstream_config,
)


def privatemode_provider() -> Provider:
    return Provider.from_json(
        {
            "name": "privatemode-test",
            "provider": "privatemode",
            "base_url": "http://privatemode-proxy:8080",
            "public_model": "private-model",
            "upstream_model": "upstream-model",
            "api_key_env": "PRIVATEMODE_API_KEY",
            "binding": "manifest_image_sha256",
            "privatemode_manifest_path": "/run/privatemode/manifest.json",
            "privatemode_manifest_sha256": "a" * 64,
            "privatemode_proxy_image_digest": f"sha256:{'b' * 64}",
        }
    )


class LaunchAggregatorTests(unittest.TestCase):
    def test_privatemode_config_binds_generated_client_auth_digest(self) -> None:
        token = "per-run-client-token"
        config = build_gateway_config(
            [privatemode_provider()],
            {"PRIVATEMODE_API_KEY": "provider-credential"},
            port=18086,
            state_dir=Path("/tmp/state"),
            upstream_seed_path=Path("/tmp/upstreams.json"),
            dstack_endpoint="unix:/tmp/dstack.sock",
            inference_token=token,
            privatemode_credential_path=Path("/run/secrets/privatemode-api-key"),
        )

        self.assertEqual(
            config["inference_token_sha256"],
            hashlib.sha256(token.encode("utf-8")).hexdigest(),
        )
        self.assertNotIn(token, json.dumps(config, sort_keys=True))
        self.assertEqual(
            config["privatemode_proxy"]["credential_sha256"],
            hashlib.sha256(b"provider-credential").hexdigest(),
        )
        self.assertEqual(
            config["privatemode_proxy"]["credential_path"],
            "/run/secrets/privatemode-api-key",
        )
        upstream = build_upstream_config(
            [privatemode_provider()],
            {"PRIVATEMODE_API_KEY": "provider-credential"},
        )
        self.assertNotIn("bearer_token", upstream[0])
        self.assertNotIn("bearer_token", redact_upstream_config(upstream)[0])

    def test_process_generates_a_distinct_high_entropy_token_per_run(self) -> None:
        first = AggregatorProcess([], port=18086, env={})
        second = AggregatorProcess([], port=18087, env={})

        self.assertNotEqual(first.inference_token, second.inference_token)
        self.assertGreaterEqual(len(first.inference_token), 32)


if __name__ == "__main__":
    unittest.main()
