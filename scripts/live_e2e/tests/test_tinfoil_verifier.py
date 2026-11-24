import base64
import contextlib
import io
import json
import unittest
from unittest.mock import patch

from tinfoil import SecureClient, VerificationDocument

from scripts.provider_verifier.tinfoil import verify_tinfoil


class TinfoilVerifierTests(unittest.IsolatedAsyncioTestCase):
    async def test_release_digest_pin_and_tag(self) -> None:
        digest = "ab" * 32
        doc = VerificationDocument(
            config_repo="tinfoilsh/confidential-model-router",
            security_verified=True,
            tls_public_key="de" * 32,
            release_tag="v1.2.3",
        )
        for observed_digest, pins, accepted in (
            (digest, [], True),
            (digest, ["cd" * 32, digest], True),
            (digest, ["cd" * 32], False),
            ("", [digest], False),
        ):
            with self.subTest(observed_digest=observed_digest, pins=pins):
                doc.release_digest = observed_digest
                request = {
                    "url_origin": "https://router.example",
                    "provider_options": {
                        f"tinfoil_accepted_subject:tinfoil-release:sha256:{pin}": "true"
                        for pin in pins
                    },
                }
                output = io.StringIO()
                with (
                    patch.object(SecureClient, "verify"),
                    patch.object(SecureClient, "get_verification_document", return_value=doc),
                    contextlib.redirect_stdout(output),
                ):
                    await verify_tinfoil(request)

                result = json.loads(output.getvalue())
                data_uri = result["evidence"]["data"]
                evidence = json.loads(base64.b64decode(data_uri.split(",", 1)[1]))
                self.assertEqual(evidence["release_tag"], "v1.2.3")
                if accepted:
                    self.assertEqual(result["result"], "verified")
                    self.assertEqual(result["provider_claims"]["release_tag"], "v1.2.3")
                    self.assertEqual(result["channel_bindings"][0]["spki_sha256"], doc.tls_public_key)
                else:
                    self.assertEqual(result["result"], "failed")
                    self.assertEqual(result["failure"], "provenance_not_accepted")
                    self.assertNotIn("channel_bindings", result)
                    self.assertNotIn("provider_claims", result)
