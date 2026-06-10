import time
import requests
import json
import base64
import logging
from typing import Any, List, Dict, Optional
from .intel import IntelTdxVerifier
from .base import Verifier
from ..types import VerificationResult, HARDWARE_INTEL_TDX, HARDWARE_AMD_SEV_SNP

logger = logging.getLogger(__name__)

# Hardcoded Tinfoil Policy Values for TDX
# derived from https://github.com/tinfoilsh/verifier/blob/main/attestation/tdx.go

# Accepted MR_SEAM values (TDX Module hash) for Tinfoil's environment
# From https://github.com/tinfoilsh/verifier/blob/main/attestation/tdx.go
ACCEPTED_MR_SEAMS = [
    # TDX Module 1.5.08
    "49b66faa451d19ebbdbe89371b8daf2b65aa3984ec90110343e9e2eec116af08850fa20e3b1aa9a874d77a65380ee7e6",
    # TDX Module 2.0.02
    "685f891ea5c20e8fa27b151bf34bf3b50fbaf7143cc53662727cbdb167c0ad8385f1f6f3571539a91e104a1c96d75e04",
    # TDX Module 1.5.16
    "7bf063280e94fb051f5dd7b1fc59ce9aac42bb961df8d44b709c9b0ff87a7b4df648657ba6d1189589feab1d5a3c9a9d",
    # TDX Module 2.0.08
    "476a2997c62bccc7837091dd58dc7c24c28ba26927f93e00e7e1997d68e3d5bb9a023c3ec0d7c0e5a29929fe5dd282ec",
]

# Expected TdAttributes: Debug must be disabled (bit 1 off) among other flags
EXPECTED_TD_ATTRIBUTES = "0000001000000000"

# Expected XFAM (Extended Features available to Guest)
# Matches Tinfoil's QEMU configuration for TDX guests
EXPECTED_XFAM = "e702060000000000"

# Zero constants for validation
ZERO_48 = "00" * 48
RTMR3_ZERO = "00" * 48


