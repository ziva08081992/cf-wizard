"""Domain model for the contents of a technical CF."""

from dataclasses import dataclass
from typing import Iterable


@dataclass(frozen=True)
class DependencyCause:
    owner: str
    required: bool


@dataclass(frozen=True)
class Stub:
    name: str
    causes: tuple[DependencyCause, ...]

    @property
    def required(self) -> bool:
        return any(cause.required for cause in self.causes)

    @property
    def referred_by(self) -> str:
        return self.causes[0].owner


@dataclass(frozen=True)
class PackageComposition:
    selected: tuple[str, ...]
    stubs: tuple[Stub, ...]

    def exclusions(self, requested: Iterable[str]) -> frozenset[str]:
        excluded = frozenset(requested)
        known = {stub.name for stub in self.stubs}
        if excluded - known:
            raise ValueError("Исключать можно заглушки из текущего предпросмотра.")
        required = sorted(stub.name for stub in self.stubs if stub.required and stub.name in excluded)
        if required:
            raise ValueError("Для этих заглушек есть обязательные ссылки.\n" + "\n".join(required))
        return excluded
