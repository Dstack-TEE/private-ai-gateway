import logging
import requests
import json
import time
from typing import Dict, Any, Optional
import hashlib
from urllib.parse import urlparse
from ..types import VerificationResult
from .base import Verifier
from .dstack import DstackVerifier
from .nvidia import NvidiaGpuVerifier
from .intel import IntelTdxVerifier

logger = logging.getLogger(__name__)


class PhalaCloudVerifier(Verifier):
    """
    Internal verifier for Phala Cloud Apps.
    Verifies dstack TEE environment (App/KMS/Gateway components) and optionally GPU.

    This is an internal class used by RedpillVerifier. Users should call
    RedpillVerifier directly for Redpill model verification.
    """

    def __init__(
        self,
        app_id: str,
        dstack_verifier_url: Optional[str] = None,
    ):
        self.app_id = app_id
        self.dstack_verifier = DstackVerifier(service_url=dstack_verifier_url)
        self.nvidia_verifier = NvidiaGpuVerifier()
        self.system_info: Optional[Dict[str, Any]] = None

    @staticmethod
    def get_system_info(app_id: str) -> Dict[str, Any]:
        """Fetches system info from Phala Cloud API."""
        url = f"https://cloud-api.phala.network/api/v1/apps/{app_id}/attestations"
        try:
            response = requests.get(url)
            response.raise_for_status()
            data = response.json()
            return data
        except Exception as e:
            logger.error(
                f"Failed to fetch system info from Phala Cloud for app {app_id}: {e}"
            )
            raise

    def _verify_component(
        self,
        name: str,
        quote: str,
        event_log: Any,
        vm_config: Any,
        app_compose: Optional[str] = None,
    ) -> Dict[str, Any]:
        """Verifies a single component (App, KMS, or Gateway)."""
        # Ensure event_log and vm_config are JSON strings (as required by dstack-verifier)
        if not isinstance(event_log, str):
            event_log = json.dumps(event_log)
        if not isinstance(vm_config, str):
            vm_config = json.dumps(vm_config)

        # 1. Verify dstack environment (quote, os_image_hash, event_log)
        dstack_result = self.dstack_verifier.verify(quote, event_log, vm_config)

        is_valid = dstack_result.get("is_valid", False)
        details = dstack_result.get("details", {})
        reason = dstack_result.get("reason")

        # 2. Verify compose_hash if app_compose is provided
        compose_ok = True
        if is_valid and app_compose:
            app_info = details.get("app_info", {})
            expected_compose_hash = app_info.get("compose_hash")
            if expected_compose_hash:
                # Calculated SHA256 of the app_compose string
                actual_compose_hash = hashlib.sha256(app_compose.encode()).hexdigest()
                if actual_compose_hash != expected_compose_hash:
                    is_valid = False
                    compose_ok = False
                    reason = f"Compose hash mismatch for {name}: expected {expected_compose_hash}, got {actual_compose_hash}"
                    logger.error(reason)

        return {
            "name": name,
            "is_valid": is_valid,
            "compose_verified": compose_ok,
            "details": details,
            "reason": reason,
        }

    async def verify(
        self,
        system_info: Optional[Dict[str, Any]] = None,
        nvidia_payload: Optional[Dict[str, Any]] = None,
    ) -> VerificationResult:
        try:
            # 1. Fetch System Info (if not provided)
            if system_info:
                self.system_info = system_info
            else:
                self.system_info = self.get_system_info(self.app_id)

            if not self.system_info.get("instances"):
                raise Exception("No instances found for this app.")

            # 2. Collect components to verify
            # Use the first instance for the main app
            instance = self.system_info["instances"][0]

            # Authoritatively fetch AppInfo from PRPC Info endpoint for Main App
            main_app_vm_config = None
            main_app_compose = None

            kms_url = self.system_info.get("kms_info", {}).get("url", "")
            if kms_url:
                parsed_kms = urlparse(kms_url)
                netloc_parts = parsed_kms.netloc.split(".")
                domain = (
                    ".".join(netloc_parts[-3:])
                    if len(netloc_parts) >= 3
                    else parsed_kms.netloc
                )
                rpc_endpoint = f"https://{self.app_id}-8090.{domain}/prpc/Info"
                logger.info(f"Fetching authoritative Main App Info from {rpc_endpoint}")
                try:
                    resp = requests.post(
                        rpc_endpoint,
                        headers={"Content-Type": "application/json"},
                        json={},
                        timeout=10,
                    )
                    if resp.status_code == 200:
                        app_prpc_info = resp.json()
                        main_app_vm_config = app_prpc_info.get("vm_config")
                        # tcb_info in AppInfo from PRPC is an escaped JSON string
                        tcb_info_str = app_prpc_info.get("tcb_info")
                        if tcb_info_str:
                            tcb_info = json.loads(tcb_info_str)
                            main_app_compose = tcb_info.get("app_compose")
                    else:
                        logger.warn(
                            f"Failed to fetch AppInfo from PRPC: {resp.status_code}"
                        )
                except Exception as e:
                    logger.warn(f"PRPC request failed: {e}")

            # Fallback for Main App vm_config if PRPC failed
            if not main_app_vm_config:
                main_app_vm_config = self.system_info.get("vm_config")

            # Define component names in lower case as requested
            MODEL_COMPONENT = "model"
            KMS_COMPONENT = "key management service"
            GATEWAY_COMPONENT = "gateway"

            components = [
                {
                    "name": MODEL_COMPONENT,
                    "quote": instance.get("quote"),
                    "event_log": instance.get("eventlog"),
                    "vm_config": main_app_vm_config,
                    "app_compose": main_app_compose,
                }
            ]

            # KMS
            kms_info = self.system_info.get("kms_guest_agent_info")
            if kms_info:
                tcb = kms_info.get("tcb_info", {})
                components.append(
                    {
                        "name": KMS_COMPONENT,
                        "quote": kms_info.get("app_certificates", [{}])[0].get("quote"),
                        "event_log": tcb.get("event_log"),
                        "vm_config": kms_info.get("vm_config"),
                        "app_compose": tcb.get("app_compose"),
                    }
                )

            # Gateway
            gw_info = self.system_info.get("gateway_guest_agent_info")
            if gw_info:
                tcb = gw_info.get("tcb_info", {})
                components.append(
                    {
                        "name": GATEWAY_COMPONENT,
                        "quote": gw_info.get("app_certificates", [{}])[0].get("quote"),
                        "event_log": tcb.get("event_log"),
                        "vm_config": gw_info.get("vm_config"),
                        "app_compose": tcb.get("app_compose"),
                    }
                )

            # 3. Verify all components
            results = []
            all_valid = True
            error_msgs = []

            for c in components:
                if not c["quote"] or not c["event_log"] or not c["vm_config"]:
                    res = {
                        "name": c["name"],
                        "is_valid": False,
                        "reason": "Missing required verification data (quote, event_log, or vm_config)",
                    }
                else:
                    res = self._verify_component(
                        c["name"],
                        c["quote"],
                        c["event_log"],
                        c["vm_config"],
                        c["app_compose"],
                    )

                results.append(res)
                if not res["is_valid"]:
                    all_valid = False
                    error_msgs.append(f"{c['name']} failed: {res.get('reason')}")

            # 4. Verify Nvidia GPU (if payload provided)
            gpu_result = None
            if nvidia_payload:
                gpu_result = await self.nvidia_verifier.verify(nvidia_payload)

            # 5. Determine level and error message
            model_verified = all_valid
            from ..types import HARDWARE_INTEL_TDX, HARDWARE_NVIDIA_CC

            hardware_types = [HARDWARE_INTEL_TDX]

            if model_verified:
                if gpu_result:
                    if gpu_result.model_verified:
                        hardware_types.append(HARDWARE_NVIDIA_CC)
                    elif gpu_result.error:
                        error_msgs.append(
                            f"GPU verification failed: {gpu_result.error}"
                        )
            else:
                if not error_msgs:
                    error_msgs.append("One or more components failed verification")

            # Prepare refined Phala claims
            instances = self.system_info.get("instances", [])
            image_version = instances[0].get("image_version") if instances else None

            phala_metadata = {
                "app_id": self.system_info.get("app_id"),
                "contract_address": self.system_info.get("contract_address"),
                "image_version": image_version,
                "kms_info": self.system_info.get("kms_info"),
            }

            # Flatten and Clean up component details
            flattened_results = {}
            for r in results:
                comp_name = r["name"]
                flattened = {
                    "is_valid": r["is_valid"],
                    "compose_verified": r["compose_verified"],
                }
                if r.get("reason"):
                    flattened["reason"] = r["reason"]

                # Merge dstack details
                details = r.get("details", {})
                if details:
                    flattened.update(details)
                    # Clean up low level dstack details if needed
                    if "quote" in flattened:
                        del flattened["quote"]

                flattened_results[comp_name] = flattened

            claims = {
                "components": flattened_results,
                "phala": phala_metadata,
            }

            if gpu_result:
                # We already cleaned up nvidia claims in its verifier
                claims["nvidia"] = gpu_result.claims

            # 6. Optional: Intel Trust Authority appraisal for the main model quote
            if instance.get("quote"):
                try:
                    quote_bytes = bytes.fromhex(instance["quote"])
                    ita_claims = await IntelTdxVerifier.verify_with_ita(quote_bytes)
                    if ita_claims:
                        claims["intel_trust_authority"] = ita_claims
                except Exception as e:
                    logger.warning(f"ITA appraisal failed in PhalaCloudVerifier: {e}")

            return VerificationResult(
                model_verified=model_verified,
                provider="phala",
                timestamp=time.time(),
                hardware_type=hardware_types,
                claims=claims,
                error="; ".join(error_msgs) if error_msgs else None,
            )

        except Exception as e:
            logger.exception("Phala Cloud verification failed")
            return VerificationResult(
                model_verified=False,
                provider="phala",
                timestamp=time.time(),
                hardware_type=[],
                claims={},
                error=str(e),
            )
