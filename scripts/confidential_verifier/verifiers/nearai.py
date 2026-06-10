from typing import Dict, Optional, Any
import json
import hashlib
import logging
import time
from .base import Verifier
from .dstack import DstackVerifier, verify_report_data
from ..types import VerificationResult
from .nvidia import NvidiaGpuVerifier
from .intel import IntelTdxVerifier

logger = logging.getLogger(__name__)


def _tdx_report_data_hex(quote: Any) -> Optional[str]:
    """Extract the 64-byte report_data from a raw Intel TDX v4 quote.

    The header is 48 bytes; report_data is the final field of the TD report body
    (body[520:584]) — the same offset used by the Chutes/Tinfoil/Intel verifiers.
    Returns None if the quote is missing or malformed so callers can fail closed.
    """
    if not quote:
        return None
    try:
        quote_bytes = bytes.fromhex(quote) if isinstance(quote, str) else bytes(quote)
    except (ValueError, TypeError):
        return None
    rd = quote_bytes[48 + 520 : 48 + 584]
    return rd.hex() if len(rd) == 64 else None


class NearAICloudVerifier(Verifier):
    def __init__(self, dstack_verifier_url: str = "http://localhost:8080"):
        self.dstack_verifier = DstackVerifier(service_url=dstack_verifier_url)
        self.nvidia_verifier = NvidiaGpuVerifier()

    def _verify_compose_hash(self, app_compose: str, expected_hash: str) -> bool:
        if not app_compose:
            return False

        # Calculate SHA256 of the raw app_compose string
        calculated_hash = hashlib.sha256(app_compose.encode("utf-8")).hexdigest()
        return calculated_hash.lower() == expected_hash.lower()

    async def _verify_component(
        self,
        name: str,
        attestation_data: Dict[str, Any],
        request_nonce: Optional[str] = None,
    ) -> Dict[str, Any]:

        results = {"name": name, "is_valid": False, "details": {}, "errors": []}

        try:
            quote = attestation_data.get("intel_quote")
            # Event log might be a JSON object or string, dstack verifier expects string if it's not None
            event_log = attestation_data.get("event_log")
            if isinstance(event_log, (dict, list)):
                event_log = json.dumps(event_log)

            info = attestation_data.get("info", {})
            tcb_info = info.get("tcb_info", {})
            if isinstance(tcb_info, str):
                try:
                    tcb_info = json.loads(tcb_info)
                except:
                    pass

            app_compose = tcb_info.get("app_compose")
            vm_config = info.get("vm_config")  # From gateway_attestation.info.vm_config
            if not vm_config:
                # Try tcb_info if not in info
                vm_config = tcb_info.get("vm_config")

            if isinstance(vm_config, (dict, list)):
                vm_config = json.dumps(vm_config)

            # 1. Dstack Verification (Quote, Event Log, OS Image)
            # Make the call Synchronous
            dstack_result = self.dstack_verifier.verify(
                quote=quote, event_log=event_log, vm_config=vm_config
            )

            # If dstack returns "is_valid": False, check if we can proceed? Usually no.
            results["details"]["dstack"] = dstack_result

            is_valid_dstack = dstack_result.get("is_valid", False)
            if not is_valid_dstack:
                results["errors"].append(
                    f"Dstack verification failed: {dstack_result.get('reason', 'unknown')}"
                )

            # 2. Compose Hash Verification
            reported_compose_hash = info.get("compose_hash")
            # If dstack verified quote, we might want to verify compose hash even if dstack failed?
            # Dstack failure might be due to collateral but integrity might be ok?
            # No, if dstack fails integrity check (quote invalid), then everything is suspect.
            # But let's proceed to check other things for diagnostic.

            compose_verified = False
            if app_compose and reported_compose_hash:
                compose_verified = self._verify_compose_hash(
                    app_compose, reported_compose_hash
                )
                if not compose_verified:
                    results["errors"].append("Compose hash mismatch")
            elif app_compose:
                # Optional warning
                pass

            results["details"]["compose_verified"] = compose_verified

            # 3. Report Data Verification (nonce, signing address, TLS SPKI binding)
            signing_address = attestation_data.get("signing_address")
            tls_cert_fingerprint = attestation_data.get("tls_cert_fingerprint")

            # report_data lives inside the quote the dstack verifier just proved
            # authentic, so parse it from those same verified quote bytes. Do NOT rely
            # on the dstack verifier to surface report_data -- it does not, and without
            # this check the request nonce, signing address, and TLS SPKI binding are
            # never validated (a replayed quote or a swapped TLS fingerprint would pass).
            report_data_hex = dstack_result.get("report_data") or _tdx_report_data_hex(quote)

            if not request_nonce or not signing_address:
                results["errors"].append(
                    "Missing request_nonce or signing_address; cannot verify report_data binding"
                )
            elif not report_data_hex:
                results["errors"].append(
                    "Could not obtain report_data from quote; cannot verify nonce/address/TLS binding"
                )
            else:
                rd_result = verify_report_data(
                    report_data_hex,
                    signing_address,
                    request_nonce,
                    tls_cert_fingerprint=tls_cert_fingerprint,
                )
                results["details"]["report_data_check"] = rd_result
                if not rd_result["valid"]:
                    results["errors"].append(
                        f"Report data check failed: {rd_result.get('error') or 'mismatch'}"
                    )

            # 4. GPU Verification
            nvidia_payload = attestation_data.get("nvidia_payload")
            if nvidia_payload:
                if isinstance(nvidia_payload, str):
                    try:
                        nvidia_payload = json.loads(nvidia_payload)
                    except:
                        pass

                gpu_nonce = nvidia_payload.get("nonce")
                if request_nonce and gpu_nonce:
                    if request_nonce.lower() != gpu_nonce.lower():
                        results["errors"].append(
                            f"GPU nonce mismatch: expected {request_nonce}, got {gpu_nonce}"
                        )

                # Nvidia verifier is async
                gpu_result = await self.nvidia_verifier.verify(nvidia_payload)
                try:
                    gpu_details = gpu_result.model_dump()
                except AttributeError:
                    gpu_details = gpu_result.dict()  # Fallback

                results["details"]["gpu"] = gpu_details

                if not gpu_result.model_verified:
                    results["errors"].append(
                        f"GPU verification failed: {gpu_result.error}"
                    )

            results["is_valid"] = (len(results["errors"]) == 0) and is_valid_dstack

        except Exception as e:
            logger.exception(f"Error verifying component {name}")
            results["errors"].append(str(e))

        return results

    async def verify(
        self,
        report_data: Dict[str, Any],
        request_nonce: Optional[str] = None,
        model_id: Optional[str] = None,
    ) -> VerificationResult:
        component_results = {}

        gateway_data = report_data.get("gateway_attestation")
        if not gateway_data:
            return VerificationResult(
                model_verified=False,
                provider="nearai",
                model_id=model_id,
                error="Missing gateway_attestation",
                timestamp=time.time(),
                hardware_type=[],
                claims={},
            )

        if not request_nonce:
            request_nonce = gateway_data.get("request_nonce")

        signing_address = gateway_data.get("signing_address")

        gateway_res = await self._verify_component(
            "gateway", gateway_data, request_nonce
        )
        component_results["gateway"] = gateway_res

        model_attestations = report_data.get("model_attestations", [])
        for i, model_data in enumerate(model_attestations):
            name = "model" if i == 0 else f"model-{i}"
            model_res = await self._verify_component(name, model_data, request_nonce)
            component_results[name] = model_res

        # Flatten and Clean up component details
        flattened_components = {}
        nvidia_claims = None
        report_data_check = None

        for comp_name, comp in component_results.items():
            flattened = {
                "is_valid": comp.get("is_valid", False),
            }
            if comp.get("errors"):
                flattened["errors"] = comp["errors"]

            # Flatten dstack
            dstack_info = comp.get("details", {}).get("dstack", {})
            if dstack_info:
                dstack_details = dstack_info.get("details", {})
                if dstack_details:
                    # Merge dstack info directly into the component
                    flattened.update(dstack_details)
                    # Remove huge raw quote if it's there
                    if "quote" in flattened:
                        del flattened["quote"]

            # Extract GPU if present -> Move to top-level "nvidia"
            gpu_info = comp.get("details", {}).get("gpu", {})
            if gpu_info:
                nvidia_claims = gpu_info.get("claims")

            # Extract report_data_check if present -> Move to top-level
            rd_check = comp.get("details", {}).get("report_data_check")
            if rd_check:
                report_data_check = rd_check

            flattened_components[comp_name] = flattened

        all_valid = all(C.get("is_valid", False) for C in component_results.values())
        errors = [
            err for C in component_results.values() for err in C.get("errors", [])
        ]

        model_verified = all_valid
        from ..types import HARDWARE_INTEL_TDX, HARDWARE_NVIDIA_CC

        hardware_types = [HARDWARE_INTEL_TDX]

        if nvidia_claims:
            hardware_types.append(HARDWARE_NVIDIA_CC)

        claims = {
            "components": flattened_components,
        }
        if nvidia_claims:
            claims["nvidia"] = nvidia_claims

        # 5. Optional: Intel Trust Authority appraisal for the main model quote
        if model_attestations and "intel_quote" in model_attestations[0]:
            try:
                quote_bytes = bytes.fromhex(model_attestations[0]["intel_quote"])
                ita_claims = await IntelTdxVerifier.verify_with_ita(quote_bytes)
                if ita_claims:
                    claims["intel_trust_authority"] = ita_claims
            except Exception as e:
                logger.warning(f"ITA appraisal failed in NearAICloudVerifier: {e}")

        return VerificationResult(
            model_verified=model_verified,
            provider="nearai",
            model_id=model_id,
            request_nonce=request_nonce,
            signing_address=signing_address,
            error="; ".join(errors) if errors else None,
            claims=claims,
            hardware_type=hardware_types,
            timestamp=time.time(),
        )

    async def verify_gateway_component(
        self,
        report_data: Dict[str, Any],
        request_nonce: Optional[str] = None,
    ) -> Dict[str, Any]:
        """Verify only the NEAR AI gateway component.

        Vendored-local addition (see scripts/confidential_verifier/VENDOR.md): the
        gateway bridge needs to verify the gateway workload in isolation, without
        re-verifying nested model attestations. Upstream only ever had this as an
        uncommitted edit, which is the drift this vendoring removes.
        """
        gateway_data = report_data.get("gateway_attestation")
        if not gateway_data:
            return {
                "name": "gateway",
                "is_valid": False,
                "details": {},
                "errors": ["Missing gateway_attestation"],
            }
        if not request_nonce:
            request_nonce = gateway_data.get("request_nonce")
        return await self._verify_component("gateway", gateway_data, request_nonce)
