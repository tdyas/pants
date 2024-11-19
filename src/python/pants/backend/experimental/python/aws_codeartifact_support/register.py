# Copyright 2024 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

from __future__ import annotations

from typing import Iterable

from pants.backend.python.util_rules import aws_codeartifact
from pants.engine.rules import Rule
from pants.engine.unions import UnionRule

def rules() -> Iterable[Rule | UnionRule]:
    return aws_codeartifact.rules()