class TinfoilSevVerifier(Verifier):
    """
    Unified Tinfoil verifier supporting both TDX and SEV-SNP attestations.
    """

    async def verify(self, quote: Any) -> VerificationResult:
        """
        Verify Tinfoil attestation.

        Automatically detects the attestation type (TDX or SEV-SNP) and
        applies the appropriate verification logic.
        """
        if isinstance(quote, dict):
            quote_type = quote.get("quote_type", "unknown")
            raw_data = quote
        else:
            quote_type = "unknown"
            raw_data = {"quote": quote}

        if quote_type == "sev-snp":
            return await self._verify_snp(quote)
        elif quote_type == "tdx":
            # Use the existing TDX verifier
            tdx_verifier = TinfoilTdxVerifier()
            return await tdx_verifier.verify(quote)
        else:
            # Try to auto-detect based on format field
            fmt = raw_data.get("format", "")
            if "sev-snp" in fmt:
                return await self._verify_snp(quote)
            elif "tdx" in fmt:
                tdx_verifier = TinfoilTdxVerifier()
                return await tdx_verifier.verify(quote)
            else:
                return VerificationResult(
                    model_verified=False,
                    provider="tinfoil",
                    timestamp=time.time(),
                    hardware_type=[],
                    claims={"format": fmt},
                    error=f"Unknown attestation format: {fmt}",
                )

    async def _verify_snp(self, quote: Any) -> VerificationResult:
        """
        Verify AMD SEV-SNP attestation from Tinfoil.

        SEV-SNP verification is simpler as the hardware handles most of the
        integrity checks. We focus on:
        1. Parsing the attestation report
        2. Manifest comparison against Sigstore golden values
        """
        if isinstance(quote, dict):
            quote_hex = quote.get("quote", quote.get("intel_quote", ""))
            raw_data = quote
        elif isinstance(quote, str):
            quote_hex = quote
            raw_data = {}
        elif isinstance(quote, bytes):
            quote_hex = quote.hex()
            raw_data = {}
        else:
            return VerificationResult(
                model_verified=False,
                provider="tinfoil",
                timestamp=time.time(),
                hardware_type=[],
                claims={},
                error="Invalid quote format",
            )

        try:
            quote_bytes = bytes.fromhex(quote_hex) if quote_hex else b""
        except Exception as e:
            return VerificationResult(
                model_verified=False,
                provider="tinfoil",
                timestamp=time.time(),
                hardware_type=[],
                claims={},
                error=f"Failed to parse quote: {e}",
            )

        used_router = raw_data.get("used_router", False)
        claims: Dict[str, Any] = {
            "attestation_type": "sev-snp",
            "quote_length": len(quote_bytes),
            "used_router": used_router,
        }

        errors = []
        repo = raw_data.get("repo")

        # Parse SNP report structure
        snp_claims = self._parse_snp_report(quote_bytes)
        claims.update(snp_claims)

        # Check for parse errors - if parsing failed, verification must fail
        if "parse_error" in snp_claims:
            errors.append(f"SNP report parsing failed: {snp_claims['parse_error']}")

        # Repo is required for manifest comparison - without it we can't verify measurements
        if not repo:
            errors.append("Missing repo for manifest comparison - cannot verify measurements")

        # Check manifest if repo is provided and no parse errors
        if repo and "parse_error" not in snp_claims:
            try:
                self._check_snp_manifest(snp_claims, repo, errors)
                claims["repo"] = repo
                if "hw_profile" in snp_claims:
                    claims["hw_profile"] = snp_claims["hw_profile"]
            except Exception as e:
                logger.warning(f"SNP manifest check failed: {e}")
                errors.append(f"Manifest check failed: {e}")

        model_verified = len(errors) == 0
        error_msg = "; ".join(errors) if errors else None

        # Only claim hardware_type if verification succeeded
        hardware_type = [HARDWARE_AMD_SEV_SNP] if model_verified else []

        return VerificationResult(
            model_verified=model_verified,
            provider="tinfoil",
            timestamp=time.time(),
            hardware_type=hardware_type,
            model_id=raw_data.get("model_id"),
            claims=claims,
            error=error_msg,
        )

    def _parse_snp_report(self, quote_bytes: bytes) -> Dict[str, Any]:
        """
        Parse AMD SEV-SNP attestation report.

        SNP report structure (simplified):
        - Version, Guest SVN, Policy at the start
        - Measurement at offset 0x90 (48 bytes)
        - Report data at offset 0x50 (64 bytes)
        """
        if len(quote_bytes) < 0x100:
            return {"parse_error": "Quote too short for SNP format"}

        try:
            return {
                "version": int.from_bytes(quote_bytes[0:4], "little"),
                "guest_svn": int.from_bytes(quote_bytes[4:8], "little"),
                "policy": quote_bytes[8:16].hex(),
                "measurement": quote_bytes[0x90:0xC0].hex(),
                "report_data": quote_bytes[0x50:0x90].hex(),
            }
        except Exception as e:
            return {"parse_error": str(e)}

    def _check_snp_manifest(
        self, claims: Dict[str, Any], repo: str, errors: List[str]
    ):
        """Check SNP measurements against Sigstore golden values."""
        # Fetch golden measurements
        bundle = self._fetch_sigstore_bundle(repo)
        payload = self._extract_payload(bundle)

        if not isinstance(payload, dict):
            errors.append(f"Invalid payload format: expected dict, got {type(payload).__name__}")
            return

        predicate_type = payload.get("predicateType", "")

        if not predicate_type:
            errors.append("Failed to fetch or parse Sigstore golden values")
            return

        if "snp-tdx-multiplatform" in predicate_type:
            snp = payload.get("predicate", {}).get("snp_measurement")
            # snp_measurement can be either a string (direct measurement) or a dict with "measurement" key
            if isinstance(snp, str):
                expected_measurement = snp
            elif isinstance(snp, dict):
                expected_measurement = snp.get("measurement")
            else:
                expected_measurement = None
            actual_measurement = claims.get("measurement")

            if not expected_measurement:
                errors.append("Golden SNP measurement not found in Sigstore bundle")
            elif not actual_measurement:
                errors.append("Actual SNP measurement missing from attestation")
            elif expected_measurement != actual_measurement:
                errors.append(
                    f"SNP measurement mismatch: expected {expected_measurement[:16]}..., got {actual_measurement[:16]}..."
                )
        elif "sev-snp-guest" in predicate_type:
            # Direct SNP predicate format
            snp = payload.get("predicate", {})
            # predicate can be a dict with "measurement" key or other format
            if isinstance(snp, dict):
                expected_measurement = snp.get("measurement")
            elif isinstance(snp, str):
                expected_measurement = snp
            else:
                expected_measurement = None
            actual_measurement = claims.get("measurement")

            if not expected_measurement:
                errors.append("Golden SNP measurement not found in Sigstore bundle")
            elif not actual_measurement:
                errors.append("Actual SNP measurement missing from attestation")
            elif expected_measurement != actual_measurement:
                errors.append(
                    f"SNP measurement mismatch: expected {expected_measurement[:16]}..., got {actual_measurement[:16]}..."
                )
        else:
            errors.append(f"Unknown Sigstore predicate type: {predicate_type}")

    def _fetch_sigstore_bundle(self, repo: str) -> Dict[str, Any]:
        """Fetch Sigstore attestation bundle for a repository."""
        try:
            url_latest = (
                f"https://api-github-proxy.tinfoil.sh/repos/{repo}/releases/latest"
            )
            resp = requests.get(url_latest, timeout=10)
            resp.raise_for_status()
            tag = resp.json().get("tag_name")

            url_hash = f"https://api-github-proxy.tinfoil.sh/{repo}/releases/download/{tag}/tinfoil.hash"
            resp_hash = requests.get(url_hash, timeout=10)
            resp_hash.raise_for_status()
            digest = resp_hash.text.strip()

            url_attestation = f"https://gh-attestation-proxy.tinfoil.sh/repos/{repo}/attestations/sha256:{digest}"
            resp_att = requests.get(url_attestation, timeout=10)
            resp_att.raise_for_status()
            att_data = resp_att.json()

            if "attestations" in att_data and len(att_data["attestations"]) > 0:
                return att_data["attestations"][0].get("bundle", {})
        except Exception as e:
            logger.warning(f"Failed to fetch Sigstore bundle for {repo}: {e}")
        return {}

    def _extract_payload(self, bundle: Dict[str, Any]) -> Dict[str, Any]:
        """Extract payload from Sigstore bundle."""
        try:
            payload_b64 = bundle.get("dsseEnvelope", {}).get("payload")
            if payload_b64:
                result = json.loads(base64.b64decode(payload_b64).decode("utf-8"))
                if isinstance(result, dict):
                    return result
                logger.warning(f"Unexpected payload type: {type(result)}")
        except Exception as e:
            logger.warning(f"Failed to extract payload: {e}")
        return {}


