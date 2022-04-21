# Copyright 2022 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

import pytest

from pants.backend.ruby.goals import tailor
from pants.backend.ruby.goals.tailor import PutativeRubyTargetsRequest
from pants.backend.ruby.target_types import RubySourcesGeneratorTarget
from pants.core.goals.tailor import (
    AllOwnedSources,
    PutativeTarget,
    PutativeTargets,
    PutativeTargetsSearchPaths,
)
from pants.engine.rules import QueryRule
from pants.testutil.rule_runner import RuleRunner


@pytest.fixture
def rule_runner() -> RuleRunner:
    return RuleRunner(
        rules=[
            *tailor.rules(),
            QueryRule(PutativeTargets, (PutativeRubyTargetsRequest, AllOwnedSources)),
        ],
        target_types=[RubySourcesGeneratorTarget],
    )


def test_find_putative_targets(rule_runner: RuleRunner) -> None:
    rule_runner.write_files(
        {
            "src/ruby/owned/BUILD": "ruby_sources()\n",
            "src/ruby/owned/OwnedFile.rb": "",
            "src/ruby/unowned/UnownedFile.rb": "\n",
        }
    )
    putative_targets = rule_runner.request(
        PutativeTargets,
        [
            PutativeRubyTargetsRequest(PutativeTargetsSearchPaths(("",))),
            AllOwnedSources(["src/ruby/owned/OwnedFile.rb"]),
        ],
    )
    assert (
        PutativeTargets(
            [
                PutativeTarget.for_target_type(
                    RubySourcesGeneratorTarget,
                    "src/ruby/unowned",
                    "unowned",
                    ["UnownedFile.rb"],
                ),
            ]
        )
        == putative_targets
    )
