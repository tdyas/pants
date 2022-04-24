# Copyright 2022 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).
from __future__ import annotations

import pytest

from pants.backend.ruby.lint.rubocop import skip_field
from pants.backend.ruby.lint.rubocop.rules import RuboCopFieldSet, RuboCopRequest
from pants.backend.ruby.lint.rubocop.rules import rules as rubocop_rules
from pants.backend.ruby.target_types import RubySourcesGeneratorTarget, RubySourceTarget
from pants.backend.ruby.target_types import rules as target_types_rules
from pants.backend.ruby.util_rules import ruby_binaries
from pants.build_graph.address import Address
from pants.core.goals.fmt import FmtResult
from pants.core.util_rules import config_files, source_files, system_binaries
from pants.core.util_rules.source_files import SourceFiles, SourceFilesRequest
from pants.engine.fs import CreateDigest, Digest, FileContent
from pants.engine.internals.native_engine import Snapshot
from pants.engine.rules import QueryRule
from pants.engine.target import Target
from pants.testutil.rule_runner import PYTHON_BOOTSTRAP_ENV, RuleRunner


@pytest.fixture
def rule_runner() -> RuleRunner:
    rule_runner = RuleRunner(
        rules=[
            *rubocop_rules(),
            *config_files.rules(),
            *target_types_rules(),
            *skip_field.rules(),
            *system_binaries.rules(),
            *ruby_binaries.rules(),
            *source_files.rules(),
            QueryRule(FmtResult, (RuboCopRequest,)),
            QueryRule(SourceFiles, (SourceFilesRequest,)),
        ],
        target_types=[RubySourceTarget, RubySourcesGeneratorTarget],
    )
    rule_runner.set_options(
        ["--no-process-cleanup"],
        env_inherit=PYTHON_BOOTSTRAP_ENV,
    )
    return rule_runner


GOOD_FILE = """\
def some_function
  assert true
end
"""

BAD_FILE = """\
def some_function
assert true
end
"""

FIXED_BAD_FILE = """\
def some_function
    assert true
end
"""


def run_rubocop(rule_runner: RuleRunner, targets: list[Target]) -> FmtResult:
    for tgt in targets:
        print(f"tgt: {tgt}")
    field_sets = [RuboCopFieldSet.create(tgt) for tgt in targets]
    input_sources = rule_runner.request(
        SourceFiles,
        [
            SourceFilesRequest(field_set.source for field_set in field_sets),
        ],
    )
    fmt_result = rule_runner.request(
        FmtResult,
        [
            RuboCopRequest(field_sets, snapshot=input_sources.snapshot),
        ],
    )
    return fmt_result


def get_snapshot(rule_runner: RuleRunner, source_files: dict[str, str]) -> Snapshot:
    files = [FileContent(path, content.encode()) for path, content in source_files.items()]
    digest = rule_runner.request(Digest, [CreateDigest(files)])
    return rule_runner.request(Snapshot, [digest])


def test_passing(rule_runner: RuleRunner) -> None:
    rule_runner.write_files({"foo.rb": GOOD_FILE, "BUILD": "ruby_sources(name='t')"})
    tgt = rule_runner.get_target(Address("", target_name="t", relative_file_path="foo.rb"))
    fmt_result = run_rubocop(rule_runner, [tgt])
    assert fmt_result.output == get_snapshot(rule_runner, {"foo.rb": GOOD_FILE})
    assert fmt_result.did_change is False


def test_failing(rule_runner: RuleRunner) -> None:
    rule_runner.write_files({"bar.rb": BAD_FILE, "BUILD": "ruby_sources(name='t')"})
    tgt = rule_runner.get_target(Address("", target_name="t", relative_file_path="bar.rb"))
    fmt_result = run_rubocop(rule_runner, [tgt])
    assert fmt_result.output == get_snapshot(rule_runner, {"bar.rb": FIXED_BAD_FILE})
    assert fmt_result.did_change is True


def test_multiple_targets(rule_runner: RuleRunner) -> None:
    rule_runner.write_files(
        {"foo.rb": GOOD_FILE, "bar.rb": BAD_FILE, "BUILD": "ruby_sources(name='t')"}
    )
    tgts = [
        rule_runner.get_target(Address("", target_name="t", relative_file_path="foo.rb")),
        rule_runner.get_target(Address("", target_name="t", relative_file_path="bar.rb")),
    ]
    fmt_result = run_rubocop(rule_runner, tgts)
    assert fmt_result.output == get_snapshot(
        rule_runner, {"foo.rb": GOOD_FILE, "bar.rb": FIXED_BAD_FILE}
    )
    assert fmt_result.did_change is True
