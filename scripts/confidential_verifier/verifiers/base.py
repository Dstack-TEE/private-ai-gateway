from typing import Any
from ..types import VerificationResult


class Verifier:
    async def verify(self, evidence: Any) -> VerificationResult:
        raise NotImplementedError
