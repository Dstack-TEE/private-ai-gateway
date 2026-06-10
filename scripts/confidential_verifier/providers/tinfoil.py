import requests
import base64
import gzip
import yaml
import os
from typing import List, Dict, Any, Optional
from .base import ServiceProvider
from ..types import AttestationReport
from ..verifiers.tinfoil import TinfoilSevVerifier
from ..verifiers import Verifier


class TinfoilProvider(ServiceProvider):
    """
    Provider for Tinfoil attestation.

    Tinfoil runs enclaves on both Intel TDX and AMD SEV-SNP hardware.
    The provider tries the specific enclave endpoint first, then falls back
    to the central router.
    """

    ROUTER_URL = "https://inference.tinfoil.sh"
    ROUTER_REPO = "tinfoilsh/confidential-model-router"

    def __init__(self, config_path: Optional[str] = None):
        if not config_path:
            config_path = os.path.join(
                os.path.dirname(__file__), "../../config/tinfoil_config.yml"
            )
            if not os.path.exists(config_path):
                config_path = "config/tinfoil_config.yml"

        self.config_path = config_path
        self._cache = None

    def get_verifier(self) -> Verifier:
        return TinfoilSevVerifier()

    def _get_model_config(self) -> Dict[str, Dict[str, Any]]:
        """Load model configuration from YAML file."""
        if self._cache is not None:
            return self._cache

        try:
            with open(self.config_path, "r") as f:
                config = yaml.safe_load(f)

            self._cache = config.get("models", {})
            return self._cache
        except Exception as e:
            print(f"Failed to load Tinfoil config from {self.config_path}: {e}")
            return {}

    def _fetch_attestation(self, url: str) -> Dict[str, Any]:
        """Fetch and parse attestation from a URL."""
        response = requests.get(url, timeout=10)
        response.raise_for_status()
        return response.json()

    def fetch_report(self, model_id: str) -> AttestationReport:
        """
        Fetch attestation report from Tinfoil.

        Tries the specific enclave endpoint first, then falls back to the router.
        Supports both Intel TDX and AMD SEV-SNP formats.
        """
        model_config = self._get_model_config()
        repo = None
        enclave_url = None

        # Try to get enclave endpoint from config
        if model_id in model_config:
            repo = model_config[model_id].get("repo")
            enclaves = model_config[model_id].get("enclaves", [])
            if enclaves:
                enclave_url = f"https://{enclaves[0]}/.well-known/tinfoil-attestation"

        data = None
        used_router = False

        # Try enclave endpoint first
        if enclave_url:
            try:
                print(f"[Tinfoil] Trying enclave: {enclave_url}")
                data = self._fetch_attestation(enclave_url)
                print(f"[Tinfoil] Successfully fetched from enclave")
            except Exception as e:
                print(f"[Tinfoil] Enclave failed ({e}), falling back to router")

        # Fall back to router
        if data is None:
            url = f"{self.ROUTER_URL}/.well-known/tinfoil-attestation"
            print(f"[Tinfoil] Fetching from router: {url}")
            data = self._fetch_attestation(url)
            used_router = True
            # When using router, use router's repo instead of model's repo
            repo = self.ROUTER_REPO
            print(f"[Tinfoil] Using router repo for verification: {repo}")

        fmt = data.get("format", "")
        body = data.get("body")

        if not body:
            raise Exception("Tinfoil response missing body")

        # Decompress the quote
        compressed_quote = base64.b64decode(body)
        quote_bytes = gzip.decompress(compressed_quote)

        # Detect attestation format
        if "sev-snp" in fmt:
            quote_type = "sev-snp"
            print(f"[Tinfoil] Detected AMD SEV-SNP attestation")
        elif "tdx" in fmt:
            quote_type = "tdx"
            print(f"[Tinfoil] Detected Intel TDX attestation")
        else:
            quote_type = "unknown"
            print(f"[Tinfoil] Unknown attestation format: {fmt}")

        data["repo"] = repo
        data["model_id"] = model_id
        data["quote_type"] = quote_type
        data["used_router"] = used_router

        return AttestationReport(
            provider="tinfoil",
            model_id=model_id,
            intel_quote=quote_bytes.hex(),  # Field name kept for compatibility
            nvidia_payload=None,
            raw=data,
        )

    def list_models(self) -> List[str]:
        """List models from config file."""
        return list(self._get_model_config().keys())
