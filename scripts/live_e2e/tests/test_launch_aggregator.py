from __future__ import annotations

import sys
import unittest
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

from live_e2e.common import Provider  # noqa: E402
from live_e2e.launch_aggregator import AggregatorProcess  # noqa: E402


class LaunchAggregatorTests(unittest.TestCase):
    def test_privatemode_entries_are_rejected_because_they_require_client_e2ee(
        self,
    ) -> None:
        with self.assertRaisesRegex(ValueError, "client E2EE v2"):
            Provider.from_json(
                {
                    "name": "privatemode-test",
                    "provider": "privatemode",
                    "base_url": "http://privatemode-proxy:8080",
                    "public_model": "private-model",
                    "upstream_model": "upstream-model",
                    "api_key_env": "PRIVATEMODE_API_KEY",
                    "binding": "proxy_image_sha256",
                }
            )

    def test_process_generates_a_distinct_high_entropy_token_per_run(self) -> None:
        first = AggregatorProcess([], port=18086, env={})
        second = AggregatorProcess([], port=18087, env={})

        self.assertNotEqual(first.inference_token, second.inference_token)
        self.assertGreaterEqual(len(first.inference_token), 32)


if __name__ == "__main__":
    unittest.main()
