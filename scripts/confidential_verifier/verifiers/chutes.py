import hashlib
import time
import base64
import jwt
import logging
from typing import Any, Dict, List, Optional
from .base import Verifier
from ..types import VerificationResult, HARDWARE_INTEL_TDX, HARDWARE_NVIDIA_CC

logger = logging.getLogger(__name__)


class ChutesVerifier(Verifier):
    """
    Offline verifier for Chutes AI TEE attestation.

    This verifier only validates pre-fetched data from AttestationReport.
    It does NOT make any network requests.

    The verification flow:
    1. Check pre-verified TDX result (status must be UpToDate)
    2. Check Debug mode is disabled (td_attributes bit 0 must be 0)
    3. Verify anti-tamper binding: hash(nonce + e2e_pubkey) matches report_data
    4. Validate pre-fetched NVIDIA GPU tokens
    """

    async def verify(self, evidence: Any) -> VerificationResult:
        """
        Verify Chutes TEE evidence for a single instance (offline).

        Args:
            evidence: Dict containing:
                - quote: Base64-encoded TDX quote
                - nonce: The nonce used for attestation request
                - e2e_pubkey: The instance's E2E public key
                - tdx_verification: Pre-verified TDX result from Provider
                - gpu_tokens: Pre-fetched NVIDIA tokens from Provider
                - instance_id: Optional instance identifier
        """
        if not isinstance(evidence, dict):
            return VerificationResult(
                model_verified=False,
                provider="chutes",
                timestamp=time.time(),
                hardware_type=[],
                claims={},
                error="Evidence must be a dict",
            )

        quote_b64 = evidence.get("quote")
        nonce = evidence.get("nonce")
        e2e_pubkey = evidence.get("e2e_pubkey")
        instance_id = evidence.get("instance_id")
        chute_id = evidence.get("chute_id")
        tdx_verification = evidence.get("tdx_verification", {})
        gpu_tokens = evidence.get("gpu_tokens", {})

        if not quote_b64:
            return VerificationResult(
                model_verified=False,
                provider="chutes",
                timestamp=time.time(),
                hardware_type=[],
                claims={},
                error="Missing TDX quote in evidence",
            )

        if not nonce or not e2e_pubkey:
            return VerificationResult(
                model_verified=False,
                provider="chutes",
                timestamp=time.time(),
                hardware_type=[],
                claims={},
                error="Missing nonce or e2e_pubkey for anti-tamper verification",
            )

        try:
            quote_bytes = base64.b64decode(quote_b64)
        except Exception as e:
            return VerificationResult(
                model_verified=False,
                provider="chutes",
                timestamp=time.time(),
                hardware_type=[],
                claims={},
                error=f"Failed to decode quote: {e}",
            )

        errors = []
        hardware_type = []
        claims: Dict[str, Any] = {}

        if instance_id:
            claims["instance_id"] = instance_id
        if chute_id:
            claims["chute_id"] = chute_id

        # Step 1: Check pre-verified TDX result
        tdx_result = tdx_verification.get("result")
        tdx_error = tdx_verification.get("error")

        if tdx_error:
            # TDX verification failed during fetch, but we can still do manual checks
            claims["tdx_online_verification"] = {"error": tdx_error}
            errors.append(f"TDX online verification failed: {tdx_error}")
        elif tdx_result:
            claims["tdx"] = tdx_result
            status = tdx_result.get("status", "Unknown")
            if status == "UpToDate":
                hardware_type.append(HARDWARE_INTEL_TDX)
            else:
                errors.append(f"TDX status is not UpToDate: {status}")

        # Step 2: Check Debug mode is disabled (manual check from quote bytes)
        td_attributes = self._extract_td_attributes(quote_bytes)
        if td_attributes:
            try:
                attr_value = int(td_attributes, 16)
                if attr_value & 1:
                    errors.append("CRITICAL: TDX environment is running in Debug mode")
                claims["td_attributes"] = td_attributes
                claims["debug_mode_disabled"] = not (attr_value & 1)
            except ValueError:
                errors.append(f"Invalid td_attributes format: {td_attributes}")

        # Step 3: Verify anti-tamper binding (nonce + e2e_pubkey -> report_data)
        expected_report_data = hashlib.sha256(
            (nonce + e2e_pubkey).encode()
        ).hexdigest().lower()

        # Extract report_data from quote bytes (offset 48+520 to 48+584)
        td_report_bytes = quote_bytes[48:632]
        report_data_hex = td_report_bytes[520:584].hex().lower()
        extracted_report_data = report_data_hex[:64]

        claims["expected_report_data"] = expected_report_data
        claims["actual_report_data"] = extracted_report_data
        claims["e2e_pubkey"] = e2e_pubkey

        if extracted_report_data != expected_report_data:
            errors.append(
                "Anti-tamper hash mismatch: E2E public key may have been tampered"
            )
        else:
            claims["e2e_binding_verified"] = True
            # Note: TDX hardware type is ONLY added when tdx_result.status == "UpToDate" (line 111)
            # Anti-tamper alone does not prove TDX hardware

        # Step 4: Validate pre-fetched NVIDIA GPU tokens (offline)
        # Pass expected_report_data as the expected nonce for replay protection
        if gpu_tokens:
            gpu_result = self._validate_gpu_tokens(gpu_tokens, expected_nonce=expected_report_data)
            claims["gpu"] = gpu_result

            if gpu_result.get("verified"):
                hardware_type.append(HARDWARE_NVIDIA_CC)
            elif gpu_result.get("error"):
                errors.append(f"GPU verification failed: {gpu_result['error']}")

        is_verified = len(errors) == 0
        error_msg = "; ".join(errors) if errors else None

        return VerificationResult(
            model_verified=is_verified,
            provider="chutes",
            timestamp=time.time(),
            hardware_type=hardware_type,
            model_id=chute_id,
            request_nonce=nonce,
            claims=claims,
            error=error_msg,
        )

    def _extract_td_attributes(self, quote_bytes: bytes) -> Optional[str]:
        """Extract td_attributes from TDX quote bytes."""
        try:
            # Header is 48 bytes. Body starts at 48.
            # td_attributes is at offset 120-128 in the body
            body = quote_bytes[48 : 48 + 584]
            return body[120:128].hex()
        except Exception:
            return None

    def _validate_gpu_tokens(self, gpu_tokens: Dict[str, Any], expected_nonce: Optional[str] = None) -> Dict[str, Any]:
        """
        Validate pre-fetched NVIDIA GPU tokens (offline).

        The tokens were fetched from NRAS during Provider.fetch_report().
        Here we decode and validate the JWT structure, including eat_nonce for replay protection.

        Args:
            gpu_tokens: Dict containing tokens and any fetch errors
            expected_nonce: Expected eat_nonce value (anti-tamper hash) for replay protection
        """
        tokens = gpu_tokens.get("tokens")
        fetch_error = gpu_tokens.get("error")

        if fetch_error:
            return {"verified": False, "error": fetch_error}

        if not tokens or not isinstance(tokens, list) or len(tokens) < 1:
            return {"verified": False, "error": "Invalid or missing GPU tokens"}

        try:
            # Platform token at index 0
            platform_entry = tokens[0]
            if not isinstance(platform_entry, list) or platform_entry[0] != "JWT":
                return {"verified": False, "error": "Invalid platform token format"}

            platform_jwt = platform_entry[1]
            # Decode JWT without signature verification (offline)
            # TODO: Cache NVIDIA JWKS and verify signature for full offline verification
            platform_claims = jwt.decode(
                platform_jwt,
                options={"verify_signature": False},
                algorithms=["RS256", "ES256", "ES384", "PS256"],
            )

            is_valid = platform_claims.get("x-nvidia-overall-att-result") is True

            # Verify eat_nonce for replay protection (CRITICAL)
            if expected_nonce:
                actual_nonce = platform_claims.get("eat_nonce")
                if not actual_nonce:
                    return {"verified": False, "error": "Missing eat_nonce in NRAS token - cannot verify replay protection"}
                if actual_nonce != expected_nonce:
                    return {
                        "verified": False,
                        "error": f"eat_nonce mismatch: expected {expected_nonce[:16]}..., got {actual_nonce[:16] if actual_nonce else 'None'}..."
                    }

            # Also decode GPU tokens if present
            gpu_claims = {}
            if len(tokens) > 1 and isinstance(tokens[1], dict):
                gpu_tokens_dict = tokens[1]
                if gpu_tokens_dict:
                    first_gpu_key = list(gpu_tokens_dict.keys())[0]
                    gpu_jwt = gpu_tokens_dict[first_gpu_key]
                    if isinstance(gpu_jwt, str):
                        gpu_claims = jwt.decode(
                            gpu_jwt,
                            options={"verify_signature": False},
                            algorithms=["RS256", "ES256", "ES384", "PS256"],
                        )

            return {
                "verified": is_valid,
                "platform_claims": platform_claims,
                "gpu_claims": gpu_claims,
                "nonce_verified": expected_nonce is not None,
                "error": None if is_valid else "NVIDIA attestation result is false",
            }

        except Exception as e:
            logger.warning(f"GPU token validation failed: {e}")
            return {"verified": False, "error": str(e)}

    async def verify_multiple_instances(
        self, instances_evidence: List[Dict[str, Any]], nonce: str, pubkeys: Dict[str, str]
    ) -> Dict[str, VerificationResult]:
        """
        Verify multiple Chutes instances (offline).

        Args:
            instances_evidence: List of evidence dicts, each containing:
                - instance_id: Instance identifier
                - quote: Base64-encoded TDX quote
                - tdx_verification: Pre-verified TDX result
                - gpu_tokens: Pre-fetched NVIDIA tokens
            nonce: The nonce used for attestation request
            pubkeys: Dict mapping instance_id to e2e_pubkey

        Returns:
            Dict mapping instance_id to VerificationResult
        """
        results = {}

        for instance_evidence in instances_evidence:
            instance_id = instance_evidence.get("instance_id")
            if not instance_id:
                continue

            e2e_pubkey = pubkeys.get(instance_id)
            if not e2e_pubkey:
                results[instance_id] = VerificationResult(
                    model_verified=False,
                    provider="chutes",
                    timestamp=time.time(),
                    hardware_type=[],
                    claims={"instance_id": instance_id},
                    error="Missing E2E public key for this instance",
                )
                continue

            evidence = {
                **instance_evidence,
                "nonce": nonce,
                "e2e_pubkey": e2e_pubkey,
            }

            results[instance_id] = await self.verify(evidence)

        return results