class TinfoilTdxVerifier(IntelTdxVerifier):
    async def verify(self, quote: Any) -> VerificationResult:
        # Use a temporary dict for policy check that contains low-level info
        # because the base IntelTdxVerifier._format_result already removed them from result.claims
        # Wait, if I call super().verify(quote), and it succeeds, it returns a result with clean claims.
        # But tinfoil needs those claims to check policy.

        # Let's override verify to do manual parse for policy check first or use the raw result if we had it.
        # Actually, let's just use the manual parse for now to get the fields needed for policy.
        if isinstance(quote, str):
            quote_bytes = bytes.fromhex(quote)
        elif isinstance(quote, bytes):
            quote_bytes = quote
        elif isinstance(quote, dict):
            quote_hex = quote.get("quote", "")
            quote_bytes = (
                bytes.fromhex(quote_hex) if isinstance(quote_hex, str) else quote_hex
            )
        else:
            return VerificationResult(
                model_verified=False,
                provider="tinfoil",
                timestamp=time.time(),
                hardware_type=[],
                claims={},
                error="Quote must be hex string, bytes, or dict",
            )

        internal_claims = self._manual_parse_tdx(quote_bytes)

        # Now call base verify to get the official verification status
        result = await super().verify(quote)
        result.provider = "tinfoil"

        # The base verifier might have set repo if it was in the quote dict
        repo = internal_claims.get("repo") or (
            quote.get("repo") if isinstance(quote, dict) else None
        )
        reasons = []

        # 1. Hardware Policy (MrSeam, Attributes, Xfam, Zero fields)
        self._check_hardware_policy(internal_claims, reasons)

        # 2. Manifest Comparison (Golden Values)
        if repo:
            try:
                await self._check_manifest_policy(internal_claims, repo, reasons)
                # If hw_profile was found, add it to the final result claims
                if "hw_profile" in internal_claims:
                    result.claims["hw_profile"] = internal_claims["hw_profile"]
                result.claims["repo"] = repo
            except Exception as e:
                reasons.append(f"Manifest check failed: {e}")

        if reasons:
            result.model_verified = False
            result.error = (
                (result.error + "; " if result.error else "")
                + "Policy violation: "
                + ", ".join(reasons)
            )

        # 3. Optional: Intel Trust Authority appraisal (redundant but ensure it stays in claims)
        if "intel_trust_authority" not in result.claims:
            ita_claims = await self.verify_with_ita(quote_bytes)
            if ita_claims:
                result.claims["intel_trust_authority"] = ita_claims

        return result

    def _check_hardware_policy(self, claims: Dict[str, Any], reasons: List[str]):
        mr_seam = claims.get("mr_seam", "")
        if mr_seam not in ACCEPTED_MR_SEAMS:
            reasons.append(f"Invalid MrSeam: {mr_seam}")

        if claims.get("td_attributes", "") != EXPECTED_TD_ATTRIBUTES:
            reasons.append(f"Invalid TdAttributes: {claims.get('td_attributes')}")

        if claims.get("xfam", "") != EXPECTED_XFAM:
            reasons.append(f"Invalid Xfam: {claims.get('xfam')}")

        if claims.get("mr_owner", "") != ZERO_48:
            reasons.append("mr_owner is not zero")

        if claims.get("mr_owner_config", "") != ZERO_48:
            reasons.append("mr_owner_config is not zero")

        # RTMR3 must be zero
        rtmr3 = claims.get("rt_mr3", "")
        if rtmr3 and rtmr3 != RTMR3_ZERO:
            reasons.append("RTMR3 is not zeroed")

    async def _check_manifest_policy(
        self, claims: Dict[str, Any], repo: str, reasons: List[str]
    ):
        # 1. Fetch Image measurements (RTMR1, RTMR2)
        golden_image = self._fetch_golden_measurements(repo)

        if not golden_image or not golden_image.get("rtmr1"):
            reasons.append(f"Failed to fetch golden measurements for {repo}")
            return

        actual_rtmr1 = claims.get("rt_mr1")
        actual_rtmr2 = claims.get("rt_mr2")

        if golden_image.get("rtmr1") != actual_rtmr1:
            reasons.append(
                f"RTMR1 mismatch: expected {golden_image.get('rtmr1')}, got {actual_rtmr1}"
            )

        if golden_image.get("rtmr2") != actual_rtmr2:
            reasons.append(
                f"RTMR2 mismatch: expected {golden_image.get('rtmr2')}, got {actual_rtmr2}"
            )

        # 2. Fetch Hardware measurements (MRTD, RTMR0) and match profile
        hw_profiles = self._fetch_hardware_measurements()

        actual_mrtd = claims.get("mr_td")
        actual_rtmr0 = claims.get("rt_mr0")

        found_profile = False
        for profile_name, values in hw_profiles.items():
            if (
                values.get("mrtd") == actual_mrtd
                and values.get("rtmr0") == actual_rtmr0
            ):
                found_profile = True
                claims["hw_profile"] = profile_name
                break

        if not found_profile:
            mrtd_str = actual_mrtd[:8] if actual_mrtd else "None"
            rtmr0_str = actual_rtmr0[:8] if actual_rtmr0 else "None"
            reasons.append(
                f"No matching hardware profile found for MRTD={mrtd_str}... RTMR0={rtmr0_str}..."
            )

    def _fetch_golden_measurements(self, repo: str) -> Dict[str, str]:
        # Tinfoil's image attestations: SnpTdxMultiPlatformV1
        bundle = self._fetch_sigstore_bundle(repo)
        payload = self._extract_payload(bundle)

        if (
            payload.get("predicateType")
            == "https://tinfoil.sh/predicate/snp-tdx-multiplatform/v1"
        ):
            tdx = payload.get("predicate", {}).get("tdx_measurement", {})
            return {
                "rtmr1": tdx.get("rtmr1"),
                "rtmr2": tdx.get("rtmr2"),
            }
        return {}

    def _fetch_hardware_measurements(self) -> Dict[str, Dict[str, str]]:
        repo = "tinfoilsh/hardware-measurements"
        bundle = self._fetch_sigstore_bundle(repo)
        payload = self._extract_payload(bundle)

        profiles = {}
        if (
            payload.get("predicateType")
            == "https://tinfoil.sh/predicate/hardware-measurements/v1"
        ):
            predicate = payload.get("predicate", {})
            for name, values in predicate.items():
                profiles[name] = {
                    "mrtd": values.get("mrtd"),
                    "rtmr0": values.get("rtmr0"),
                }
        return profiles

    def _fetch_sigstore_bundle(self, repo: str) -> Dict[str, Any]:
        try:
            url_latest = (
                f"https://api-github-proxy.tinfoil.sh/repos/{repo}/releases/latest"
            )
            resp = requests.get(url_latest, timeout=10)
            resp.raise_for_status()
            tag = resp.json().get("tag_name")

            url_hash = f"https://api-github-proxy.tinfoil.sh/{repo}/releases/download/{tag}/tinfoil.hash"
            resp_hash = requests.get(url_hash, timeout=10)
            resp_hash.raise_for_status()
            digest = resp_hash.text.strip()

            url_attestation = f"https://gh-attestation-proxy.tinfoil.sh/repos/{repo}/attestations/sha256:{digest}"
            resp_att = requests.get(url_attestation, timeout=10)
            resp_att.raise_for_status()
            att_data = resp_att.json()

            if "attestations" in att_data and len(att_data["attestations"]) > 0:
                return att_data["attestations"][0].get("bundle", {})
        except Exception as e:
            print(f"Warning: Failed to fetch Sigstore bundle for {repo}: {e}")
        return {}

    def _extract_payload(self, bundle: Dict[str, Any]) -> Dict[str, Any]:
        try:
            payload_b64 = bundle.get("dsseEnvelope", {}).get("payload")
            if payload_b64:
                result = json.loads(base64.b64decode(payload_b64).decode("utf-8"))
                if isinstance(result, dict):
                    return result
                print(f"Warning: Unexpected payload type: {type(result)}")
        except Exception as e:
            print(f"Warning: Failed to extract payload: {e}")
        return {}
