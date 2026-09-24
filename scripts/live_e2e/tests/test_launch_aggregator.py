from __future__ import annotations

import hashlib
import json
import unittest
from pathlib import Path

from scripts.live_e2e.common import Provider
from scripts.live_e2e.launch_aggregator import (
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
            "binding": "proxy_image_sha256",
            "privatemode_manifest_log_path": "/run/privatemode-manifests/log.txt",
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


if __name__ == "__main__":
    unittest.main()
