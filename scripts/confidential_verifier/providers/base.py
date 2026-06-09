from ..types import AttestationReport
from ..verifiers import Verifier, IntelTdxVerifier
from typing import List


class ServiceProvider:
    def fetch_report(self, model_id: str) -> AttestationReport:
        raise NotImplementedError

    def list_models(self) -> List[str]:
        raise NotImplementedError

    def get_verifier(self) -> Verifier:
        """Return the appropriate verifier for this provider."""
        return IntelTdxVerifier()
