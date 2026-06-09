import requests
import secrets
import json
from typing import List
from .base import ServiceProvider
from ..types import AttestationReport


class RedpillProvider(ServiceProvider):
    """
    Provider for Redpill AI models.

    Redpill models are Phala Cloud apps. The SDK handles verification
    specially using RedpillVerifier which uses PhalaCloudVerifier internally.
    """

    def __init__(self):
        self.api_base = "https://api.redpill.ai/v1"

    def fetch_report(self, model_id: str) -> AttestationReport:
        url = f"{self.api_base}/attestation/report"
        nonce = secrets.token_hex(32)
        print(
            f"[Redpill] Fetching from {url} for model {model_id} with nonce {nonce[:8]}..."
        )

        response = requests.get(url, params={"model": model_id, "nonce": nonce})
        response.raise_for_status()
        data = response.json()

        model_atts = data.get("model_attestations", [])
        intel_quote = data.get("intel_quote")
        if not intel_quote and model_atts:
            intel_quote = model_atts[0].get("intel_quote")

        if not intel_quote:
            raise Exception("Redpill report missing intel_quote")

        nvidia_payload = data.get("nvidia_payload")
        if isinstance(nvidia_payload, str):
            try:
                nvidia_payload = json.loads(nvidia_payload)
            except:
                pass

        # Include model_id in raw data for verifier to look up app_id
        data["model_id"] = model_id

        return AttestationReport(
            provider="redpill",
            model_id=model_id,
            intel_quote=intel_quote,
            request_nonce=nonce,
            nvidia_payload=nvidia_payload,
            raw=data,
        )

    def list_models(self) -> List[str]:
        url = f"{self.api_base}/models"
        print(f"[Redpill] Fetching models from {url}")
        response = requests.get(url)
        response.raise_for_status()
        data = response.json()

        models = data if isinstance(data, list) else data.get("data", [])
        return [m["id"] for m in models]
