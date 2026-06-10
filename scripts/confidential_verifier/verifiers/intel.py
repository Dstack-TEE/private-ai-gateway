import dcap_qvl
import time
import json
import os
import requests
import base64
import jwt
from typing import Any, Dict, Optional
from ..types import VerificationResult
from .base import Verifier
from dotenv import load_dotenv

load_dotenv()

ITA_API_KEY = os.getenv("INTEL_TRUST_AUTHORITY_API_KEY")
ITA_URL = "https://api.trustauthority.intel.com/appraisal/v2/attest"


class IntelTdxVerifier(Verifier):
    async def verify(self, quote: Any) -> VerificationResult:
        model_id = None
        repo = None
        if isinstance(quote, str):
            quote_bytes = bytes.fromhex(quote)
        elif isinstance(quote, bytes):
            quote_bytes = quote
        elif isinstance(quote, dict):
            quote_hex = quote.get("quote", "")
            quote_bytes = (
                bytes.fromhex(quote_hex) if isinstance(quote_hex, str) else quote_hex
            )
            model_id = quote.get("model_id")
            repo = quote.get("repo")
        else:
            raise ValueError("Quote must be hex string, bytes, or dict")

        try:
            result = await dcap_qvl.get_collateral_and_verify(quote_bytes)
            res = self._format_result(result)
            if model_id:
                res.model_id = model_id
            if repo:
                res.claims["repo"] = repo

            # Optional: Intel Trust Authority appraisal
            if ITA_API_KEY:
                ita_claims = await self.verify_with_ita(quote_bytes)
                if ita_claims:
                    res.claims["intel_trust_authority"] = ita_claims

            return res
        except Exception as e:
            # Best effort: manual parse for registers if verification fails
            # We still keep the claims clean in the final result, but
            # TinfoilTdxVerifier might need these fields for policy check.
            # So we keep the internal parsing but we will filter it out in the final result if needed.
            # Actually, let's keep the manual parse results in claims for NOW,
            # and let the subclasses or final formatter clean it up.
            claims = self._manual_parse_tdx(quote_bytes)
            if repo:
                claims["repo"] = repo

            # Ensure status is at least present
            claims["status"] = "Error"

            # Optional: Intel Trust Authority appraisal even on local failure
            if ITA_API_KEY:
                ita_claims = await self.verify_with_ita(quote_bytes)
                if ita_claims:
                    claims["intel_trust_authority"] = ita_claims

            return VerificationResult(
                model_verified=False,
                provider="intel",
                timestamp=time.time(),
                hardware_type=[],  # Don't claim hardware type on failure
                model_id=model_id,
                claims=claims,
                error=f"Verification failed: {e}",
            )

    def _format_result(self, result: Any) -> VerificationResult:
        is_success = result.status in [
            "UpToDate",
            "SWHardeningNeeded",
            "ConfigurationNeeded",
            "ConfigurationAndSWHardeningNeeded",
        ]

        claims = {
            "status": result.status,
            "advisory_ids": getattr(result, "advisory_ids", []),
        }

        if not is_success:
            return VerificationResult(
                model_verified=False,
                provider="intel",
                timestamp=time.time(),
                hardware_type=[],  # Don't claim hardware type on failure
                claims=claims,
                error=f"Verification failed with status: {result.status}",
            )

        return VerificationResult(
            model_verified=True,
            provider="intel",
            timestamp=time.time(),
            hardware_type=["INTEL_TDX"],
            claims=claims,
        )

    @staticmethod
    async def verify_with_ita(quote_bytes: bytes) -> Optional[Dict[str, Any]]:
        """Appraise the quote using Intel Trust Authority API."""
        if not ITA_API_KEY:
            return None
        try:
            quote_base64 = base64.b64encode(quote_bytes).decode("utf-8")
            headers = {
                "Accept": "application/json",
                "x-api-key": ITA_API_KEY,
                "Content-Type": "application/json",
            }
            data = {"tdx": {"quote": quote_base64}}

            response = requests.post(ITA_URL, headers=headers, json=data)
            if response.status_code == 200:
                token = response.json().get("token")
                if token:
                    # TODO: Fetch JWKS from Intel and enable signature verification for better security.
                    # Currently trusting the secure HTTPS connection + API Key.
                    return jwt.decode(
                        token,
                        options={"verify_signature": False},
                        algorithms=["RS256", "ES256", "ES384", "PS256"],
                    )
            return None
        except Exception:
            # Silent failure for optional ITA verification
            return None

    def _manual_parse_tdx(self, quote_bytes: bytes) -> Dict[str, Any]:
        """Manually extract TDX V4 fields from quote bytes."""
        try:
            # Header is 48 bytes. Body starts at 48.
            body = quote_bytes[48 : 48 + 584]
            return {
                "tee_tcb_svn": body[0:16].hex(),
                "mr_seam": body[16:64].hex(),
                "mr_signer_seam": body[64:112].hex(),
                "seam_attributes": body[112:120].hex(),
                "td_attributes": body[120:128].hex(),
                "xfam": body[128:136].hex(),
                "mr_td": body[136:184].hex(),
                "mr_config_id": body[184:232].hex(),
                "mr_owner": body[232:280].hex(),
                "mr_owner_config": body[280:328].hex(),
                "rt_mr0": body[328:376].hex(),
                "rt_mr1": body[376:424].hex(),
                "rt_mr2": body[424:472].hex(),
                "rt_mr3": body[472:520].hex(),
                "report_data": body[520:584].hex(),
                "registers": [
                    body[136:184].hex(),
                    body[328:376].hex(),
                    body[376:424].hex(),
                    body[424:472].hex(),
                    body[472:520].hex(),
                ],
            }
        except Exception:
            return {}
