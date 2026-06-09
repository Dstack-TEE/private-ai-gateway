import logging
import requests
import json
import time
import jwt
from typing import Dict, Any
from ..types import VerificationResult, HARDWARE_NVIDIA_CC
from .base import Verifier

logger = logging.getLogger(__name__)


class NvidiaGpuVerifier(Verifier):
    def __init__(self):
        self.nras_url = "https://nras.attestation.nvidia.com/v3/attest/gpu"

    async def verify(self, payload: Dict[str, Any]) -> VerificationResult:
        try:
            response = requests.post(
                self.nras_url,
                json=payload,
                headers={
                    "accept": "application/json",
                    "content-type": "application/json",
                },
            )

            if response.status_code != 200:
                raise Exception(f"NRAS responded with status {response.status_code}")

            tokens = response.json()
            if not isinstance(tokens, list) or len(tokens) < 1:
                raise Exception("Invalid response format: expected array")

            # Platform token at index 0
            platform_entry = tokens[0]
            if not isinstance(platform_entry, list) or platform_entry[0] != "JWT":
                raise Exception("Invalid platform token format")

            platform_jwt = platform_entry[1]
            platform_claims = self._decode_jwt(platform_jwt)

            is_valid = platform_claims.get("x-nvidia-overall-att-result") is True

            gpu_claims = {}
            if len(tokens) > 1 and isinstance(tokens[1], dict):
                gpu_tokens = tokens[1]
                if gpu_tokens:
                    first_gpu_key = list(gpu_tokens.keys())[0]
                    gpu_jwt = gpu_tokens[first_gpu_key]
                    if isinstance(gpu_jwt, str):
                        gpu_claims = self._decode_jwt(gpu_jwt)

            claims = {**platform_claims, **gpu_claims}

            # Only claim hardware_type if verification succeeded
            return VerificationResult(
                model_verified=is_valid,
                provider="nvidia",
                timestamp=time.time(),
                hardware_type=[HARDWARE_NVIDIA_CC] if is_valid else [],
                claims=claims,
                error=None if is_valid else "Nvidia attestation result is false",
            )

        except Exception as e:
            logger.exception("Nvidia verification failed")
            return VerificationResult(
                model_verified=False,
                provider="nvidia",
                timestamp=time.time(),
                hardware_type=[],
                claims={},
                error=str(e),
            )

    def _decode_jwt(self, token: str) -> Dict[str, Any]:
        try:
            # TODO: Fetch JWKS from Nvidia and enable signature verification for better security.
            return jwt.decode(
                token,
                options={"verify_signature": False},
                algorithms=["RS256", "ES256", "ES384", "PS256"],
            )
        except Exception as e:
            logger.error(f"Failed to decode JWT: {e}")
            return {}
