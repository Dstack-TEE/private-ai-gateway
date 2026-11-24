from __future__ import annotations

import unittest
from dataclasses import replace
from unittest.mock import patch

from scripts.live_e2e.common import ROOT, Provider
from scripts.live_e2e.provider_verify import (
    assert_strict_reference,
    provider_options,
    redact_verifier_request,
    verify_provider,
)


def near_image_provider() -> Provider:
    return Provider.from_json(
        {
            "name": "near-ai-live-images",
            "provider": "near-ai",
            "base_url": "https://cloud-api.near.ai",
            "public_model": "live-near-images",
            "upstream_model": "black-forest-labs/FLUX.2-klein-4B",
            "api_key_env": "NEARAI_API_KEY",
            "binding": "tls_spki_sha256",
        }
    )


class ProviderVerifyTests(unittest.TestCase):
    def test_near_options_use_the_configured_credential(self) -> None:
        provider = near_image_provider()
        self.assertEqual(provider_options(provider, {}), {})
        self.assertEqual(provider_options(provider, {"OTHER_KEY": "unused"}), {})
        self.assertEqual(
            provider_options(provider, {"NEARAI_API_KEY": "test-credential"}),
            {"near_ai_api_key": "test-credential"},
        )
        self.assertEqual(
            provider_options(
                replace(provider, api_key_env="CUSTOM_NEAR_KEY"),
                {"CUSTOM_NEAR_KEY": "custom-credential", "NEARAI_API_KEY": "unused"},
            ),
            {"near_ai_api_key": "custom-credential"},
        )

    def test_near_verification_passes_auth_but_redacts_artifacts(self) -> None:
        provider = near_image_provider()
        output = {
            "result": "verified",
            "evidence": {"digest": "test-digest", "data": "data:application/json,{}"},
            "channel_bindings": [{"type": "tls_spki_sha256"}],
        }
        with (
            patch(
                "scripts.live_e2e.provider_verify.run_cmd_json", return_value=output
            ) as run,
            patch("scripts.live_e2e.provider_verify.write_json") as write,
        ):
            verify_provider(
                provider,
                env={"NEARAI_API_KEY": "test-credential"},
                artifact_dir=ROOT / ".task-tmp/test-verifier",
                strict=True,
            )
        request = run.call_args.kwargs["input_value"]
        recorded = write.call_args_list[0].args[1]
        self.assertEqual(recorded["provider_options"]["near_ai_api_key"], "<redacted>")
        self.assertNotIn("test-credential", str(write.call_args_list))
        self.assertEqual(request["provider_options"]["near_ai_api_key"], "test-credential")

    def test_near_image_reference_requires_the_router_binding(self) -> None:
        provider = near_image_provider()
        with self.assertRaisesRegex(RuntimeError, "expected tls_spki_sha256"):
            assert_strict_reference(
                provider,
                {"channel_bindings": [{"type": "other"}]},
                ROOT / "scripts/live_e2e/provider_refs",
            )

    def test_redaction_preserves_nonsecret_options(self) -> None:
        request = {
            "provider_options": {"near_ai_api_key": "test-credential", "option": "value"}
        }
        self.assertEqual(
            redact_verifier_request(request, "NEARAI_API_KEY"),
            {
                "provider_options": {"near_ai_api_key": "<redacted>", "option": "value"},
                "api_key_env": "NEARAI_API_KEY",
            },
        )


if __name__ == "__main__":
    unittest.main()
