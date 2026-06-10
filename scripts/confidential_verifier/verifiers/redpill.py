import logging
import time
import json
import requests
from typing import Dict, Any, Optional, List
from .base import Verifier
from .phala import PhalaCloudVerifier
from .tinfoil import TinfoilTdxVerifier
from .nearai import NearAICloudVerifier
from .dstack import verify_report_data
from ..types import VerificationResult

logger = logging.getLogger(__name__)


class RedpillVerifier(Verifier):
    """
    Verifier for Redpill models.

    Redpill models are Phala Cloud apps, so this verifier uses PhalaCloudVerifier
    internally to verify the dstack TEE environment (App/KMS/Gateway components).
    Additionally verifies nvidia GPU attestation and report data binding.
    """

    def __init__(self, dstack_verifier_url: Optional[str] = None):
        self.dstack_verifier_url = dstack_verifier_url

    @staticmethod
    def _extract_report_data_from_quote(intel_quote: str) -> Optional[str]:
        """
        Extract report_data (64 bytes) from TDX quote for nonce/address binding check.
        The Redpill intel_quote contains the nonce/address that was passed to the API.
        """
        try:
            quote_bytes = bytes.fromhex(intel_quote)
            # Header is 48 bytes, body starts at 48, report_data is at offset 520-584 in body
            body = quote_bytes[48 : 48 + 584]
            report_data = body[520:584]
            return report_data.hex()
        except Exception:
            return None

    @staticmethod
    def get_redpill_models() -> List[Dict[str, Any]]:
        """Fetches running models from Redpill API."""
        url = "https://api.redpill.ai/v1/models"
        try:
            response = requests.get(url)
            response.raise_for_status()
            data = response.json()
            return data.get("data", [])
        except Exception as e:
            logger.warning(f"Failed to fetch Redpill models: {e}")
            return []

    def _get_model_info(self, model_id: str) -> Optional[Dict[str, Any]]:
        """Look up the model info for a given Redpill model_id."""
        models = self.get_redpill_models()
        for model in models:
            if model.get("id") == model_id:
                return model
        return None

    def _get_app_id_for_model(self, model_id: str) -> Optional[str]:
        """Look up the Phala app_id for a given Redpill model_id."""
        model = self._get_model_info(model_id)
        if model:
            return model.get("metadata", {}).get("appid")
        return None

    async def verify(self, report_data: Dict[str, Any]) -> VerificationResult:
        """
        Verify a Redpill model attestation report.

        Args:
            report_data: Raw attestation report from Redpill API containing:
                - model_id or model: The model identifier
                - intel_quote: TDX quote (optional, for report data verification)
                - nvidia_payload: GPU attestation payload
                - signing_address: Address for report data binding
                - request_nonce: Nonce for report data binding

        Returns:
            VerificationResult with verification status and claims.
        """
        try:
            # 1. Extract model_id and look up model info
            model_id = report_data.get("model_id") or report_data.get("model")
            if not model_id:
                return VerificationResult(
                    model_verified=False,
                    provider="redpill",
                    timestamp=time.time(),
                    hardware_type=[],
                    claims={},
                    error="Missing model_id in report data",
                )

            model_info = self._get_model_info(model_id)
            if not model_info:
                return VerificationResult(
                    model_verified=False,
                    provider="redpill",
                    timestamp=time.time(),
                    hardware_type=[],
                    claims={"model_id": model_id},
                    error=f"Could not find model info for model {model_id}",
                )

            providers = model_info.get("providers", [])

            # --- Tinfoil Distribution ---
            if "tinfoil" in providers:
                # Mapping tinfoil model ID
                tinfoil_mapping = {
                    "qwen/qwen3-coder-480b-a35b-instruct": "qwen3-coder-480b",
                    "deepseek/deepseek-r1-0528": "deepseek-r1-0528",
                    "meta-llama/llama-3.3-70b-instruct": "llama3-3-70b",
                    "moonshotai/kimi-k2-thinking": "kimi-k2-thinking",
                }
                tinfoil_id = tinfoil_mapping.get(model_id)
                if not tinfoil_id:
                    return VerificationResult(
                        model_verified=False,
                        provider="redpill",
                        timestamp=time.time(),
                        hardware_type=[],
                        claims={"model_id": model_id, "providers": providers},
                        error=f"No Tinfoil mapping for model {model_id}",
                    )

                # Fetch repo from TinfoilProvider
                from ..providers.tinfoil import TinfoilProvider

                tinfoil_provider = TinfoilProvider()
                repo = None
                try:
                    with open(tinfoil_provider.config_path, "r") as f:
                        import yaml

                        config = yaml.safe_load(f)
                        repo = config.get("models", {}).get(tinfoil_id, {}).get("repo")
                except:
                    pass

                tinfoil_verifier = TinfoilTdxVerifier()
                intel_quote = report_data.get("intel_quote")
                result = await tinfoil_verifier.verify(
                    {"quote": intel_quote, "repo": repo, "model_id": tinfoil_id}
                )
                result.provider = "redpill"
                result.model_id = model_id
                result.claims["model_provider"] = "tinfoil"
                result.claims["tinfoil_model_id"] = tinfoil_id
                return result

            # --- NearAI Distribution ---
            if "near-ai" in providers:
                # Mapping near-ai model ID
                nearai_mapping = {
                    "z-ai/glm-4.6": "zai-org/GLM-4.6",
                    "qwen/qwen3-30b-a3b-instruct-2507": "Qwen/Qwen3-30B-A3B-Instruct-2507",
                    "deepseek/deepseek-chat-v3.1": "deepseek-ai/DeepSeek-V3.1",
                }
                nearai_id = nearai_mapping.get(model_id)
                if not nearai_id:
                    return VerificationResult(
                        model_verified=False,
                        provider="redpill",
                        timestamp=time.time(),
                        hardware_type=[],
                        claims={"model_id": model_id, "providers": providers},
                        error=f"No NearAI mapping for model {model_id}",
                    )

                nearai_verifier = NearAICloudVerifier(
                    dstack_verifier_url=self.dstack_verifier_url
                )
                # NearAI verifier expects the raw data from NearAI API
                raw_data = report_data.get("raw") or report_data

                # For NearAI resold via Redpill, we only care about the first model attestation
                if isinstance(raw_data, dict) and "model_attestations" in raw_data:
                    model_atts = raw_data.get("model_attestations", [])
                    if len(model_atts) > 1:
                        raw_data = raw_data.copy()
                        raw_data["model_attestations"] = [model_atts[0]]

                result = await nearai_verifier.verify(raw_data)
                result.provider = "redpill"
                result.model_id = model_id
                result.claims["nearai_model_id"] = nearai_id
                result.claims["model_provider"] = "nearai"
                return result

            # --- Phala (Default) Distribution ---
            if "phala" in providers or not providers:
                app_id = model_info.get("metadata", {}).get("appid")
                if not app_id:
                    return VerificationResult(
                        model_verified=False,
                        provider="redpill",
                        timestamp=time.time(),
                        hardware_type=[],
                        claims={"model_id": model_id},
                        error=f"Could not find Phala app_id for model {model_id}",
                    )

                # 2. Parse nvidia_payload if it's a string
                nvidia_payload = report_data.get("nvidia_payload")
                if isinstance(nvidia_payload, str):
                    try:
                        nvidia_payload = json.loads(nvidia_payload)
                    except Exception as e:
                        logger.warning(f"Failed to parse nvidia_payload: {e}")
                        nvidia_payload = None

                # 3. Use PhalaCloudVerifier for TEE infrastructure verification
                phala_verifier = PhalaCloudVerifier(
                    app_id=app_id,
                    dstack_verifier_url=self.dstack_verifier_url,
                )
                result = await phala_verifier.verify(nvidia_payload=nvidia_payload)
                result.provider = "redpill"
                result.model_id = model_id

                # Update Phala metadata
                if "phala" in result.claims:
                    result.claims["phala"]["model_provider"] = "phala"
                else:
                    result.claims["model_provider"] = "phala"

                # If TEE verification failed, return early
                if not result.model_verified:
                    return result

                # 4. Verify report data binding (nonce/address) against the Redpill intel_quote
                request_nonce = report_data.get("request_nonce")
                signing_address = report_data.get("signing_address")
                intel_quote = report_data.get("intel_quote")

                # Extract report_data from the Redpill intel_quote
                report_data_hex = None
                if intel_quote:
                    report_data_hex = self._extract_report_data_from_quote(intel_quote)

                if report_data_hex and request_nonce and signing_address:
                    rd_check = verify_report_data(
                        report_data_hex, signing_address, request_nonce
                    )

                    if not rd_check["valid"]:
                        return VerificationResult(
                            model_verified=False,
                            provider="redpill",
                            timestamp=result.timestamp,
                            hardware_type=result.hardware_type,
                            model_id=model_id,
                            request_nonce=request_nonce,
                            signing_address=signing_address,
                            claims=result.claims,
                            error=f"Report data binding failed: {rd_check.get('error') or 'Address/Nonce mismatch'}",
                        )

                    result.request_nonce = request_nonce
                    result.signing_address = signing_address

                return result

            # Not verifiable
            return VerificationResult(
                model_verified=False,
                provider="redpill",
                timestamp=time.time(),
                hardware_type=[],
                claims={"model_id": model_id, "providers": providers},
                error=f"Model provided by {providers} is not verifiable",
            )

        except Exception as e:
            logger.exception("Redpill verification failed")
            # Try to get model provider from model_info if available
            model_provider = None
            try:
                model_id = report_data.get("model_id") or report_data.get("model")
                if model_id:
                    model_info = self._get_model_info(model_id)
                    if model_info:
                        providers = model_info.get("providers", [])
                        if "phala" in providers or not providers:
                            model_provider = "phala"
                        elif "tinfoil" in providers:
                            model_provider = "tinfoil"
                        elif "near-ai" in providers:
                            model_provider = "nearai"
            except Exception:
                pass

            claims = {"model_provider": model_provider} if model_provider else {}
            return VerificationResult(
                model_verified=False,
                provider="redpill",
                timestamp=time.time(),
                hardware_type=[],
                claims=claims,
                error=str(e),
            )
